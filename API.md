# Invoice & Payment Service — API Reference

Base URL (docker compose): `http://localhost:8000`

For a step-by-step walkthrough with real responses, see `README.md`.

## Conventions

- **Auth:** every endpoint requires `Authorization: Bearer sk_<prefix>_<secret>`. Keys are scoped to one business; every resource you read or write belongs to that business.
  Demo key (seeded by migration `0005`): `sk_demo0001_devonlysecretdevonlysecret00`
- **Format:** JSON request and response bodies. IDs are UUIDs. Timestamps are RFC 3339 UTC. `due_date` is `YYYY-MM-DD`.
- **Money:** USD only, **integer cents** (`*_cents`). The server computes all totals.
- **Strict bodies:** unknown fields, wrong types (e.g. `"quantity": 1.5`) and malformed dates (e.g. `01-01-2026`) are rejected with `400 invalid_request`. A client-supplied `total_cents` is therefore an error, not silently ignored.
- **Ignored input:** `Idempotency-Key` is read only by `POST /invoices/{id}/pay`; other endpoints ignore it. Endpoints with no documented body (`GET`s, `finalize`, `void`) ignore any body sent.
- **Lists:** `{"data": [...]}`, newest first. Query params `limit` (1–100, default 50) and `offset` (≥ 0, default 0).

```bash
export KEY=sk_demo0001_devonlysecretdevonlysecret00
export API=http://localhost:8000
```

## Errors

Every error, including unknown routes and malformed JSON/UUIDs/query strings, uses one shape:

```json
{ "error": { "code": "invoice_not_payable", "message": "invoice is paid; only open invoices can be paid" } }
```

| HTTP | `code` | When |
|---|---|---|
| 400 | `invalid_request` | Unparseable JSON, unknown field, wrong type, bad date, bad UUID in path, bad query string, missing `Idempotency-Key` on `/pay` |
| 401 | `unauthorized` | Missing, malformed, unknown, or revoked key (deliberately indistinguishable). Response carries `WWW-Authenticate: Bearer` |
| 404 | `not_found` | Resource doesn't exist **or belongs to another business** (never 403, so ids can't be probed); unknown route |
| 409 | `invalid_state_transition` | Transition not allowed from the invoice's current state |
| 409 | `invoice_not_payable` | `/pay` on an invoice that is not `open` |
| 409 | `payment_in_progress` | A payment attempt is `pending`/`unknown` for this invoice (also blocks `void`) |
| 422 | `validation_error` | Well-formed request violating a rule: see per-endpoint rules; also `limit`/`offset`/`state` out of range, `Idempotency-Key` > 255 chars, `card_token` not 1–100 chars |
| 422 | `idempotency_key_reuse` | `Idempotency-Key` already used with a different request |
| 500 | `internal_error` | Unexpected; details are logged, never returned |

---

## Customers

### `POST /customers`

```bash
curl -s -X POST $API/customers -H "Authorization: Bearer $KEY" \
  -H 'Content-Type: application/json' \
  -d '{"name":"Ada Lovelace","email":"ada@example.com"}'
```

Request: `name` (1–200 chars, trimmed), `email` (valid-looking address, ≤ 254 chars).

`201 Created`
```json
{ "id": "6f1c…", "name": "Ada Lovelace", "email": "ada@example.com", "created_at": "2026-10-09T10:00:00Z" }
```

### `GET /customers/{id}` → `200` customer, or `404`
### `GET /customers?limit=&offset=` → `200 {"data":[customer,…]}`

---

## Invoices

Invoice object:

```json
{
  "id": "b2e4…",
  "customer_id": "6f1c…",
  "state": "draft",
  "total_cents": 5000,
  "due_date": "2026-11-01",
  "created_at": "2026-10-09T10:00:00Z",
  "updated_at": "2026-10-09T10:00:00Z",
  "paid_at": null,
  "line_items": [
    { "description": "Consulting", "quantity": 2, "unit_amount_cents": 2500, "amount_cents": 5000 }
  ]
}
```

`state` ∈ `draft | open | paid | void`. `GET /invoices` returns the same object **without** `line_items`.

