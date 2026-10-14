// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Shield policy integration for Bill8:Payments — thin re-export of
//! the shared bridge. Step 5 is the ONLY gate.

#[allow(unused_imports)]
pub use sentin8::shield::runtime::{
    check_policy, engine_for, policy_bus_client, request_ctx, spawn_hot_reload,
};
pub use sentin8::shield::PolicyEngine as Engine;

/// Step 5 for payments: verify (steps 1–4) already ran; evaluate + 403 on
/// enforced deny.
pub use sentin8::shield::runtime::require_policy as require;
