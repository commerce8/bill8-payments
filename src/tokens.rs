// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Bill8:Payments
// SPDX-License-Identifier: Apache-2.0

//! Tokenization surface (PAY-10) - single-use instrument references.
//!
//! `POST /v1/tokens` is callable through the BFF with a publishable
//! credential restricted to the tokenization scope; a publishable token
//! can do NOTHING else. Tokens are single-use and short-TTL: a consumed
//! or expired token is rejected on reuse (guarded).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{Duration, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::auth;
use crate::handlers::{emit_event, err_db, set_tenant_context, AppState};

/// Request body for `POST /v1/tokens`.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct CreateTokenRequest {
    /// The payment method to tokenize (must already exist; the token is
    /// a single-use reference to it - card data itself never transits).
    pub payment_method_id: Uuid,
}

/// Response of `POST /v1/tokens`.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct TokenResponse {
    pub id: Uuid,
    /// Single-use token value (short-TTL; consumed on first use).
    pub token_value: String,
    pub expires_at: chrono::DateTime<Utc>,
}

/// `POST /v1/tokens` - mint a single-use token for a payment method.
///
/// Authorization: either the full `bill8:payments:tokens:write`
/// scope, OR a publishable credential (the Shield scope-mirror permits
/// the tokenization action for the publishable scope) - a publishable
/// token can do nothing else (every other payments action requires its
/// own scope).
#[utoipa::path(
    post,
    path = "/v1/tokens",
    request_body = CreateTokenRequest,
    responses(
        (status = 200, description = "Token minted", body = TokenResponse),
        (status = 403, description = "Missing tokenization scope"),
        (status = 404, description = "Payment method not found"),
    ),
    security(("bill8:payments:tokens:write" = [])),
    tag = "payments",
)]
pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Json<TokenResponse>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:tokens:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // The referenced method must exist in this env.
    let exists = sqlx::query("SELECT id FROM payments.payment_methods WHERE id = $1")
        .bind(req.payment_method_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err_db)?
        .is_some();
    if !exists {
        return Err((StatusCode::NOT_FOUND, "payment method not found".into()));
    }

    let id = Uuid::new_v4();
    let token_value = format!("tok_{}", Uuid::new_v4().simple());
    let expires_at = Utc::now() + Duration::minutes(10);

    sqlx::query(
        r#"INSERT INTO payments.tokens (id, env_id, token_value, payment_method_id, expires_at)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(a.env_id())
    .bind(&token_value)
    .bind(req.payment_method_id)
    .bind(expires_at)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    sqlx::query(
        r#"INSERT INTO payments.outbox (env_id, event_type, payload)
           VALUES ($1, 'bill8:payments:token:created', $2)"#,
    )
    .bind(a.env_id())
    .bind(serde_json::json!({ "token_id": id }))
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;

    conn.commit().await.map_err(err_db)?;

    emit_event(
        &state.pool,
        a.env_id(),
        "bill8:payments:token:created",
        serde_json::json!({ "token_id": id }),
    )
    .await;
    Ok(Json(TokenResponse {
        id,
        token_value,
        expires_at,
    }))
}

/// `POST /v1/tokens/{token_value}/consume` - consume a token
/// (single-use guard: the first consume wins; every later consume of the
/// same token is rejected).
#[utoipa::path(
    post,
    path = "/v1/tokens/{token_value}/consume",
    responses(
        (status = 200, description = "Token consumed - the payment method id"),
        (status = 409, description = "Token already used or expired"),
        (status = 404, description = "Token not found"),
    ),
    security(("bill8:payments:tokens:write" = [])),
    tag = "payments",
)]
pub async fn consume_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token_value): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("bill8:payments:tokens:write"),
    )
    .await?;

    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, a.env_id())
        .await
        .map_err(err_db)?;

    // Guarded single-use: the UPDATE only matches an unused, unexpired
    // token - a second consume finds no row (409), and an expired token
    // likewise never matches.
    let row = sqlx::query(
        r#"UPDATE payments.tokens
           SET used = true
           WHERE token_value = $1 AND used = false AND expires_at > now()
           RETURNING payment_method_id"#,
    )
    .bind(&token_value)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?;

    let method_id = match row {
        Some(r) => r.get::<Uuid, _>("payment_method_id"),
        None => {
            // Distinguish not-found from used/expired for the caller.
            let exists = sqlx::query("SELECT 1 as one FROM payments.tokens WHERE token_value = $1")
                .bind(&token_value)
                .fetch_optional(&mut *conn)
                .await
                .map_err(err_db)?
                .is_some();
            return Err(if exists {
                (StatusCode::CONFLICT, "token already used or expired".into())
            } else {
                (StatusCode::NOT_FOUND, "token not found".into())
            });
        }
    };

    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "payment_method_id": method_id })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_value_shape() {
        // Token values are opaque single-use references.
        let v = format!("tok_{}", Uuid::new_v4().simple());
        assert!(v.starts_with("tok_"));
        assert!(v.len() > "tok_".len() + 8);
    }
}
