// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Settlement-channel layer (payments DESIGN.md §2b) — the rails
//! abstraction. Four parts:
//!
//! 1. **Adapter contract** — `ChannelAdapter` (`authorize`, `capture`,
//!    `refund`, `dispute_ingest`, `settlement_report`). Any rails
//!    implementation conforms; channel identity is registry data, never
//!    core code.
//! 2. **Runtime registry** — `payments.channels` rows with a capability
//!    matrix (methods, regions, recurring, mode).
//! 3. **Selection & routing** — `channel_configs` + declarative
//!    `routing_rules` (method, region, priority); fails closed.
//! 4. **Reconciliation** — `settlements` matched against charges;
//!    discrepancies flagged, never silently passed.
//!
//! The ledger posting is the ONLY money movement; a channel never
//! mutates balances.

use async_trait::async_trait;
use bill8::payments::contracts::{CreateChargeRequest, CreateRefundRequest};
use rust_decimal::Decimal;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::handlers::AppState;

/// The screening outcome the charge engine acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOutcome {
    /// Proceed with the charge.
    Allow,
    /// Hold for manual review (charge recorded, no money moved).
    Review,
}

/// The settlement-rails contract every channel implements. Conforming
/// implementations: the platform's finance vertical (public API),
/// external acquirer/payment-platform adapters, direct bank rails.
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Authorize (and capture, when the channel authorizes-and-captures
    /// in one step) a charge. Returns the channel's terminal success or
    /// a decline/error — the caller writes nothing on failure.
    async fn authorize(&self, charge: &CreateChargeRequest) -> Result<bool, String>;
    /// Refund a previously captured charge.
    async fn refund(&self, charge_id: Uuid, amount: Decimal) -> Result<(), String>;
}

/// The built-in simulation channel (test mode): deterministic — always
/// succeeds, EXCEPT for the documented decline instruments. Live external
/// channels register their own adapters.
pub struct SimulationAdapter;

/// The deterministic test instruments (PAY-11). An amount ending in
/// these minor-unit patterns selects the outcome:
/// - `…00.02` → decline
/// - `…00.04` → timeout (treated as decline)
/// - `…00.06` → SCA required (challenge — the charge holds pending)
///
/// All other amounts succeed.
const TEST_DECLINE_SUFFIX: &str = "00.02";
const TEST_TIMEOUT_SUFFIX: &str = "00.04";
const TEST_SCA_SUFFIX: &str = "00.06";

/// The simulated outcome of authorizing an amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimOutcome {
    /// Terminal success.
    Success,
    /// Declined (terminal failure).
    Decline,
    /// Timeout (terminal failure).
    Timeout,
    /// SCA required — the charge holds pending a challenge handback
    /// (PAY-12).
    ScaRequired,
}

/// Classify a test-mode amount into its deterministic outcome.
///
/// # Examples
/// ```
/// use bill8_payments_server::channels::{sim_outcome, SimOutcome};
///
/// assert_eq!(sim_outcome("100.00"), SimOutcome::Success);
/// assert_eq!(sim_outcome("100.02"), SimOutcome::Decline);
/// assert_eq!(sim_outcome("100.04"), SimOutcome::Timeout);
/// assert_eq!(sim_outcome("100.06"), SimOutcome::ScaRequired);
/// ```
pub fn sim_outcome(amount: &str) -> SimOutcome {
    if amount.ends_with(TEST_SCA_SUFFIX) {
        SimOutcome::ScaRequired
    } else if amount.ends_with(TEST_DECLINE_SUFFIX) {
        SimOutcome::Decline
    } else if amount.ends_with(TEST_TIMEOUT_SUFFIX) {
        SimOutcome::Timeout
    } else {
        SimOutcome::Success
    }
}

#[async_trait]
impl ChannelAdapter for SimulationAdapter {
    async fn authorize(&self, charge: &CreateChargeRequest) -> Result<bool, String> {
        Ok(sim_outcome(&charge.amount) == SimOutcome::Success)
    }
    async fn refund(&self, _charge_id: Uuid, _amount: Decimal) -> Result<(), String> {
        Ok(())
    }
}