### `POST /invoices`

```bash
curl -s -X POST $API/invoices -H "Authorization: Bearer $KEY" \
  -H 'Content-Type: application/json' \
  -d '{"customer_id":"<customer id>","due_date":"2026-11-01",
       "line_items":[{"description":"Consulting","quantity":2,"unit_amount_cents":2500}]}'
```

Rules: `customer_id` must belong to you (else `404`); 1–100 line items; `description` 1–500 chars; `quantity` integer ≥ 1; `unit_amount_cents` integer ≥ 0; resulting total must be > 0 and must not overflow (`422`). `total_cents` is computed by the server (sum of `quantity × unit_amount_cents`) and cannot be supplied. `due_date` is not checked against today (a past date is accepted). The invoice starts in `draft` and emits `invoice.created`.

`201 Created` → invoice object (above).

### `GET /invoices/{id}` → `200` invoice with `line_items`, or `404`
### `GET /invoices?state=&limit=&offset=` → `200 {"data":[invoice,…]}`

`state` must be one of the four states, otherwise `422`.

### State transitions

| Endpoint | From → To | Notes |
|---|---|---|
| `POST /invoices/{id}/finalize` | `draft` → `open` | Makes the invoice payable |
| `POST /invoices/{id}/void` | `draft`, `open` → `void` | `409 payment_in_progress` if an attempt is `pending`/`unknown` |
| `POST /invoices/{id}/pay` | `open` → `paid` | Only via a successful PSP charge; see below |

No request body. `200` returns the updated invoice. Any other source state → `409 invalid_state_transition`. `paid` and `void` are terminal. `finalize` and `void` emit **no** webhook (only `invoice.created`, `invoice.paid` and `invoice.payment_failed` exist).

---

## Pay an invoice

### `POST /invoices/{id}/pay`

Headers: `Idempotency-Key: <1–255 chars>` (**required**: `400` if missing, `422` if longer than 255).
Body: `{"card_token": "<1–100 chars>"}`

```bash
curl -s -i -X POST $API/invoices/<invoice id>/pay -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: $(uuidgen)" -H 'Content-Type: application/json' \
  -d '{"card_token":"tok_success"}'
```

Response body (a *payment attempt*):

```json
{
  "id": "9a7d…",
  "invoice_id": "b2e4…",
  "status": "succeeded",
  "amount_cents": 5000,
  "psp_ref": "0c5e…",
  "failure_code": null,
  "invoice_state": "paid"
}
```

| HTTP | `status` | Meaning |
|---|---|---|
| 200 | `succeeded` | Charged; invoice is `paid`; `invoice.paid` emitted |
| 402 | `failed` | PSP definitively declined (`failure_code`: `card_declined`, `insufficient_funds`, …). Invoice stays `open`; retry with a **new** key. `invoice.payment_failed` emitted |
| 202 | `pending` / `unknown` | Outcome not known yet (PSP slow/unreachable). The invoice stays locked against new attempts while the server keeps resolving it. **Retry the same request with the same `Idempotency-Key`** to poll; or wait for the `invoice.paid` / `invoice.payment_failed` webhook |
| 409 | – | `invoice_not_payable` or `payment_in_progress` |
| 404 | – | Invoice doesn't exist or belongs to another business |
| 422 | – | `idempotency_key_reuse` (same key, different invoice or `card_token`) or `validation_error` |

Failed payment (`tok_card_declined`), `402`:

```json
{ "id": "cd7f…", "invoice_id": "0555…", "status": "failed", "amount_cents": 500,
  "psp_ref": null, "failure_code": "card_declined", "invoice_state": "open" }
```

Outcome unknown (`tok_timeout`, `tok_network_error`), `202`:

```json
{ "id": "877d…", "invoice_id": "0555…", "status": "pending", "amount_cents": 500,
  "psp_ref": null, "failure_code": null, "invoice_state": "open" }
```

An unrecognised token is a definitive decline: `402` with `failure_code: "invalid_token"`.

