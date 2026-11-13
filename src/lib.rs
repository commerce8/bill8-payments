// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! # Bill8:Payments - core payments service
//!
//! The system of record for payment acceptance: customers, payment
//! methods, intents, charges, refunds, and disputes - routed through
//! selectable settlement channels. Charges are append-only; refunds and
//! disputes are compensating records; money is only moved through the
//! ledger (a channel never mutates balances).
//!
//! See `DESIGN.md` for the full detailed design (§2 charge sequence,
//! §2b channel architecture, §2c SCA handback, §3 data model).

#![forbid(unsafe_code)]

pub mod auth;
pub mod channels;
pub mod config;
pub mod consumers;
pub mod handlers;
pub mod openapi;

// The utoipa dump for registration::print - mirrored from main.rs; the
// lib crate compiles registration::print too (test harnesses embedding
// via #[path] never invoke it, so the pointer is referenced, not run).
#[cfg_attr(test, allow(dead_code))]
static OPENAPI_DUMP: fn() -> serde_json::Value = || {
    use utoipa::OpenApi as _;
    serde_json::to_value(openapi::PaymentsOpenApi::openapi()).unwrap_or(serde_json::Value::Null)
};

pub mod policy;
pub mod registration;
pub mod server;
pub mod tokens;
pub mod types;
