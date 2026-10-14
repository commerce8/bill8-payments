// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! # Bill8:Payments — core payments service
//!
//! The system of record for payment acceptance: customers, payment
//! methods, intents, charges, refunds, and disputes — routed through
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
pub mod policy;
pub mod registration;
pub mod server;
pub mod tokens;
pub mod types;
