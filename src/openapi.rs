// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! OpenAPI spec for the Bill8:Payments API — utoipa-generated.

use utoipa::OpenApi;

use crate::handlers;

/// The payments service's OpenAPI document.
#[derive(OpenApi)]
#[openapi(paths(
    handlers::health,
    handlers::customers::create_customer,
    handlers::customers::list_customers,
    handlers::charges::create_charge,
    handlers::charges::list_charges,
    handlers::charges::get_charge,
    handlers::refunds::create_refund,
    handlers::refunds::list_refunds,
    handlers::disputes::create_dispute,
    handlers::disputes::list_disputes,
    handlers::disputes::submit_evidence,
    handlers::disputes::resolve_dispute,
    handlers::intents::create_payment_intent,
    handlers::intents::list_payment_intents,
    handlers::intents::confirm_payment_intent,
    handlers::intents::create_setup_intent,
    handlers::intents::list_setup_intents,
    handlers::intents::confirm_setup_intent,
    handlers::channels_admin::list_channels,
    handlers::channels_admin::set_channel_config,
    handlers::channels_admin::get_channel_config,
    handlers::channels_admin::create_routing_rule,
    handlers::channels_admin::method_catalog,
    handlers::channels_admin::sca_handback,
    crate::tokens::create_token,
    crate::tokens::consume_token,
))]
pub struct PaymentsOpenApi;
