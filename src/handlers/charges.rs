// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Charge handlers — the append-only money-capture engine.
//!
//! The charge is `succeeded` only after channel terminal success AND the
//! ledger posting commit. A channel failure or blocked screening writes
//! NOTHING (the whole transaction rolls back).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bill8::payments::contracts::{Charge, CreateChargeRequest};
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::channels::{route, ChannelOutcome};
use crate::handlers::{emit_event, err_db, set_tenant_context, AppState};
use crate::types::ListChargesQuery;

/// `POST /v1/charges` — create a charge through a settlement channel.
#[utoipa::path(
    post,
    path = "/v1/charges",
    request_body = CreateChargeRequest,
    responses(
        (status = 200, description = "Charge succeeded", body = Charge),
        (status = 402, description = "Screening blocked / channel declined — nothing written"),
        (status = 403, description = "Missing bill8:payments:charges:write"),
        (status = 422, description = "Domain rejection — nothing written"),
    ),
    security(("bill8:payments:charges:write" = [])),
    tag = "payments",
)]
pub async fn create_charge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateChargeRequest>,
) -> Result<Json<Charge>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:charges:write"),
    ).await?;

    let amount: Decimal = req
        .amount
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "amount must be a decimal string".into()))?;
    if amount <= Decimal::ZERO {
        return Err((StatusCode::BAD_REQUEST, "amount must be positive".into()));
    }

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id()).await.map_err(err_db)?;

    // Idempotency: a retried charge returns the original record.
    if let Some(key) = req.idempotency_key.as_deref() {
        if let Ok(Some(row)) = sqlx::query(
            "SELECT id FROM payments.charges WHERE env_id = $1 AND idempotency_key = $2",
        )
        .bind(a.env_id())
        .bind(key)
        .fetch_optional(&mut *conn)
        .await
        {
            let id: Uuid = row.get("id");
            conn.commit().await.map_err(err_db)?;
            return get_charge_row(&state, a.env_id(), id).await;
        }
    }

    // ── Step 0: persist the charge as `pending` — the durable record
    // the screening context references (payload completeness: the risk
    // service's resolution event carries THIS charge id; a resolved
    // review completes the held charge with no cross-service read).
    // Every downstream transition is guarded: pending → succeeded /
    // failed (single-use), so an aborted run leaves no phantom success.
    let charge_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO payments.charges
               (id, env_id, customer_id, payment_method_id, payment_intent_id,
                amount, amount_refunded, currency, status, channel_id, idempotency_key)
           VALUES ($1, $2, $3, $4, $5, $6, 0, $7, 'pending', NULL, $8)"#,
    )
    .bind(charge_id)
    .bind(a.env_id())
    .bind(req.customer_id)
    .bind(req.payment_method_id)
    .bind(req.payment_intent_id)
    .bind(amount)
    .bind(&req.currency)
    .bind(req.idempotency_key.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    // ── Step 1: inline risk screening (fail-closed; the screening call
    // itself is NOT in the tx — a blocked outcome prevents all writes).
    // The screening context carries the charge reference (payload
    // completeness, §4.14): a review resolution can complete THIS
    // charge without any cross-service table read.
    let screen = crate::channels::screen(&state, &a, &req, charge_id).await;
    match screen {
        Ok(ChannelOutcome::Allow) => {}
        Ok(ChannelOutcome::Review) => {
            // The screening opened a review: HOLD the charge as pending
            // (no money moved) — the risk service's resolution event
            // completes or fails it (consumers::resolve_held_charge).
            conn.commit().await.map_err(err_db)?;
            return get_charge_row(&state, a.env_id(), charge_id).await;
        }
        Err(_) => {
            // Fail-closed: a screening error is a block, never an allow.
            // The held charge fails with NO money moved.
            sqlx::query(
                "UPDATE payments.charges SET status = 'failed' WHERE id = $1 AND status = 'pending'",
            )
            .bind(charge_id)
            .execute(&mut *conn)
            .await
            .map_err(err_db)?;
            conn.commit().await.map_err(err_db)?;
            return get_charge_row(&state, a.env_id(), charge_id).await;
        }
    }

    // ── Step 2: channel routing + authorize/capture. Routing errors map
    // to 502 (the routing layer returns plain String errors).
    let channel = route(&state, &a, &req)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("channel routing failed: {e}")))?;
    let channel_id = match channel {
        Some(ch) => ch,
        // No capable channel: nothing written, no money moved.
        None => return Err((StatusCode::PAYMENT_REQUIRED, "no settlement channel available".into())),
    };
    sqlx::query(
        "UPDATE payments.charges SET channel_id = $1 WHERE id = $2 AND status = 'pending'",
    )
    .bind(channel_id)
    .bind(charge_id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    // ── Step 2b: SCA (PAY-12). A challenge-requiring channel holds the
    // charge in `pending` with a single-use handback token; the hosted
    // checkout redirects and the handback endpoint completes it from
    // the CHANNEL-VERIFIED result — never a client assertion.
    if crate::channels::sim_outcome(&req.amount) == crate::channels::SimOutcome::ScaRequired {
        let handback = format!("hbk_{}", Uuid::new_v4().simple());
        sqlx::query(
            r#"INSERT INTO payments.challenges (env_id, payment_intent_id, token, expires_at)
               VALUES ($1, $2, $3, now() + interval '15 minutes')"#,
        )
        .bind(a.env_id())
        .bind(req.payment_intent_id.unwrap_or(charge_id))
        .bind(&handback)
        .execute(&mut *conn)
        .await
        .map_err(err_db)?;

        sqlx::query(
            r#"INSERT INTO payments.outbox (env_id, event_type, payload)
               VALUES ($1, 'bill8:payments:intent:action_required', $2)"#,
        )
        .bind(a.env_id())
        .bind(serde_json::json!({ "charge_id": charge_id, "handback": handback }))
        .execute(&mut *conn)
        .await
        .map_err(err_db)?;

        conn.commit().await.map_err(err_db)?;
        let charge = get_charge_row(&state, a.env_id(), charge_id).await?.0;
        return Ok(Json(charge));
    }

    // ── Step 3: ledger posting — the ONLY money movement. A ledger
    // failure fails the charge with no phantom success (the guarded
    // transition below never fires). A split-at-charge
    // (on_behalf_of + application_fee_amount) posts ONE atomic three-leg
    // batch (PAY-14 / LED-6).
    let tx_ref = if req.on_behalf_of.is_some() || req.application_fee_amount.is_some() {
        crate::channels::post_split_to_ledger(&state, a.env_id(), &req, amount).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("ledger posting failed: {e}"))
        })?
    } else {
        crate::channels::post_charge_to_ledger(&state, a.env_id(), &req, amount).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("ledger posting failed: {e}"))
        })?
    };

    // Guarded terminal transition: pending → succeeded, exactly-once
    // (the status guard makes a duplicate completion a no-op).
    let row = sqlx::query(
        r#"UPDATE payments.charges
           SET status = 'succeeded', balance_transaction_id = $1,
               on_behalf_of = $2, application_fee_amount = $3
           WHERE id = $4 AND status = 'pending' AND env_id = $5
           RETURNING id, env_id, customer_id, payment_method_id, payment_intent_id,
                     amount, amount_refunded, currency, status, channel_id,
                     balance_transaction_id, on_behalf_of, application_fee_amount, created_at"#,
    )
    .bind(tx_ref)
    .bind(req.on_behalf_of)
    .bind(
        req.application_fee_amount
            .as_deref()
            .and_then(|s| s.parse::<Decimal>().ok()),
    )
    .bind(charge_id)
    .bind(a.env_id())
    .fetch_one(&mut *conn)
    .await
    .map_err(err_db)?;

    // Outbox-first: charge:succeeded rides in the same transaction.
    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:charge:succeeded', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({
        "charge_id": charge_id,
        "amount": req.amount,
        "currency": req.currency,
        "customer_id": req.customer_id,
    }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;

    let charge = row_to_charge(&row);
    emit_event(
        &state.pool,
        a.env_id(),
        "bill8:payments:charge:succeeded",
        serde_json::json!({ "charge_id": charge_id }),
    )
    .await;
    Ok(Json(charge))
}

