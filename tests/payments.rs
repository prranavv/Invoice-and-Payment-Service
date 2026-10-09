//! Integration tests for the payment path. They talk to a RUNNING stack over HTTP:
//!
//!   docker compose up --build -d
//!   cargo test --test payments
//!
//! Override targets with API_URL (default http://localhost:8000) and PSP_URL
//! (default http://localhost:9000). The demo business from migration 0005 is used.
//!
//! The tests share one mock PSP, so they run one at a time (LOCK) and use a unique
//! amount per test, which lets them count exactly how many charges reached the PSP.

use std::time::{Duration, Instant};

use reqwest::{Client, Response};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

const DEMO_KEY: &str = "sk_demo0001_devonlysecretdevonlysecret00";

static LOCK: Mutex<()> = Mutex::const_new(());

fn api() -> String {
    std::env::var("API_URL").unwrap_or_else(|_| "http://localhost:8000".into())
}

fn psp() -> String {
    std::env::var("PSP_URL").unwrap_or_else(|_| "http://localhost:9000".into())
}

fn client() -> Client {
    Client::builder().timeout(Duration::from_secs(20)).build().unwrap()
}

async fn post(c: &Client, path: &str, idem: Option<&str>, body: Value) -> Response {
    let mut req = c
        .post(format!("{}{path}", api()))
        .bearer_auth(DEMO_KEY)
        .json(&body);
    if let Some(k) = idem {
        req = req.header("Idempotency-Key", k);
    }
    req.send().await.expect("service not reachable; is `docker compose up` running?")
}

async fn get_json(c: &Client, url: String) -> Value {
    c.get(url).bearer_auth(DEMO_KEY).send().await.unwrap().json().await.unwrap()
}

