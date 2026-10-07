-- 0004: webhooks (transactional outbox)
--
-- Flow: the API handler inserts a webhook_event and one webhook_delivery per active
-- endpoint IN THE SAME TRANSACTION as the invoice state change. A background worker
-- claims due deliveries with FOR UPDATE SKIP LOCKED and POSTs them. The API request
-- path never makes an outbound HTTP call to a business's server.

CREATE TABLE webhook_endpoints (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id uuid        NOT NULL REFERENCES businesses (id),
    url         text        NOT NULL,
    -- HMAC key. Must be readable by the app to sign, so it cannot be hashed.
    -- Production gap: encrypt at rest (KMS / envelope encryption).
    secret      text        NOT NULL,
    is_active   boolean     NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX webhook_endpoints_business_idx
    ON webhook_endpoints (business_id) WHERE is_active;

-- Immutable log of what happened. Also the basis for reconciliation:
-- a business can call GET /events?since=<id or timestamp> to catch up on missed deliveries.
CREATE TABLE webhook_events (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id uuid        NOT NULL REFERENCES businesses (id),
    type        text        NOT NULL
                CHECK (type IN ('invoice.created', 'invoice.paid', 'invoice.payment_failed')),
    invoice_id  uuid        NOT NULL REFERENCES invoices (id),
    payload     jsonb       NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX webhook_events_business_created_idx
    ON webhook_events (business_id, created_at, id);

-- One row per (event, endpoint). Tracks delivery state and retry schedule.
-- Retry policy lives in application code, e.g. delays of 10s, 30s, 2m, 10m, 1h, 6h
-- (max 7 attempts, ~7h total), then status = 'failed' (dead letter).
CREATE TABLE webhook_deliveries (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id         uuid        NOT NULL REFERENCES webhook_events (id),
    endpoint_id      uuid        NOT NULL REFERENCES webhook_endpoints (id),
    status           text        NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'delivered', 'failed')),
    attempts         integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  timestamptz NOT NULL DEFAULT now(),
    last_attempt_at  timestamptz,
    last_status_code integer,
    last_error       text,
    delivered_at     timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),

    UNIQUE (event_id, endpoint_id),
    CONSTRAINT deliveries_delivered_consistent
        CHECK ((status = 'delivered') = (delivered_at IS NOT NULL))
);

-- The worker's poll query: due, pending deliveries. Partial index keeps it tiny
-- because delivered/failed rows (the vast majority over time) are excluded.
CREATE INDEX webhook_deliveries_due_idx
    ON webhook_deliveries (next_attempt_at) WHERE status = 'pending';