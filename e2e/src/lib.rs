//! E2E harness — boots the full scribe stack on loopback for smoke tests.
//!
//! Spawns backend + shim + press + shelf with DEV_AUTH and temp dirs, waits
//! for each to listen, and exposes a [`reqwest::Client`] with a cookie store
//! for the session flow. [`Stack`]'s `Drop` kills every child process and
//! removes the temp tree, so a panicking test never leaks a server.
//!
//! The Rust binaries must already be built (`cargo build -p scribe-backend
//! -p scribe-press -p scribe-shelf`); `shim` is launched via `uv run shim`.
//! The CI `e2e` job does the build first.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

// Fixed loopback ports, offset from the dev defaults (3003–3006) so a
// running dev stack doesn't collide with a test run.
const BACKEND_PORT: u16 = 3913;
const SHIM_PORT: u16 = 3914;
const PRESS_PORT: u16 = 3915;
const SHELF_PORT: u16 = 3916;

const PRESS_TOKEN: &str = "smoke-press-token-0123456789abcdef";
const SHELF_KEY: &str = "smoke-shelf-apikey-0123456789abcdef";

pub struct Stack {
    children: Vec<Child>,
    _tmp: tempfile::TempDir,
    pub http: reqwest::Client,
    pub backend: String,
    pub shim: String,
    pub press: String,
    pub shelf: String,
    pub shelf_key: String,
}

impl Stack {
    /// Boot the whole stack and block until every service is listening.
    pub async fn start() -> Stack {
        let root = workspace_root();
        let tmp = tempfile::tempdir().expect("tempdir");
        let t = tmp.path();
        for sub in ["library", "original", "covers", "press", "shim"] {
            std::fs::create_dir_all(t.join(sub)).expect("mkdir");
        }
        let db = t.join("scribe.db");
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");

        let mut children = Vec::new();

        // shim (Python, via uv) — no Audible creds needed, dev passphrase.
        children.push(
            Command::new("uv")
                .current_dir(root.join("shim"))
                .args(["run", "shim"])
                .env("SHIM_DEV", "1")
                .env("SHIM_HOST", "127.0.0.1")
                .env("SHIM_PORT", SHIM_PORT.to_string())
                .env("SHIM_RELOAD", "0")
                .env("SHIM_DATA_DIR", t.join("shim"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn shim — is `uv` installed?"),
        );

        // press
        children.push(
            Command::new(bin("scribe-press"))
                .env("PRESS_BIND", format!("127.0.0.1:{PRESS_PORT}"))
                .env("PRESS_TOKEN", PRESS_TOKEN)
                .env("PRESS_TMP_DIR", t.join("press"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn press"),
        );

        // backend — creates scribe.db on boot. OIDC_* forced empty so the
        // run is hermetic (assert oidc_configured == false) regardless of
        // any ambient env or .env file.
        children.push(
            Command::new(bin("scribe-backend"))
                .env("DEV_AUTH", "1")
                .env("SCRIBE_BIND", format!("127.0.0.1:{BACKEND_PORT}"))
                .env("SCRIBE_DB_PATH", &db)
                .env("SCRIBE_SHIM_URL", format!("http://127.0.0.1:{SHIM_PORT}"))
                .env("SCRIBE_PRESS_URL", format!("http://127.0.0.1:{PRESS_PORT}"))
                .env("SCRIBE_PRESS_TOKEN", PRESS_TOKEN)
                .env("SCRIBE_SHELF_URL", format!("http://127.0.0.1:{SHELF_PORT}"))
                .env("SCRIBE_LIBRARY_DIR", t.join("library"))
                .env("SCRIBE_ORIGINAL_DIR", t.join("original"))
                .env("SCRIBE_COVERS_DIR", t.join("covers"))
                .env("OIDC_ISSUER", "")
                .env("OIDC_CLIENT_ID", "")
                .env("OIDC_CLIENT_SECRET", "")
                .env("OIDC_REDIRECT_URL", "")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn backend"),
        );

        let backend = format!("http://127.0.0.1:{BACKEND_PORT}");
        let shim = format!("http://127.0.0.1:{SHIM_PORT}");
        let press = format!("http://127.0.0.1:{PRESS_PORT}");
        let shelf = format!("http://127.0.0.1:{SHELF_PORT}");

        wait_listening(&http, &format!("{backend}/status"), "backend").await;
        wait_listening(&http, &format!("{shim}/health"), "shim").await;
        wait_listening(&http, &format!("{press}/health"), "press").await;

        // shelf opens scribe.db read-only; start it after the backend has
        // created the file.
        children.push(
            Command::new(bin("scribe-shelf"))
                .env("SHELF_BIND", format!("127.0.0.1:{SHELF_PORT}"))
                .env("SHELF_API_KEY", SHELF_KEY)
                .env("SHELF_DB_PATH", &db)
                .env("SHELF_LIBRARY_DIR", t.join("library"))
                .env("SHELF_COVERS_DIR", t.join("covers"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn shelf"),
        );
        wait_listening(&http, &format!("{shelf}/ping"), "shelf").await;

        Stack {
            children,
            _tmp: tmp,
            http,
            backend,
            shim,
            press,
            shelf,
            shelf_key: SHELF_KEY.to_string(),
        }
    }

    /// GET a URL and parse the JSON body. Panics on transport/parse error.
    pub async fn json(&self, url: &str) -> Value {
        let r = self.http.get(url).send().await.expect("request");
        r.json().await.expect("json")
    }

    /// GET a URL and return just the status code.
    pub async fn code(&self, url: &str) -> u16 {
        self.http.get(url).send().await.expect("request").status().as_u16()
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is the e2e crate dir; the workspace is its parent.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Locate a built workspace binary next to the test executable
/// (`target/<profile>/<name>`).
fn bin(name: &str) -> PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop(); // the test binary itself
    if dir.ends_with("deps") {
        dir.pop();
    }
    let p = dir.join(name);
    assert!(
        p.exists(),
        "missing binary {name} at {p:?} — run `cargo build -p scribe-backend \
         -p scribe-press -p scribe-shelf` first",
    );
    p
}

async fn wait_listening(http: &reqwest::Client, url: &str, name: &str) {
    for _ in 0..120 {
        if http.get(url).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("{name} never came up at {url}");
}
