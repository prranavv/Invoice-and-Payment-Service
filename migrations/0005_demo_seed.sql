-- 0005: DEMO SEED (development only)
--
-- Solves the bootstrap problem: how does a business get its first API key without
-- an unauthenticated admin endpoint? For this take-home, a seed migration.
-- In production you would delete this file and mint keys via a CLI/admin tool.
--
-- Demo API key (use as: Authorization: Bearer <key>):
--     sk_demo0001_devonlysecretdevonlysecret00
-- Prefix is "demo0001". Only the SHA-256 hash is stored.

WITH b AS (
    INSERT INTO businesses (name)
    VALUES ('Demo Business')
    RETURNING id
)
INSERT INTO api_keys (business_id, prefix, key_hash, label)
SELECT b.id,
       'demo0001',
       digest('sk_demo0001_devonlysecretdevonlysecret00', 'sha256'),
       'demo seed key'
FROM b;