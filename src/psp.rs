use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Hard cap on how long a request thread waits for the PSP. tok_timeout sleeps 30 s,
/// so it always trips this; the attempt is then recovered in the background.
const PSP_TIMEOUT: Duration = Duration::from_secs(5);

/// A *definitive* answer from the PSP. Anything else (timeout, connection error, 5xx,
/// garbled body) is an `Err`, meaning "we do not know whether the card was charged".
#[derive(Debug)]
pub enum PspResult {
    Succeeded { psp_ref: String },
    Failed { code: String },
}

#[derive(Clone)]
pub struct PspClient {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Serialize)]
struct ChargeRequest<'a> {
    token: &'a str,
    amount_cents: i64,
}

#[derive(Deserialize)]
struct ChargeResponse {
    status: String,
    psp_ref: Option<String>,
    code: Option<String>,
}

impl PspClient {
    pub fn new(base_url: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(PSP_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// `attempt_id` is sent as the PSP's Idempotency-Key. Calling this again for the same
    /// attempt (a retry, or the recovery sweeper) can never create a second charge.
    pub async fn charge(
        &self,
        attempt_id: Uuid,
        token: &str,
        amount_cents: i64,
    ) -> anyhow::Result<PspResult> {
        let resp = self
            .http
            .post(format!("{}/charge", self.base_url))
            .header("Idempotency-Key", attempt_id.to_string())
            .json(&ChargeRequest { token, amount_cents })
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("PSP returned HTTP {}", resp.status());
        }

        let body: ChargeResponse = resp.json().await?;
        match (body.status.as_str(), body.psp_ref, body.code) {
            ("succeeded", Some(psp_ref), _) => Ok(PspResult::Succeeded { psp_ref }),
            ("failed", _, Some(code)) => Ok(PspResult::Failed { code }),
            (other, _, _) => anyhow::bail!("unrecognized PSP response status {other:?}"),
        }
    }
}