async fn open_invoice(c: &Client) -> (String, i64) {
    let amount = 1_000_000 + (Uuid::new_v4().as_u128() % 1_000_000) as i64;

    let customer: Value = post(
        c,
        "/customers",
        None,
        json!({"name": "Test Customer", "email": "test@example.com"}),
    )
    .await
    .json()
    .await
    .unwrap();

    let invoice: Value = post(
        c,
        "/invoices",
        None,
        json!({
            "customer_id": customer["id"],
            "due_date": "2030-01-01",
            "line_items": [{"description": "test", "quantity": 1, "unit_amount_cents": amount}],
        }),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(invoice["total_cents"], amount);
    let id = invoice["id"].as_str().unwrap().to_string();

    let finalized = post(c, &format!("/invoices/{id}/finalize"), None, json!({})).await;
    assert_eq!(finalized.status(), 200);
    (id, amount)
}

async fn psp_state(c: &Client) -> Value {
    c.get(format!("{}/charges", psp())).send().await.unwrap().json().await.unwrap()
}

fn psp_calls(state: &Value) -> u64 {
    state["calls"].as_u64().unwrap()
}

fn charges_for(state: &Value, amount: i64) -> usize {
    state["charges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["amount_cents"] == amount)
        .count()
}

async fn invoice_state(c: &Client, id: &str) -> String {
    get_json(c, format!("{}/invoices/{id}", api())).await["state"]
        .as_str()
        .unwrap()
        .to_string()
}

/// N concurrent /pay calls with different idempotency keys: at most one succeeds, only one
/// charge reaches the PSP, and the invoice ends up paid exactly once.
#[tokio::test]
async fn concurrent_payments_charge_once() {
    let _guard = LOCK.lock().await;
    let c = client();
    let (id, amount) = open_invoice(&c).await;

    const N: usize = 12;
    let mut tasks = Vec::new();
    for _ in 0..N {
        let c = c.clone();
        let id = id.clone();
        tasks.push(tokio::spawn(async move {
            let key = Uuid::new_v4().to_string();
            let resp = post(&c, &format!("/invoices/{id}/pay"), Some(&key), json!({"card_token": "tok_success"})).await;
            (resp.status().as_u16(), resp.json::<Value>().await.unwrap())
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }

    let succeeded = results.iter().filter(|(s, _)| *s == 200).count();
    assert_eq!(succeeded, 1, "exactly one request may succeed: {results:?}");
    for (status, body) in results.iter().filter(|(s, _)| *s != 200) {
        assert_eq!(*status, 409, "losers must be rejected cleanly: {body}");
        let code = body["error"]["code"].as_str().unwrap();
        assert!(
            code == "payment_in_progress" || code == "invoice_not_payable",
            "unexpected error code {code}"
        );
    }

    assert_eq!(charges_for(&psp_state(&c).await, amount), 1, "double charge at the PSP");
    assert_eq!(invoice_state(&c, &id).await, "paid");
}

/// Retrying with the same key returns the same response and makes no second PSP call.
/// The same key with a different body is rejected.
#[tokio::test]
async fn idempotent_retry_replays_without_second_psp_call() {
    let _guard = LOCK.lock().await;
    let c = client();
    let (id, amount) = open_invoice(&c).await;
    let path = format!("/invoices/{id}/pay");
    let key = Uuid::new_v4().to_string();

    let calls_before = psp_calls(&psp_state(&c).await);

    let first = post(&c, &path, Some(&key), json!({"card_token": "tok_success"})).await;
    assert_eq!(first.status(), 200);
    assert!(first.headers().get("idempotent-replayed").is_none());
    let first_body: Value = first.json().await.unwrap();
    let after_first = psp_state(&c).await;
    assert_eq!(psp_calls(&after_first) - calls_before, 1);

    let second = post(&c, &path, Some(&key), json!({"card_token": "tok_success"})).await;
    assert_eq!(second.status(), 200);
    assert_eq!(second.headers().get("idempotent-replayed").unwrap(), "true");
    let second_body: Value = second.json().await.unwrap();
    assert_eq!(first_body, second_body, "replay must return the identical response");

    let after_second = psp_state(&c).await;
    assert_eq!(psp_calls(&after_second), psp_calls(&after_first), "replay called the PSP again");
    assert_eq!(charges_for(&after_second, amount), 1);

    let different = post(&c, &path, Some(&key), json!({"card_token": "tok_card_declined"})).await;
    assert_eq!(different.status(), 422);
    let body: Value = different.json().await.unwrap();
    assert_eq!(body["error"]["code"], "idempotency_key_reuse");
    assert_eq!(psp_calls(&psp_state(&c).await), psp_calls(&after_first));

    let again = post(&c, &path, Some(&Uuid::new_v4().to_string()), json!({"card_token": "tok_success"})).await;
    assert_eq!(again.status(), 409);
    let body: Value = again.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invoice_not_payable");
}

/// tok_network_error (PSP 500) and tok_timeout (PSP sleeps 30 s): the endpoint answers
/// promptly with 202, the invoice is not corrupted (still open, not paid), and it stays
/// protected against a second attempt, and a void while the outcome is unknown.
#[tokio::test]
async fn psp_failures_leave_invoice_consistent() {
    let _guard = LOCK.lock().await;
    let c = client();

    for token in ["tok_network_error", "tok_timeout"] {
        let (id, _) = open_invoice(&c).await;
        let path = format!("/invoices/{id}/pay");
        let key = Uuid::new_v4().to_string();

        let started = Instant::now();
        let resp = post(&c, &path, Some(&key), json!({"card_token": token})).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{token}: endpoint hung for {:?}",
            started.elapsed()
        );
        assert_eq!(resp.status(), 202, "{token}: ambiguous outcome must be 202");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "pending");
        assert_eq!(body["invoice_state"], "open");
        assert_eq!(invoice_state(&c, &id).await, "open", "{token}: invoice must not be paid or failed");

        let retry = post(&c, &path, Some(&Uuid::new_v4().to_string()), json!({"card_token": "tok_success"})).await;
        assert_eq!(retry.status(), 409);
        let err: Value = retry.json().await.unwrap();
        assert_eq!(err["error"]["code"], "payment_in_progress");

        for action in ["void"] {
            let r = post(&c, &format!("/invoices/{id}/{action}"), None, json!({})).await;
            assert_eq!(r.status(), 409, "{token}: {action} must be refused");
            let err: Value = r.json().await.unwrap();
            assert_eq!(err["error"]["code"], "payment_in_progress");
        }

        let poll = post(&c, &path, Some(&key), json!({"card_token": token})).await;
        assert_eq!(poll.status(), 202);
        assert_eq!(poll.headers().get("idempotent-replayed").unwrap(), "true");
        let polled: Value = poll.json().await.unwrap();
        assert_eq!(polled["id"], body["id"]);
    }
}
