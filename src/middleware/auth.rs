use actix_web::Result;
use jsonwebtoken::{decode, DecodingKey, Validation};
use std::env;

use crate::models::auth::Claims;

/// Validation config matching what auth-service actually issues (iss/aud claims).
/// jsonwebtoken defaults `validate_aud = true`; if a token carries an `aud` claim
/// but the validator's own `aud` is left unset, decoding fails with
/// `InvalidAudience` — so this must be set explicitly even though we don't need
/// strict audience *enforcement* beyond "it came from our auth-service".
fn build_validation() -> Validation {
    let mut validation = Validation::default();
    validation.set_issuer(&["isupercoder-auth"]);
    validation.set_audience(&["isupercoder-api"]);
    validation
}

/// Outcome of remote validation. `Rejected` (an explicit auth-service
/// verdict, including malformed valid-without-claims responses) must never
/// fall back to local validation; `Unavailable` (transport failure or a
/// non-JSON response) may.
enum RemoteValidation {
    Validated(Claims),
    Rejected,
    Unavailable,
}

#[derive(serde::Deserialize)]
struct ValidateClaims {
    #[serde(rename = "userId")]
    user_id: String,
    email: String,
    name: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    exp: Option<usize>,
}

#[derive(serde::Deserialize)]
struct ValidateResponse {
    success: bool,
    valid: bool,
    claims: Option<ValidateClaims>,
}

async fn verify_with_auth_service(auth_service_url: &str, token: &str) -> RemoteValidation {
    let client = reqwest::Client::new();
    let response = match client
        .post(format!("{}/api/auth/validate", auth_service_url))
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return RemoteValidation::Unavailable,
    };

    let result: ValidateResponse = match response.json().await {
        Ok(v) => v,
        Err(_) => return RemoteValidation::Unavailable,
    };

    if result.success && result.valid {
        match result.claims {
            Some(c) => {
                let role = c.role.unwrap_or_else(|| "customer".to_string());
                RemoteValidation::Validated(Claims {
                    user_id: c.user_id,
                    email: c.email,
                    name: c.name,
                    role_type: role.clone(),
                    role,
                    exp: c.exp.unwrap_or(0),
                })
            }
            // A well-formed "valid" response without claims is malformed:
            // treat as an explicit rejection, not an excuse to fall back.
            None => RemoteValidation::Rejected,
        }
    } else {
        RemoteValidation::Rejected
    }
}

/// Local HS256 validation, used only when the auth-service is unavailable.
fn verify_local(jwt_secret: &str, token: &str) -> Result<Claims, actix_web::Error> {
    let validation = build_validation();
    match decode::<Claims>(
        token,
        &DecodingKey::from_secret(jwt_secret.as_ref()),
        &validation,
    ) {
        Ok(token_data) => Ok(token_data.claims),
        Err(_) => Err(actix_web::error::ErrorUnauthorized(
            "Invalid or expired token",
        )),
    }
}

/// Validate a bearer token: auth-service first (it holds the blacklist);
/// an explicit rejection is final, and only transport-level unavailability
/// falls back to local validation (availability trade-off, consistent with
/// projects-api and file-management).
pub async fn validate_bearer_with(
    auth_service_url: &str,
    token: &str,
    jwt_secret: &str,
) -> Result<Claims, actix_web::Error> {
    match verify_with_auth_service(auth_service_url, token).await {
        RemoteValidation::Validated(claims) => Ok(claims),
        RemoteValidation::Rejected => Err(actix_web::error::ErrorUnauthorized(
            "Token has been revoked",
        )),
        RemoteValidation::Unavailable => verify_local(jwt_secret, token),
    }
}

// Simple JWT extractor for handlers
pub async fn extract_claims_from_request(
    req: &actix_web::HttpRequest,
) -> Result<Claims, actix_web::Error> {
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| actix_web::error::ErrorUnauthorized("Authorization header missing"))?;

    if !auth_header.starts_with("Bearer ") {
        return Err(actix_web::error::ErrorUnauthorized(
            "Invalid authorization format",
        ));
    }

    let token = &auth_header[7..];
    let jwt_secret = env::var("JWT_SECRET").expect(
        "JWT_SECRET environment variable must be set — refusing to start with insecure default",
    );
    let auth_service_url =
        env::var("AUTH_SERVICE_URL").unwrap_or_else(|_| "http://auth-service:8080".to_string());

    validate_bearer_with(&auth_service_url, token, &jwt_secret).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    /// Regression test: auth-service's real Claims struct also carries `iss`/`aud`
    /// (unlike this service's own `Claims`, which doesn't declare those fields).
    /// Extra JSON fields are fine for serde, but `Validation` must be configured
    /// to accept the `aud` claim or decoding rejects the token outright.
    #[test]
    fn accepts_token_shaped_like_real_auth_service_output() {
        use serde::Serialize;

        #[derive(Serialize)]
        struct AuthServiceClaims {
            #[serde(rename = "userId")]
            user_id: String,
            email: String,
            name: String,
            #[serde(rename = "type")]
            role_type: String,
            role: String,
            iss: String,
            aud: String,
            exp: usize,
        }

        let secret = "test-secret";
        let issued = AuthServiceClaims {
            user_id: "user-42".to_string(),
            email: "user@example.com".to_string(),
            name: "Test User".to_string(),
            role_type: "customer".to_string(),
            role: "customer".to_string(),
            iss: "isupercoder-auth".to_string(),
            aud: "isupercoder-api".to_string(),
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize,
        };

        let token = encode(
            &Header::default(),
            &issued,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let data = decode::<Claims>(
            &token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &build_validation(),
        )
        .expect("must accept a token shaped like auth-service's real output");
        assert_eq!(data.claims.user_id, "user-42");
    }

    #[test]
    fn rejects_mismatched_audience() {
        use serde::Serialize;

        #[derive(Serialize)]
        struct ClaimsWithAud {
            #[serde(rename = "userId")]
            user_id: String,
            aud: String,
            exp: usize,
        }

        let secret = "test-secret";
        let issued = ClaimsWithAud {
            user_id: "user-42".to_string(),
            aud: "some-other-api".to_string(),
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize,
        };
        let token = encode(
            &Header::default(),
            &issued,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        // build_validation() expects aud = "isupercoder-api"
        let result = decode::<Claims>(
            &token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &build_validation(),
        );
        assert!(result.is_err());
    }
}
