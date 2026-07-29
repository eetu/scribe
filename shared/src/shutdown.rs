//! Graceful shutdown for the three axum binaries.
//!
//! Every service runs as PID 1 in its own container, and PID 1 gets no
//! default signal actions from the kernel — a process with no SIGTERM handler
//! installed simply *ignores* it. So `systemctl restart` (and every deploy)
//! used to sit through podman's 10 s stop timeout and then SIGKILL:
//!
//! ```text
//! StopSignal SIGTERM failed to stop container shelf in 10 seconds, resorting to SIGKILL
//! shelf.service: Main process exited, code=exited, status=137/n/a
//! ```
//!
//! SIGKILL severs in-flight responses, which for shelf means a Watch or phone
//! download dies mid-transfer. The quadlets carry `AutoUpdate=registry` +
//! `Pull=newer`, so that can happen unattended, with nobody watching.
//!
//! Handling the signal fixes both halves: the listener stops accepting, and
//! in-flight requests get to finish instead of being cut.
//!
//! The drain is deliberately bounded. `with_graceful_shutdown` on its own
//! waits for every open connection, and a 1 GB audiobook over a slow link can
//! hold one for many minutes — long enough that podman's stop timeout would
//! SIGKILL us anyway, and long enough to wedge a deploy. So after the signal a
//! watchdog exits the process once the grace window expires. A download cut
//! that way is recoverable: the shelf stream serves validators and honours
//! `If-Range`, so the client resumes from its partial bytes rather than
//! starting over.
//!
//! The default window sits just under podman's 10 s stop timeout so this works
//! with the quadlets as they are. If you raise [`GRACE_ENV`] past that, raise
//! the container's `StopTimeout` to match or podman kills us mid-drain and
//! we're back to severed transfers.

use std::time::Duration;

/// Env var overriding the drain window, in seconds.
pub const GRACE_ENV: &str = "SCRIBE_SHUTDOWN_GRACE_S";

/// Default drain window: under podman's 10 s stop timeout, so the drain always
/// completes on its own terms rather than being SIGKILLed halfway through.
///
/// Deliberately not sized to cover a whole download — a 1 GB book is minutes,
/// and no deploy should wait that long. It covers what actually finishes
/// quickly (JSON, metadata, covers) and lets long transfers be cut and
/// resumed. Raising it past 9 s only helps if the quadlet's `StopTimeout` goes
/// up to match, otherwise podman kills us mid-drain regardless.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(9);

/// The drain window, from [`GRACE_ENV`] or [`DEFAULT_GRACE`].
pub fn grace_from_env() -> Duration {
    std::env::var(GRACE_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_GRACE)
}

/// Resolves on SIGTERM or SIGINT, then arms a watchdog that force-exits once
/// `grace` has elapsed. Pass to `axum::serve(..).with_graceful_shutdown(..)`.
///
/// Exiting 0 rather than aborting is deliberate: a bounded drain is a normal,
/// successful stop, and `Restart=always` shouldn't see it as a crash.
pub async fn signal(grace: Duration) {
    let received = wait_for_signal().await;
    tracing::info!(
        signal = received,
        grace_secs = grace.as_secs(),
        "shutdown signal received — draining in-flight requests"
    );
    tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        tracing::warn!(
            grace_secs = grace.as_secs(),
            "drain window expired with requests still open — exiting anyway; \
             clients that support resume will pick up where they left off"
        );
        std::process::exit(0);
    });
}

/// [`signal`] with the window from the environment.
pub async fn signal_from_env() {
    signal(grace_from_env()).await
}

#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{signal as unix_signal, SignalKind};
    // A failure here means the process would silently keep ignoring SIGTERM,
    // i.e. back to the SIGKILL behaviour this module exists to remove — so it
    // is worth failing loudly at startup rather than degrading quietly.
    let mut term = unix_signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = unix_signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() -> &'static str {
    tokio::signal::ctrl_c()
        .await
        .expect("install ctrl-c handler");
    "ctrl-c"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_defaults_when_unset_or_junk() {
        // Serial within one test: these mutate process-wide env.
        unsafe { std::env::remove_var(GRACE_ENV) };
        assert_eq!(grace_from_env(), DEFAULT_GRACE);

        unsafe { std::env::set_var(GRACE_ENV, "not-a-number") };
        assert_eq!(grace_from_env(), DEFAULT_GRACE);

        unsafe { std::env::set_var(GRACE_ENV, "45") };
        assert_eq!(grace_from_env(), Duration::from_secs(45));

        // A zero window is honoured — "drop everything now" is a legitimate
        // ask for a service with no long-lived responses.
        unsafe { std::env::set_var(GRACE_ENV, "0") };
        assert_eq!(grace_from_env(), Duration::ZERO);

        unsafe { std::env::remove_var(GRACE_ENV) };
    }
}
