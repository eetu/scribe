//! Full-stack smoke test.
//!
//! Boots backend + shim + press + shelf (see [`scribe_e2e::Stack`]) and
//! probes the cross-service wiring that per-crate tests can't reach: the
//! `/status` health fan-out, the DEV_AUTH session flow, and each sidecar's
//! auth boundary. No Audible credentials or DRM material — this guards
//! stack integrity, not the download/convert path.
//!
//! `#[ignore]` so `cargo test --workspace` (which doesn't build the service
//! binaries first) skips it. Run it explicitly:
//!     cargo build -p scribe-backend -p scribe-press -p scribe-shelf
//!     cargo test -p scribe-e2e -- --ignored

use scribe_e2e::Stack;

#[tokio::test]
#[ignore = "boots the full stack; run via the e2e CI job or `cargo test -p scribe-e2e -- --ignored`"]
async fn full_stack_smoke() {
    let s = Stack::start().await;

    // --- /status: cross-service health fan-out ---------------------------
    let status = s.json(&format!("{}/status", s.backend)).await;
    assert_eq!(status["shim_healthy"], true, "shim should be healthy");
    assert_eq!(status["press_healthy"], true, "press should be healthy");
    assert_eq!(status["shelf_healthy"], true, "shelf should be healthy");
    assert_eq!(status["dev_auth"], true, "dev_auth should be on");
    assert_eq!(
        status["oidc_configured"], false,
        "oidc must be unconfigured in this hermetic run"
    );

    // --- DEV_AUTH session flow -------------------------------------------
    assert_eq!(
        s.code(&format!("{}/api/me", s.backend)).await,
        401,
        "/api/me must reject without a session cookie"
    );
    // Login mints the signed session cookie; the client's cookie store
    // carries it on the subsequent /api/me + /api/library calls.
    s.http
        .get(format!("{}/auth/login?username=smoke", s.backend))
        .send()
        .await
        .expect("login");
    let me = s.json(&format!("{}/api/me", s.backend)).await;
    assert_eq!(me["sub"], "smoke", "session should resolve the dev user");
    let library = s.json(&format!("{}/api/library", s.backend)).await;
    assert_eq!(library["items"].as_array().map(|a| a.len()), Some(0), "library empty");

    // --- press auth boundary ---------------------------------------------
    assert_eq!(s.code(&format!("{}/health", s.press)).await, 200, "press /health anon");
    let press_jobs = s
        .http
        .post(format!("{}/jobs", s.press))
        .send()
        .await
        .expect("press jobs")
        .status()
        .as_u16();
    assert_eq!(press_jobs, 401, "press /jobs must require the bearer token");

    // --- shim ------------------------------------------------------------
    assert_eq!(s.code(&format!("{}/health", s.shim)).await, 200, "shim /health");
    let accounts = s.json(&format!("{}/accounts", s.shim)).await;
    assert_eq!(accounts.as_array().map(|a| a.len()), Some(0), "no accounts linked");

    // --- shelf auth + library --------------------------------------------
    assert_eq!(s.code(&format!("{}/ping", s.shelf)).await, 200, "shelf /ping anon");
    assert_eq!(
        s.code(&format!("{}/api/libraries", s.shelf)).await,
        401,
        "shelf /api/libraries must require the api key"
    );
    // The ?token= query is scoped to the audio stream route only — it must
    // NOT authorize the JSON/metadata routes (keeps the key out of logs).
    assert_eq!(
        s.code(&format!("{}/api/libraries?token={}", s.shelf, s.shelf_key)).await,
        401,
        "query token must not authorize JSON routes"
    );
    let libs = s
        .http
        .get(format!("{}/api/libraries", s.shelf))
        .header("Authorization", format!("Bearer {}", s.shelf_key))
        .send()
        .await
        .expect("shelf libraries")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert!(
        libs["libraries"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "shelf should expose at least one library"
    );
}
