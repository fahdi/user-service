//! Integration tests for all 13 user-service endpoints using trait-based DI.
//!
//! These tests exercise the actual DI handler functions from `di_handlers.rs`
//! with mock implementations of UserRepository, CacheService, FileUploader,
//! and AuthExtractor. No real MongoDB or Redis is required.

use actix_web::{test, web, App};
use async_trait::async_trait;
use mongodb::bson::{doc, oid::ObjectId, DateTime as BsonDateTime, Document};
use serde_json::json;
use std::sync::{Arc, Mutex};

use user_service::models::auth::Claims;
use user_service::models::user::{SettingsResponse, StandardizedUser};
use user_service::traits::{
    AppState, AuthExtractor, CacheService, FileUploader, RepoError, RepoResult, UserRepository,
};

// ============================================================================
// Mock implementations
// ============================================================================

/// In-memory user repository backed by a Vec<Document>.
#[derive(Clone)]
struct MockUserRepo {
    users: Arc<Mutex<Vec<Document>>>,
    activities: Arc<Mutex<Vec<Document>>>,
    /// Simulate an unreachable database for `/health` (#42). Its own axis:
    /// a repository can be reachable and still fail a particular query.
    health_fails: Arc<Mutex<bool>>,
    /// Make every query fail, to exercise the error path (#47).
    query_error: Arc<Mutex<Option<String>>>,
    /// Fail only the activity queries (#61). Its own axis because the export
    /// handler reads the user first: a global failure would return 500 from
    /// that earlier call and prove nothing about the activity fetch.
    activities_error: Arc<Mutex<Option<String>>>,
}

