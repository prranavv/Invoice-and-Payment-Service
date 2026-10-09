-- 0006: an attempt whose PSP outcome is unknown may or may not have charged the card,
-- so it must keep blocking new attempts on the same invoice until a human resolves it.
-- Otherwise a second attempt (with a fresh PSP idempotency key) could double-charge.

DROP INDEX payment_attempts_one_pending_per_invoice;

CREATE UNIQUE INDEX payment_attempts_one_in_flight_per_invoice
    ON payment_attempts (invoice_id) WHERE status IN ('pending', 'unknown');