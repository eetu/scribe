use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::press::PressClient;
use crate::queue::QueueEvent;
use crate::shim::{LoginFinishReq, LoginStartReq, ShimClient};
use crate::state::AppState;
use crate::sync;

pub fn router(state: AppState) -> Router {
    use crate::auth as a;
    Router::new()
        .route("/status", get(status))
        .route("/auth/login", get(a::login))
        .route("/auth/logout", post(a::logout))
        .route("/api/me", get(me))
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/login/start", post(login_start))
        .route("/api/accounts/login/finish", post(login_finish))
        .route("/api/accounts/{id}/refresh", post(refresh_account))
        .route("/api/accounts/{id}/deregister", post(deregister_account))
        .route("/api/library", get(list_library))
        .route("/api/library/sync", post(sync_library))
        .route("/api/jobs", get(list_jobs).post(enqueue_job))
        .route("/api/jobs/enqueue_all", post(enqueue_all))
        .route("/api/jobs/{id}/sse", get(job_sse))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/reorg/preview", get(reorg_preview))
        .route("/api/reorg/commit", post(reorg_commit))
        .with_state(state)
}

// ---------- public probes ----------

async fn status(State(state): State<AppState>) -> Json<Value> {
    let shim = ShimClient::new(&state);
    let press = PressClient::new(&state);
    let (shim_healthy, press_health) = tokio::join!(shim.health(), async {
        press.health().await.unwrap_or(false)
    });
    Json(json!({
        "service": "scribe",
        "version": env!("CARGO_PKG_VERSION"),
        "shim_url": state.cfg.shim_url,
        "shim_healthy": shim_healthy,
        "press_url": state.cfg.press_url,
        "press_healthy": press_health,
        "dev_auth": state.cfg.dev_auth,
        "auto_enqueue": state.cfg.auto_enqueue_new,
        "library_dir": state.cfg.library_dir,
        "original_dir": state.cfg.original_dir,
        "poll_interval_min": state.cfg.poll_interval_min,
    }))
}

// ---------- session probe ----------

async fn me(user: AuthUser) -> Json<Value> {
    Json(json!({ "sub": user.sub }))
}

// ---------- accounts (proxy to shim) ----------