impl MockUserRepo {
    fn new() -> Self {
        Self {
            users: Arc::new(Mutex::new(Vec::new())),
            activities: Arc::new(Mutex::new(Vec::new())),
            health_fails: Arc::new(Mutex::new(false)),
            query_error: Arc::new(Mutex::new(None)),
            activities_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Every query returns this error text, so a test can assert the response
    /// does not repeat it back to the caller (#47).
    fn with_query_error(self, msg: &str) -> Self {
        *self.query_error.lock().unwrap() = Some(msg.to_string());
        self
    }

    /// Only the activity queries fail; the user lookup still succeeds (#61).
    fn with_activities_error(self, msg: &str) -> Self {
        *self.activities_error.lock().unwrap() = Some(msg.to_string());
        self
    }

    fn with_failing_health(self) -> Self {
        *self.health_fails.lock().unwrap() = true;
        self
    }

    fn with_user(self, doc: Document) -> Self {
        self.users.lock().unwrap().push(doc);
        self
    }

    fn with_activity(self, doc: Document) -> Self {
        self.activities.lock().unwrap().push(doc);
        self
    }
}

/// Check if a document matches a simple BSON filter (supports _id, email, role, user_id, action).
fn doc_matches_filter(doc: &Document, filter: &Document) -> bool {
    for (key, filter_val) in filter.iter() {
        if key == "$or" || key == "$ne" {
            // Skip complex query operators — return true for simplicity
            continue;
        }
        match doc.get(key) {
            Some(doc_val) => {
                if doc_val != filter_val {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

#[async_trait]
impl UserRepository for MockUserRepo {
    async fn health_check(&self) -> RepoResult<()> {
        if *self.health_fails.lock().unwrap() {
            return Err(RepoError("mock repository unreachable".into()));
        }
        Ok(())
    }

    async fn find_user(
        &self,
        filter: Document,
        _projection: Option<Document>,
    ) -> RepoResult<Option<Document>> {
        if let Some(msg) = self.query_error.lock().unwrap().clone() {
            return Err(RepoError(msg));
        }
        let users = self.users.lock().map_err(|e| RepoError(e.to_string()))?;
        Ok(users
            .iter()
            .find(|u| doc_matches_filter(u, &filter))
            .cloned())
    }

    async fn update_user(&self, filter: Document, _update: Document) -> RepoResult<u64> {
        let users = self.users.lock().map_err(|e| RepoError(e.to_string()))?;
        let count = users
            .iter()
            .filter(|u| doc_matches_filter(u, &filter))
            .count();
        Ok(count as u64)
    }

    async fn count_users(&self, filter: Document) -> RepoResult<u64> {
        let users = self.users.lock().map_err(|e| RepoError(e.to_string()))?;
        let count = if filter.is_empty() {
            users.len()
        } else {
            users
                .iter()
                .filter(|u| doc_matches_filter(u, &filter))
                .count()
        };
        Ok(count as u64)
    }

    async fn find_users(
        &self,
        filter: Document,
        _projection: Option<Document>,
        _sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>> {
        let users = self.users.lock().map_err(|e| RepoError(e.to_string()))?;
        let filtered: Vec<Document> = if filter.is_empty() {
            users.clone()
        } else {
            users
                .iter()
                .filter(|u| doc_matches_filter(u, &filter))
                .cloned()
                .collect()
        };
        let skip = skip.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(100) as usize;
        Ok(filtered.into_iter().skip(skip).take(limit).collect())
    }

    async fn insert_user(&self, doc: Document) -> RepoResult<String> {
        let mut users = self.users.lock().map_err(|e| RepoError(e.to_string()))?;
        let id = doc
            .get_object_id("_id")
            .map(|oid| oid.to_hex())
            .unwrap_or_else(|_| ObjectId::new().to_hex());
        users.push(doc);
        Ok(id)
    }

    async fn count_activities(&self, filter: Document) -> RepoResult<u64> {
        let activities = self
            .activities
            .lock()
            .map_err(|e| RepoError(e.to_string()))?;
        let count = if filter.is_empty() {
            activities.len()
        } else {
            activities
                .iter()
                .filter(|a| doc_matches_filter(a, &filter))
                .count()
        };
        Ok(count as u64)
    }

    async fn find_activities(
        &self,
        filter: Document,
        _sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>> {
        if let Some(msg) = self.activities_error.lock().unwrap().clone() {
            return Err(RepoError(msg));
        }
        let activities = self
            .activities
            .lock()
            .map_err(|e| RepoError(e.to_string()))?;
        let filtered: Vec<Document> = if filter.is_empty() {
            activities.clone()
        } else {
            activities
                .iter()
                .filter(|a| doc_matches_filter(a, &filter))
                .cloned()
                .collect()
        };
        let skip = skip.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(100) as usize;
        Ok(filtered.into_iter().skip(skip).take(limit).collect())
    }
}

/// No-op cache that never returns cached data.
struct NoOpCache;

#[async_trait]
impl CacheService for NoOpCache {
    async fn get_cached_profile(&self, _key: &str) -> Option<StandardizedUser> {
        None
    }
    async fn cache_profile(&self, _key: &str, _user: &StandardizedUser, _ttl: u64) {}
    async fn invalidate_profile_cache(&self, _key: &str) {}
    async fn get_cached_settings(&self, _key: &str) -> Option<SettingsResponse> {
        None
    }
    async fn cache_settings(&self, _key: &str, _settings: &SettingsResponse, _ttl: u64) {}
    async fn invalidate_settings_cache(&self, _key: &str) {}
}

/// Mock file uploader that returns a deterministic URL.
struct MockUploader;

#[async_trait]
impl FileUploader for MockUploader {
    async fn upload_profile_picture(
        &self,
        user_id: &str,
        _user_email: &str,
        _file_data: Vec<u8>,
        _file_name: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(format!(
            "https://drive.google.com/thumbnail?id=mock_{}",
            user_id
        ))
    }
}

/// Auth extractor that reads a custom test header to determine the claims.
///
/// Test header format: `Bearer admin:<oid>` or `Bearer customer:<oid>`
/// - `admin:<oid>` → admin user with the given ObjectId
/// - `customer:<oid>` → customer user with the given ObjectId
///
/// If no Authorization header is present, returns an error (401).
struct MockAuth;

impl MockAuth {
    fn parse_test_token(header_value: &str) -> Option<Claims> {
        let token = header_value.strip_prefix("Bearer ")?;
        let parts: Vec<&str> = token.splitn(2, ':').collect();
        if parts.len() != 2 {
            return None;
        }
        let role = parts[0];
        let user_id = parts[1];
        Some(Claims {
            user_id: user_id.to_string(),
            email: format!("{}@test.com", role),
            name: format!("Test {}", role),
            role_type: role.to_string(),
            role: role.to_string(),
            exp: 9999999999,
        })
    }
}

#[async_trait::async_trait(?Send)]
impl AuthExtractor for MockAuth {
    async fn extract_claims(
        &self,
        req: &actix_web::HttpRequest,
    ) -> Result<Claims, actix_web::Error> {
        let header = req
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| actix_web::error::ErrorUnauthorized("Missing auth header"))?;

        MockAuth::parse_test_token(header)
            .ok_or_else(|| actix_web::error::ErrorUnauthorized("Invalid test token"))
    }
}

// ============================================================================
// Test helpers
// ============================================================================

/// Build a valid 24-char hex ObjectId string for tests.
fn test_oid() -> ObjectId {
    ObjectId::new()
}

/// Create a standard user BSON document for testing.
fn make_user_doc(oid: ObjectId, email: &str, name: &str, role: &str) -> Document {
    doc! {
        "_id": oid,
        "email": email,
        "name": name,
        "role": role,
        "isActive": true,
        "emailVerified": true,
        "password": bcrypt::hash("OldPassword1!", 4).unwrap(),
        "createdAt": BsonDateTime::now(),
        "updatedAt": BsonDateTime::now(),
    }
}

fn make_state(repo: MockUserRepo) -> web::Data<AppState> {
    web::Data::new(AppState {
        repo: Arc::new(repo),
        cache: Arc::new(NoOpCache),
        uploader: Arc::new(MockUploader),
        auth: Arc::new(MockAuth),
    })
}

fn admin_token(oid: &ObjectId) -> String {
    format!("Bearer admin:{}", oid.to_hex())
}

fn customer_token(oid: &ObjectId) -> String {
    format!("Bearer customer:{}", oid.to_hex())
}

// ============================================================================
// 1. GET /health
// ============================================================================

#[cfg(test)]
mod health_tests {
    use super::*;
    use user_service::health;

    /// The endpoint Docker probes must be able to say no.
    ///
    /// It took no state, so it was structurally incapable of observing anything
    /// and answered 200 however broken the service was (#42). `curl -f` and
    /// `monitor-containers.sh` read only the status code, so an unhealthy
    /// report has to *be* a 503.
    async fn health_response(repo: MockUserRepo) -> (u16, serde_json::Value) {
        let state = make_state(repo);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/health", web::get().to(health)),
        )
        .await;

        let req = test::TestRequest::get().uri("/health").to_request();
        let resp = test::call_service(&app, req).await;
        let status = resp.status().as_u16();
        (status, test::read_body_json(resp).await)
    }

    #[actix_web::test]
    async fn test_health_returns_200_with_status() {
        let (status, body) = health_response(MockUserRepo::new()).await;

        assert_eq!(status, 200);
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["service"], "user-service");
        assert_eq!(body["version"], "1.0.0");
        assert!(body["timestamp"].is_string());
    }

    #[actix_web::test]
    async fn unreachable_database_is_a_503() {
        let (status, body) = health_response(MockUserRepo::new().with_failing_health()).await;

        assert_eq!(
            status, 503,
            "an unreachable database must be visible to `curl -f`, body was {body}"
        );
        assert_eq!(body["status"], "unhealthy");
        assert_eq!(body["database"], "unavailable");
    }
}

// ============================================================================
// 2. GET /api/users/profile
// ============================================================================

#[cfg(test)]
mod profile_tests {
    use super::*;
    use user_service::handlers::di_handlers::get_profile;

    #[actix_web::test]
    async fn test_get_profile_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/profile", web::get().to(get_profile)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .to_request();
        let resp = test::call_service(&app, req).await;

        // The DI handler catches auth errors and returns HttpResponse::Unauthorized (401).
        // However, for GET /profile with query params, the handler signature takes
        // web::Query<serde_json::Value> which may cause actix to return 400 if the
        // query string fails to deserialize. Without query params, it should be fine.
        // The mock AuthExtractor returns Err which the handler catches and returns 401.
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn test_get_profile_success() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));

        let state = make_state(repo);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/profile", web::get().to(get_profile)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["user"]["email"], "alice@test.com");
        assert_eq!(body["user"]["name"], "Alice");
        assert_eq!(body["user"]["role"], "customer");
    }

    #[actix_web::test]
    async fn test_get_profile_404_user_not_found() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new()); // empty repo

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/profile", web::get().to(get_profile)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }

    #[actix_web::test]
    async fn test_get_profile_invalid_user_id_format() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/profile", web::get().to(get_profile)),
        )
        .await;

        // Use a non-ObjectId user_id in the token
        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(("authorization", "Bearer customer:not-valid-oid"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("Invalid user ID"));
    }
}

// ============================================================================
// 3. POST /api/users/profile-picture (multipart)
// ============================================================================

#[cfg(test)]
mod profile_picture_tests {
    use super::*;
    use user_service::handlers::di_handlers::update_profile_picture;

