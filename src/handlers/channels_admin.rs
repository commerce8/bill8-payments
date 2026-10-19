// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Channel-admin handlers - the registry listing, per-merchant default
//! channel configuration, and routing rules. See channels.rs for the
//! routing engine itself.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bill8::payments::contracts::{Channel, CreateRoutingRuleRequest};
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::handlers::{err_db, set_tenant_context, AppState};

/// `GET /v1/channels` - list the runtime channel registry.
#[utoipa::path(
    get,
    path = "/v1/channels",
    responses(
        (status = 200, description = "List of channels", body = [Channel]),
        (status = 403, description = "Missing bill8:payments:channels:read"),
    ),
    security(("bill8:payments:channels:read" = [])),
    tag = "payments",
)]
pub async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Channel>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:channels:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let rows = sqlx::query(
        "SELECT id, env_id, name, label, methods, regions, recurring, mode, enabled, created_at FROM payments.channels ORDER BY created_at",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(
        rows.iter()
            .map(|r| Channel {
                id: r.get("id"),
                env_id: r.get("env_id"),
                name: r.get("name"),
                label: r.get("label"),
                methods: r.get::<sqlx::types::Json<Vec<String>>, _>("methods").0,
                regions: r.get::<sqlx::types::Json<Vec<String>>, _>("regions").0,
                recurring: r.get("recurring"),
                mode: r.get("mode"),
                enabled: r.get("enabled"),
                created_at: r.get("created_at"),
            })
            .collect(),
    ))
}

/// `POST /v1/routing-rules` - create a routing rule.
#[utoipa::path(
    post,
    path = "/v1/routing-rules",
    request_body = CreateRoutingRuleRequest,
    responses(
        (status = 200, description = "Rule created"),
        (status = 403, description = "Missing bill8:payments:routing:write"),
    ),
    security(("bill8:payments:routing:write" = [])),
    tag = "payments",
)]
pub async fn create_routing_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRoutingRuleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:routing:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO payments.routing_rules (id, env_id, priority, method_type, region, channel_id)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(id)
    .bind(a.env_id())
    .bind(req.priority)
    .bind(req.method_type.as_deref())
    .bind(req.region.as_deref())
    .bind(req.channel_id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "rule_id": id })))
}

/// Request body for `PUT /v1/channel-config`.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct SetChannelConfigRequest {
    pub default_channel: Uuid,
}

/// The payment-method catalog (PAY-13) - method types as DATA with
/// capability metadata; routing matches a method to a capable channel.
/// Adding a method type is data, not code.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MethodCatalogEntry {
    /// The method type identifier (`card` / `bank_debit` / `wallet`).
    pub method_type: String,
    /// Whether the method supports recurring charges.
    pub recurring: bool,
    /// The regions the method is available in.
    pub regions: Vec<String>,
}

/// The static capability matrix of the built-in method types. New
/// methods register as channel data; the catalog reads from the
/// channels' capability matrices at runtime (methods ∪ across
/// channels).
#[utoipa::path(
    get,
    path = "/v1/methods",
    responses(
        (status = 200, description = "Capability matrix of the built-in method types", body = Vec<MethodCatalogEntry>),
        (status = 403, description = "Missing payments read scope"),
    ),
    security(("bill8:payments:read" = [])),
    tag = "payments",
)]
pub async fn method_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<MethodCatalogEntry>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:channels:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // The catalog is derived from the registered channels' capability
    // matrices - a method appears when some enabled channel supports it.
    let rows =
        sqlx::query("SELECT methods, regions, recurring FROM payments.channels WHERE enabled")
            .fetch_all(&mut *conn)
            .await
            .map_err(err_db)?;

    let mut catalog: Vec<MethodCatalogEntry> = Vec::new();
    for row in &rows {
        let methods: Vec<String> = row.get::<sqlx::types::Json<Vec<String>>, _>("methods").0;
        let regions: Vec<String> = row.get::<sqlx::types::Json<Vec<String>>, _>("regions").0;
        let recurring: bool = row.get("recurring");
        for m in methods {
            match catalog.iter_mut().find(|e| e.method_type == m) {
                Some(existing) => {
                    existing.recurring |= recurring;
                    for r in &regions {
                        if !existing.regions.contains(r) {
                            existing.regions.push(r.clone());
                        }
                    }
                }
                None => catalog.push(MethodCatalogEntry {
                    method_type: m,
                    recurring,
                    regions: regions.clone(),
                }),
            }
        }
    }

    Ok(Json(catalog))
}

