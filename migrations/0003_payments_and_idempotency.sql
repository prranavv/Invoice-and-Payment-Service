-- 0003: payment attempts and idempotency

CREATE TABLE payment_attempts (
    -- This id is also sent to the PSP as its idempotency key, so a retry of the
    -- same attempt can never produce a second charge at the PSP.
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id      uuid        NOT NULL REFERENCES invoices (id),
    -- Snapshot of what we asked the PSP to charge.
    amount_cents    bigint      NOT NULL CHECK (amount_cents > 0),
    card_token      text        NOT NULL,
    -- pending: PSP call in flight (or outcome not yet recorded)
    -- succeeded / failed: definitive PSP answer
    -- unknown: PSP outcome ambiguous (timeout / dropped connection); needs reconciliation
    status          text        NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'succeeded', 'failed', 'unknown')),
    psp_ref         text,
    failure_code    text,
    idempotency_key text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    completed_at    timestamptz,

    CONSTRAINT attempts_succeeded_has_ref
        CHECK (status <> 'succeeded' OR psp_ref IS NOT NULL),
    CONSTRAINT attempts_failed_has_code
        CHECK (status <> 'failed' OR failure_code IS NOT NULL),
    CONSTRAINT attempts_completed_consistent
        CHECK ((status IN ('pending')) = (completed_at IS NULL))
);

-- Backstop #1: at most one in-flight attempt per invoice. Even if the row lock
-- logic had a bug, the second concurrent insert fails with a unique violation.
CREATE UNIQUE INDEX payment_attempts_one_pending_per_invoice
    ON payment_attempts (invoice_id) WHERE status = 'pending';

-- Backstop #2: at most one successful charge per invoice. Makes double-charging
-- impossible at the schema level.
CREATE UNIQUE INDEX payment_attempts_one_success_per_invoice
    ON payment_attempts (invoice_id) WHERE status = 'succeeded';

-- Backstop #3: the same Idempotency-Key cannot create two attempts on one invoice.
CREATE UNIQUE INDEX payment_attempts_invoice_idem_key
    ON payment_attempts (invoice_id, idempotency_key);

-- Sweeper: find attempts stuck in pending or unknown.
CREATE INDEX payment_attempts_stuck_idx
    ON payment_attempts (created_at) WHERE status IN ('pending', 'unknown');

-- Idempotency records. Scoped per business so tenants cannot collide.
-- request_hash covers method + path + body; same key with a different hash -> 422.
-- in_progress + same hash -> 409 (or resume); completed + same hash -> replay stored response.
CREATE TABLE idempotency_keys (
    business_id     uuid        NOT NULL REFERENCES businesses (id),
    key             text        NOT NULL CHECK (length(key) BETWEEN 1 AND 255),
    request_hash    bytea       NOT NULL,
    status          text        NOT NULL DEFAULT 'in_progress'
                    CHECK (status IN ('in_progress', 'completed')),
    response_status integer,
    response_body   jsonb,
    created_at      timestamptz NOT NULL DEFAULT now(),
    completed_at    timestamptz,

    PRIMARY KEY (business_id, key),
    CONSTRAINT idem_completed_has_response
        CHECK ((status = 'completed') = (response_status IS NOT NULL))
);

-- For a periodic cleanup job (e.g. delete keys older than 24h).
CREATE INDEX idempotency_keys_created_idx ON idempotency_keys (created_at);