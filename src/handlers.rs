// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Payments HTTP handlers — the charge lifecycle.
//!
//! **Charge sequence** (payments DESIGN.md §2): Shield gate →
//! idempotency → inline risk screening (fail-closed) → channel routing
//! → channel authorize/capture → ledger posting (the ONLY money
//! movement) → charge record + outbox in ONE transaction → COMMIT.
//! A blocked screening or channel failure writes NOTHING.

pub mod channels_admin;
pub mod charges;
pub mod customers;
pub mod disputes;
pub mod intents;
pub mod refunds;

use std::sync::Arc;

use crate::config::PaymentsConfig;
use crate::policy::Engine;

/// Shared application state bound into the shell.
#[derive(Clone)]
pub struct AppState {
    /// Service configuration.
    pub config: Arc<PaymentsConfig>,
    /// Capped PostgreSQL pool.
    pub pool: sqlx::PgPool,
    /// Cert-presenting Bus client.
    pub bus_client: sentin8::bus::client::BusClient,
    /// Shield policy engine — step 5 is the ONLY gate.
    pub policy: Arc<Engine>,
}

/// Set the transaction-local RLS tenant context (same-connection rule).
pub async fn set_tenant_context(
    conn: &mut sqlx::PgConnection,
    env_id: uuid::Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT sentin8_set_tenant_context($1, NULL, NULL, NULL, false, false, false, NULL, 'payments')")
        .bind(env_id)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Standard DB error → HTTP 500.
pub fn err_db(e: sqlx::Error) -> (axum::http::StatusCode, String) {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Best-effort outbox emit (non-transactional callers).
pub async fn emit_event(
    pool: &sqlx::PgPool,
    env_id: uuid::Uuid,
    event_type: &str,
    payload: serde_json::Value,
) {
    if let Err(e) =
        sentin8::bus::outbox::insert_outbox_row_pool(pool, env_id, "payments", event_type, &payload)
            .await
    {
        tracing::warn!(error = %e, event_type, "bill8:payments: failed to emit outbox event");
    }
}

#[utoipa::path(
    get,
    path = "/v1/health",
    responses(
        (status = 200, description = "Liveness (version, build)", body = serde_json::Value),
    ),
    security(),
    tag = "meta",
)]
/// Health probe payload.
pub async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "payments",
        "version": sentin8::build_info::id(), "build": sentin8::build_info::build_hash() }))
}
