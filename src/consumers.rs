// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Sentin8:Bus consumers for Bill8:Payments (ENV-1 + RSK-3):
//!
//! - `sys` family, group `bill8:payments:envinit` — idempotent
//!   per-tenant seeding on `sentin8:sys:service:updated {enabled}`.
//! - `risk` family, group `bill8:payments:review` — resolves held
//!   charges when reviews resolve (approved → complete; declined →
//!   fail; exactly-once via the terminal-state guard).

use std::time::Duration;

use futures_util::StreamExt;
use sqlx::Row;
use uuid::Uuid;

use crate::handlers::AppState;

/// Reconnect delay.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Seed the default channel + config for a newly-enabled env
/// (idempotent: replaying the event creates zero duplicate rows).
async fn seed_env(state: &AppState, env_id: Uuid) {
    let mut conn = match state.pool.acquire().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "payments: envinit acquire failed");
            return;
        }
    };
    if let Err(e) = crate::handlers::set_tenant_context(&mut conn, env_id).await {
        tracing::warn!(error = %e, "payments: envinit context failed");
        return;
    }

    // Seed the built-in simulation channel (test mode) + default config.
    let sim = sqlx::query(
        r#"INSERT INTO payments.channels (env_id, name, label, methods, regions, recurring, mode)
           VALUES ($1, 'simulation', 'Simulation', '["card","bank_debit","wallet"]', '["global"]', true, 'test')
           ON CONFLICT (env_id, name) DO NOTHING
           RETURNING id"#,
    )
    .bind(env_id)
    .fetch_one(&mut *conn)
    .await;
    match sim {
        Ok(row) => {
            let channel_id: Uuid = row.get("id");
            // Default-channel config (only when absent — idempotent).
            sqlx::query(
                r#"INSERT INTO payments.channel_configs (env_id, default_channel)
                   VALUES ($1, $2)
                   ON CONFLICT (env_id) DO NOTHING"#,
            )
            .bind(env_id)
            .bind(channel_id)
            .execute(&mut *conn)
            .await
            .ok();
        }
        Err(e) => {
            tracing::warn!(error = %e, "payments: envinit channel seed failed");
        }
    }
}

/// Resolve a held charge when its review resolves (RSK-3 wiring).
/// The resolution event's payload carries the CHARGE reference
/// (payload completeness, architecture-principles §4.14 — the consumer
/// never reads the risk service's tables; a cross-schema grant is a
/// defect). Approved → complete the charge (ledger posting — the ONLY
/// money movement); declined → fail it (no money moved). Exactly-once:
/// the guarded `status = 'pending'` transition makes a duplicate
/// resolution a no-op.
async fn resolve_held_charge(state: &AppState, env_id: Uuid, payload: serde_json::Value) {
    let Some(charge_id) = payload
        .get("charge_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        tracing::warn!("payments: review resolution lacks the charge reference — ignoring (the risk service carries it in the resolution payload)");
        return;
    };
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("declined");

    let mut conn = match state.pool.acquire().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "payments: review consumer acquire failed");
            return;
        }
    };
    if let Err(e) = crate::handlers::set_tenant_context(&mut conn, env_id).await {
        tracing::warn!(error = %e, "payments: review consumer context failed");
        return;
    }

    // Row-lock the held charge; its amount drives the ledger posting.
    let held = sqlx::query(
        "SELECT id, amount, currency, on_behalf_of, application_fee_amount FROM payments.charges WHERE id = $1 AND status = 'pending' FOR UPDATE",
    )
    .bind(charge_id)
    .fetch_optional(&mut *conn)
    .await;
    let Ok(Some(held)) = held else {
        return; // Not pending — a duplicate resolution is a no-op.
    };

    let amount: rust_decimal::Decimal = held.get("amount");
    let currency: String = held.get("currency");
    let on_behalf_of: Option<Uuid> = held.get("on_behalf_of");
    let application_fee: Option<rust_decimal::Decimal> = held.get("application_fee_amount");
    let charge_id: Uuid = held.get("id");

    if status == "approved" {
        // Complete the held charge: routing is already satisfied (the
        // screening passed review); post to the ledger — the ONLY money
        // movement — then the guarded terminal transition.
        let charge_req = bill8::payments::contracts::CreateChargeRequest {
            customer_id: None,
            payment_method_id: None,
            payment_intent_id: None,
            amount: amount.to_string(),
            currency,
            on_behalf_of,
            application_fee_amount: application_fee.map(|d| d.to_string()),
            idempotency_key: payload
                .get("idempotency_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    Some(format!(
                        "review-{}-{charge_id}",
                        payload
                            .get("review_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("x")
                    ))
                }),
        };
        let tx_ref = if on_behalf_of.is_some() || application_fee.is_some() {
            crate::channels::post_split_to_ledger(state, env_id, &charge_req, amount).await
        } else {
            crate::channels::post_charge_to_ledger(state, env_id, &charge_req, amount).await
        };
        match tx_ref {
            Ok(tx) => {
                if let Err(e) = sqlx::query(
                    "UPDATE payments.charges SET status = 'succeeded', balance_transaction_id = $1 WHERE id = $2 AND status = 'pending'",
                )
                .bind(tx)
                .bind(charge_id)
                .execute(&mut *conn)
                .await
                {
                    tracing::warn!(error = %e, "payments: review completion update failed");
                }
                let _ = sqlx::query(
                    r#"INSERT INTO payments.outbox (env_id, event_type, payload)
                       VALUES ($1, 'bill8:payments:charge:succeeded', $2)"#,
                )
                .bind(env_id)
                .bind(serde_json::json!({ "charge_id": charge_id, "amount": amount.to_string() }))
                .execute(&mut *conn)
                .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "payments: review completion ledger posting failed");
                let _ = sqlx::query(
                    "UPDATE payments.charges SET status = 'failed' WHERE id = $1 AND status = 'pending'",
                )
                .bind(charge_id)
                .execute(&mut *conn)
                .await;
            }
        }
    } else {
        // Declined: fail the held charge (no money moved).
        let _ = sqlx::query(
            "UPDATE payments.charges SET status = 'failed' WHERE id = $1 AND status = 'pending'",
        )
        .bind(charge_id)
        .execute(&mut *conn)
        .await;
    }
}

