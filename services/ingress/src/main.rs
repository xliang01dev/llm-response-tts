use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
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
    session: String,
    output_dir: String,
}

#[derive(Serialize)]
struct EnqueueResponse {
    id: i64,
}

#[derive(Serialize)]
struct QueuedJob {
    id: i64,
    text: String,
    session: String,
    output_dir: String,
    epoch: i64,
}

#[derive(Serialize)]
struct NextResponse {
    id: i64,
    filename: String,
    status: &'static str,
}

#[derive(Deserialize)]
struct SessionQuery {
    session: String,
}

#[derive(Deserialize)]
struct AckRequest {
    id: i64,
    session: String,
}

#[derive(Deserialize)]
struct ClearRequest {
    session: String,
}

const NEXT_ID_KEY: &str = "llm-response-tts:next_id";
const WORK_QUEUE_KEY: &str = "llm-response-tts:work_queue";
const SESSIONS_KEY: &str = "llm-response-tts:sessions";

fn pending_ids_key(session: &str) -> String {
    format!("llm-response-tts:pending_ids:{session}")
}

fn epoch_key(session: &str) -> String {
    format!("llm-response-tts:epoch:{session}")
}

fn wav_filename(id: i64) -> String {
    format!("{:010}.wav", id)
}

fn status_key(id: i64) -> String {
    format!("llm-response-tts:status:{id}")
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
        .route("/clear-all", post(clear_all))
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
        .get(epoch_key(&req.session))
        .await
        .unwrap_or(None)
        .unwrap_or(0);

    let payload = serde_json::to_string(&QueuedJob {
        id,
        text: req.text,
        session: req.session.clone(),
        output_dir: req.output_dir,
        epoch,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    redis::pipe()
        .atomic()
        .cmd("LPUSH").arg(WORK_QUEUE_KEY).arg(payload).ignore()
        .cmd("RPUSH").arg(pending_ids_key(&req.session)).arg(id).ignore()
        .cmd("SADD").arg(SESSIONS_KEY).arg(&req.session).ignore()
        .query_async::<()>(&mut state.redis)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok((StatusCode::ACCEPTED, Json(EnqueueResponse { id })))
}

async fn next(
    State(mut state): State<AppState>,
    Query(q): Query<SessionQuery>,
) -> Result<Json<NextResponse>, StatusCode> {
    let id: Option<i64> = state
        .redis
        .lindex(pending_ids_key(&q.session), 0)
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
    let popped: Option<i64> = state
        .redis
        .lpop(pending_ids_key(&req.session), None)
        .await
        .unwrap_or(None);
    if popped != Some(req.id) {
        tracing::warn!("ack mismatch: requested {}, popped {:?}", req.id, popped);
    }
    let _: Result<(), _> = state.redis.del(status_key(req.id)).await;
    StatusCode::NO_CONTENT
}

// Drops everything not yet playing *for this session*: clears its ordering list so player's
// next poll sees nothing pending, and bumps its epoch so any job a worker already popped (and
// is mid-synthesis) gets silently discarded instead of writing an orphaned wav nobody will ever
// ask for. work_queue itself is left alone - it's shared across sessions now, and the epoch
// bump is what neutralizes this session's still-queued-but-unpopped jobs once a worker gets to
// them. Whatever's already playing on the host finishes on its own - this only stops what comes
// after it.
async fn clear(State(mut state): State<AppState>, Json(req): Json<ClearRequest>) -> StatusCode {
    let result: Result<(), _> = redis::pipe()
        .atomic()
        .cmd("INCR").arg(epoch_key(&req.session)).ignore()
        .cmd("DEL").arg(pending_ids_key(&req.session)).ignore()
        .query_async::<()>(&mut state.redis)
        .await;

    match result {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

// Same as clear(), but for every session that's ever enqueued something, plus a full
// work_queue drain - safe here specifically because every session's epoch is bumped in the
// same pipeline, so any job any worker pops afterward (regardless of which session it's
// tagged with) gets silently discarded anyway.
async fn clear_all(State(mut state): State<AppState>) -> StatusCode {
    let sessions: Vec<String> = match state.redis.smembers(SESSIONS_KEY).await {
        Ok(s) => s,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    };

    let mut pipe = redis::pipe();
    pipe.atomic().cmd("DEL").arg(WORK_QUEUE_KEY).ignore();
    for session in &sessions {
        pipe.cmd("INCR").arg(epoch_key(session)).ignore();
        pipe.cmd("DEL").arg(pending_ids_key(session)).ignore();
    }

    match pipe.query_async::<()>(&mut state.redis).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
