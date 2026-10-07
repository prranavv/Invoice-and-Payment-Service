CREATE TABLE customers (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id uuid        NOT NULL REFERENCES businesses (id),
    name        text        NOT NULL CHECK (length(btrim(name)) > 0),
    email       text        NOT NULL CHECK (position('@' in email) > 1),
    created_at  timestamptz NOT NULL DEFAULT now(),

    -- Target for the composite FK from invoices, so the DB itself guarantees
    -- an invoice and its customer belong to the same business.
    UNIQUE (id, business_id)
);

-- Serves "list my customers, newest first" with keyset pagination.
CREATE INDEX customers_business_created_idx
    ON customers (business_id, created_at DESC, id DESC);

CREATE TABLE invoices (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id uuid        NOT NULL REFERENCES businesses (id),
    customer_id uuid        NOT NULL,
    state       text        NOT NULL DEFAULT 'draft'
                CHECK (state IN ('draft', 'open', 'paid', 'void', 'uncollectible')),
    total_cents bigint      NOT NULL DEFAULT 0 CHECK (total_cents >= 0),
    due_date    date        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    paid_at     timestamptz,

    -- Tenancy enforced by the schema: customer must belong to the same business.
    FOREIGN KEY (customer_id, business_id) REFERENCES customers (id, business_id),

    -- paid_at is set if and only if the invoice is paid.
    CONSTRAINT invoices_paid_at_consistent
        CHECK ((state = 'paid') = (paid_at IS NOT NULL)),

    -- A zero-total invoice can never be collected, so it cannot be open or paid.
    CONSTRAINT invoices_payable_has_total
        CHECK (state NOT IN ('open', 'paid') OR total_cents > 0)
);

-- "List invoices, optionally filtered by state" within one business.
CREATE INDEX invoices_business_state_created_idx
    ON invoices (business_id, state, created_at DESC, id DESC);
CREATE INDEX invoices_customer_idx ON invoices (customer_id);

CREATE TABLE invoice_line_items (
    id                uuid    PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id        uuid    NOT NULL REFERENCES invoices (id) ON DELETE CASCADE,
    position          integer NOT NULL CHECK (position >= 0),
    description       text    NOT NULL CHECK (length(btrim(description)) > 0),
    quantity          integer NOT NULL CHECK (quantity > 0),
    unit_amount_cents bigint  NOT NULL CHECK (unit_amount_cents >= 0),
    -- Derived by the database, so it can never disagree with its inputs.
    -- bigint * bigint raises an error on overflow instead of wrapping.
    amount_cents      bigint  GENERATED ALWAYS AS (quantity::bigint * unit_amount_cents) STORED,

    UNIQUE (invoice_id, position)
);