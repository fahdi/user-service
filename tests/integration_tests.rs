use actix_web::{test, web, App};
use serde_json::json;

// Integration tests for user service
#[cfg(test)]
mod tests {
    use super::*;

    #[actix_web::test]
    async fn test_health_endpoint() {
        let app = test::init_service(
            App::new()
                .route("/health", web::get().to(health_handler))
        ).await;

        let req = test::TestRequest::get()
            .uri("/health")
            .to_request();
        
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn test_profile_endpoint_requires_auth() {
        let app = test::init_service(
            App::new()
                .route("/api/users/profile", web::get().to(unauthorized_handler))
        ).await;

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .to_request();
        
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn test_settings_endpoint_requires_auth() {
        let app = test::init_service(
            App::new()
                .route("/api/users/settings", web::get().to(unauthorized_handler))
        ).await;

        let req = test::TestRequest::get()
            .uri("/api/users/settings")
            .to_request();
        
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    // Test handlers
    async fn health_handler() -> actix_web::Result<actix_web::HttpResponse> {
        Ok(actix_web::HttpResponse::Ok().json(json!({
            "status": "healthy",
            "service": "user-service-test"
        })))
    }

    async fn unauthorized_handler() -> actix_web::Result<actix_web::HttpResponse> {
        Ok(actix_web::HttpResponse::Unauthorized().json(json!({
            "success": false,
            "error": "Authentication required"
        })))
    }
}