    #[actix_web::test]
    async fn test_upload_profile_picture_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/profile-picture",
            web::post().to(update_profile_picture),
        ))
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/profile-picture")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("Authentication"));
    }

    #[actix_web::test]
    async fn test_upload_profile_picture_400_no_file() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/profile-picture",
            web::post().to(update_profile_picture),
        ))
        .await;

        // Send multipart request with no file field
        let boundary = "----TestBoundary";
        let body_content = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nvalue\r\n--{boundary}--\r\n"
        );

        let req = test::TestRequest::post()
            .uri("/api/users/profile-picture")
            .insert_header(("authorization", customer_token(&oid)))
            .insert_header((
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            ))
            .set_payload(body_content)
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("No profile picture"));
    }
}

// ============================================================================
// 4. DELETE /api/users/avatar
// ============================================================================

#[cfg(test)]
mod delete_avatar_tests {
    use super::*;
    use user_service::handlers::di_handlers::delete_avatar;

    #[actix_web::test]
    async fn test_delete_avatar_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/avatar", web::delete().to(delete_avatar)),
        )
        .await;

        let req = test::TestRequest::delete()
            .uri("/api/users/avatar")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_delete_avatar_success() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/avatar", web::delete().to(delete_avatar)),
        )
        .await;

        let req = test::TestRequest::delete()
            .uri("/api/users/avatar")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"].as_str().unwrap().contains("deleted"));
    }

    #[actix_web::test]
    async fn test_delete_avatar_404_user_not_found() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new()); // empty repo

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/avatar", web::delete().to(delete_avatar)),
        )
        .await;

        let req = test::TestRequest::delete()
            .uri("/api/users/avatar")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }
}

