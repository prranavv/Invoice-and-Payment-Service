//! Mock payment processor. Run with: cargo run --bin mock_psp
//!
//! Outcome is chosen by the card token:
//!   tok_success            -> succeeded after ~100 ms
//!   tok_insufficient_funds -> failed (insufficient_funds) after ~100 ms
//!   tok_card_declined      -> failed (card_declined) after ~100 ms
//!   tok_timeout            -> succeeded, but only after 30 s
//!   tok_network_error      -> HTTP 500 immediately, nothing is charged
//!
//! It also hosts a tiny webhook sink so the whole flow can be demoed offline:
//!   POST /webhooks/sink      -> records the delivery, returns 200
//!   POST /webhooks/fail      -> records the delivery, returns 500 (to watch retries)
//!   GET  /webhooks/received  -> everything received so far (headers + body)
//!
//! Like a real PSP it dedups on the Idempotency-Key header: the first call with a key
//! decides the outcome and the time it becomes ready; every later call with that key
//! returns the same result (waiting out whatever time remains) and never charges again.
//! Because readiness is a timestamp, not a running task, a client that disconnects during
//! the 30 s sleep and retries later still gets the original charge's result.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::{sleep_until, Instant};
use uuid::Uuid;

#[derive(Clone, Serialize)]
struct Charge {
    idempotency_key: String,
    token: String,
    amount_cents: i64,
    status: &'static str,
    code: Option<&'static str>,
    psp_ref: Option<String>,
    #[serde(skip)]
    ready_at: Instant,
}

#[derive(Clone, Serialize)]
struct Received {
    endpoint: &'static str,
    status_returned: u16,
    webhook_id: Option<String>,
    event: Option<String>,
    attempt: Option<String>,
    signature: Option<String>,
    body: String,
}

#[derive(Clone, Default)]
struct PspState {
    charges: Arc<Mutex<HashMap<String, Charge>>>,
    calls: Arc<AtomicU64>,
    received: Arc<Mutex<Vec<Received>>>,
}

#[derive(Deserialize)]
struct ChargeRequest {
    token: String,
    amount_cents: i64,
}

#[derive(Serialize)]
struct ChargeResponse {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    psp_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
}

fn new_charge(key: &str, req: &ChargeRequest) -> Charge {
    let (status, code, psp_ref, delay) = match req.token.as_str() {
        "tok_success" => ("succeeded", None, Some(Uuid::new_v4().to_string()), Duration::from_millis(100)),
        "tok_timeout" => ("succeeded", None, Some(Uuid::new_v4().to_string()), Duration::from_secs(30)),
        "tok_insufficient_funds" => ("failed", Some("insufficient_funds"), None, Duration::from_millis(100)),
        "tok_card_declined" => ("failed", Some("card_declined"), None, Duration::from_millis(100)),
        _ => ("failed", Some("invalid_token"), None, Duration::from_millis(100)),
    };
    Charge {
        idempotency_key: key.to_string(),
        token: req.token.clone(),
        amount_cents: req.amount_cents,
        status,
        code,
        psp_ref,
        ready_at: Instant::now() + delay,
    }
}

async fn charge(
    State(state): State<PspState>,
    headers: HeaderMap,
    Json(req): Json<ChargeRequest>,
) -> Result<Json<ChargeResponse>, StatusCode> {
    state.calls.fetch_add(1, Ordering::SeqCst);

    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?
        .to_string();

    // A transport-level failure is not an outcome: nothing is recorded, so a retry
    // with the same key fails the same way.
    if req.token == "tok_network_error" {
        println!("[mock-psp] key={key} token={} -> 500", req.token);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // First call with a key fixes the outcome; later calls reuse it. The lock is dropped
    // before any await.
    let charge = {
        let mut charges = state.charges.lock().unwrap();
        charges
            .entry(key.clone())
            .or_insert_with(|| new_charge(&key, &req))
            .clone()
    };

    sleep_until(charge.ready_at).await;
    println!("[mock-psp] key={key} token={} -> {}", charge.token, charge.status);

    Ok(Json(ChargeResponse {
        status: charge.status,
        psp_ref: charge.psp_ref,
        code: charge.code,
    }))
}

/// Debug endpoint for tests and demos: how many HTTP calls arrived, and which distinct
/// charges exist. One charge per idempotency key, never more.
async fn list(State(state): State<PspState>) -> Json<serde_json::Value> {
    let charges: Vec<Charge> = state.charges.lock().unwrap().values().cloned().collect();
    Json(json!({
        "calls": state.calls.load(Ordering::SeqCst),
        "charges": charges,
    }))
}

fn record_webhook(
    state: &PspState,
    endpoint: &'static str,
    status: StatusCode,
    headers: &HeaderMap,
    body: String,
) -> StatusCode {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let entry = Received {
        endpoint,
        status_returned: status.as_u16(),
        webhook_id: header("webhook-id"),
        event: header("webhook-event"),
        attempt: header("webhook-attempt"),
        signature: header("webhook-signature"),
        body,
    };
    println!(
        "[mock-psp] webhook {endpoint} -> {} event={:?} attempt={:?}",
        status.as_u16(),
        entry.event,
        entry.attempt
    );
    let mut received = state.received.lock().unwrap();
    received.push(entry);
    if received.len() > 100 {
        received.remove(0);
    }
    status
}

async fn webhook_sink(
    State(state): State<PspState>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    record_webhook(&state, "sink", StatusCode::OK, &headers, body)
}

async fn webhook_fail(
    State(state): State<PspState>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    record_webhook(&state, "fail", StatusCode::INTERNAL_SERVER_ERROR, &headers, body)
}

async fn webhooks_received(State(state): State<PspState>) -> Json<Vec<Received>> {
    Json(state.received.lock().unwrap().clone())
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/charge", post(charge))
        .route("/charges", get(list))
        .route("/webhooks/sink", post(webhook_sink))
        .route("/webhooks/fail", post(webhook_fail))
        .route("/webhooks/received", get(webhooks_received))
        .with_state(PspState::default());

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:9000".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("[mock-psp] listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}