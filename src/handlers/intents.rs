// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Payment-intent and setup-intent handlers (PAY-5): the two-phase
//! confirmation surface. An intent reserves nothing — it records the
//! customer's WILLINGNESS to pay (or to save a method); `confirm` is
//! the single guarded transition into processing, and the terminal
//! states (`succeeded`/`failed`) never regress (payments DESIGN.md §3).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::handlers::{err_db, set_tenant_context, AppState};
use bill8::payments::contracts::{
    CreatePaymentIntentRequest, CreateSetupIntentRequest, PaymentIntent, SetupIntent,
};

/// `POST /v1/payment-intents` — create an intent in
/// `requires_confirmation` (no money moves).
#[utoipa::path(
    post,
    path = "/v1/payment-intents",
    request_body = CreatePaymentIntentRequest,
    responses(
        (status = 200, description = "Intent created", body = PaymentIntent),
        (status = 400, description = "Amount must be a positive decimal string"),
        (status = 403, description = "Missing bill8:payments:intents:write"),
    ),
    security(("bill8:payments:intents:write" = [])),
    tag = "payments",
)]
pub async fn create_payment_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreatePaymentIntentRequest>,
) -> Result<Json<PaymentIntent>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:intents:write"),
    )
    .await?;

    let amount: Decimal = req.amount.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "amount must be a decimal string".into(),
        )
    })?;
    if amount <= Decimal::ZERO {
        return Err((StatusCode::BAD_REQUEST, "amount must be positive".into()));
    }

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // Idempotency: a retried create returns the original intent.
    if let Some(key) = req.idempotency_key.as_deref() {
        if let Ok(Some(row)) = sqlx::query(
            "SELECT id FROM payments.payment_intents WHERE env_id = $1 AND idempotency_key = $2",
        )
        .bind(a.env_id())
        .bind(key)
        .fetch_optional(&mut *conn)
        .await
        {
            let id: Uuid = row.get("id");
            conn.commit().await.map_err(err_db)?;
            return Ok(Json(get_intent_row(&state, a.env_id(), id).await?));
        }
    }

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO payments.payment_intents
               (id, env_id, customer_id, payment_method_id, amount, currency, idempotency_key)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(id)
    .bind(a.env_id())
    .bind(req.customer_id)
    .bind(req.payment_method_id)
    .bind(amount)
    .bind(&req.currency)
    .bind(req.idempotency_key.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:intent:created', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "intent_id": id, "amount": req.amount, "currency": req.currency }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(get_intent_row(&state, a.env_id(), id).await?))
}

/// `GET /v1/payment-intents` — list intents for the env.
#[utoipa::path(
    get,
    path = "/v1/payment-intents",
    responses(
        (status = 200, description = "List of intents", body = [PaymentIntent]),
        (status = 403, description = "Missing bill8:payments:intents:read"),
    ),
    security(("bill8:payments:intents:read" = [])),
    tag = "payments",
)]
pub async fn list_payment_intents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PaymentIntent>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:intents:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let rows = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, amount, currency,
                  status, channel_id, created_at
           FROM payments.payment_intents ORDER BY created_at DESC LIMIT 200"#,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(rows.iter().map(row_to_intent).collect()))
}

/// `POST /v1/payment-intents/{id}/confirm` — the guarded
/// `requires_confirmation → processing` transition. Terminal states
/// never regress; confirming a terminal intent is a 409.
#[utoipa::path(
    post,
    path = "/v1/payment-intents/{id}/confirm",
    responses(
        (status = 200, description = "Intent confirmed → processing", body = PaymentIntent),
        (status = 404, description = "Intent not found"),
        (status = 409, description = "Intent already terminal"),
    ),
    security(("bill8:payments:intents:write" = [])),
    tag = "payments",
)]
pub async fn confirm_payment_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<PaymentIntent>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:intents:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // Guarded transition: only a requires_confirmation intent confirms.
    let result = sqlx::query(
        "UPDATE payments.payment_intents
         SET status = 'processing', resource_version = resource_version + 1
         WHERE id = $1 AND env_id = $2 AND status = 'requires_confirmation'
         RETURNING id",
    )
    .bind(id)
    .bind(a.env_id())
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::CONFLICT, "intent not confirmable".into()))?;

    let _ = result;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:intent:updated', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "intent_id": id, "status": "processing" }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(get_intent_row(&state, a.env_id(), id).await?))
}

