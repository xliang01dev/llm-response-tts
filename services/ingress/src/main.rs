use axum::{extract::State, http::StatusCode, routing::{get, post}, Json, Router};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Clone)]
struct AppState {
    redis: ConnectionManager,
}

#[derive(Deserialize)]
struct EnqueueRequest {
    text: String,
}

#[derive(Serialize)]
struct EnqueueResponse {
    id: i64,
}

#[derive(Serialize)]
struct QueuedJob {
    id: i64,
    text: String,
    epoch: i64,
}

#[derive(Serialize)]
struct NextResponse {
    id: i64,
    filename: String,
    status: &'static str,
}

#[derive(Deserialize)]
struct AckRequest {
    id: i64,
}

const NEXT_ID_KEY: &str = "kokoros:next_id";
const WORK_QUEUE_KEY: &str = "kokoros:work_queue";
const PENDING_IDS_KEY: &str = "kokoros:pending_ids";
const EPOCH_KEY: &str = "kokoros:epoch";

fn wav_filename(id: i64) -> String {
    format!("{:010}.wav", id)
}

fn status_key(id: i64) -> String {
    format!("kokoros:status:{id}")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://redis:6379".to_string());
    let client = redis::Client::open(redis_url).expect("invalid REDIS_URL");
    let redis = ConnectionManager::new(client)
        .await
        .expect("failed to connect to redis on startup");

    let app = Router::new()
        .route("/", post(enqueue))
        .route("/next", get(next))
        .route("/ack", post(ack))
        .route("/clear", post(clear))
        .with_state(AppState { redis });

    let addr = SocketAddr::from(([0, 0, 0, 0], 3001));
    tracing::info!("ingress listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn enqueue(
    State(mut state): State<AppState>,
    Json(req): Json<EnqueueRequest>,
) -> Result<(StatusCode, Json<EnqueueResponse>), StatusCode> {
    if req.text.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let id: i64 = state
        .redis
        .incr(NEXT_ID_KEY, 1)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let epoch: i64 = state
        .redis
        .get(EPOCH_KEY)
        .await
        .unwrap_or(None)
        .unwrap_or(0);

    let payload = serde_json::to_string(&QueuedJob { id, text: req.text, epoch })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    redis::pipe()
        .atomic()
        .cmd("LPUSH").arg(WORK_QUEUE_KEY).arg(payload).ignore()
        .cmd("RPUSH").arg(PENDING_IDS_KEY).arg(id).ignore()
        .query_async::<()>(&mut state.redis)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok((StatusCode::ACCEPTED, Json(EnqueueResponse { id })))
}

async fn next(State(mut state): State<AppState>) -> Result<Json<NextResponse>, StatusCode> {
    let id: Option<i64> = state
        .redis
        .lindex(PENDING_IDS_KEY, 0)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let Some(id) = id else {
        return Err(StatusCode::NO_CONTENT);
    };

    let complete: bool = state
        .redis
        .exists(status_key(id))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok(Json(NextResponse {
        id,
        filename: wav_filename(id),
        status: if complete { "COMPLETE" } else { "PROCESSING" },
    }))
}

async fn ack(State(mut state): State<AppState>, Json(req): Json<AckRequest>) -> StatusCode {
    let popped: Option<i64> = state.redis.lpop(PENDING_IDS_KEY, None).await.unwrap_or(None);
    if popped != Some(req.id) {
        tracing::warn!("ack mismatch: requested {}, popped {:?}", req.id, popped);
    }
    let _: Result<(), _> = state.redis.del(status_key(req.id)).await;
    StatusCode::NO_CONTENT
}

// Drops everything not yet playing: clears both queues so player's next poll sees
// nothing pending, and bumps the epoch so any job a worker already popped (and is
// mid-synthesis) gets silently discarded instead of writing an orphaned wav nobody
// will ever ask for. Whatever's already playing on the host finishes on its own -
// this only stops what comes after it.
async fn clear(State(mut state): State<AppState>) -> StatusCode {
    let result: Result<(), _> = redis::pipe()
        .atomic()
        .cmd("INCR").arg(EPOCH_KEY).ignore()
        .cmd("DEL").arg(WORK_QUEUE_KEY).ignore()
        .cmd("DEL").arg(PENDING_IDS_KEY).ignore()
        .query_async::<()>(&mut state.redis)
        .await;

    match result {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
