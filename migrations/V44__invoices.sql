-- Local invoice tracking. Each row maps to a Stripe Invoice created via the
-- Invoices API. Stores enough data to serve GET /billing/invoices without
-- round-tripping to Stripe on every request.

CREATE TABLE invoices (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id     UUID NOT NULL REFERENCES workspaces(id),
    stripe_invoice_id TEXT NOT NULL UNIQUE,
    stripe_number    TEXT,
    status           TEXT NOT NULL DEFAULT 'draft'
                     CHECK (status IN ('draft', 'open', 'paid', 'void', 'uncollectible')),
    amount_cents     BIGINT NOT NULL DEFAULT 0,
    hosted_url       TEXT,
    pdf_url          TEXT,
    period_start     TIMESTAMPTZ NOT NULL,
    period_end       TIMESTAMPTZ NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_invoices_workspace ON invoices(workspace_id, created_at DESC);

-- Track the last invoice generation timestamp per workspace so the monthly
-- task knows where to pick up from. Avoids re-invoicing the same period.
ALTER TABLE workspaces ADD COLUMN last_invoice_at TIMESTAMPTZ;
