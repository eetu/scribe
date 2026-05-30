use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use scribe_shared::JobEvent;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth::bearer_guard;
use crate::ffmpeg;
use crate::jobs::{self, JobReq, Phase};
use crate::state::PressState;

pub fn router(state: PressState) -> Router {
    let protected = Router::new()
        .route("/jobs", post(create_job))
        .route("/jobs/{id}", get(job_status).delete(delete_job))
        .route("/jobs/{id}/sse", get(job_sse))
        .route("/jobs/{id}/aaxc", get(get_aaxc))
        .route("/jobs/{id}/m4b", get(get_m4b))
        .route_layer(middleware::from_fn_with_state(state.clone(), bearer_guard))
        .with_state(state.clone());

    Router::new().route("/health", get(health)).merge(protected)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "scribe-press" }))
}

async fn create_job(
    State(state): State<PressState>,
    Json(req): Json<JobReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if let Err(msg) = req.drm.validate() {
        return Err((StatusCode::BAD_REQUEST, msg.into()));
    }
    // `file://` reads an arbitrary local path back through /jobs/{id}/aaxc —
    // gated behind an explicit opt-in so it can't be abused in prod (the
    // normal pipeline never sends it).
    if req.content_url.starts_with("file://") && !state.cfg.allow_file_url {
        return Err((
            StatusCode::BAD_REQUEST,
            "file:// content_url is disabled (set PRESS_ALLOW_FILE_URL=1 for local testing)".into(),
        ));
    }
    let s = state
        .jobs
        .create(req)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let job_id = { s.lock().await.id };

    // Run the pipeline in a background task. Concurrency capped by the semaphore.
    let sem = state.jobs.sem.clone();
    let ffmpeg_bin = state.cfg.ffmpeg_bin.clone();
    tokio::spawn(async move {
        let _permit = match sem.acquire_owned().await {
            Ok(p) => p,
            Err(_) => return,
        };
        if let Err(e) = ffmpeg::run(s.clone(), ffmpeg_bin).await {
            tracing::error!(%job_id, error = %e, "job failed");
            let mut g = s.lock().await;
            g.phase = Phase::Failed;
            if g.error.is_none() {
                g.error = Some(e.to_string());
            }
            let _ = g.events.send(JobEvent::Failed { message: e.to_string() });
        }
    });

    Ok(Json(serde_json::json!({ "job_id": job_id })))
}

async fn job_status(
    State(state): State<PressState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let s = state.jobs.get(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let status = s.lock().await.status();
    let body = serde_json::to_value(status).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(body))
}

async fn job_sse(
    State(state): State<PressState>,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusCode> {
    let s = state.jobs.get(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let rx = s.lock().await.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| {
        res.ok()
            .and_then(|ev| serde_json::to_string(&ev).ok())
            .map(|s| Ok(Event::default().data(s)))
    });
    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

async fn get_aaxc(State(state): State<PressState>, Path(id): Path<Uuid>) -> Result<Response, StatusCode> {
    serve_file(&state, id, FileKind::Aaxc).await
}

async fn get_m4b(State(state): State<PressState>, Path(id): Path<Uuid>) -> Result<Response, StatusCode> {
    serve_file(&state, id, FileKind::M4b).await
}

#[derive(Clone, Copy)]
enum FileKind {
    Aaxc,
    M4b,
}

async fn serve_file(state: &PressState, id: Uuid, kind: FileKind) -> Result<Response, StatusCode> {
    let s = state.jobs.get(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let (path, content_type) = {
        let g = s.lock().await;
        if g.phase != Phase::Ready {
            return Err(StatusCode::CONFLICT);
        }
        match kind {
            FileKind::Aaxc => (g.aaxc_path(), "audio/vnd.audible.aaxc"),
            FileKind::M4b => (g.m4b_path(), "audio/mp4"),
        }
    };
    let file = tokio::fs::File::open(&path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let len = file.metadata().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.len();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
        ],
        body,
    )
        .into_response())
}

async fn delete_job(State(state): State<PressState>, Path(id): Path<Uuid>) -> Result<StatusCode, StatusCode> {
    let s = state.jobs.remove(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let dir = { s.lock().await.dir.clone() };
    jobs::purge_dir(&dir).await;
    Ok(StatusCode::NO_CONTENT)
}
