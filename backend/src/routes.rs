use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use rusqlite::OptionalExtension;

use crate::auth::AuthProfile;
use crate::error::{AppError, AppResult};
use crate::press::PressClient;
use crate::profile;
use crate::queue::QueueEvent;
use crate::shim::{LoginFinishReq, LoginStartReq, ShimClient};
use crate::state::AppState;
use crate::sync;

pub fn router(state: AppState) -> Router {
    use crate::auth as a;
    use tower_http::services::{ServeDir, ServeFile};

    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./dist".into());
    // SPA fallback: any path that doesn't match a built asset returns
    // index.html so TanStack Router can take over client-side (refreshing
    // on /accounts or /jobs still works).
    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));
    let spa_service = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index));

    Router::new()
        .route("/status", get(status))
        .route("/auth/login", get(a::login))
        .route("/auth/callback", get(a::callback))
        .route("/auth/logout", post(a::logout))
        .route("/api/me", get(me))
        .route("/api/settings", get(get_settings).patch(patch_settings))
        .route("/api/settings/{key}", delete(reset_setting))
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/login/start", post(login_start))
        .route("/api/accounts/login/finish", post(login_finish))
        .route("/api/accounts/{id}/refresh", post(refresh_account))
        .route("/api/accounts/{id}/deregister", post(deregister_account))
        .route("/api/library", get(list_library))
        .route("/api/library/sync", post(sync_library))
        .route("/api/library/reconcile", post(reconcile_route))
        .route("/api/library/refresh", post(refresh_library))
        .route("/api/books/{asin}", delete(remove_book))
        .route("/api/books/{asin}/cover", get(book_cover))
        .route("/api/books/{asin}/refresh", post(refresh_book))
        .route("/api/jobs", get(list_jobs).post(enqueue_job))
        .route("/api/jobs/enqueue_all", post(enqueue_all))
        .route("/api/jobs/{id}/sse", get(job_sse))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/jobs/{id}/reconvert", post(reconvert_job))
        .route("/internal/aaxc/{token}", get(serve_internal_aaxc))
        .fallback_service(spa_service)
        .with_state(state)
}

// ---------- public probes ----------

async fn status(State(state): State<AppState>) -> Json<Value> {
    let shim = ShimClient::new(&state);
    let press = PressClient::new(&state);
    let (shim_healthy, press_health, shelf_healthy) = tokio::join!(
        shim.health(),
        async { press.health().await.unwrap_or(false) },
        async { shelf_health(&state).await },
    );
    Json(json!({
        "service": "scribe",
        "version": env!("CARGO_PKG_VERSION"),
        "shim_url": state.cfg.shim_url,
        "shim_healthy": shim_healthy,
        "press_url": state.cfg.press_url,
        "press_healthy": press_health,
        "shelf_url": state.cfg.shelf_url,
        "shelf_healthy": shelf_healthy,
        "dev_auth": state.cfg.dev_auth,
        "auto_enqueue_default": state.cfg.auto_enqueue_new,
        "library_dir": state.cfg.library_dir,
        "original_dir": state.cfg.original_dir,
        "poll_interval_min_default": state.cfg.poll_interval_min,
    }))
}