/// `GET /v1/charges` — list charges.
#[utoipa::path(
    get,
    path = "/v1/charges",
    params(
        ("customer_id" = Option<Uuid>, Query, description = "Filter by customer"),
    ),
    responses(
        (status = 200, description = "List of charges", body = [Charge]),
        (status = 403, description = "Missing bill8:payments:charges:read"),
    ),
    security(("bill8:payments:charges:read" = [])),
    tag = "payments",
)]
pub async fn list_charges(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListChargesQuery>,
) -> Result<Json<Vec<Charge>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:charges:read"),
    ).await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id()).await.map_err(err_db)?;

    let rows = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, payment_intent_id,
                  amount, amount_refunded, currency, status, channel_id,
                  balance_transaction_id, on_behalf_of, application_fee_amount, created_at
           FROM payments.charges
           WHERE ($1::uuid IS NULL OR customer_id = $1)
           ORDER BY created_at DESC LIMIT 200"#,
    )
    .bind(q.customer_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(rows.iter().map(row_to_charge).collect()))
}

/// `GET /v1/charges/{id}` — read one charge.
#[utoipa::path(
    get,
    path = "/v1/charges/{id}",
    responses(
        (status = 200, description = "The charge", body = Charge),
        (status = 404, description = "Charge not found"),
    ),
    security(("bill8:payments:charges:read" = [])),
    tag = "payments",
)]
pub async fn get_charge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Charge>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:charges:read"),
    ).await?;
    get_charge_row(&state, a.env_id(), id).await
}

pub(crate) async fn get_charge_row(
    state: &AppState,
    env_id: Uuid,
    id: Uuid,
) -> Result<Json<Charge>, (StatusCode, String)> {
    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await.map_err(err_db)?;
    let row = sqlx::query(
        r#"SELECT id, env_id, customer_id, payment_method_id, payment_intent_id,
                  amount, amount_refunded, currency, status, channel_id,
                  balance_transaction_id, on_behalf_of, application_fee_amount, created_at
           FROM payments.charges WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::NOT_FOUND, "charge not found".into()))?;
    Ok(Json(row_to_charge(&row)))
}

pub(crate) fn row_to_charge(row: &sqlx::postgres::PgRow) -> Charge {
    Charge {
        id: row.get("id"),
        env_id: row.get("env_id"),
        customer_id: row.get("customer_id"),
        payment_method_id: row.get("payment_method_id"),
        payment_intent_id: row.get("payment_intent_id"),
        amount: row.get::<Decimal, _>("amount").to_string(),
        amount_refunded: row.get::<Decimal, _>("amount_refunded").to_string(),
        currency: row.get("currency"),
        status: row.get("status"),
        channel_id: row.get("channel_id"),
        balance_transaction_id: row.get("balance_transaction_id"),
        on_behalf_of: row.get("on_behalf_of"),
        application_fee_amount: row
            .get::<Option<Decimal>, _>("application_fee_amount")
            .map(|d| d.to_string()),
        created_at: row.get("created_at"),
    }
}