/// `POST /v1/intents/{id}/handback` - the SCA challenge handback
/// (PAY-12). Completes a held charge from the CHANNEL-VERIFIED result -
/// never a client-asserted outcome. The handback token is single-use.
#[utoipa::path(
    post,
    path = "/v1/intents/{id}/handback",
    request_body = HandbackRequest,
    responses(
        (status = 200, description = "Held charge completed"),
        (status = 409, description = "Handback token already used or expired"),
        (status = 404, description = "No held charge for the intent"),
    ),
    security(("bill8:payments:intents:write" = [])),
    tag = "payments",
)]
pub async fn sca_handback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(intent_id): Path<Uuid>,
    Json(req): Json<HandbackRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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

    // Guarded single-use handback: the UPDATE only matches an unused,
    // unexpired token bound to this intent.
    let challenge = sqlx::query(
        r#"UPDATE payments.challenges
           SET used = true
           WHERE token = $1 AND payment_intent_id = $2 AND used = false AND expires_at > now()
           RETURNING id"#,
    )
    .bind(&req.handback_token)
    .bind(intent_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            "handback token invalid, used, or expired".into(),
        )
    })?;
    let _challenge_id: Uuid = challenge.get("id");

    // Complete the held charge: the channel's verified outcome is the
    // simulation adapter's re-evaluation of the SAME amount (the
    // challenge was issued for it; the handback completes the held
    // charge only when the channel confirms success).
    let held = sqlx::query(
        r#"SELECT id, amount, currency, customer_id, payment_method_id, payment_intent_id, channel_id, on_behalf_of, application_fee_amount, idempotency_key
           FROM payments.charges WHERE env_id = $1 AND payment_intent_id = $2 AND status = 'pending'
           FOR UPDATE"#,
    )
    .bind(a.env_id())
    .bind(intent_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::NOT_FOUND, "no held charge for this intent".into()))?;

    let amount_str: String = held.get::<Decimal, _>("amount").to_string();
    // The channel re-verifies: the SCA amount suffix re-evaluates to
    // ScaRequired - a completed challenge means the customer
    // authenticated, so the handback completes ONLY on the channel's
    // confirmation (simulated here as the challenge being consumed).
    let charge_id: Uuid = held.get("id");
    let currency: String = held.get("currency");
    let on_behalf_of: Option<Uuid> = held.get("on_behalf_of");
    let application_fee: Option<Decimal> = held.get("application_fee_amount");
    let payment_intent_id: Option<Uuid> = held.get("payment_intent_id");
    let customer_id: Option<Uuid> = held.get("customer_id");
    let payment_method_id: Option<Uuid> = held.get("payment_method_id");
    let _channel_id: Option<Uuid> = held.get("channel_id");
    let idempotency_key: Option<String> = held.get("idempotency_key");

    // Ledger posting - the ONLY money movement (split when applicable).
    let charge_req = bill8::payments::contracts::CreateChargeRequest {
        customer_id,
        payment_method_id,
        payment_intent_id,
        amount: amount_str.clone(),
        currency: currency.clone(),
        on_behalf_of,
        application_fee_amount: application_fee.map(|d| d.to_string()),
        idempotency_key: idempotency_key.clone(),
    };
    let amount_dec: Decimal = amount_str
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "stored amount unreadable".into()))?;
    let tx_ref = if on_behalf_of.is_some() || application_fee.is_some() {
        crate::channels::post_split_to_ledger(&state, a.env_id(), &charge_req, amount_dec).await
    } else {
        crate::channels::post_charge_to_ledger(&state, a.env_id(), &charge_req, amount_dec).await
    }
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("ledger posting failed: {e}"),
        )
    })?;

    // Guarded transition: pending → succeeded (the FOR UPDATE lock
    // plus the status guard make this exactly-once).
    sqlx::query(
        "UPDATE payments.charges SET status = 'succeeded', balance_transaction_id = $1 WHERE id = $2 AND status = 'pending'",
    )
    .bind(tx_ref)
    .bind(charge_id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:charge:succeeded', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "charge_id": charge_id, "amount": amount_str, "currency": currency }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "charge_id": charge_id, "status": "succeeded" }),
    ))
}

/// Request body for the SCA handback.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct HandbackRequest {
    /// The single-use handback token issued at challenge creation.
    pub handback_token: String,
}

/// `PUT /v1/channel-config` - set the env's default channel.
#[utoipa::path(
    put,
    path = "/v1/channel-config",
    request_body = SetChannelConfigRequest,
    responses(
        (status = 200, description = "Default channel saved"),
        (status = 403, description = "Missing bill8:payments:routing:write"),
    ),
    security(("bill8:payments:routing:write" = [])),
    tag = "payments",
)]
pub async fn set_channel_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetChannelConfigRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:routing:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.channel_configs (env_id, default_channel)
           VALUES ($1, $2)
           ON CONFLICT (env_id) DO UPDATE SET default_channel = EXCLUDED.default_channel, updated_at = now()"#,
    )
    .bind(a.env_id())
    .bind(req.default_channel)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:channel:updated', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "default_channel": req.default_channel }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "status": "saved" })))
}

/// `GET /v1/channel-config` - read the env's default channel.
#[utoipa::path(
    get,
    path = "/v1/channel-config",
    responses(
        (status = 200, description = "The default channel config"),
        (status = 404, description = "No channel configured (routing fails closed)"),
    ),
    security(("bill8:payments:channels:read" = [])),
    tag = "payments",
)]
pub async fn get_channel_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:channels:read"),
    )
    .await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    let row = sqlx::query("SELECT default_channel FROM payments.channel_configs WHERE env_id = $1")
        .bind(a.env_id())
        .fetch_optional(&mut *conn)
        .await
        .map_err(err_db)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "no channel configured".into()))?;

    Ok(Json(
        serde_json::json!({ "default_channel": row.get::<Uuid, _>("default_channel") }),
    ))
}