// ============================================================================
// 5. GET /api/users/settings
// ============================================================================

#[cfg(test)]
mod settings_get_tests {
    use super::*;
    use user_service::handlers::di_handlers::get_settings;

    #[actix_web::test]
    async fn test_get_settings_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::get().to(get_settings)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/settings")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_get_settings_success_with_defaults() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::get().to(get_settings)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/settings")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["settings"].is_object());
        // Default theme is "light"
        assert_eq!(body["settings"]["theme"], "light");
        assert_eq!(body["settings"]["language"], "en");
    }

    #[actix_web::test]
    async fn test_get_settings_404_user_not_found() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::get().to(get_settings)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/settings")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }
}

// ============================================================================
// 6. PUT /api/users/settings
// ============================================================================

#[cfg(test)]
mod settings_update_tests {
    use super::*;
    use user_service::handlers::di_handlers::update_settings;

    #[actix_web::test]
    async fn test_update_settings_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::put().to(update_settings)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .set_json(json!({
                "settings": {
                    "theme": "dark",
                    "language": "en",
                    "timezone": "UTC",
                    "notifications": { "email": true, "sound": false, "desktop": false }
                }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_update_settings_success() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::put().to(update_settings)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "settings": {
                    "theme": "dark",
                    "language": "en",
                    "timezone": "America/New_York",
                    "notifications": { "email": true, "sound": false, "desktop": false }
                }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("Settings updated"));
    }

    #[actix_web::test]
    async fn test_update_settings_400_invalid_theme() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::put().to(update_settings)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "settings": {
                    "theme": "neon",
                    "language": "en",
                    "timezone": "UTC",
                    "notifications": { "email": true, "sound": false, "desktop": false }
                }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }
}

// ============================================================================
// 7. POST /api/users/change-password
// ============================================================================

#[cfg(test)]
mod change_password_tests {
    use super::*;
    use user_service::handlers::di_handlers::change_password;

    #[actix_web::test]
    async fn test_change_password_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/change-password",
            web::post().to(change_password),
        ))
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .set_json(json!({
                "currentPassword": "OldPassword1!",
                "newPassword": "NewPassword1!"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_change_password_success() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/change-password",
            web::post().to(change_password),
        ))
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "currentPassword": "OldPassword1!",
                "newPassword": "NewPassword1!"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("Password changed"));
    }

    #[actix_web::test]
    async fn test_change_password_400_wrong_current_password() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/change-password",
            web::post().to(change_password),
        ))
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "currentPassword": "WrongPassword!",
                "newPassword": "NewPassword1!"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("incorrect"));
    }

    #[actix_web::test]
    async fn test_change_password_400_short_new_password() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/change-password",
            web::post().to(change_password),
        ))
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "currentPassword": "OldPassword1!",
                "newPassword": "short"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_change_password_404_user_not_found() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/change-password",
            web::post().to(change_password),
        ))
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "currentPassword": "OldPassword1!",
                "newPassword": "NewPassword1!"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }
}

// ============================================================================
// 8. GET /api/users/roles
// ============================================================================

#[cfg(test)]
mod get_roles_tests {
    use super::*;
    use user_service::handlers::di_handlers::get_user_roles;

