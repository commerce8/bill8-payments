// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Auth token verification for Bill8:Payments (steps 1–4). Step 5 —
//! the Shield gate — is the authorization decision.

use axum::http::{HeaderMap, StatusCode};

use crate::config::PaymentsConfig;

pub use sentin8::auth::client::AuthContext;

/// Verify the Bearer token and return the auth context.
///
/// # Errors
/// Returns `401` with a reason when the bearer token is missing, expired,
/// or fails signature/issuer verification.
pub async fn verify_token(
    config: &PaymentsConfig,
    headers: &HeaderMap,
) -> Result<AuthContext, (StatusCode, String)> {
    sentin8::auth::client::verify_bearer_token(&config.sentin8_auth_client, headers).await
}