// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Payments configuration loaded from environment variables.

use std::env;

use sentin8::auth::client::{Client as AuthClient, ClientOptions};

/// Payments configuration.
#[derive(Clone)]
pub struct PaymentsConfig {
    /// Service port (assigned by Sentin8:Captain).
    pub port: u16,
    /// PostgreSQL DSN (role `bill8_payments_service`, db `bill8`).
    pub database_url: String,
    /// Auth issuer base URL.
    pub sentin8_auth_issuer: String,
    /// Auth JWKS URL.
    pub sentin8_auth_jwks_url: String,
    /// Bus base URL (outbox relay target).
    pub sentin8_bus_url: String,
    /// Bill8:Ledger base URL - the ONLY money-movement path (charges,
    /// refunds, dispute reversals post through its public API via the
    /// `bill8-payments-ledger` workload certificate).
    pub bill8_ledger_url: String,
    /// Bill8:Risk base URL - inline pre-authorization screening via
    /// the `bill8-payments-risk` workload certificate.
    pub bill8_risk_url: String,
    /// Stream family this service produces to.
    pub stream_family: String,
    /// Shared Auth client (discovery + JWKS caches reused).
    pub sentin8_auth_client: AuthClient,
    /// Certificate-presenting transport for the service's own workload
    /// identity (CERTFREE-4V - no M2M token exists).
    pub m2m_http: Option<reqwest::Client>,
    /// Certificate-presenting transport for the cross-service ledger
    /// workload (posts balance transactions).
    pub ledger_http: Option<reqwest::Client>,
    /// Certificate-presenting transport for the cross-service risk
    /// workload (inline screening).
    pub risk_http: Option<reqwest::Client>,
}

impl std::fmt::Debug for PaymentsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaymentsConfig")
            .field("port", &self.port)
            .field("database_url", &"<redacted>")
            .field("bill8_ledger_url", &self.bill8_ledger_url)
            .field("bill8_risk_url", &self.bill8_risk_url)
            .field("m2m_http", &self.m2m_http.is_some())
            .field("ledger_http", &self.ledger_http.is_some())
            .field("risk_http", &self.risk_http.is_some())
            .finish_non_exhaustive()
    }
}

impl PaymentsConfig {
    /// Load configuration from environment variables.
    ///
    /// # Panics
    /// Panics when `BILL8_PAYMENTS_PORT` is unset (the captain always
    /// sets it) or not a valid `u16`.
    pub fn from_env() -> Self {
        let cfg = sentin8::config::Sentin8Config::from_env();
        let issuer = env::var("SENTIN8_AUTH_ISSUER").unwrap_or_else(|_| cfg.sentin8_auth_issuer());
        Self {
            port: env::var("BILL8_PAYMENTS_PORT")
                .expect("BILL8_PAYMENTS_PORT must be set by captain")
                .parse()
                .expect("BILL8_PAYMENTS_PORT must be a valid u16"),
            database_url: env::var("BILL8_PAYMENTS_DATABASE_URL")
                .or_else(|_| env::var("DATABASE_URL"))
                .unwrap_or_else(|_| {
                    sentin8::config::Sentin8Config::default_database_url().to_string()
                }),
            sentin8_auth_issuer: issuer.clone(),
            sentin8_auth_jwks_url: env::var("SENTIN8_AUTH_JWKS_URL")
                .unwrap_or_else(|_| cfg.sentin8_auth_jwks_url()),
            sentin8_bus_url: env::var("SENTIN8_BUS_URL").unwrap_or_else(|_| cfg.sentin8_bus_url()),
            bill8_ledger_url: env::var("BILL8_LEDGER_URL")
                .unwrap_or_else(|_| "http://localhost:3205".into()),
            bill8_risk_url: env::var("BILL8_RISK_URL")
                .unwrap_or_else(|_| "http://localhost:3207".into()),
            stream_family: "payments".into(),
            sentin8_auth_client: AuthClient::new(ClientOptions {
                base_url: issuer.clone(),
                ..Default::default()
            }),
            m2m_http: None,
            ledger_http: None,
            risk_http: None,
        }
    }
}