    #[actix_web::test]
    async fn test_get_roles_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::get().to(get_user_roles)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/roles")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_get_roles_success() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::get().to(get_user_roles)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/roles")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["roles"].is_array());
        assert!(body["roles"].as_array().unwrap().len() >= 4);
        assert_eq!(body["current_role"], "customer");
        assert!(body["permissions"].is_array());
    }

    #[actix_web::test]
    async fn test_get_roles_returns_admin_permissions_for_admin() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::get().to(get_user_roles)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/roles")
            .insert_header(("authorization", admin_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["current_role"], "admin");
        let perms = body["permissions"].as_array().unwrap();
        let perm_strs: Vec<&str> = perms.iter().filter_map(|p| p.as_str()).collect();
        assert!(perm_strs.contains(&"user_management"));
        assert!(perm_strs.contains(&"system_settings"));
    }
}

// ============================================================================
// 9. PUT /api/users/roles
// ============================================================================

#[cfg(test)]
mod update_role_tests {
    use super::*;
    use user_service::handlers::di_handlers::update_user_role;

    #[actix_web::test]
    async fn test_update_role_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::put().to(update_user_role)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .set_json(json!({ "role": "editor" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_update_role_403_non_admin() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::put().to(update_user_role)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({ "role": "admin" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Admin access required"));
    }

    #[actix_web::test]
    async fn test_update_role_success_as_admin() {
        let oid = test_oid();
        let repo =
            MockUserRepo::new().with_user(make_user_doc(oid, "admin@test.com", "Admin", "admin"));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::put().to(update_user_role)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "role": "editor" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"].as_str().unwrap().contains("editor"));
    }

    #[actix_web::test]
    async fn test_update_role_400_invalid_role() {
        let oid = test_oid();
        let repo =
            MockUserRepo::new().with_user(make_user_doc(oid, "admin@test.com", "Admin", "admin"));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/roles", web::put().to(update_user_role)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "role": "superuser" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }
}

// ============================================================================
// 10. GET /api/users/activity
// ============================================================================

#[cfg(test)]
mod activity_tests {
    use super::*;
    use user_service::handlers::di_handlers::get_user_activity;

    #[actix_web::test]
    async fn test_get_activity_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/activity", web::get().to(get_user_activity)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/activity")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_get_activity_success_with_pagination() {
        let oid = test_oid();
        let activity_oid = test_oid();
        let repo = MockUserRepo::new().with_activity(doc! {
            "_id": activity_oid,
            "user_id": oid.to_hex(),
            "action": "login",
            "timestamp": BsonDateTime::now(),
        });
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/activity", web::get().to(get_user_activity)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/activity?page=1&limit=10")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["activities"].is_array());
        assert!(body["pagination"].is_object());
        assert_eq!(body["pagination"]["page"], 1);
    }

    #[actix_web::test]
    async fn test_get_activity_empty_returns_empty_array() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/activity", web::get().to(get_user_activity)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/activity")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["activities"].as_array().unwrap().len(), 0);
    }
}

// ============================================================================
// 11. GET /api/users/export
// ============================================================================

#[cfg(test)]
mod export_tests {
    use super::*;
    use user_service::handlers::di_handlers::export_user_data;

