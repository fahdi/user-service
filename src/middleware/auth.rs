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

// Simple JWT extractor for handlers
pub fn extract_claims_from_request(req: &actix_web::HttpRequest) -> Result<Claims, actix_web::Error> {
    let auth_header = req.headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| actix_web::error::ErrorUnauthorized("Authorization header missing"))?;

    if !auth_header.starts_with("Bearer ") {
        return Err(actix_web::error::ErrorUnauthorized("Invalid authorization format"));
    }

    let token = &auth_header[7..];
    let jwt_secret = env::var("JWT_SECRET")
        .expect("JWT_SECRET environment variable must be set — refusing to start with insecure default");

    let validation = build_validation();

    match decode::<Claims>(token, &DecodingKey::from_secret(jwt_secret.as_ref()), &validation) {
        Ok(token_data) => Ok(token_data.claims),
        Err(_) => Err(actix_web::error::ErrorUnauthorized("Invalid or expired token")),
    }
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

        let token = encode(&Header::default(), &issued, &EncodingKey::from_secret(secret.as_bytes())).unwrap();

        let data = decode::<Claims>(&token, &DecodingKey::from_secret(secret.as_bytes()), &build_validation())
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
        let token = encode(&Header::default(), &issued, &EncodingKey::from_secret(secret.as_bytes())).unwrap();

        // build_validation() expects aud = "isupercoder-api"
        let result = decode::<Claims>(&token, &DecodingKey::from_secret(secret.as_bytes()), &build_validation());
        assert!(result.is_err());
    }
}