// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Payments domain types - internal shapes and the domain error surface.

use serde::Deserialize;

/// Domain errors - every variant maps to an HTTP status and a
/// `no rows written` guarantee on the money path.
#[derive(Debug, thiserror::Error)]
pub enum PaymentsError {
    /// The charge is not in a state that allows the transition.
    #[error("illegal state transition: {from} -> {to} on charge {id}")]
    IllegalTransition {
        id: uuid::Uuid,
        from: String,
        to: String,
    },
    /// The refund exceeds the remaining refundable amount.
    #[error("refund {requested} exceeds remaining refundable {remaining}")]
    OverRefund {
        requested: String,
        remaining: String,
    },
    /// The referenced charge/customer/method was not found.
    #[error("charge {0} not found")]
    ChargeNotFound(uuid::Uuid),
    /// The screening blocked the charge (no money moved).
    #[error("screening blocked the charge")]
    Blocked,
    /// The charge is on review hold.
    #[error("screening routed the charge to review")]
    Review,
    /// The ledger posting failed (everything rolled back atomically).
    #[error("ledger posting failed: {0}")]
    Ledger(String),
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Request body for `POST /v1/charges/{id}/capture`.
#[derive(Debug, Deserialize)]
pub struct CaptureRequest {
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// Query params for charge reads.
#[derive(Debug, Default, Deserialize)]
pub struct ListChargesQuery {
    pub customer_id: Option<uuid::Uuid>,
}