/// Best-effort health ping for the optional shelf sidecar. Returns
/// `false` for any non-success — unconfigured, unreachable, timeout,
/// or auth glitch — so the UI can render a single "shelf is live"
/// indicator without branching on the failure mode.
async fn shelf_health(state: &AppState) -> bool {
    let Some(url) = state.cfg.shelf_url.as_deref() else {
        return false;
    };
    let probe = format!("{}/ping", url.trim_end_matches('/'));
    let req = state
        .http
        .get(probe)
        .timeout(std::time::Duration::from_secs(2));
    match req.send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

// ---------- session probe ----------

async fn me(user: AuthProfile, State(state): State<AppState>) -> Json<Value> {
    // Shelf API key is shown to logged-in users only. The /api/me route
    // already requires AuthProfile, so any kanidm-authenticated client
    // can read this and configure their Listen This (or any ABS-compat
    // client) without a separate secret-distribution channel. Stays
    // null when shelf isn't deployed.
    Json(json!({
        "sub": user.sub(),
        "profile_id": user.id(),
        "email": user.profile.email,
        "shelf_url": state.cfg.shelf_url,
        "shelf_api_key": state.cfg.shelf_api_key,
    }))
}

// ---------- per-profile settings ----------
//
// Effective value resolution: profile override (DB) overrides env default.
// Keys exposed here are the user-tunable subset; library/original/template
// stay env-only for now — flipping those at runtime breaks already-written
// paths so they need migration handling first.

const USER_TUNABLE_KEYS: &[&str] = &["auto_enqueue", "poll_interval_min"];

async fn get_settings(user: AuthProfile, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let overrides = profile::all_settings(&state, user.id()).await?;
    let overrides_map: std::collections::HashMap<_, _> = overrides.into_iter().collect();
    let mut out = serde_json::Map::new();
    for key in USER_TUNABLE_KEYS {
        let env_default = env_default_for(&state, key);
        let effective = overrides_map.get(*key).cloned().unwrap_or_else(|| env_default.clone());
        out.insert(
            (*key).into(),
            json!({
                "value": effective,
                "env_default": env_default,
                "overridden": overrides_map.contains_key(*key),
            }),
        );
    }
    Ok(Json(Value::Object(out)))
}

#[derive(Debug, Deserialize)]
struct PatchSettingsIn(std::collections::HashMap<String, String>);

async fn patch_settings(
    user: AuthProfile,
    State(state): State<AppState>,
    Json(body): Json<PatchSettingsIn>,
) -> AppResult<Json<Value>> {
    for (k, v) in &body.0 {
        if !USER_TUNABLE_KEYS.contains(&k.as_str()) {
            return Err(AppError::BadRequest(format!("unknown setting key: {k}")));
        }
        profile::set_setting(&state, user.id(), k, v).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn reset_setting(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> AppResult<Json<Value>> {
    if !USER_TUNABLE_KEYS.contains(&key.as_str()) {
        return Err(AppError::BadRequest(format!("unknown setting key: {key}")));
    }
    profile::delete_setting(&state, user.id(), &key).await?;
    Ok(Json(json!({ "ok": true })))
}

fn env_default_for(state: &AppState, key: &str) -> String {
    match key {
        "auto_enqueue" => state.cfg.auto_enqueue_new.to_string(),
        "poll_interval_min" => state.cfg.poll_interval_min.to_string(),
        _ => String::new(),
    }
}

// ---------- accounts (proxy to shim) ----------

async fn list_accounts(user: AuthProfile, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let summaries = ShimClient::new(&state).list_accounts().await?;
    let pid = user.id();
    let local: std::collections::HashMap<String, (Option<i64>, i64, i64)> = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT a.id,
                        a.last_synced_at,
                        (SELECT COUNT(*) FROM books b WHERE b.account_id = a.id),
                        (SELECT COUNT(*) FROM jobs j
                          WHERE j.account_id = a.id
                            AND j.status NOT IN ('done','failed','cancelled'))
                 FROM accounts a
                 WHERE a.profile_id = ?1",
            )?;
            let rows = stmt.query_map([pid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut m = std::collections::HashMap::new();
            for row in rows {
                let (id, last, books, jobs) = row?;
                m.insert(id, (last, books, jobs));
            }
            Ok(m)
        })
        .await?;

    let enriched: Vec<Value> = summaries
        .into_iter()
        .filter_map(|a| {
            let extra = local.get(&a.account_id)?;
            let mut v = serde_json::to_value(&a).ok()?;
            if let Some(obj) = v.as_object_mut() {
                obj.insert("last_synced_at".into(), json!(extra.0));
                obj.insert("book_count".into(), json!(extra.1));
                obj.insert("active_jobs".into(), json!(extra.2));
            }
            Some(v)
        })
        .collect();
    Ok(Json(Value::Array(enriched)))
}

#[derive(Debug, Deserialize)]
struct LoginStartIn {
    locale: String,
    #[serde(default)]
    with_username: bool,
}

async fn login_start(
    _user: AuthProfile,
    State(state): State<AppState>,
    Json(body): Json<LoginStartIn>,
) -> AppResult<Json<Value>> {
    let resp = ShimClient::new(&state)
        .login_start(LoginStartReq {
            locale: &body.locale,
            with_username: body.with_username,
        })
        .await?;
    Ok(Json(serde_json::to_value(resp).unwrap()))
}

#[derive(Debug, Deserialize)]
struct LoginFinishIn {
    session_id: String,
    redirect_url: String,
}

async fn refresh_account(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    assert_owns_account(&state, user.id(), &id).await?;
    let r = state
        .http
        .post(format!("{}/accounts/{}/refresh", state.cfg.shim_url.trim_end_matches('/'), id))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(Json(r))
}

async fn deregister_account(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    assert_owns_account(&state, user.id(), &id).await?;
    let r = state
        .http
        .post(format!(
            "{}/accounts/{}/deregister",
            state.cfg.shim_url.trim_end_matches('/'),
            id
        ))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    let id_s = id.clone();
    state
        .db
        .with(move |c| {
            c.execute("DELETE FROM accounts WHERE id = ?1", rusqlite::params![id_s])?;
            Ok(())
        })
        .await?;
    Ok(Json(r))
}

async fn login_finish(
    user: AuthProfile,
    State(state): State<AppState>,
    Json(body): Json<LoginFinishIn>,
) -> AppResult<Json<Value>> {
    let resp = ShimClient::new(&state)
        .login_finish(LoginFinishReq {
            session_id: &body.session_id,
            redirect_url: &body.redirect_url,
        })
        .await?;
    let summary = ShimClient::new(&state)
        .list_accounts()
        .await?
        .into_iter()
        .find(|a| a.account_id == resp.account_id);
    sync::register_account(
        &state,
        &resp.account_id,
        resp.locale.as_deref().unwrap_or(""),
        summary.as_ref().map(|s| s.email_masked.as_str()).unwrap_or(""),
        resp.customer_name.as_deref(),
        user.id(),
    )
    .await?;

    let state_clone = state.clone();
    let aid = resp.account_id.clone();
    tokio::spawn(async move {
        if let Err(e) = sync::full(&state_clone, &aid).await {
            tracing::warn!(account = %aid, error = ?e, "initial full sync failed");
        }
    });

    Ok(Json(serde_json::to_value(resp).unwrap()))
}

// ---------- library + jobs (DB-backed) ----------

async fn list_library(user: AuthProfile, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let pid = user.id();
    let rows = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.asin, b.account_id, b.title, b.authors_json, b.cover_url,
                        b.status, b.purchase_date, a.locale, b.runtime_length_ms,
                        b.codec, b.bitrate_kbps, b.sample_rate, b.channels
                 FROM books b
                 JOIN accounts a ON a.id = b.account_id
                 WHERE a.profile_id = ?1
                 ORDER BY b.purchase_date DESC",
            )?;
            let rows = stmt
                .query_map([pid], |r| {
                    Ok(json!({
                        "asin": r.get::<_, String>(0)?,
                        "account_id": r.get::<_, String>(1)?,
                        "title": r.get::<_, String>(2)?,
                        "authors": serde_json::from_str::<Vec<String>>(&r.get::<_, String>(3)?).unwrap_or_default(),
                        "cover_url": r.get::<_, Option<String>>(4)?,
                        "status": r.get::<_, String>(5)?,
                        "purchase_date": r.get::<_, Option<String>>(6)?,
                        // Region pulled from the owning account so the
                        // UI can render a per-book badge without a
                        // separate accounts lookup. Same join we already
                        // need for profile-scoping; near-zero cost.
                        "region": r.get::<_, Option<String>>(7)?,
                        // Authoritative total from Audible — the preview
                        // player uses it as a finite seek denominator
                        // since a streamed <audio>.duration can be Infinity.
                        "runtime_length_ms": r.get::<_, Option<i64>>(8)?,
                        // Audio quality probed from the m4b (we don't
                        // transcode, so it's the delivered tier). null
                        // until the book is converted + probed.
                        "codec": r.get::<_, Option<String>>(9)?,
                        "bitrate_kbps": r.get::<_, Option<i64>>(10)?,
                        "sample_rate": r.get::<_, Option<i64>>(11)?,
                        "channels": r.get::<_, Option<i64>>(12)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    Ok(Json(json!({ "items": rows })))
}

#[derive(Debug, Deserialize)]
struct SyncIn {
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    full: bool,
}

async fn sync_library(
    user: AuthProfile,
    State(state): State<AppState>,
    Json(body): Json<SyncIn>,
) -> AppResult<Json<Value>> {
    let accounts = match body.account_id {
        Some(id) => {
            assert_owns_account(&state, user.id(), &id).await?;
            vec![id]
        }
        None => state
            .db
            .with({
                let pid = user.id();
                move |c| {
                    let mut stmt = c.prepare("SELECT id FROM accounts WHERE profile_id = ?1")?;
                    let v = stmt
                        .query_map([pid], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(v)
                }
            })
            .await?,
    };

    let mut reports = Vec::with_capacity(accounts.len());
    for acct in accounts {
        let r = if body.full {
            sync::full(&state, &acct).await?
        } else {
            sync::incremental(&state, &acct).await?
        };
        reports.push(r);
    }
    Ok(Json(json!({ "syncs": reports })))
}

async fn list_jobs(user: AuthProfile, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let pid = user.id();
    type Row = (
        String,
        String,
        String,
        String,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT j.id, j.asin, j.account_id, j.status, j.created_at, j.updated_at,
                        j.error, j.m4b_path, j.aaxc_path
                 FROM jobs j
                 JOIN accounts a ON a.id = j.account_id
                 WHERE a.profile_id = ?1
                 ORDER BY j.updated_at DESC LIMIT 200",
            )?;
            let rows = stmt
                .query_map([pid], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    // Stat each path to surface filesystem drift — when a user deletes
    // an m4b from ABS or the NAS share, the chip can flip to "missing"
    // and the UI can offer a re-convert without waiting for the next
    // poll/reconcile cycle.
    let items = rows
        .into_iter()
        .map(|(id, asin, account_id, status, created_at, updated_at, error, m4b_path, aaxc_path)| {
            let m4b_present = m4b_path
                .as_deref()
                .map(|p| std::path::Path::new(p).is_file())
                .unwrap_or(false);
            let aaxc_present = aaxc_path
                .as_deref()
                .map(|p| std::path::Path::new(p).is_file())
                .unwrap_or(false);
            json!({
                "id": id,
                "asin": asin,
                "account_id": account_id,
                "status": status,
                "created_at": created_at,
                "updated_at": updated_at,
                "error": error,
                "m4b_present": m4b_present,
                "aaxc_present": aaxc_present,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct EnqueueIn {
    account_id: String,
    asin: String,
}

async fn enqueue_job(
    user: AuthProfile,
    State(state): State<AppState>,
    Json(body): Json<EnqueueIn>,
) -> AppResult<Json<Value>> {
    assert_owns_account(&state, user.id(), &body.account_id).await?;
    let id = state.queue().enqueue(&body.account_id, &body.asin).await?;
    Ok(Json(json!({ "job_id": id })))
}

#[derive(Debug, Deserialize)]
struct EnqueueAllIn {
    #[serde(default)]
    account_id: Option<String>,
}

async fn enqueue_all(
    user: AuthProfile,
    State(state): State<AppState>,
    Json(body): Json<EnqueueAllIn>,
) -> AppResult<Json<Value>> {
    let accounts: Vec<String> = match body.account_id {
        Some(id) => {
            assert_owns_account(&state, user.id(), &id).await?;
            vec![id]
        }
        None => state
            .db
            .with({
                let pid = user.id();
                move |c| {
                    let mut stmt = c.prepare("SELECT id FROM accounts WHERE profile_id = ?1")?;
                    let v = stmt
                        .query_map([pid], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(v)
                }
            })
            .await?,
    };
    let mut total = 0usize;
    for acct in &accounts {
        match state.queue().enqueue_pending(acct).await {
            Ok(ids) => total += ids.len(),
            Err(e) => tracing::warn!(account = %acct, error = ?e, "bulk enqueue failed"),
        }
    }
    Ok(Json(json!({ "queued": total, "accounts": accounts.len() })))
}

async fn job_sse(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, AppError> {
    let job_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("invalid uuid".into()))?;
    assert_owns_job(&state, user.id(), job_id).await?;
    let rx = state.queue().subscribe(job_id).await.ok_or(AppError::NotFound)?;
    let stream = BroadcastStream::new(rx).filter_map(|res| {
        res.ok().and_then(|ev: QueueEvent| {
            serde_json::to_string(&ev).ok().map(|s| Ok(Event::default().data(s)))
        })
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn cancel_job(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let job_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("invalid uuid".into()))?;
    assert_owns_job(&state, user.id(), job_id).await?;
    let cancelled = state.queue().cancel(job_id).await?;
    Ok(Json(json!({ "cancelled": cancelled })))
}

async fn reconcile_route(_user: AuthProfile, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let report = crate::reconcile::scan(&state)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(json!({
        "sidecars_seen": report.sidecars_seen,
        "jobs_inserted": report.jobs_inserted,
        "jobs_promoted": report.jobs_promoted,
        "jobs_already": report.jobs_already,
        "errors": report.errors,
    })))
}

/// Remove a book from scribe's tracking: drop its `books` + `jobs` rows
/// for every account the caller owns, and tombstone the asin so the boot
/// reconcile pass won't resurrect it from the leftover sidecar. Files on
/// disk (the encrypted source, the voucher sidecar, any m4b) are left
/// untouched on purpose — removal is reversible by hand, and a future
/// importer can re-adopt a decoded m4b when the voucher is gone for good.
async fn remove_book(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(asin): Path<String>,
) -> AppResult<Json<Value>> {
    let pid = user.id();
    let (books_deleted, jobs_deleted) = state
        .db
        .with(move |c| {
            // Tombstone every (asin, account) the caller owns that has a
            // book or job row, before deleting. Scope through accounts so
            // one user can't remove another's books.
            let now = chrono::Utc::now().timestamp();
            c.execute(
                "INSERT OR IGNORE INTO removed_books (asin, account_id, removed_at)
                 SELECT DISTINCT ?1, account_id, ?2 FROM (
                     SELECT account_id FROM books WHERE asin = ?1
                     UNION SELECT account_id FROM jobs WHERE asin = ?1
                 )
                 WHERE account_id IN (SELECT id FROM accounts WHERE profile_id = ?3)",
                rusqlite::params![asin, now, pid],
            )?;
            let jobs_deleted = c.execute(
                "DELETE FROM jobs WHERE asin = ?1
                 AND account_id IN (SELECT id FROM accounts WHERE profile_id = ?2)",
                rusqlite::params![asin, pid],
            )?;
            let books_deleted = c.execute(
                "DELETE FROM books WHERE asin = ?1
                 AND account_id IN (SELECT id FROM accounts WHERE profile_id = ?2)",
                rusqlite::params![asin, pid],
            )?;
            Ok((books_deleted, jobs_deleted))
        })
        .await?;
    if books_deleted == 0 && jobs_deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({
        "removed": true,
        "books_deleted": books_deleted,
        "jobs_deleted": jobs_deleted,
    })))
}

/// Refresh one book in place: force a fresh cover download (a CDN URL may
/// have rotated) and re-probe the m4b's quality, for every account the
/// caller owns that holds this asin. Cheap, no Audible catalog call —
/// metadata refresh is the global pass below.
async fn refresh_book(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(asin): Path<String>,
) -> AppResult<Json<Value>> {
    let pid = user.id();
    let asin_q = asin.clone();
    let rows: Vec<(String, Option<String>)> = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.account_id, b.cover_url FROM books b
                 JOIN accounts a ON a.id = b.account_id
                 WHERE b.asin = ?1 AND a.profile_id = ?2",
            )?;
            let v = stmt
                .query_map(rusqlite::params![asin_q, pid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    if rows.is_empty() {
        return Err(AppError::NotFound);
    }
    for (account, cover_url) in &rows {
        if let Some(url) = cover_url {
            let _ = crate::covers::fetch_and_store(&state, &asin, url).await;
        }
        crate::quality::reprobe(&state, &asin, account).await;
        crate::chapters::refetch(&state, account, &asin).await;
    }
    Ok(Json(json!({ "refreshed": rows.len() })))
}

/// Global refresh: re-sync every owned account's catalog metadata from
/// Audible (fresh titles, series, cover URLs), then force-recache all
/// covers and re-probe all quality. Runs detached on the 1 GB Pi —
/// returns immediately; progress shows up as the library refetches.
async fn refresh_library(
    user: AuthProfile,
    State(state): State<AppState>,
) -> AppResult<Json<Value>> {
    let pid = user.id();
    let accounts: Vec<String> = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare("SELECT id FROM accounts WHERE profile_id = ?1")?;
            let v = stmt
                .query_map([pid], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let st = state.clone();
    tokio::spawn(async move {
        for acct in &accounts {
            if let Err(e) = sync::full(&st, acct).await {
                tracing::warn!(account = %acct, error = ?e, "refresh sync failed");
            }
        }
        crate::covers::recache_owned(&st, pid).await;
        crate::quality::reprobe_owned(&st, pid).await;
        crate::chapters::refetch_owned(&st, pid).await;
        tracing::info!(profile = pid, "library refresh complete");
    });
    Ok(Json(json!({ "started": true })))
}

/// Serve a book's cover from the local disk cache, lazily mirroring it
/// from the Amazon CDN on first request. Requires a session and is scoped
/// to a book the caller's profile owns — a plain `<img src>` works because
/// same-origin requests carry the session cookie. ETag/Cache-Control let
/// the browser skip re-fetching; the cached bytes survive Amazon later
/// pulling the title.
async fn book_cover(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(asin): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, AppError> {
    use axum::http::header;

    // Ownership gate first. The on-disk cache is keyed by asin alone, so
    // checking ownership *before* find_cached stops an authenticated user
    // from pulling another user's cached cover by guessing an asin (and
    // stops anonymous access entirely). 404 when the caller owns no such
    // book; the inner Option is the (nullable) cover_url.
    let pid = user.id();
    let asin_q = asin.clone();
    let cover_url: Option<String> = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT cover_url FROM books
                 WHERE asin = ?1
                   AND account_id IN (SELECT id FROM accounts WHERE profile_id = ?2)
                 LIMIT 1",
                rusqlite::params![asin_q, pid],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
        })
        .await?
        .ok_or(AppError::NotFound)?;

    let (path, mime) = match crate::covers::find_cached(&state.cfg.covers_dir, &asin).await {
        Some(pm) => pm,
        None => {
            let url = cover_url.ok_or(AppError::NotFound)?;
            crate::covers::fetch_and_store(&state, &asin, &url)
                .await
                .map_err(|_| AppError::NotFound)?
        }
    };

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::NotFound)?;
    let etag = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("\"{}-{}\"", asin, d.as_secs()))
        .unwrap_or_else(|| format!("\"{asin}\""));

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(axum::response::Response::builder()
            .status(axum::http::StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .unwrap());
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound)?;
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::ETAG, etag)
        .body(axum::body::Body::from(bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))
}

async fn assert_owns_account(state: &AppState, profile_id: i64, account_id: &str) -> AppResult<()> {
    let aid = account_id.to_string();
    let n: i64 = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM accounts WHERE id = ?1 AND profile_id = ?2",
                rusqlite::params![aid, profile_id],
                |r| r.get(0),
            )
        })
        .await?;
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Serve a locally-stored AAXC to the press worker over the LAN. Token
/// in the URL is the only auth — minted by `reconvert_job` for the
/// duration of one press round-trip, then revoked. AAXC bytes are still
/// encrypted at this layer; the matching voucher key/iv lives only in
/// the in-memory press request body, never in this URL.
async fn serve_internal_aaxc(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<axum::response::Response, AppError> {
    let path = state
        .aaxc_tokens
        .lookup(&token)
        .await
        .ok_or(AppError::NotFound)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::NotFound)?;
    let len = file
        .metadata()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
        .len();
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);
    use axum::http::header;
    use axum::response::IntoResponse;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            // Press uses Range probes for total-size detection. Advertise
            // support so the Content-Range/Content-Length flow mirrors
            // what the Audible CDN serves.
            (header::ACCEPT_RANGES, "bytes".to_string()),
        ],
        body,
    )
        .into_response())
}

async fn reconvert_job(
    user: AuthProfile,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    assert_owns_job(&state, user.id(), id).await?;
    crate::reconvert::kick_off(state.clone(), id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn assert_owns_job(state: &AppState, profile_id: i64, job_id: Uuid) -> AppResult<()> {
    let jid = job_id.to_string();
    let n: i64 = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM jobs j JOIN accounts a ON a.id = j.account_id
                 WHERE j.id = ?1 AND a.profile_id = ?2",
                rusqlite::params![jid, profile_id],
                |r| r.get(0),
            )
        })
        .await?;
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}
