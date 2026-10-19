// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Dispute handlers - challenges with evidence (Storage references) and
//! the dispute lifecycle. Channel/routing-rule handlers live in
//! `channels_admin.rs`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bill8::payments::contracts::{Dispute, SubmitEvidenceRequest};
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::handlers::{err_db, set_tenant_context, AppState};

/// `POST /v1/disputes` - open a dispute on a charge.
#[utoipa::path(
    post,
    path = "/v1/disputes",
    request_body = Dispute,
    responses(
        (status = 200, description = "Dispute opened", body = Dispute),
        (status = 403, description = "Missing bill8:payments:disputes:write"),
    ),
    security(("bill8:payments:disputes:write" = [])),
    tag = "payments",
)]
pub async fn create_dispute(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:disputes:write"),
    )
    .await?;

    let charge_id: Uuid = body["charge_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "charge_id required".into()))?;
    let reason = body["reason"]
        .as_str()
        .unwrap_or("unrecognized")
        .to_string();
    let amount_str = body["amount"].as_str().unwrap_or("0");
    let amount: Decimal = amount_str.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "amount must be a decimal string".into(),
        )
    })?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO payments.disputes (id, env_id, charge_id, amount, reason, status)
           VALUES ($1, $2, $3, $4, $5, 'needs_response')"#,
    )
    .bind(id)
    .bind(a.env_id())
    .bind(charge_id)
    .bind(amount)
    .bind(&reason)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    // A dispute moves the charge to the guarded disputed state.
    sqlx::query(
        "UPDATE payments.charges SET status = 'disputed', resource_version = resource_version + 1 WHERE id = $1",
    )
    .bind(charge_id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:dispute:created', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "dispute_id": id, "charge_id": charge_id }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "dispute_id": id, "status": "needs_response" }),
    ))
}

/// `GET /v1/disputes` - list disputes.
#[utoipa::path(
    get,
    path = "/v1/disputes",
    responses(
        (status = 200, description = "List of disputes", body = [Dispute]),
        (status = 403, description = "Missing bill8:payments:disputes:read"),
    ),
    security(("bill8:payments:disputes:read" = [])),
    tag = "payments",
)]
pub async fn list_disputes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:disputes:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let rows = sqlx::query(
        "SELECT id, charge_id, amount, reason, status, created_at FROM payments.disputes ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id"),
                    "charge_id": r.get::<Uuid, _>("charge_id"),
                    "amount": r.get::<Decimal, _>("amount").to_string(),
                    "reason": r.get::<String, _>("reason"),
                    "status": r.get::<String, _>("status"),
                })
            })
            .collect(),
    ))
}

/// `POST /v1/disputes/{id}/evidence` - submit evidence (Storage refs).
#[utoipa::path(
    post,
    path = "/v1/disputes/{id}/evidence",
    request_body = SubmitEvidenceRequest,
    responses(
        (status = 200, description = "Evidence attached"),
        (status = 404, description = "Dispute not found"),
    ),
    security(("bill8:payments:disputes:write" = [])),
    tag = "payments",
)]
pub async fn submit_evidence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<SubmitEvidenceRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:disputes:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    sqlx::query(
        "UPDATE payments.disputes SET evidence_refs = $1, status = 'under_review', resource_version = resource_version + 1 WHERE id = $2",
    )
    .bind(sqlx::types::Json(&req.evidence_refs))
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "status": "under_review" })))
}

/// Request body for the dispute resolution.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct ResolveDisputeRequest {
    /// The outcome: `won` (funds stay) or `lost` (funds clawed back).
    pub outcome: String,
}

/// `POST /v1/disputes/{id}/resolve` - close a dispute with an outcome
/// (PAY-6). A `lost` dispute claws the funds back via a ledger
/// reversal transaction (the ONLY money movement) and returns the
/// charge to `succeeded` minus the disputed amount; a `won` dispute
/// keeps the charge as-is. The `under_review → won|lost` transition is
/// guarded - resolving twice is a 409, never a second clawback.
#[utoipa::path(
    post,
    path = "/v1/disputes/{id}/resolve",
    request_body = ResolveDisputeRequest,
    responses(
        (status = 200, description = "Dispute closed"),
        (status = 404, description = "Dispute not found"),
        (status = 409, description = "Dispute already closed"),
    ),
    security(("bill8:payments:disputes:write" = [])),
    tag = "payments",
)]
pub async fn resolve_dispute(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<ResolveDisputeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:disputes:write"),
    )
    .await?;

    if req.outcome != "won" && req.outcome != "lost" {
        return Err((
            StatusCode::BAD_REQUEST,
            "outcome must be won or lost".into(),
        ));
    }

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // Guarded terminal transition: under_review → won|lost. The charge
    // join carries the currency the clawback posts in.
    let result = sqlx::query(
        "UPDATE payments.disputes SET status = $1, resolved_at = now(), resource_version = resource_version + 1 WHERE id = $2 AND status IN ('needs_response', 'under_review') RETURNING charge_id, amount",
    )
    .bind(&req.outcome)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::CONFLICT, "dispute already closed".into()))?;
    let charge_id: Uuid = result.get("charge_id");
    let amount: Decimal = result.get("amount");
    let charge_row = sqlx::query("SELECT currency FROM payments.charges WHERE id = $1")
        .bind(charge_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(err_db)?;
    let currency: String = charge_row.get("currency");

    // A lost dispute claws the funds back: one ledger reversal for the
    // disputed amount (the ONLY money movement), then the charge
    // returns to succeeded (it keeps its balance_transaction minus the
    // clawed amount - the reversal transaction is the record).
    if req.outcome == "lost" {
        let refund_req = bill8::payments::contracts::CreateRefundRequest {
            charge_id,
            amount: Some(amount.to_string()),
            idempotency_key: Some(format!("dispute-{id}")),
        };
        let tx_ref = crate::channels::post_refund_to_ledger(
            &state,
            a.env_id(),
            &refund_req,
            amount,
            &currency,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("clawback posting failed: {e}"),
            )
        })?;
        let _ = tx_ref;
        sqlx::query(
            "UPDATE payments.charges SET status = 'succeeded', resource_version = resource_version + 1 WHERE id = $1",
        )
        .bind(charge_id)
        .execute(&mut *conn)
        .await
        .map_err(err_db)?;
    }

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:dispute:closed', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "dispute_id": id, "charge_id": charge_id, "outcome": req.outcome }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "dispute_id": id, "status": req.outcome }),
    ))
}
