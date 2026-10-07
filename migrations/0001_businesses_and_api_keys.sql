CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE businesses (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text        NOT NULL CHECK (length(btrim(name)) > 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Key format: sk_<prefix>_<random>. Only the prefix and a SHA-256 hash of the
-- full key are stored. The plaintext key is shown once at creation.
-- Lookup: find by prefix (unique index), then constant-time compare the hash.
-- SHA-256 (not bcrypt/argon2) is fine here because keys are high-entropy random
-- strings, so there is nothing to brute-force, and auth runs on every request.
CREATE TABLE api_keys (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id uuid        NOT NULL REFERENCES businesses (id),
    prefix      text        NOT NULL UNIQUE,
    key_hash    bytea       NOT NULL CHECK (octet_length(key_hash) = 32),
    label       text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz            -- NULL = active. Revocation is a soft delete.
);

CREATE INDEX api_keys_business_idx ON api_keys (business_id);