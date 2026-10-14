// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Customer handlers — the customer registry (holder-scoped).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bill8::payments::contracts::{CreateCustomerRequest, Customer};
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::handlers::{emit_event, err_db, set_tenant_context, AppState};

/// `POST /v1/customers` — create a customer.
#[utoipa::path(
    post,
    path = "/v1/customers",
    request_body = CreateCustomerRequest,
    responses(
        (status = 200, description = "Customer created", body = Customer),
        (status = 403, description = "Missing bill8:payments:customers:write"),
    ),
    security(("bill8:payments:customers:write" = [])),
    tag = "payments",
)]
pub async fn create_customer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateCustomerRequest>,
) -> Result<Json<Customer>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:customers:write"),
    ).await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id()).await.map_err(err_db)?;

    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"INSERT INTO payments.customers (id, env_id, display_name, email, sentin8_auth_user_id)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING id, env_id, display_name, email, status, sentin8_auth_user_id, created_at"#,
    )
    .bind(id)
    .bind(a.env_id())
    .bind(&req.display_name)
    .bind(req.email.as_deref())
    .bind(a.claims.sentin8_auth_user_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:customer:created', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "customer_id": id, "display_name": req.display_name }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;

    let customer = row_to_customer(&row);
    emit_event(
        &state.pool,
        a.env_id(),
        "bill8:payments:customer:created",
        serde_json::json!({ "customer_id": id }),
    )
    .await;
    Ok(Json(customer))
}

/// `GET /v1/customers` — list customers.
#[utoipa::path(
    get,
    path = "/v1/customers",
    responses(
        (status = 200, description = "List of customers", body = [Customer]),
        (status = 403, description = "Missing bill8:payments:customers:read"),
    ),
    security(("bill8:payments:customers:read" = [])),
    tag = "payments",
)]
pub async fn list_customers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Customer>>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:customers:read"),
    ).await?;

    let mut conn = state.pool.acquire().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id()).await.map_err(err_db)?;

    let rows = sqlx::query(
        "SELECT id, env_id, display_name, email, status, sentin8_auth_user_id, created_at FROM payments.customers ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(err_db)?;

    Ok(Json(rows.iter().map(row_to_customer).collect()))
}

fn row_to_customer(row: &sqlx::postgres::PgRow) -> Customer {
    Customer {
        id: row.get("id"),
        env_id: row.get("env_id"),
        display_name: row.get("display_name"),
        email: row.get("email"),
        status: row.get("status"),
        sentin8_auth_user_id: row.get("sentin8_auth_user_id"),
        created_at: row.get("created_at"),
    }
}