/// `POST /v1/setup-intents` — save a method for later use (no charge).
#[utoipa::path(
    post,
    path = "/v1/setup-intents",
    request_body = CreateSetupIntentRequest,
    responses(
        (status = 200, description = "Setup intent created", body = SetupIntent),
        (status = 403, description = "Missing bill8:payments:setup-intents:write"),
    ),
    security(("bill8:payments:setup-intents:write" = [])),
    tag = "payments",
)]
pub async fn create_setup_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateSetupIntentRequest>,
) -> Result<Json<SetupIntent>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:setup-intents:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO payments.setup_intents
               (id, env_id, customer_id, payment_method_id, idempotency_key)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(a.env_id())
    .bind(req.customer_id)
    .bind(req.payment_method_id)
    .bind(req.idempotency_key.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:setup_intent:created', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "setup_intent_id": id, "customer_id": req.customer_id }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    let row = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, status, created_at
           FROM payments.setup_intents WHERE id = $1"#,
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(err_db)?;

    Ok(Json(row_to_setup(&row)))
}

/// `GET /v1/setup-intents` — list setup intents for the env.
#[utoipa::path(
    get,
    path = "/v1/setup-intents",
    responses(
        (status = 200, description = "List of setup intents", body = [SetupIntent]),
        (status = 403, description = "Missing bill8:payments:setup-intents:read"),
    ),
    security(("bill8:payments:setup-intents:read" = [])),
    tag = "payments",
)]
pub async fn list_setup_intents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SetupIntent>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:setup-intents:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let rows = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, status, created_at
           FROM payments.setup_intents ORDER BY created_at DESC LIMIT 200"#,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(rows.iter().map(row_to_setup).collect()))
}

/// `POST /v1/setup-intents/{id}/confirm` — the guarded
/// `requires_confirmation → processing` transition for setup intents.
#[utoipa::path(
    post,
    path = "/v1/setup-intents/{id}/confirm",
    responses(
        (status = 200, description = "Setup intent confirmed → processing", body = SetupIntent),
        (status = 404, description = "Setup intent not found"),
        (status = 409, description = "Setup intent already terminal"),
    ),
    security(("bill8:payments:setup-intents:write" = [])),
    tag = "payments",
)]
pub async fn confirm_setup_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<SetupIntent>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:setup-intents:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let result = sqlx::query(
        "UPDATE payments.setup_intents
         SET status = 'processing'
         WHERE id = $1 AND env_id = $2 AND status = 'requires_confirmation'
         RETURNING id",
    )
    .bind(id)
    .bind(a.env_id())
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::CONFLICT, "setup intent not confirmable".into()))?;
    let _ = result;

    conn.commit().await.map_err(err_db)?;
    let row = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, status, created_at
           FROM payments.setup_intents WHERE id = $1"#,
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(err_db)?;

    Ok(Json(row_to_setup(&row)))
}

/// Fetch one intent row as its wire type.
async fn get_intent_row(
    state: &AppState,
    env_id: Uuid,
    id: Uuid,
) -> Result<PaymentIntent, (StatusCode, String)> {
    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id)
        .await
        .map_err(err_db)?;
    let row = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, amount, currency,
                  status, channel_id, created_at
           FROM payments.payment_intents WHERE id = $1"#,
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| (StatusCode::NOT_FOUND, "intent not found".into()))?;
    Ok(row_to_intent(&row))
}

/// Map a row to the intent wire type (money as string).
fn row_to_intent(row: &sqlx::postgres::PgRow) -> PaymentIntent {
    PaymentIntent {
        id: row.get("id"),
        env_id: row.get("env_id"),
        customer_id: row.get("customer_id"),
        payment_method_id: row.get("payment_method_id"),
        amount: row.get::<Decimal, _>("amount").to_string(),
        currency: row.get("currency"),
        status: row.get("status"),
        channel_id: row.get("channel_id"),
        created_at: row.get("created_at"),
    }
}

/// Map a row to the setup-intent wire type.
fn row_to_setup(row: &sqlx::postgres::PgRow) -> SetupIntent {
    SetupIntent {
        id: row.get("id"),
        env_id: row.get("env_id"),
        customer_id: row.get("customer_id"),
        payment_method_id: row.get("payment_method_id"),
        status: row.get("status"),
        created_at: row.get("created_at"),
    }
}
