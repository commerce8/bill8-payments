// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Refund handlers — compensating records bounded by the charge's
//! remaining refundable amount (row-lock serialized; PAY-3 invariants).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bill8::payments::contracts::{CreateRefundRequest, Refund};
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::handlers::{err_db, set_tenant_context, AppState};
use crate::types::PaymentsError;

/// `POST /v1/refunds` — refund a charge (bounded).
#[utoipa::path(
    post,
    path = "/v1/refunds",
    request_body = CreateRefundRequest,
    responses(
        (status = 200, description = "Refund succeeded", body = Refund),
        (status = 403, description = "Missing bill8:payments:refunds:write"),
        (status = 422, description = "Over-refund / non-succeeded charge — nothing written"),
    ),
    security(("bill8:payments:refunds:write" = [])),
    tag = "payments",
)]
pub async fn create_refund(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRefundRequest>,
) -> Result<Json<Refund>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:refunds:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // Idempotency: a retried refund returns the original.
    if let Some(key) = req.idempotency_key.as_deref() {
        if let Ok(Some(row)) = sqlx::query(
            "SELECT id FROM payments.refunds WHERE env_id = $1 AND idempotency_key = $2",
        )
        .bind(a.env_id())
        .bind(key)
        .fetch_optional(&mut *conn)
        .await
        {
            let id: Uuid = row.get("id");
            conn.commit().await.map_err(err_db)?;
            return get_refund_row(&state, a.env_id(), id).await;
        }
    }

    // Row-lock the charge — serializes the refundable computation.
    let charge = sqlx::query(
        "SELECT id, amount, amount_refunded, status FROM payments.charges WHERE id = $1 FOR UPDATE",
    )
    .bind(req.charge_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::NOT_FOUND, "charge not found".into()))?;

    let status: String = charge.get("status");
    if status != "succeeded" && status != "partially_refunded" {
        return Err(map_refund_domain(PaymentsError::IllegalTransition {
            id: req.charge_id,
            from: status,
            to: "refunded".into(),
        }));
    }

    let charge_amount: Decimal = charge.get("amount");
    let already_refunded: Decimal = charge.get("amount_refunded");
    let remaining = charge_amount - already_refunded;

    let amount: Decimal = match req.amount.as_deref() {
        Some(s) => s.parse().map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "amount must be a decimal string".into(),
            )
        })?,
        // Default: refund the remaining amount.
        None => remaining,
    };
    if amount <= Decimal::ZERO {
        return Err((StatusCode::BAD_REQUEST, "amount must be positive".into()));
    }
    if amount > remaining {
        return Err(map_refund_domain(PaymentsError::OverRefund {
            requested: amount.to_string(),
            remaining: remaining.to_string(),
        }));
    }

    // Ledger reversal via the public API (the ONLY money movement). The
    // charge's currency rides along (the refund request carries none).
    let currency: String = sqlx::query("SELECT currency FROM payments.charges WHERE id = $1")
        .bind(req.charge_id)
        .fetch_one(&mut *conn)
        .await
        .map(|r| r.get("currency"))
        .map_err(err_db)?;
    let tx_ref =
        crate::channels::post_refund_to_ledger(&state, a.env_id(), &req, amount, &currency)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("ledger reversal failed: {e}"),
                )
            })?;

    let refund_id = Uuid::new_v4();
    let row = sqlx::query(
        r#"INSERT INTO payments.refunds (id, env_id, charge_id, amount, status, balance_transaction_id, idempotency_key)
           VALUES ($1, $2, $3, $4, 'succeeded', $5, $6)
           RETURNING id, charge_id, amount, status, balance_transaction_id, created_at"#,
    )
    .bind(refund_id)
    .bind(a.env_id())
    .bind(req.charge_id)
    .bind(amount)
    .bind(tx_ref)
    .bind(req.idempotency_key.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(err_db)?;

    // Guarded charge update: accumulate amount_refunded; transition to
    // the terminal refunded state when fully refunded.
    let new_refunded = already_refunded + amount;
    let new_status = if new_refunded == charge_amount {
        "refunded"
    } else {
        "partially_refunded"
    };
    sqlx::query(
        "UPDATE payments.charges SET amount_refunded = $1, status = $2, resource_version = resource_version + 1, updated_at = now() WHERE id = $3",
    )
    .bind(new_refunded)
    .bind(new_status)
    .bind(req.charge_id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:refund:created', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "refund_id": refund_id, "charge_id": req.charge_id, "amount": amount.to_string() }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(row_to_refund(&row)))
}

/// `GET /v1/refunds` — list refunds.
#[utoipa::path(
    get,
    path = "/v1/refunds",
    responses(
        (status = 200, description = "List of refunds", body = [Refund]),
        (status = 403, description = "Missing bill8:payments:refunds:read"),
    ),
    security(("bill8:payments:refunds:read" = [])),
    tag = "payments",
)]
pub async fn list_refunds(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Refund>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:refunds:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let rows = sqlx::query(
        "SELECT id, charge_id, amount, status, balance_transaction_id, created_at FROM payments.refunds ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(rows.iter().map(row_to_refund).collect()))
}

async fn get_refund_row(
    state: &AppState,
    env_id: Uuid,
    id: Uuid,
) -> Result<Json<Refund>, (StatusCode, String)> {
    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id)
        .await
        .map_err(err_db)?;
    let row = sqlx::query(
        "SELECT id, charge_id, amount, status, balance_transaction_id, created_at FROM payments.refunds WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::NOT_FOUND, "refund not found".into()))?;
    Ok(Json(row_to_refund(&row)))
}

fn map_refund_domain(e: PaymentsError) -> (StatusCode, String) {
    match e {
        PaymentsError::IllegalTransition { .. } | PaymentsError::OverRefund { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
        PaymentsError::Db(db) => err_db(db),
        other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}

fn row_to_refund(row: &sqlx::postgres::PgRow) -> Refund {
    Refund {
        id: row.get("id"),
        charge_id: row.get("charge_id"),
        amount: row.get::<Decimal, _>("amount").to_string(),
        status: row.get("status"),
        balance_transaction_id: row.get("balance_transaction_id"),
        created_at: row.get("created_at"),
    }
}
