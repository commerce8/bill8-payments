// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Service self-registration data for Bill8:Payments.

use sentin8::registration::{
    AdminScope, AdminUi, AdminUiPage, ErrorCode, M2mGrant, PgRole, PolicySetDecl, Registration,
    SqlMigration,
};
use utoipa::OpenApi;

const PG_ROLES: &[PgRole] = &[PgRole {
    scope: "bill8:payments:read,bill8:payments:write",
    role: "bill8_payments_service",
    bypass_rls: false,
    database: "bill8",
    cross_schema_grants: &[],
}];

/// Workload scope grants — one entry per workload. The cross-service
/// workloads (`bill8-payments-ledger`, `bill8-payments-risk`)
/// hold ONLY the public-API scopes they call; a vertical service never
/// receives grants on another vertical's tables.
const M2M_GRANTS: &[M2mGrant] = &[
    M2mGrant {
        workload: "bill8:payments",
        scopes: &[
            "sentin8:bus:produce",
            "sentin8:bus:consume",
            "bill8:payments:admin",
            "sentin8:stat:measure",
        ],
    },
    M2mGrant {
        workload: "payments",
        scopes: &[
            "sentin8:bus:produce",
            "sentin8:bus:consume",
            "bill8:payments:admin",
            "sentin8:stat:measure",
        ],
    },
    M2mGrant {
        workload: "payments-audit",
        scopes: &["sentin8:sys:audit:write"],
    },
    M2mGrant {
        workload: "bill8:payments-policy",
        scopes: &["sentin8:bus:consume", "sentin8:stat:measure"],
    },
    M2mGrant {
        workload: "payments-policy",
        scopes: &["sentin8:stat:measure"],
    },
    M2mGrant {
        workload: "bill8-payments-tls",
        scopes: &["sentin8:sys:instances:read"],
    },
    // Cross-service: posts balance transactions through the ledger's
    // PUBLIC API (charges/refunds/dispute reversals — the only money path).
    M2mGrant {
        workload: "bill8-payments-ledger",
        scopes: &["bill8:ledger:balance-transactions:write"],
    },
    // Cross-service: inline pre-authorization screening on the risk
    // PUBLIC API (M2M-only write surface).
    M2mGrant {
        workload: "bill8-payments-risk",
        scopes: &["bill8:risk:screenings:write"],
    },
];

const ADMIN_SCOPES: &[AdminScope] = &[
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:customers:read",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:customers:write",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:charges:read",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:charges:write",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:refunds:read",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:refunds:write",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:disputes:read",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:disputes:write",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:channels:read",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:routing:write",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:policy:read",
    },
    AdminScope::Typed {
        role: "bill8:payments:admin",
        scope: "bill8:payments:policy:write",
    },
];

const ERROR_CODES: &[ErrorCode] = &[
    ErrorCode {
        enum_name: "PaymentsChargeNotFound",
        error_code: "payments_charge_not_found",
        status_code: 404,
    },
    ErrorCode {
        enum_name: "PaymentsIllegalTransition",
        error_code: "payments_illegal_transition",
        status_code: 422,
    },
    ErrorCode {
        enum_name: "PaymentsOverRefund",
        error_code: "payments_over_refund",
        status_code: 422,
    },
    ErrorCode {
        enum_name: "PaymentsBlocked",
        error_code: "payments_blocked",
        status_code: 402,
    },
    ErrorCode {
        enum_name: "PaymentsNoChannel",
        error_code: "payments_no_channel",
        status_code: 402,
    },
];

const ADMIN_UI_PAGES: &[AdminUiPage] = &[
    AdminUiPage {
        route: "overview",
        label: "Overview",
        ui_endpoint: "/v1/admin/ui/overview",
        data_endpoint: "/v1/admin/overview",
        icon: "0xe1b1",
    },
    AdminUiPage {
        route: "charges",
        label: "Charges",
        ui_endpoint: "/v1/admin/ui/charges",
        data_endpoint: "/v1/admin/charges",
        icon: "0xe53e",
    },
    AdminUiPage {
        route: "refunds",
        label: "Refunds",
        ui_endpoint: "/v1/admin/ui/refunds",
        data_endpoint: "/v1/admin/refunds",
        icon: "0xe5cd",
    },
    AdminUiPage {
        route: "disputes",
        label: "Disputes",
        ui_endpoint: "/v1/admin/ui/disputes",
        data_endpoint: "/v1/admin/disputes",
        icon: "0xe8ad",
    },
    AdminUiPage {
        route: "channels",
        label: "Channels",
        ui_endpoint: "/v1/admin/ui/channels",
        data_endpoint: "/v1/admin/channels",
        icon: "0xe0d0",
    },
    AdminUiPage {
        route: "customers",
        label: "Customers",
        ui_endpoint: "/v1/admin/ui/customers",
        data_endpoint: "/v1/admin/customers",
        icon: "0xe7fd",
    },
];