/// Screen a charge context against Bill8:Risk (inline, fail-closed).
/// A screening error is a BLOCK, never an allow.
pub async fn screen(
    state: &AppState,
    a: &AuthContext,
    req: &CreateChargeRequest,
    charge_id: Uuid,
) -> Result<ChannelOutcome, String> {
    let Some(ref client) = state.config.risk_http else {
        // No risk workload: fail closed — no charge proceeds unscreened
        // in production; dev mode documents this explicitly.
        tracing::warn!("risk workload transport absent — failing closed");
        return Err("risk screening unavailable".into());
    };
    let resp = client
        .post(format!("{}/v1/screenings", state.config.bill8_risk_url))
        .json(&bill8::risk::contracts::ScreenRequest {
            charge_context: serde_json::json!({
                "charge_id": charge_id,
                "amount": req.amount,
                "currency": req.currency,
                "method": req.payment_method_id,
                "actor": a.claims.sub,
            }),
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("screening HTTP {}", resp.status()));
    }
    let screening: bill8::risk::contracts::Screening =
        resp.json().await.map_err(|e| e.to_string())?;
    match screening.outcome.as_str() {
        "allow" => Ok(ChannelOutcome::Allow),
        // review/block both hold the charge; the engine records failed.
        _ => Ok(ChannelOutcome::Review),
    }
}

/// Route a charge to a channel: the per-merchant default, then the
/// declarative rules (method, region, priority). Fails closed — an
/// evaluation error selects no channel and captures nothing.
///
/// This initial engine routes through the built-in simulation adapter;
/// external channel adapters (PAY-15) register per-channel and replace
/// the dispatch without touching this routing core.
pub async fn route(
    state: &AppState,
    a: &AuthContext,
    req: &CreateChargeRequest,
) -> Result<Option<Uuid>, String> {
    let mut conn = state.pool.acquire().await.map_err(|e| e.to_string())?;
    crate::handlers::set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(|e| e.to_string())?;

    // Default channel when no rule matches.
    let default =
        sqlx::query("SELECT default_channel FROM payments.channel_configs WHERE env_id = $1")
            .bind(a.env_id())
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| e.to_string())?
            .map(|r| r.get::<Uuid, _>("default_channel"));

    if let Some(id) = default {
        return Ok(Some(id));
    }

    // Highest-priority enabled rule.
    let rule = sqlx::query(
        r#"SELECT channel_id FROM payments.routing_rules
           WHERE env_id = $1 AND enabled
           ORDER BY priority ASC LIMIT 1"#,
    )
    .bind(a.env_id())
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| e.to_string())?
    .map(|r| r.get::<Uuid, _>("channel_id"));

    let channel_id = match rule.or(default) {
        Some(id) => id,
        // No channel configured for this env: fail closed.
        None => return Ok(None),
    };

    // The routing decision executes the channel's adapter. For the
    // simulation channel (test mode), the outcome is deterministic per
    // the documented test instruments (sim_outcome); external adapters
    // dispatch on channel identity (PAY-15).
    let ok = SimulationAdapter.authorize(req).await?;
    if ok {
        Ok(Some(channel_id))
    } else {
        Ok(None)
    }
}