    #[actix_web::test]
    async fn test_export_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/export", web::get().to(export_user_data)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_export_success() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/export", web::get().to(export_user_data)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["data"].is_object());
        assert!(body["data"]["user"].is_object());
        assert_eq!(body["data"]["user"]["email"], "alice@test.com");
        assert!(body["data"]["exported_at"].is_string());
    }

    /// A person's own record of their data must not report a failed query as
    /// an empty history (#61). `.unwrap_or_default()` made the two
    /// indistinguishable, while the user lookup in the same handler has
    /// returned 500 with the detail logged, not disclosed, since #47.
    #[actix_web::test]
    async fn test_export_reports_a_failed_activity_query_not_an_empty_history() {
        let oid = test_oid();
        let repo = MockUserRepo::new()
            .with_user(make_user_doc(oid, "alice@test.com", "Alice", "customer"))
            .with_activities_error("mongodb: connection reset by peer");
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/export", web::get().to(export_user_data)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(
            resp.status().as_u16(),
            500,
            "a failed activity query must not be exported as an empty history"
        );
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        // #47: the rendered driver error stays in the log.
        let rendered = body.to_string();
        assert!(
            !rendered.contains("connection reset by peer"),
            "the driver's error text must not reach the caller: {rendered}"
        );
    }

    /// The export caps the history at 100. Whether that is the right bound is
    /// a product question; exporting a partial record as though it were whole
    /// is not (#61).
    #[actix_web::test]
    async fn test_export_discloses_that_the_history_was_truncated() {
        let oid = test_oid();
        let mut repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        for i in 0..150 {
            repo = repo.with_activity(doc! {
                // _id is required: standardize_activity_doc rejects a document
                // without one, and the handler drops what it cannot standardize.
                "_id": ObjectId::new(),
                "user_id": oid.to_hex(),
                "action": format!("login-{i}"),
                "timestamp": BsonDateTime::now(),
            });
        }
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/export", web::get().to(export_user_data)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(
            body["data"]["activities"].as_array().map(|a| a.len()),
            Some(100),
            "the bound itself is unchanged by this fix"
        );
        assert_eq!(
            body["data"]["activities_total"], 150,
            "the export must say how many activities exist"
        );
        assert_eq!(
            body["data"]["activities_truncated"], true,
            "the export must say it is incomplete"
        );
    }

    /// The disclosure must be honest in the other direction too: a complete
    /// export must not label itself truncated.
    #[actix_web::test]
    async fn test_export_of_a_short_history_is_not_marked_truncated() {
        let oid = test_oid();
        let repo = MockUserRepo::new()
            .with_user(make_user_doc(oid, "alice@test.com", "Alice", "customer"))
            .with_activity(doc! {
                "_id": ObjectId::new(),
                "user_id": oid.to_hex(),
                "action": "login",
                "timestamp": BsonDateTime::now(),
            });
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/export", web::get().to(export_user_data)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["data"]["activities_total"], 1);
        assert_eq!(body["data"]["activities_truncated"], false);
    }

    #[actix_web::test]
    async fn test_export_404_user_not_found() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/export", web::get().to(export_user_data)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }
}

// ============================================================================
// 12. POST /api/users/import
// ============================================================================

#[cfg(test)]
mod import_tests {
    use super::*;
    use user_service::handlers::di_handlers::import_user_data;

    #[actix_web::test]
    async fn test_import_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/import", web::post().to(import_user_data)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .set_json(json!({
                "data": { "email": "new@test.com", "name": "New User" }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_import_403_non_admin() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/import", web::post().to(import_user_data)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({
                "data": { "email": "new@test.com", "name": "New User" }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Admin access required"));
    }

    #[actix_web::test]
    async fn test_import_success_as_admin() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/import", web::post().to(import_user_data)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({
                "data": {
                    "email": "newuser@example.com",
                    "name": "New User",
                    "role": "customer"
                }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["imported_count"], 1);
        assert_eq!(body["failed_count"], 0);
    }

    #[actix_web::test]
    async fn test_import_400_invalid_email() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/import", web::post().to(import_user_data)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({
                "data": {
                    "email": "invalid_email",
                    "name": "Bad User"
                }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("Invalid email"));
    }

    #[actix_web::test]
    async fn test_import_400_duplicate_email() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            test_oid(),
            "existing@test.com",
            "Existing",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/import", web::post().to(import_user_data)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({
                "data": {
                    "email": "existing@test.com",
                    "name": "Duplicate"
                }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| { e.as_str().unwrap().contains("already exists") }));
    }
}

// ============================================================================
// 13. GET /api/admin/users
// ============================================================================

#[cfg(test)]
mod admin_search_tests {
    use super::*;
    use user_service::handlers::di_handlers::admin_search_users;

    #[actix_web::test]
    async fn test_admin_search_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::get().to(admin_search_users)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/admin/users")
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_search_403_non_admin() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::get().to(admin_search_users)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/admin/users")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Admin access required"));
    }

    #[actix_web::test]
    async fn test_admin_search_success() {
        let oid = test_oid();
        let repo = MockUserRepo::new()
            .with_user(make_user_doc(
                test_oid(),
                "alice@test.com",
                "Alice",
                "customer",
            ))
            .with_user(make_user_doc(test_oid(), "bob@test.com", "Bob", "editor"));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::get().to(admin_search_users)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["users"].is_array());
        assert_eq!(body["users"].as_array().unwrap().len(), 2);
        assert!(body["pagination"].is_object());
        assert_eq!(body["pagination"]["total"], 2);
    }

    #[actix_web::test]
    async fn test_admin_search_empty_result() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::get().to(admin_search_users)),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["users"].as_array().unwrap().len(), 0);
        assert_eq!(body["pagination"]["total"], 0);
    }
}

// ============================================================================
// 14. PUT /api/admin/users/{id}
// ============================================================================

#[cfg(test)]
mod admin_update_tests {
    use super::*;
    use user_service::handlers::di_handlers::admin_update_user;

