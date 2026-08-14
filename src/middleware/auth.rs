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

/// How long to wait for auth-service before treating it as unavailable.
///
/// Matches the bound the rest of the fleet uses for outbound calls. The exact
/// value matters less than there being one.
const AUTH_SERVICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Built once for the process, not per request (super#182).
///
/// A `reqwest::Client` owns its connection pool, so constructing one per call
/// discarded the pool immediately and every authenticated request opened a new
/// TCP connection to auth-service. Measured in projects-api with a counting
/// listener: 8 requests made 8 connections with a fresh client and 1 shared.
///
/// The timeout is the same bound the fleet added when an unbounded client made
/// the `Unavailable` fallback unreachable; a shared client carries it equally.
/// `Option` rather than `expect` so a build failure keeps the
/// availability-preserving path instead of aborting at first use.
static AUTH_SERVICE_CLIENT: std::sync::LazyLock<Option<reqwest::Client>> =
    std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(AUTH_SERVICE_TIMEOUT)
            .build()
            .map_err(|e| log::error!("Failed to build auth-service client: {}", e))
            .ok()
    });

async fn verify_with_auth_service(auth_service_url: &str, token: &str) -> RemoteValidation {
    // Bounded on purpose. `reqwest::Client::new()` sets no timeout at all, so
    // an unanswered request fell through to the OS TCP timeout. That made the
    // `Unavailable` fallback below unreachable when auth-service *hangs* as
    // opposed to refusing: `send()` never returns, so the arm that preserves
    // availability never runs (#49).
    let client = match AUTH_SERVICE_CLIENT.as_ref() {
        Some(client) => client,
        // Building a client with only a timeout set should not fail. If it
        // does, that is this service's problem, not a verdict on the token, so
        // it takes the same path as an unreachable auth-service.
        None => {
            log::error!("auth-service client could not be built");
            return RemoteValidation::Unavailable;
        }
    };
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

    // Inspect the status before parsing: auth-service's 4xx responses (401,
    // 403, and 429 rate-limit rejections) carry bodies without the `valid`
    // field, and a parse failure must not be misread as an outage. A 4xx is
    // a verdict on the request; only transport errors and 5xx are outages.
    let status = response.status();
    if status.is_client_error() {
        return RemoteValidation::Rejected;
    }
    if !status.is_success() {
        return RemoteValidation::Unavailable;
    }

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

/// Resolve the JWT secret for request handling. Missing configuration is a
/// server error response (500), never a panic: `extract_claims_from_request`
/// runs inside 13 request handlers, and unwinding there aborts the
/// connection while /health keeps passing. Takes the env result as input so
/// tests need no env-var mutation (which races parallel tests).
/// Shortest secret this service will run with, matching the floor
/// utilities-forms, auth-service and projects-api enforce.
pub const MIN_JWT_SECRET_LEN: usize = 32;

/// Resolve the JWT secret, rejecting one that is present but unusable.
///
/// This previously mapped only the `Err` arm. That asks whether the variable
/// is *set*, which is a different question from whether it is *usable*:
/// `env::var` returns `Ok("")` for a set-but-empty variable, and
/// `docker-compose.production.yml` uses `JWT_SECRET=${JWT_SECRET}` with no
/// default, which Compose substitutes as `""` when the host variable is
/// missing. HMAC-SHA256 with an empty key is valid, so tokens signed with
/// nothing would have verified (infra#55).
pub fn jwt_secret_from(
    var: Result<String, std::env::VarError>,
) -> Result<String, actix_web::Error> {
    let secret = var.map_err(|_| {
        log::error!("JWT_SECRET is not set; rejecting authenticated request");
        actix_web::error::ErrorInternalServerError("Server configuration error")
    })?;

    if secret.trim().is_empty() {
        log::error!("JWT_SECRET is set but empty; rejecting authenticated request");
        return Err(actix_web::error::ErrorInternalServerError(
            "Server configuration error",
        ));
    }

    if secret.len() < MIN_JWT_SECRET_LEN {
        log::error!(
            "JWT_SECRET is {} characters; at least {} are required",
            secret.len(),
            MIN_JWT_SECRET_LEN
        );
        return Err(actix_web::error::ErrorInternalServerError(
            "Server configuration error",
        ));
    }

    Ok(secret)
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
    let jwt_secret = jwt_secret_from(env::var("JWT_SECRET"))?;
    let auth_service_url =
        env::var("AUTH_SERVICE_URL").unwrap_or_else(|_| "http://auth-service:8080".to_string());

    validate_bearer_with(&auth_service_url, token, &jwt_secret).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    // ── JWT secret usability (infra#55) ─────────────────────────────
    //
    // `jwt_secret_from` only mapped the `Err` arm, and `main` gated startup on
    // `env::var(..).is_err()`. Both ask whether the variable is *set*, which
    // is a different question from whether it is *usable*: `env::var` returns
    // `Ok("")` for a set-but-empty variable, and production compose uses
    // `JWT_SECRET=${JWT_SECRET}` with no default, which Compose substitutes as
    // `""` when the host variable is missing. HMAC with an empty key is valid,
    // so tokens signed with nothing would have verified.
    //
    // This function already takes the env lookup as a parameter, so these need
    // no process-environment mutation and no serialisation.

    #[test]
    fn empty_secret_is_rejected() {
        assert!(jwt_secret_from(Ok(String::new())).is_err());
    }

    #[test]
    fn whitespace_only_secret_is_rejected() {
        assert!(jwt_secret_from(Ok("   ".to_string())).is_err());
    }

    #[test]
    fn short_secret_is_rejected() {
        let secret = "a".repeat(MIN_JWT_SECRET_LEN - 1);
        assert!(jwt_secret_from(Ok(secret)).is_err());
    }

    #[test]
    fn missing_secret_is_still_rejected() {
        assert!(jwt_secret_from(Err(std::env::VarError::NotPresent)).is_err());
    }

    #[test]
    fn the_deployed_local_dev_secret_is_accepted() {
        // The value infra/local-dev/docker-compose.dev.yml actually supplies,
        // so this guard cannot break the environment it ships with.
        let secret = "local-dev-jwt-secret-isupercoder-2025-not-for-production";
        assert_eq!(jwt_secret_from(Ok(secret.to_string())).unwrap(), secret);
    }

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

#[cfg(test)]
mod auth_client_reuse_tests {
    //! The auth-service client must be shared across requests (super#182).
    //!
    //! A `reqwest::Client` owns its connection pool, so building one per call
    //! discarded the pool immediately: every authenticated request opened a
    //! new TCP connection to auth-service.
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Counts accepted connections and answers keep-alive, so the test
    /// measures pooling rather than server behaviour.
    fn counting_server() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut s = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                counter.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 2048];
                    while let Ok(n) = s.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let _ = s.write_all(
                            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\n\r\n{}",
                        );
                        let _ = s.flush();
                    }
                });
            }
        });
        (addr, accepts)
    }

    #[tokio::test]
    async fn repeated_validations_reuse_one_connection() {
        let (addr, accepts) = counting_server();

        for _ in 0..6 {
            let _ = verify_with_auth_service(&addr, "any-token").await;
        }

        let connections = accepts.load(Ordering::SeqCst);
        assert_eq!(
            connections, 1,
            "six validations should share one pooled connection, opened {connections}. \
             A client built per call discards its pool immediately (super#182)"
        );
    }

    #[tokio::test]
    async fn the_shared_client_exists() {
        assert!(
            AUTH_SERVICE_CLIENT.is_some(),
            "the shared client must build successfully"
        );
    }
}
