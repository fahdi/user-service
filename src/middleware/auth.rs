use actix_web::Result;
use jsonwebtoken::{decode, DecodingKey, Validation};
use std::env;

use crate::models::auth::Claims;

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
        .unwrap_or_else(|_| "your-secret-key-change-in-production-isupercoder-2024".to_string());
    
    let validation = Validation::default();
    
    match decode::<Claims>(token, &DecodingKey::from_secret(jwt_secret.as_ref()), &validation) {
        Ok(token_data) => Ok(token_data.claims),
        Err(_) => Err(actix_web::error::ErrorUnauthorized("Invalid or expired token")),
    }
}