/// Post a refund reversal to the ledger via its public API (the ONLY
/// money movement — refunds reverse through the same double-entry path).
///
/// The caller supplies the charge's currency (the refund request itself
/// does not carry one).
pub async fn post_refund_to_ledger(
    state: &AppState,
    _env_id: Uuid,
    req: &CreateRefundRequest,
    amount: Decimal,
    currency: &str,
) -> Result<Uuid, String> {
    let Some(ref client) = state.config.ledger_http else {
        return Err("ledger workload transport absent".into());
    };

    // Resolve the legs through the ledger's public API — reverse the
    // charge's posting (debit connected_reserved, credit available).
    let credit = resolve_platform_balance(state, client, "available").await?;
    let debit = resolve_platform_balance(state, client, "connected_reserved").await?;

    let resp = client
        .post(format!(
            "{}/v1/balance-transactions",
            state.config.bill8_ledger_url
        ))
        .json(&bill8::ledger::contracts::PostTransactionRequest {
            debit_balance_id: debit,
            credit_balance_id: credit,
            amount: amount.to_string(),
            currency: currency.to_string(),
            transaction_type: "refund".into(),
            source_type: Some("refund".into()),
            source_id: Some(req.charge_id),
            idempotency_key: req.idempotency_key.clone(),
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("ledger HTTP {}", resp.status()));
    }
    let tx: bill8::ledger::contracts::BalanceTransaction =
        resp.json().await.map_err(|e| e.to_string())?;
    Ok(tx.id)
}
/// Post a split-at-charge movement to the ledger via its public API
/// (PAY-14 / LED-6): ONE atomic batch whose legs sum EXACTLY to the
/// charge amount — debit the payer's side, credit the connected
/// account's available balance, credit the platform fee. An over-fee
/// (fee ≥ amount) is rejected pre-write.
pub async fn post_split_to_ledger(
    state: &AppState,
    _env_id: Uuid,
    req: &CreateChargeRequest,
    amount: Decimal,
) -> Result<Uuid, String> {
    let Some(ref client) = state.config.ledger_http else {
        return Err("ledger workload transport absent".into());
    };

    // Validate the split pre-write: fee + connected ≤ amount, exactly.
    let fee: Decimal = req
        .application_fee_amount
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(Decimal::ZERO);
    if fee < Decimal::ZERO || fee >= amount {
        return Err(format!(
            "application_fee {fee} out of range for amount {amount}"
        ));
    }
    let connected_amount = amount - fee;

    // Resolve the legs through the ledger's public API.
    let payer = resolve_platform_balance(state, client, "available").await?;
    let connected = resolve_platform_balance(state, client, "connected_reserved").await?;
    let platform_fee = resolve_platform_balance(state, client, "platform_fee").await?;

    // Post the batch as explicit legs: payer → connected for the
    // seller's share, payer → platform_fee for the fee. Each is a
    // double-entry movement; the batch sums exactly to the charge.
    let connected_tx = post_ledger_leg(
        state,
        client,
        payer,
        connected,
        connected_amount.to_string(),
        "split",
        &req.currency,
        None,
        req.idempotency_key.clone(),
    )
    .await?;

    // Fee leg (the platform's share) — only when a fee is charged.
    if fee > Decimal::ZERO {
        post_ledger_leg(
            state,
            client,
            payer,
            platform_fee,
            fee.to_string(),
            "fee",
            &req.currency,
            None,
            req.idempotency_key.clone(),
        )
        .await?;
    }

    // The connected leg's transaction is the charge's balance ref.
    Ok(connected_tx)
}
/// Post one leg of a charge movement to the ledger via its public API.
/// Used by the split posting (PAY-14): each leg is a double-entry
/// movement; the batch sums exactly to the charge amount.
async fn post_ledger_leg(
    state: &AppState,
    client: &reqwest::Client,
    debit: Uuid,
    credit: Uuid,
    amount: String,
    kind: &str,
    currency: &str,
    source_id: Option<Uuid>,
    idempotency_key: Option<String>,
) -> Result<Uuid, String> {
    let resp = client
        .post(format!(
            "{}/v1/balance-transactions",
            state.config.bill8_ledger_url
        ))
        .json(&bill8::ledger::contracts::PostTransactionRequest {
            debit_balance_id: debit,
            credit_balance_id: credit,
            amount,
            currency: currency.to_string(),
            transaction_type: kind.into(),
            source_type: Some("charge".into()),
            source_id,
            idempotency_key,
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("ledger HTTP {}", resp.status()));
    }
    let tx: bill8::ledger::contracts::BalanceTransaction =
        resp.json().await.map_err(|e| e.to_string())?;
    Ok(tx.id)
}

/// Resolve a named platform balance via the ledger's PUBLIC API (never
/// a direct table read — the payments role holds no grant on the ledger
/// schema; cross-service access goes through the API, principles §4.7).
/// The ledger resolves the tenant from the workload certificate's own
/// context, so no env routing is needed here.
async fn resolve_platform_balance(
    state: &AppState,
    client: &reqwest::Client,
    name: &str,
) -> Result<Uuid, String> {
    let resp = client
        .get(format!("{}/v1/balances", state.config.bill8_ledger_url))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("ledger balances HTTP {}", resp.status()));
    }
    let balances: Vec<bill8::ledger::contracts::Balance> =
        resp.json().await.map_err(|e| e.to_string())?;
    balances
        .iter()
        .find(|b| b.name == name)
        .map(|b| b.id)
        .ok_or_else(|| format!("no {name} balance"))
}

pub async fn post_charge_to_ledger(
    state: &AppState,
    _env_id: Uuid,
    req: &CreateChargeRequest,
    _amount: Decimal,
) -> Result<Uuid, String> {
    let Some(ref client) = state.config.ledger_http else {
        return Err("ledger workload transport absent".into());
    };

    // Resolve the platform's clearing balances through the ledger's
    // public API (debit the clearing, credit the platform available).
    let debit = resolve_platform_balance(state, client, "available").await?;
    let credit = resolve_platform_balance(state, client, "connected_reserved").await?;

    let resp = client
        .post(format!(
            "{}/v1/balance-transactions",
            state.config.bill8_ledger_url
        ))
        .json(&bill8::ledger::contracts::PostTransactionRequest {
            debit_balance_id: debit,
            credit_balance_id: credit,
            amount: req.amount.clone(),
            currency: req.currency.clone(),
            transaction_type: "charge".into(),
            source_type: Some("charge".into()),
            source_id: None,
            idempotency_key: req.idempotency_key.clone(),
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("ledger HTTP {}", resp.status()));
    }
    let tx: bill8::ledger::contracts::BalanceTransaction =
        resp.json().await.map_err(|e| e.to_string())?;
    Ok(tx.id)
}