/// Spawn the payments service's consumer loops (called from
/// `build_state`).
pub fn spawn_consumers(state: AppState) {
    // Env-init: idempotent per-tenant seeding on service:updated.
    {
        let bus = state.bus_client.clone();
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                match bus.sse_consume("sys", "bill8:payments:envinit").await {
                    Ok(resp) => {
                        let mut stream = resp.bytes_stream();
                        let mut buffer = String::new();
                        while let Some(chunk) = stream.next().await {
                            let Ok(c) = chunk else { break };
                            buffer.push_str(&String::from_utf8_lossy(&c));
                            while let Some(idx) = buffer.find('\n') {
                                let line = buffer[..idx].to_string();
                                buffer.drain(..=idx);
                                let Some(data) = line.strip_prefix("data: ") else {
                                    continue;
                                };
                                if let Ok(msg) =
                                    serde_json::from_str::<serde_json::Value>(data.trim())
                                {
                                    // ACK contract (BUSACK-1): an entry
                                    // stays PENDING in the consumer group
                                    // until ACKed — unacked entries are
                                    // re-delivered by bus's XAUTOCLAIM
                                    // every 10s forever, starving the
                                    // produce path fleet-wide. Ack EVERY
                                    // processed message, including ones
                                    // with a payload this consumer ignores
                                    // (seed_env is idempotent; a re-delivery
                                    // costs nothing once acked).
                                    let message_id = msg
                                        .get("message_id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let env_id = msg
                                        .get("env_id")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| Uuid::parse_str(s).ok());
                                    let event_type = msg
                                        .get("event_type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if event_type == "sentin8:sys:service:updated" {
                                        if let Some(env_id) = env_id {
                                            seed_env(&state, env_id).await;
                                        }
                                    }
                                    if !message_id.is_empty() {
                                        if let Err(e) = bus
                                            .ack("sys", "bill8:payments:envinit", message_id)
                                            .await
                                        {
                                            tracing::warn!(error = %e, message_id, "payments: envinit ack failed");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "payments envinit consume failed — reconnecting")
                    }
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        });
    }

    // Review resolution: complete/fail held charges (RSK-3).
    {
        let bus = state.bus_client.clone();
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                match bus.sse_consume("risk", "bill8:payments:review").await {
                    Ok(resp) => {
                        let mut stream = resp.bytes_stream();
                        let mut buffer = String::new();
                        while let Some(chunk) = stream.next().await {
                            let Ok(c) = chunk else { break };
                            buffer.push_str(&String::from_utf8_lossy(&c));
                            while let Some(idx) = buffer.find('\n') {
                                let line = buffer[..idx].to_string();
                                buffer.drain(..=idx);
                                let Some(data) = line.strip_prefix("data: ") else {
                                    continue;
                                };
                                if let Ok(msg) =
                                    serde_json::from_str::<serde_json::Value>(data.trim())
                                {
                                    let env_id = msg
                                        .get("env_id")
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| Uuid::parse_str(s).ok());
                                    let event_type = msg
                                        .get("event_type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let payload =
                                        msg.get("payload").cloned().unwrap_or(msg.clone());
                                    if event_type == "bill8:risk:review:resolved" {
                                        if let Some(env_id) = env_id {
                                            resolve_held_charge(&state, env_id, payload).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "payments review consume failed — reconnecting")
                    }
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        });
    }
}
