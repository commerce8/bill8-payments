-- SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
-- SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
-- SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
-- SPDX-License-Identifier: Apache-2.0

-- Bill8:Payments - the service's own schema `payments`:
-- domain tables + payments.outbox / payments.audit_journal producer
-- tables, all FORCE-RLS env-scoped. Charges are append-only; refunds and
-- disputes are compensating records; a channel never mutates balances.

CREATE SCHEMA IF NOT EXISTS payments;

CREATE TABLE IF NOT EXISTS payments.customers (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    display_name    TEXT NOT NULL,
    email           TEXT,
    status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'deleted')),
    sentin8_auth_user_id UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resource_version INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_payments_customers_env ON payments.customers (env_id);

CREATE TABLE IF NOT EXISTS payments.payment_methods (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    customer_id     UUID NOT NULL REFERENCES payments.customers(id),
    method_type     TEXT NOT NULL CHECK (method_type IN ('card', 'bank_debit', 'wallet')),
    display         TEXT NOT NULL,
    secret_ref      TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_payment_methods_env ON payments.payment_methods (env_id);
CREATE INDEX IF NOT EXISTS idx_payments_payment_methods_customer ON payments.payment_methods (customer_id);

CREATE TABLE IF NOT EXISTS payments.payment_intents (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    customer_id     UUID REFERENCES payments.customers(id),
    payment_method_id UUID REFERENCES payments.payment_methods(id),
    amount          DECIMAL(20,2) NOT NULL CHECK (amount > 0),
    currency        TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'requires_confirmation'
                    CHECK (status IN ('requires_confirmation', 'processing', 'requires_action', 'succeeded', 'failed')),
    channel_id      UUID,
    idempotency_key TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resource_version INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_payment_intents_env ON payments.payment_intents (env_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_payment_intents_idempotency
    ON payments.payment_intents (env_id, idempotency_key) WHERE idempotency_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS payments.setup_intents (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    customer_id     UUID REFERENCES payments.customers(id),
    payment_method_id UUID REFERENCES payments.payment_methods(id),
    status          TEXT NOT NULL DEFAULT 'requires_confirmation',
    idempotency_key TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_setup_intents_env ON payments.setup_intents (env_id);

-- Charges: APPEND-ONLY money captures. No UPDATE of amount exists; only
-- guarded status transitions and amount_refunded accumulation.
CREATE TABLE IF NOT EXISTS payments.charges (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id              UUID NOT NULL,
    customer_id         UUID REFERENCES payments.customers(id),
    payment_method_id   UUID REFERENCES payments.payment_methods(id),
    payment_intent_id   UUID REFERENCES payments.payment_intents(id),
    amount              DECIMAL(20,2) NOT NULL CHECK (amount > 0),
    amount_refunded     DECIMAL(20,2) NOT NULL DEFAULT 0,
    currency            TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending', 'succeeded', 'failed', 'partially_refunded', 'refunded', 'disputed')),
    channel_id          UUID,
    balance_transaction_id UUID,
    on_behalf_of        UUID,
    application_fee_amount DECIMAL(20,2),
    idempotency_key     TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    resource_version    INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_payments_charges_env ON payments.charges (env_id);
CREATE INDEX IF NOT EXISTS idx_payments_charges_customer ON payments.charges (customer_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_payments_charges_idempotency
    ON payments.charges (env_id, idempotency_key) WHERE idempotency_key IS NOT NULL;

-- Refunds: compensating records bounded by the charge's refundable amount.
CREATE TABLE IF NOT EXISTS payments.refunds (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id                  UUID NOT NULL,
    charge_id               UUID NOT NULL REFERENCES payments.charges(id),
    amount                  DECIMAL(20,2) NOT NULL CHECK (amount > 0),
    status                  TEXT NOT NULL DEFAULT 'succeeded' CHECK (status IN ('succeeded', 'failed')),
    balance_transaction_id  UUID,
    idempotency_key         TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_refunds_env ON payments.refunds (env_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_payments_refunds_idempotency
    ON payments.refunds (env_id, idempotency_key) WHERE idempotency_key IS NOT NULL;

-- Disputes: challenges with evidence (Storage references).
CREATE TABLE IF NOT EXISTS payments.disputes (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    charge_id       UUID NOT NULL REFERENCES payments.charges(id),
    amount          DECIMAL(20,2) NOT NULL CHECK (amount > 0),
    reason          TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'needs_response'
                    CHECK (status IN ('needs_response', 'under_review', 'won', 'lost')),
    balance_transaction_id UUID,
    evidence_refs   JSONB NOT NULL DEFAULT '[]',
    idempotency_key TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resource_version INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_payments_disputes_env ON payments.disputes (env_id);

-- Tokens: single-use instrument references (guarded reuse).
CREATE TABLE IF NOT EXISTS payments.tokens (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    token_value     TEXT NOT NULL UNIQUE,
    payment_method_id UUID NOT NULL REFERENCES payments.payment_methods(id),
    used            BOOLEAN NOT NULL DEFAULT false,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_tokens_env ON payments.tokens (env_id);

-- ── Settlement-channel layer (payments DESIGN.md §2b) ────────────────
CREATE TABLE IF NOT EXISTS payments.channels (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    name        TEXT NOT NULL,
    label       TEXT NOT NULL,
    methods     JSONB NOT NULL DEFAULT '[]',
    regions     JSONB NOT NULL DEFAULT '[]',
    recurring   BOOLEAN NOT NULL DEFAULT false,
    mode        TEXT NOT NULL DEFAULT 'live' CHECK (mode IN ('live', 'test')),
    enabled     BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (env_id, name)
);
CREATE INDEX IF NOT EXISTS idx_payments_channels_env ON payments.channels (env_id);

CREATE TABLE IF NOT EXISTS payments.channel_configs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    default_channel UUID NOT NULL REFERENCES payments.channels(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resource_version INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_payments_channel_configs_env ON payments.channel_configs (env_id);

CREATE TABLE IF NOT EXISTS payments.routing_rules (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    priority        INTEGER NOT NULL DEFAULT 100,
    method_type     TEXT,
    region          TEXT,
    channel_id      UUID NOT NULL REFERENCES payments.channels(id),
    enabled         BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_routing_rules_env ON payments.routing_rules (env_id);

CREATE TABLE IF NOT EXISTS payments.settlements (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    channel_id      UUID NOT NULL REFERENCES payments.channels(id),
    batch_ref       TEXT NOT NULL,
    matched_count   INTEGER NOT NULL DEFAULT 0,
    flagged_count   INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'reconciled' CHECK (status IN ('reconciled', 'flagged')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_settlements_env ON payments.settlements (env_id);

-- SCA challenges: single-use, short-TTL handback tokens.
CREATE TABLE IF NOT EXISTS payments.challenges (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    payment_intent_id UUID NOT NULL REFERENCES payments.payment_intents(id),
    token           TEXT NOT NULL UNIQUE,
    used            BOOLEAN NOT NULL DEFAULT false,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_challenges_env ON payments.challenges (env_id);

-- ── Row-level security ────────────────────────────────────────────────
ALTER TABLE payments.customers ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.customers FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.payment_methods ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.payment_methods FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.payment_intents ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.payment_intents FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.setup_intents ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.setup_intents FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.charges ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.charges FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.refunds ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.refunds FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.disputes ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.disputes FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.tokens FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.channels ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.channels FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.channel_configs ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.channel_configs FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.routing_rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.routing_rules FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.settlements ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.settlements FORCE ROW LEVEL SECURITY;
ALTER TABLE payments.challenges ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.challenges FORCE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation ON payments.customers USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.payment_methods USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.payment_intents USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.setup_intents USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.charges USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.refunds USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.disputes USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.tokens USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.channels USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.channel_configs USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.routing_rules USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.settlements USING (env_id = current_setting('app.env_id', true)::uuid);
CREATE POLICY tenant_isolation ON payments.challenges USING (env_id = current_setting('app.env_id', true)::uuid);

-- ── Producer tables ────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS payments.outbox (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id              UUID NOT NULL,
    event_type          TEXT NOT NULL,
    stream_family       TEXT NOT NULL,
    payload             JSONB NOT NULL,
    publisher_id_token  TEXT,
    publisher_subject   TEXT,
    relayed             BOOLEAN NOT NULL DEFAULT false,
    relayed_at          TIMESTAMPTZ,
    relay_attempts      INT NOT NULL DEFAULT 0,
    next_retry_at       TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_outbox_unrelayed ON payments.outbox (created_at) WHERE relayed = false;

CREATE OR REPLACE FUNCTION payments.notify_outbox_changed() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('payments_outbox_changed', NEW.id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS payments_outbox_notify ON payments.outbox;
CREATE TRIGGER payments_outbox_notify AFTER INSERT ON payments.outbox
    FOR EACH ROW EXECUTE FUNCTION payments.notify_outbox_changed();

CREATE TABLE IF NOT EXISTS payments.audit_journal (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    action          TEXT NOT NULL,
    target_resource_type TEXT,
    target_resource_id    TEXT,
    actor_user_id   TEXT,
    actor_subject_type TEXT,
    severity        TEXT NOT NULL DEFAULT 'info',
    correlation_id  TEXT,
    before_state    JSONB,
    after_state     JSONB,
    details         JSONB,
    pushed          BOOLEAN NOT NULL DEFAULT false,
    pushed_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payments_audit_journal_unpushed
    ON payments.audit_journal (created_at) WHERE pushed = false;
ALTER TABLE payments.audit_journal ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments.audit_journal FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS audit_journal_dml ON payments.audit_journal;
CREATE POLICY audit_journal_dml ON payments.audit_journal
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())
    WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
-- ── Role grants (schema-ownership convention, bank8-ledger's form) ─────
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bill8_payments_service') THEN
        ALTER ROLE bill8_payments_service SET search_path = payments, public;
        GRANT USAGE ON SCHEMA payments TO bill8_payments_service;
        GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA payments TO bill8_payments_service;
        GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA payments TO bill8_payments_service;
    END IF;
END $$;