/// The scope-mirror Shield text — action-granular permits over the LIVE
/// effective scope set (the exact step-5 gates this service evaluates).
pub const SCOPE_MIRROR_POLICIES: &str = "permit(principal, action == Action::\"bill8:payments:customers:read\", resource) when { principal.scopes.contains(\"bill8:payments:customers:read\") };\npermit(principal, action == Action::\"bill8:payments:customers:read\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };\npermit(principal, action == Action::\"bill8:payments:customers:read\", resource) when { principal.scopes.contains(\"sentin8:sys:org:admin\") };\npermit(principal, action == Action::\"bill8:payments:customers:read\", resource) when { principal.scopes.contains(\"sentin8:sys:env:admin\") };\npermit(principal, action == Action::\"bill8:payments:customers:write\", resource) when { principal.scopes.contains(\"bill8:payments:customers:write\") };\npermit(principal, action == Action::\"bill8:payments:customers:write\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };\npermit(principal, action == Action::\"bill8:payments:charges:read\", resource) when { principal.scopes.contains(\"bill8:payments:charges:read\") };\npermit(principal, action == Action::\"bill8:payments:charges:read\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };\npermit(principal, action == Action::\"bill8:payments:charges:write\", resource) when { principal.scopes.contains(\"bill8:payments:charges:write\") };\npermit(principal, action == Action::\"bill8:payments:charges:write\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };\npermit(principal, action == Action::\"bill8:payments:charges:write\", resource) when { principal.scopes.contains(\"sentin8:sys:org:admin\") };\npermit(principal, action == Action::\"bill8:payments:charges:write\", resource) when { principal.scopes.contains(\"sentin8:sys:env:admin\") };\npermit(principal, action == Action::\"bill8:payments:refunds:read\", resource) when { principal.scopes.contains(\"bill8:payments:refunds:read\") };\npermit(principal, action == Action::\"bill8:payments:refunds:write\", resource) when { principal.scopes.contains(\"bill8:payments:refunds:write\") };\npermit(principal, action == Action::\"bill8:payments:refunds:write\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };\npermit(principal, action == Action::\"bill8:payments:disputes:read\", resource) when { principal.scopes.contains(\"bill8:payments:disputes:read\") };\npermit(principal, action == Action::\"bill8:payments:disputes:write\", resource) when { principal.scopes.contains(\"bill8:payments:disputes:write\") };\npermit(principal, action == Action::\"bill8:payments:channels:read\", resource) when { principal.scopes.contains(\"bill8:payments:channels:read\") };\npermit(principal, action == Action::\"bill8:payments:routing:write\", resource) when { principal.scopes.contains(\"bill8:payments:routing:write\") };\npermit(principal, action == Action::\"bill8:payments:routing:write\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };\n";

const POLICIES: &[PolicySetDecl] = &[PolicySetDecl {
    policy_set_name: "scope-mirror",
    description: "Action-granular scope permits for bill8:payments over the LIVE effective scope set. Step 5 is the only gate.",
    policies: SCOPE_MIRROR_POLICIES,
    schema: None,
    mode: "enforced",
    default_enabled: true,
}];

// Event triggers are declarative in `vertical-registry.json` (the
// review-resolution consumer group), mirroring the platform's registry
// pattern — `Registration.event_triggers` stays empty here.
const EVENT_TRIGGERS: &[sentin8::registration::EventTriggerDecl] = &[];

const SQL_1_INIT: &str = include_str!("../sql/init.sql");

fn openapi_spec() -> serde_json::Value {
    serde_json::to_value(crate::openapi::PaymentsOpenApi::openapi())
        .unwrap_or(serde_json::Value::Null)
}

/// Print the registration JSON to stdout.
pub fn print(section: Option<String>) -> anyhow::Result<()> {
    let reg = Registration {
        service_type: "bill8:payments",
        subdomain: "bill8-payments",
        health_path: "/v1/health",
        admin_scopes: ADMIN_SCOPES,
        error_codes: ERROR_CODES,
        consent_providers: &[],
        consent_flows: &[],
        policies: POLICIES,
        relations: &[],
        event_triggers: EVENT_TRIGGERS,
        schedules: &[],
        admin_ui: Some(AdminUi {
            label: "Payments",
            icon: "0xe53e",
            service_flag: "payments",
            pages: ADMIN_UI_PAGES,
        }),
        sql_migrations: vec![SqlMigration {
            version: 1,
            name: "init",
            sql: SQL_1_INIT,
        }],
        pg_roles: PG_ROLES,
        audit_findings: None,
        service_actions: None,
        m2m_grants: Some(M2M_GRANTS),
        openapi_url: "/openapi.json",
        openapi: openapi_spec,
    };
    sentin8::registration::print_registration(section, &reg, openapi_spec)
}