async fn list_accounts(user: AuthUser, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let summaries = ShimClient::new(&state).list_accounts().await?;
    // Enrich each shim account with our local DB facts: last_synced_at,
    // book_count, active_jobs. Only accounts owned by the caller's user_sub
    // make it through.
    let sub = user.sub.clone();
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
                 WHERE a.user_sub = ?1",
            )?;
            let rows = stmt.query_map([sub], |r| {
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
    _user: AuthUser,
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
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    assert_owns_account(&state, &user.sub, &id).await?;
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
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    assert_owns_account(&state, &user.sub, &id).await?;
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
    // Local accounts row + cascaded books cleared.
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
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<LoginFinishIn>,
) -> AppResult<Json<Value>> {
    let resp = ShimClient::new(&state)
        .login_finish(LoginFinishReq {
            session_id: &body.session_id,
            redirect_url: &body.redirect_url,
        })
        .await?;
    // Look up the email-masked / customer_name the shim now knows about so
    // we can stash a complete row instead of NULLs that need a follow-up
    // /api/accounts fetch to populate.
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
        &user.sub,
    )
    .await?;

    // Kick a full sync on register — first-time users get an immediate library.
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

async fn list_library(
    user: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<Value>> {
    let rows = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.asin, b.account_id, b.title, b.authors_json, b.cover_url, b.status, b.purchase_date
                 FROM books b
                 JOIN accounts a ON a.id = b.account_id
                 WHERE a.user_sub = ?1
                 ORDER BY b.purchase_date DESC",
            )?;
            let rows = stmt
                .query_map([user.sub.as_str()], |r| {
                    Ok(json!({
                        "asin": r.get::<_, String>(0)?,
                        "account_id": r.get::<_, String>(1)?,
                        "title": r.get::<_, String>(2)?,
                        "authors": serde_json::from_str::<Vec<String>>(&r.get::<_, String>(3)?).unwrap_or_default(),
                        "cover_url": r.get::<_, Option<String>>(4)?,
                        "status": r.get::<_, String>(5)?,
                        "purchase_date": r.get::<_, Option<String>>(6)?,
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
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<SyncIn>,
) -> AppResult<Json<Value>> {
    let accounts = match body.account_id {
        Some(id) => {
            // Verify the caller owns this account.
            let owned = state
                .db
                .with({
                    let id = id.clone();
                    let sub = user.sub.clone();
                    move |c| {
                        let n: i64 = c.query_row(
                            "SELECT COUNT(*) FROM accounts WHERE id = ?1 AND user_sub = ?2",
                            rusqlite::params![id, sub],
                            |r| r.get(0),
                        )?;
                        Ok(n)
                    }
                })
                .await?;
            if owned == 0 {
                return Err(AppError::NotFound);
            }
            vec![id]
        }
        None => state
            .db
            .with({
                let sub = user.sub.clone();
                move |c| {
                    let mut stmt = c.prepare("SELECT id FROM accounts WHERE user_sub = ?1")?;
                    let v = stmt
                        .query_map([sub], |r| r.get::<_, String>(0))?
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

async fn list_jobs(user: AuthUser, State(state): State<AppState>) -> AppResult<Json<Value>> {
    let rows = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT j.id, j.asin, j.account_id, j.status, j.created_at, j.updated_at, j.error
                 FROM jobs j
                 JOIN accounts a ON a.id = j.account_id
                 WHERE a.user_sub = ?1
                 ORDER BY j.updated_at DESC LIMIT 200",
            )?;
            let rows = stmt
                .query_map([user.sub.as_str()], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "asin": r.get::<_, String>(1)?,
                        "account_id": r.get::<_, String>(2)?,
                        "status": r.get::<_, String>(3)?,
                        "created_at": r.get::<_, i64>(4)?,
                        "updated_at": r.get::<_, i64>(5)?,
                        "error": r.get::<_, Option<String>>(6)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    Ok(Json(json!({ "items": rows })))
}

#[derive(Debug, Deserialize)]
struct EnqueueIn {
    account_id: String,
    asin: String,
}

async fn enqueue_job(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<EnqueueIn>,
) -> AppResult<Json<Value>> {
    assert_owns_account(&state, &user.sub, &body.account_id).await?;
    let id = state.queue().enqueue(&body.account_id, &body.asin).await?;
    Ok(Json(json!({ "job_id": id })))
}

#[derive(Debug, Deserialize)]
struct EnqueueAllIn {
    #[serde(default)]
    account_id: Option<String>,
}

async fn enqueue_all(
    user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<EnqueueAllIn>,
) -> AppResult<Json<Value>> {
    let accounts: Vec<String> = match body.account_id {
        Some(id) => {
            assert_owns_account(&state, &user.sub, &id).await?;
            vec![id]
        }
        None => state
            .db
            .with({
                let sub = user.sub.clone();
                move |c| {
                    let mut stmt = c.prepare("SELECT id FROM accounts WHERE user_sub = ?1")?;
                    let v = stmt
                        .query_map([sub], |r| r.get::<_, String>(0))?
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
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, AppError> {
    let job_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("invalid uuid".into()))?;
    assert_owns_job(&state, &user.sub, job_id).await?;
    let rx = state.queue().subscribe(job_id).await.ok_or(AppError::NotFound)?;
    let stream = BroadcastStream::new(rx).filter_map(|res| {
        res.ok().and_then(|ev: QueueEvent| {
            serde_json::to_string(&ev).ok().map(|s| Ok(Event::default().data(s)))
        })
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn cancel_job(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let job_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("invalid uuid".into()))?;
    assert_owns_job(&state, &user.sub, job_id).await?;
    let cancelled = state.queue().cancel(job_id).await?;
    Ok(Json(json!({ "cancelled": cancelled })))
}

async fn assert_owns_account(state: &AppState, user_sub: &str, account_id: &str) -> AppResult<()> {
    let sub = user_sub.to_string();
    let aid = account_id.to_string();
    let n: i64 = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM accounts WHERE id = ?1 AND user_sub = ?2",
                rusqlite::params![aid, sub],
                |r| r.get(0),
            )
        })
        .await?;
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

async fn assert_owns_job(state: &AppState, user_sub: &str, job_id: Uuid) -> AppResult<()> {
    let sub = user_sub.to_string();
    let jid = job_id.to_string();
    let n: i64 = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM jobs j JOIN accounts a ON a.id = j.account_id
                 WHERE j.id = ?1 AND a.user_sub = ?2",
                rusqlite::params![jid, sub],
                |r| r.get(0),
            )
        })
        .await?;
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

async fn reorg_preview(_user: AuthUser) -> AppResult<Json<Value>> {
    Err(AppError::BadRequest("reorg lands in task #10".into()))
}

async fn reorg_commit(_user: AuthUser) -> AppResult<Json<Value>> {
    Err(AppError::BadRequest("reorg lands in task #10".into()))
}