**Idempotency:** keys are scoped to your business. Replaying the same `(invoice, card_token)` with the same key returns the stored/current result **without a second PSP call**, with response header `Idempotent-Replayed: true`. Once the outcome is final, a replay returns the **stored** status code (`200` or `402`); while it is still pending, `202` with the current status. Requests rejected for business reasons (e.g. `409 invoice_not_payable`) do not consume the key.

The server calls the PSP with a 5 s timeout. A timeout, dropped connection or PSP `5xx` is *not* treated as a failure (the card may have been charged); the attempt stays `pending` → `unknown` after 120 s, and a background sweeper keeps re-asking the PSP with the same attempt id until it gets a definitive answer.

---

## Webhook endpoints

### `POST /webhook_endpoints`

```bash
curl -s -X POST $API/webhook_endpoints -H "Authorization: Bearer $KEY" \
  -H 'Content-Type: application/json' -d '{"url":"http://mock-psp:9000/webhooks/sink"}'
```

`url`: absolute `http`/`https` URL, ≤ 2048 chars.

> **Hostnames:** the webhook worker runs inside the compose network, so register the sink as `http://mock-psp:9000/webhooks/sink`. From your own shell the same service is `http://localhost:9000` (e.g. `curl localhost:9000/webhooks/received` to see what arrived); `mock-psp` does not resolve outside docker.

`201 Created`
```json
{ "id": "…", "url": "http://mock-psp:9000/webhooks/sink", "secret": "whsec_…", "is_active": true, "created_at": "…" }
```

`secret` is shown **only in this response**; store it. Events created before registration are not sent to the new endpoint.

### Delivery

Each event is `POST`ed to every active endpoint as:

```json
{
  "id": "<event uuid>",
  "type": "invoice.paid",
  "created_at": "2026-10-09T10:00:05Z",
  "data": { "invoice_id": "…", "invoice_state": "paid", "payment_attempt": { /* payment attempt object */ } }
}
```

| `type` | `data` |
|---|---|
| `invoice.created` | `invoice_id, customer_id, state ("draft"), total_cents, due_date` |
| `invoice.paid` | `invoice_id, invoice_state ("paid"), payment_attempt` |
| `invoice.payment_failed` | `invoice_id, invoice_state ("open"), payment_attempt` |

Headers: `Webhook-Id` (event id, **identical across retries**), `Webhook-Event`, `Webhook-Attempt` (1-based), `Webhook-Signature: t=<unix seconds>,v1=<hex>`, `Content-Type: application/json`, `User-Agent: invoice-service-webhooks/1.0`.

**Verify:** `v1 = hex(HMAC_SHA256(secret, "<t>" + "." + raw_body))`. Recompute over the **raw bytes** you received, compare in constant time, and reject if `|now − t| > 300 s` (replay protection). The timestamp is refreshed on every retry.

**Respond with any `2xx` within 10 s** to acknowledge (timeouts, connection errors and redirects count as failures). Otherwise delivery is retried after 10 s, 30 s, 2 min, 10 min, 1 h, 6 h (7 attempts total, ≈ 7 h 13 m), then dead-lettered. Delivery is **at-least-once**: dedupe on `Webhook-Id`.

---

## Mock PSP (port 9000)

Called by the service at `POST /charge` with `{token, amount_cents}` and an `Idempotency-Key` header (the payment attempt id). It dedups on that key.

| Token | Behaviour |
|---|---|
| `tok_success` | `{"status":"succeeded","psp_ref":"<uuid>"}` after ~100 ms |
| `tok_insufficient_funds` | `{"status":"failed","code":"insufficient_funds"}` after ~100 ms |
| `tok_card_declined` | `{"status":"failed","code":"card_declined"}` after ~100 ms |
| `tok_timeout` | succeeds only after 30 s (the service times out at 5 s → `202`, resolved later by the sweeper) |
| `tok_network_error` | HTTP 500 immediately (→ `202`, retried by the sweeper) |
| anything else | `failed` / `invalid_token` |

Demo helpers: `GET /charges` (call count + distinct charges), `POST /webhooks/sink` (returns 200), `POST /webhooks/fail` (returns 500, to watch retries), `GET /webhooks/received` (last 100 deliveries with headers and body).