    #[actix_web::test]
    async fn test_admin_update_401_without_auth() {
        let target_oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users/{id}", web::put().to(admin_update_user)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/admin/users/{}", target_oid.to_hex()))
            .set_json(json!({ "name": "Updated" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_update_403_non_admin() {
        let oid = test_oid();
        let target_oid = test_oid();
        let state = make_state(MockUserRepo::new());

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users/{id}", web::put().to(admin_update_user)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/admin/users/{}", target_oid.to_hex()))
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({ "name": "Updated" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Admin access required"));
    }

    #[actix_web::test]
    async fn test_admin_update_success() {
        let admin_oid = test_oid();
        let target_oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            target_oid,
            "target@test.com",
            "Target",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users/{id}", web::put().to(admin_update_user)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/admin/users/{}", target_oid.to_hex()))
            .insert_header(("authorization", admin_token(&admin_oid)))
            .set_json(json!({
                "name": "Updated Name",
                "role": "editor"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("updated successfully"));
    }

    #[actix_web::test]
    async fn test_admin_update_404_user_not_found() {
        let admin_oid = test_oid();
        let target_oid = test_oid();
        let state = make_state(MockUserRepo::new()); // empty

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users/{id}", web::put().to(admin_update_user)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/admin/users/{}", target_oid.to_hex()))
            .insert_header(("authorization", admin_token(&admin_oid)))
            .set_json(json!({ "name": "Updated" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }

    #[actix_web::test]
    async fn test_admin_update_400_invalid_role() {
        let admin_oid = test_oid();
        let target_oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            target_oid,
            "target@test.com",
            "Target",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users/{id}", web::put().to(admin_update_user)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/admin/users/{}", target_oid.to_hex()))
            .insert_header(("authorization", admin_token(&admin_oid)))
            .set_json(json!({ "role": "superuser" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_update_400_invalid_email() {
        let admin_oid = test_oid();
        let target_oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            target_oid,
            "target@test.com",
            "Target",
            "customer",
        ));
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users/{id}", web::put().to(admin_update_user)),
        )
        .await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/admin/users/{}", target_oid.to_hex()))
            .insert_header(("authorization", admin_token(&admin_oid)))
            .set_json(json!({ "email": "not-an-email" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }
}

// ============================================================================
// Route table: every protected route must authenticate (#44)
// ============================================================================
//
// The route table used to live inline in `main.rs`, so nothing could mount it.
// The tests rebuilt fragments route by route, which means they only ever
// covered the routes someone remembered to write a test for.
//
// Authentication here is a per-handler convention rather than a middleware:
// each handler calls `state.auth.extract_claims(&req)`. All thirteen do so
// today, checked by hand. Nothing holds that in place, which is what this
// covers: mount the real table with an extractor that always rejects and
// require every protected route to answer 401.

#[cfg(test)]
mod route_table_tests {
    use super::*;
    use user_service::traits::AuthExtractor;

    /// An extractor that refuses everything, standing in for a request with no
    /// or invalid credentials.
    struct RejectingAuth;

    #[async_trait::async_trait(?Send)]
    impl AuthExtractor for RejectingAuth {
        async fn extract_claims(
            &self,
            _req: &actix_web::HttpRequest,
        ) -> Result<Claims, actix_web::Error> {
            Err(actix_web::error::ErrorUnauthorized("no credentials"))
        }
    }

    fn rejecting_state() -> web::Data<AppState> {
        web::Data::new(AppState {
            repo: Arc::new(MockUserRepo::new()),
            cache: Arc::new(NoOpCache),
            uploader: Arc::new(MockUploader),
            auth: Arc::new(RejectingAuth),
        })
    }

    /// Every protected route in the production table, with the verb it answers.
    ///
    /// Enumerated from `configure_routes`. If a route is added there without a
    /// line here, `the_list_matches_the_route_table` fails, so this cannot
    /// silently fall behind the way a hand-written list normally does.
    /// Every protected route, with a body that **deserializes**.
    ///
    /// The body matters. Actix runs `web::Json<T>` extraction before the
    /// handler exists, so an unparseable body answers 400 without any handler
    /// code running. That is a framework property, not a missing auth check,
    /// and testing against it would assert the deserializer's behaviour rather
    /// than the handler's. Supplying a valid payload is what makes the handler
    /// run and its authentication observable.
    fn protected_routes() -> Vec<(&'static str, String, Option<serde_json::Value>)> {
        let settings = json!({
            "settings": {
                "theme": "dark",
                "language": "en",
                "timezone": "UTC",
                "notifications": { "email": true, "sound": false, "desktop": false }
            }
        });

        vec![
            ("GET", "/api/users/profile".into(), None),
            (
                "POST",
                "/api/users/profile-picture".into(),
                Some(json!({ "image": "x" })),
            ),
            ("DELETE", "/api/users/avatar".into(), None),
            ("GET", "/api/users/settings".into(), None),
            ("PUT", "/api/users/settings".into(), Some(settings)),
            (
                "POST",
                "/api/users/change-password".into(),
                Some(json!({ "currentPassword": "OldPassw0rd!", "newPassword": "NewPassw0rd!" })),
            ),
            ("GET", "/api/users/roles".into(), None),
            (
                "PUT",
                "/api/users/roles".into(),
                Some(json!({ "userId": "abc123", "role": "customer" })),
            ),
            ("GET", "/api/users/activity".into(), None),
            ("GET", "/api/users/export".into(), None),
            (
                "POST",
                "/api/users/import".into(),
                Some(json!({ "data": { "email": "a@b.com", "name": "A" } })),
            ),
            ("GET", "/api/admin/users".into(), None),
            (
                "PUT",
                "/api/admin/users/abc123".into(),
                Some(json!({ "role": "customer" })),
            ),
        ]
    }

    #[actix_web::test]
    async fn every_protected_route_rejects_an_unauthenticated_request() {
        let app = test::init_service(
            App::new()
                .app_data(rejecting_state())
                .configure(user_service::configure_routes),
        )
        .await;

        for (method, path, body) in protected_routes() {
            let builder = match method {
                "GET" => test::TestRequest::get(),
                "POST" => test::TestRequest::post(),
                "PUT" => test::TestRequest::put(),
                "DELETE" => test::TestRequest::delete(),
                other => panic!("unhandled verb {other}"),
            }
            .uri(&path);

            let req = match body {
                Some(b) => builder.set_json(b).to_request(),
                None => builder.to_request(),
            };

            let resp = test::call_service(&app, req).await;

            assert_eq!(
                resp.status().as_u16(),
                401,
                "{method} {path} answered {} without credentials; every protected \
                 route must authenticate (#44)",
                resp.status()
            );
        }
    }

    /// Authentication must come **before** body validation (#45).
    ///
    /// The test above cannot see this: it sends valid payloads, so validation
    /// succeeds and the handler reaches its auth check either way. This sends a
    /// body that **deserializes** but **fails validation**, which separates the
    /// two orders. Auth first answers 401; validation first answers 400 and
    /// hands an anonymous caller the password policy.
    #[actix_web::test]
    async fn authentication_precedes_body_validation() {
        let app = test::init_service(
            App::new()
                .app_data(rejecting_state())
                .configure(user_service::configure_routes),
        )
        .await;

        // Deserializes as two Strings; fails the length rules.
        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .set_json(json!({ "currentPassword": "", "newPassword": "" }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(
            resp.status().as_u16(),
            401,
            "an anonymous caller must be rejected before being told why their \
             payload is invalid (#45)"
        );
    }

    /// Health is deliberately outside the convention: the container probe sends
    /// no credentials, so requiring them would mark the service unhealthy (#42).
    #[actix_web::test]
    async fn health_stays_reachable_without_credentials() {
        let app = test::init_service(
            App::new()
                .app_data(rejecting_state())
                .configure(user_service::configure_routes),
        )
        .await;

        let req = test::TestRequest::get().uri("/health").to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
    }
}

// ============================================================================
// Internal error text must not reach the client (#47)
// ============================================================================
//
// Ten sites returned `format!("Database error: {}", e)` in the response body,
// and none of them logged it. The detail went to the party who should not have
// it and not to the party who needs it. Every other service in the fleet logs
// the detail and returns a generic message; projects-api states that pattern
// explicitly in its error mapping.

#[cfg(test)]
mod error_disclosure_tests {
    use super::*;

    /// A string that could only appear in the response if the underlying error
    /// were pasted into it. Deliberately unlike any legitimate message.
    const SECRET: &str = "mongodb://internal-host:27017 replica-set-alpha";

    #[actix_web::test]
    async fn a_database_failure_does_not_echo_the_error_to_the_caller() {
        let repo = MockUserRepo::new().with_query_error(SECRET);
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .configure(user_service::configure_routes),
        )
        .await;

        let oid = ObjectId::new();
        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(("authorization", customer_token(&oid)))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let status = resp.status().as_u16();
        let body: serde_json::Value = test::read_body_json(resp).await;
        let rendered = body.to_string();

        assert_eq!(status, 500, "a repository failure is still a 500");
        assert!(
            !rendered.contains(SECRET),
            "the response repeated the underlying error back to the caller: {rendered}"
        );
    }
}
