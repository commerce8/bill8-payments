// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Axum server setup for Bill8:Payments - state binding + routes.

use axum::routing::get;
use std::sync::Arc;

use crate::handlers;

/// Build a cert-presenting HTTP transport for `workload` (CERTFREE-4V,
/// auth DESIGN §4.3f): mint-or-load the Acme-issued pair and wrap it
/// in a reqwest client. The receiving service's TLS layer authenticates
/// the handshake and the enroll-published registry map authorizes the
/// scopes - no bearer token exists on this path.
///
/// # Errors
/// Returns the certificate-ensure or transport-build failure (the
/// caller logs and degrades).
async fn build_workload_http(auth_issuer: &str, workload: &str) -> anyhow::Result<reqwest::Client> {
    let cert = sentin8::auth::m2m::ensure_workload_certificate(
        auth_issuer,
        workload,
        &sentin8::auth::m2m::platform_ca_path(),
    )
    .await?;
    let mtls = sentin8::middleware::mtls::MtlsConfig::new(
        &cert.ca_cert_path,
        &cert.cert_path,
        &cert.key_path,
    );
    sentin8::middleware::mtls::build_mtls_client(&mtls)
        .map_err(|e| anyhow::anyhow!("workload '{workload}' transport: {e}"))
}

/// Bind the service state: config, pool, shell-booted Shield engine,
/// Bus client, and the cross-service workload transports (ledger, risk).
pub async fn build_state(core: sentin8::shell::ServiceCore) -> anyhow::Result<handlers::AppState> {
    let mut config = crate::config::PaymentsConfig::from_env();

    // Cert-presenting Bus client (CERTFREE-4V): the `payments` workload
    // CERTIFICATE is the identity - no DCR, no token (auth DESIGN
    // §4.3f); the mint retries while the fleet boots.
    let bus_client = loop {
        match sentin8::bus::client::BusClient::for_workload(
            &config.sentin8_bus_url,
            &config.sentin8_auth_issuer,
            "payments",
        )
        .await
        {
            Ok(c) => break c,
            Err(e) => {
                tracing::debug!(error = %e, "payments bus client: certificate not ready yet");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    // The service's own workload transport (CERTFREE-4V): the
    // `payments` certificate presents in the TLS handshake - the
    // receiving service's TLS layer authenticates it and the registry
    // map authorizes the scopes.
    match build_workload_http(&config.sentin8_auth_issuer, "payments").await {
        Ok(http) => config.m2m_http = Some(http),
        Err(e) => tracing::warn!(error = %e, "payments workload transport unavailable - degraded"),
    }

    // Cross-service ledger workload transport (the ONLY money-movement
    // path).
    match build_workload_http(&config.sentin8_auth_issuer, "bill8-payments-ledger").await {
        Ok(http) => config.ledger_http = Some(http),
        Err(e) => {
            tracing::warn!(error = %e, "payments-ledger transport unavailable - charges cannot move money")
        }
    }

    // Cross-service risk workload transport (inline screening).
    match build_workload_http(&config.sentin8_auth_issuer, "bill8-payments-risk").await {
        Ok(http) => config.risk_http = Some(http),
        Err(e) => {
            tracing::warn!(error = %e, "payments-risk transport unavailable - screening fails closed")
        }
    }

    let policy_engine = core.admin.policy.clone();

    let state = handlers::AppState {
        config: Arc::new(config),
        pool: core.pool.clone(),
        bus_client,
        policy: policy_engine,
    };

    // Spawn the Bus consumers (env-init + review resolution) - the
    // event-driven wiring (consumers.rs; billing DESIGN.md §7 pattern).
    crate::consumers::spawn_consumers(state.clone());

    Ok(state)
}
pub fn routes(router: axum::Router<handlers::AppState>) -> axum::Router<handlers::AppState> {
    router
        .route("/v1/health", get(handlers::health))
        .route(
            "/v1/customers",
            get(handlers::customers::list_customers).post(handlers::customers::create_customer),
        )
        .route(
            "/v1/charges",
            get(handlers::charges::list_charges).post(handlers::charges::create_charge),
        )
        .route("/v1/charges/{id}", get(handlers::charges::get_charge))
        .route(
            "/v1/refunds",
            get(handlers::refunds::list_refunds).post(handlers::refunds::create_refund),
        )
        .route(
            "/v1/disputes",
            get(handlers::disputes::list_disputes).post(handlers::disputes::create_dispute),
        )
        .route(
            "/v1/disputes/{id}/evidence",
            axum::routing::post(handlers::disputes::submit_evidence),
        )
        .route(
            "/v1/disputes/{id}/resolve",
            axum::routing::post(handlers::disputes::resolve_dispute),
        )
        .route("/v1/channels", get(handlers::channels_admin::list_channels))
        .route(
            "/v1/channel-config",
            get(handlers::channels_admin::get_channel_config)
                .put(handlers::channels_admin::set_channel_config),
        )
        .route(
            "/v1/routing-rules",
            axum::routing::post(handlers::channels_admin::create_routing_rule),
        )
        // Tokenization surface (PAY-10) - publishable-credential callable.
        .route(
            "/v1/tokens",
            axum::routing::post(crate::tokens::create_token),
        )
        .route(
            "/v1/tokens/{token_value}/consume",
            axum::routing::post(crate::tokens::consume_token),
        )
        // SCA challenge handback (PAY-12) + method catalog (PAY-13).
        .route(
            "/v1/intents/{id}/handback",
            axum::routing::post(handlers::channels_admin::sca_handback),
        )
        .route("/v1/methods", get(handlers::channels_admin::method_catalog))
        // Intents (PAY-5): two-phase confirmation surface.
        .route(
            "/v1/payment-intents",
            get(handlers::intents::list_payment_intents)
                .post(handlers::intents::create_payment_intent),
        )
        .route(
            "/v1/payment-intents/{id}/confirm",
            axum::routing::post(handlers::intents::confirm_payment_intent),
        )
        .route(
            "/v1/setup-intents",
            get(handlers::intents::list_setup_intents).post(handlers::intents::create_setup_intent),
        )
        .route(
            "/v1/setup-intents/{id}/confirm",
            axum::routing::post(handlers::intents::confirm_setup_intent),
        )
}
