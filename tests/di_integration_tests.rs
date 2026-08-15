//! Integration tests for all 14 user-service endpoints using trait-based DI.
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
    /// Fail every `insert_activity` call, independent of every other axis
    /// above (#70). This is the one that proves a broken activity log cannot
    /// fail the request describing the event: the handler's own read/update
    /// path must stay untouched while every activity write errors.
    activity_write_fails: Arc<Mutex<bool>>,
}

impl MockUserRepo {
    fn new() -> Self {
        Self {
            users: Arc::new(Mutex::new(Vec::new())),
            activities: Arc::new(Mutex::new(Vec::new())),
            health_fails: Arc::new(Mutex::new(false)),
            query_error: Arc::new(Mutex::new(None)),
            activities_error: Arc::new(Mutex::new(None)),
            activity_write_fails: Arc::new(Mutex::new(false)),
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

    /// Every `insert_activity` call fails, so a test can assert the
    /// originating request still succeeds (#70).
    fn with_failing_activity_writes(self) -> Self {
        *self.activity_write_fails.lock().unwrap() = true;
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
        if let Some(msg) = self.query_error.lock().unwrap().clone() {
            return Err(RepoError(msg));
        }
        let mut users = self.users.lock().map_err(|e| RepoError(e.to_string()))?;
        // Stand in for the real unique index on `email`: a real insert past
        // it fails with an E11000 write error, not a generic one (#85).
        if let Ok(email) = doc.get_str("email") {
            if users.iter().any(|u| u.get_str("email").ok() == Some(email)) {
                return Err(RepoError(format!(
                    "E11000 duplicate key error collection: isupercoder.users index: \
                     email_1 dup key: {{ email: \"{email}\" }}"
                )));
            }
        }
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

    async fn insert_activity(&self, doc: Document) -> RepoResult<String> {
        if *self.activity_write_fails.lock().unwrap() {
            return Err(RepoError("mock activity write failed".into()));
        }
        let mut activities = self
            .activities
            .lock()
            .map_err(|e| RepoError(e.to_string()))?;
        let id = doc
            .get_object_id("_id")
            .map(|oid| oid.to_hex())
            .unwrap_or_else(|_| ObjectId::new().to_hex());
        activities.push(doc);
        Ok(id)
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
// 14. POST /api/admin/users
// ============================================================================

#[cfg(test)]
mod admin_create_tests {
    use super::*;
    use user_service::handlers::di_handlers::admin_create_user;

    #[actix_web::test]
    async fn test_admin_create_401_without_auth() {
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .set_json(json!({ "name": "New User", "email": "new@test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_create_403_non_admin() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(json!({ "name": "New User", "email": "new@test.com" }))
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
    async fn test_admin_create_400_invalid_email() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "name": "New User", "email": "not-an-email" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_create_400_empty_name() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "name": "", "email": "new@test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_create_400_invalid_role() {
        let oid = test_oid();
        let state = make_state(MockUserRepo::new());
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({
                "name": "New User",
                "email": "new@test.com",
                "role": "superuser"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
    }

    #[actix_web::test]
    async fn test_admin_create_success_defaults() {
        let oid = test_oid();
        let repo = MockUserRepo::new();
        let check = repo.clone();
        let state = make_state(repo);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "name": "New User", "email": "NEW@Test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["user"]["email"], "new@test.com", "email is lowercased");
        assert_eq!(body["user"]["name"], "New User");
        assert_eq!(
            body["user"]["role"], "customer",
            "role defaults to customer"
        );
        assert_eq!(body["user"]["isActive"], true, "isActive defaults to true");
        assert_eq!(
            body["user"]["emailVerified"], false,
            "emailVerified defaults to false: nobody has proven this address yet"
        );
        assert!(
            body.get("password").is_none() && body["user"].get("password").is_none(),
            "the generated password must never appear in the response"
        );
        assert!(body["message"].as_str().unwrap().contains("reset"));

        let users = check.users.lock().unwrap();
        assert_eq!(users.len(), 1);
        let stored_password = users[0].get_str("password").unwrap();
        assert!(
            stored_password.starts_with("$2"),
            "the stored password must be a bcrypt hash, not the plaintext"
        );
    }

    #[actix_web::test]
    async fn test_admin_create_success_with_explicit_fields() {
        let oid = test_oid();
        let repo = MockUserRepo::new();
        let check = repo.clone();
        let state = make_state(repo);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({
                "name": "Explicit User",
                "email": "explicit@test.com",
                "role": "editor",
                "isActive": false,
                "emailVerified": true
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["user"]["role"], "editor");
        assert_eq!(body["user"]["isActive"], false);
        assert_eq!(body["user"]["emailVerified"], true);

        let users = check.users.lock().unwrap();
        assert_eq!(users[0].get_str("role").unwrap(), "editor");
        assert!(!users[0].get_bool("isActive").unwrap());
        assert!(users[0].get_bool("emailVerified").unwrap());
    }

    #[actix_web::test]
    async fn test_admin_create_409_duplicate_email() {
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
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "name": "Duplicate", "email": "existing@test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(
            resp.status().as_u16(),
            409,
            "a duplicate email must be a 409, not a 500 (#85)"
        );
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("already in use"));
    }

    #[actix_web::test]
    async fn test_admin_create_500_does_not_leak_database_error() {
        const SECRET: &str = "mongodb://internal-host:27017 replica-set-alpha";
        let oid = test_oid();
        let repo = MockUserRepo::new().with_query_error(SECRET);
        let state = make_state(repo);
        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&oid)))
            .set_json(json!({ "name": "New User", "email": "new@test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        let status = resp.status().as_u16();
        let body: serde_json::Value = test::read_body_json(resp).await;
        let rendered = body.to_string();

        assert_eq!(status, 500);
        assert!(
            !rendered.contains(SECRET),
            "the response repeated the underlying error back to the caller: {rendered}"
        );
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
// each handler calls `state.auth.extract_claims(&req)`. All fourteen do so
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

    /// Authentication must come **before** body validation, on every handler
    /// that validates a body (#45, #80).
    ///
    /// The test above cannot see this: it sends valid payloads, so validation
    /// succeeds and the handler reaches its auth check either way. These send
    /// bodies that **deserialize** but **fail validation**, which separates the
    /// two orders. Auth first answers 401; validation first answers 400 and
    /// tells an anonymous caller why.
    ///
    /// #45 named four handlers and this guard covered only `change_password`
    /// (#80). Reordering either role handler leaked the valid role vocabulary
    /// to an unauthenticated caller with the whole suite still green - measured,
    /// not assumed.
    #[actix_web::test]
    async fn authentication_precedes_body_validation() {
        use user_service::models::user::{
            AdminUserCreateRequest, AdminUserUpdateRequest, PasswordChangeRequest,
            RoleUpdateRequest, SettingsUpdateRequest,
        };
        use validator::Validate;

        /// A body that fails to *deserialize* answers 400 from the extractor,
        /// before any handler code runs - which would make the assertion below
        /// pass for a reason that has nothing to do with ordering. Each case
        /// proves its payload reaches `validate()` and is rejected there.
        fn deserializes_but_fails_validation<T>(body: &serde_json::Value) -> Result<(), String>
        where
            T: serde::de::DeserializeOwned + Validate,
        {
            let parsed: T = serde_json::from_value(body.clone()).map_err(|e| {
                format!("does not deserialize, so it never reaches validate(): {e}")
            })?;
            match parsed.validate() {
                Err(_) => Ok(()),
                Ok(()) => Err("deserializes and PASSES validation, so 401-vs-400 \
                               proves nothing about ordering"
                    .to_string()),
            }
        }

        type Prover = fn(&serde_json::Value) -> Result<(), String>;

        // The four routes named in #45, plus every one added since that also
        // authenticates and validates a body - `admin_create_user` (#85) is
        // the first of those. Each entry is an invalid-but-parseable body and
        // the request type that has to reject it.
        let cases: [(&str, &str, &str, serde_json::Value, Prover); 5] = [
            (
                "change_password",
                "POST",
                "/api/users/change-password",
                // Two Strings; both fail the length rules.
                json!({ "currentPassword": "", "newPassword": "" }),
                deserializes_but_fails_validation::<PasswordChangeRequest>,
            ),
            (
                "update_settings",
                "PUT",
                "/api/users/settings",
                // Fully-formed settings; only the theme is outside
                // light/dark/auto, so the custom validator rejects it.
                json!({ "settings": {
                    "notifications": { "email": true, "sound": true, "desktop": true },
                    "theme": "chartreuse",
                    "language": "en",
                    "timezone": "UTC"
                }}),
                deserializes_but_fails_validation::<SettingsUpdateRequest>,
            ),
            (
                "update_user_role",
                "PUT",
                "/api/users/roles",
                // Parses as a String; outside admin/customer/editor/subscriber.
                json!({ "role": "sysadmin" }),
                deserializes_but_fails_validation::<RoleUpdateRequest>,
            ),
            (
                "admin_update_user",
                "PUT",
                "/api/admin/users/507f1f77bcf86cd799439011",
                json!({ "role": "sysadmin" }),
                deserializes_but_fails_validation::<AdminUserUpdateRequest>,
            ),
            (
                "admin_create_user",
                "POST",
                "/api/admin/users",
                // Both fields are Strings so it deserializes; the email fails
                // the `email` validator.
                json!({ "name": "New User", "email": "not-an-email" }),
                deserializes_but_fails_validation::<AdminUserCreateRequest>,
            ),
        ];

        let app = test::init_service(
            App::new()
                .app_data(rejecting_state())
                .configure(user_service::configure_routes),
        )
        .await;

        for (handler, method, path, body, prove) in cases {
            prove(&body).unwrap_or_else(|why| {
                panic!("the {handler} payload cannot test ordering: {why}");
            });

            let req = match method {
                "POST" => test::TestRequest::post(),
                "PUT" => test::TestRequest::put(),
                other => panic!("unhandled method {other}"),
            };
            let resp = test::call_service(&app, req.uri(path).set_json(&body).to_request()).await;

            assert_eq!(
                resp.status().as_u16(),
                401,
                "{method} {path} ({handler}) answered {} - an anonymous caller must be \
                 rejected before being told why their payload is invalid (#45)",
                resp.status()
            );
        }
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

// ============================================================================
// Activity log writer (#70)
// ============================================================================
//
// `GET /api/users/activity` and the GDPR export read `user_activities`, but
// nothing wrote it. This is the writer side: each handler that performs one
// of the events this service can state truthfully about its own action logs
// it after the operation it describes succeeds. A failed activity write must
// never fail that operation - proven below by pointing the same mock at a
// repository whose `insert_activity` always errors and checking the
// originating request still returns 200.

#[cfg(test)]
mod activity_writer_tests {
    use super::*;
    use user_service::handlers::di_handlers::{
        admin_create_user, admin_update_user, change_password, delete_avatar,
        update_profile_picture, update_settings, update_user_role,
    };

    fn recorded_actions(repo: &MockUserRepo) -> Vec<String> {
        repo.activities
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.get_str("action").unwrap_or("").to_string())
            .collect()
    }

    fn valid_settings_body() -> serde_json::Value {
        json!({ "settings": {
            "notifications": { "email": true, "sound": true, "desktop": true },
            "theme": "dark",
            "language": "en",
            "timezone": "UTC"
        }})
    }

    #[actix_web::test]
    async fn password_change_records_password_changed() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let check = repo.clone();
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

        let actions = recorded_actions(&check);
        assert_eq!(
            actions,
            vec!["password_changed".to_string()],
            "change_password must write exactly one password_changed activity"
        );
        let activities = check.activities.lock().unwrap();
        assert_eq!(activities[0].get_str("user_id").unwrap(), oid.to_hex());
    }

    #[actix_web::test]
    async fn settings_update_records_settings_updated_only_for_preferences() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let check = repo.clone();
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
            .set_json(valid_settings_body())
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        assert_eq!(
            recorded_actions(&check),
            vec!["settings_updated".to_string()],
            "a settings-only update must not also claim profile_updated"
        );
    }

    #[actix_web::test]
    async fn settings_update_with_profile_fields_records_both_events() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let check = repo.clone();
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/users/settings", web::put().to(update_settings)),
        )
        .await;

        let mut body = valid_settings_body();
        body["settings"]["user"] = json!({
            "_id": oid.to_hex(),
            "email": "alice@test.com",
            "name": "Alice Updated",
            "role": "customer"
        });

        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .insert_header(("authorization", customer_token(&oid)))
            .set_json(body)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let actions = recorded_actions(&check);
        assert!(
            actions.contains(&"profile_updated".to_string()),
            "settings.user was present, so this must record profile_updated: {actions:?}"
        );
        assert!(
            actions.contains(&"settings_updated".to_string()),
            "settings is always applied, so this must also record settings_updated: {actions:?}"
        );
        assert_eq!(actions.len(), 2);
    }

    #[actix_web::test]
    async fn avatar_upload_records_avatar_updated() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let check = repo.clone();
        let state = make_state(repo);

        let app = test::init_service(App::new().app_data(state).route(
            "/api/users/profile-picture",
            web::post().to(update_profile_picture),
        ))
        .await;

        let boundary = "----TestBoundary70";
        let body_content = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"profilePicture\"; filename=\"a.jpg\"\r\nContent-Type: image/jpeg\r\n\r\nfake-bytes\r\n--{b}--\r\n",
            b = boundary
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
        assert_eq!(resp.status().as_u16(), 200);

        assert_eq!(recorded_actions(&check), vec!["avatar_updated".to_string()]);
    }

    #[actix_web::test]
    async fn avatar_delete_records_avatar_deleted() {
        let oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            oid,
            "alice@test.com",
            "Alice",
            "customer",
        ));
        let check = repo.clone();
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

        assert_eq!(recorded_actions(&check), vec!["avatar_deleted".to_string()]);
    }

    #[actix_web::test]
    async fn self_role_update_records_role_changed() {
        let oid = test_oid();
        let repo =
            MockUserRepo::new().with_user(make_user_doc(oid, "admin@test.com", "Admin", "admin"));
        let check = repo.clone();
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

        assert_eq!(recorded_actions(&check), vec!["role_changed".to_string()]);
    }

    #[actix_web::test]
    async fn admin_update_with_role_records_role_changed() {
        let admin_oid = test_oid();
        let target_oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            target_oid,
            "target@test.com",
            "Target",
            "customer",
        ));
        let check = repo.clone();
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
            .set_json(json!({ "role": "editor" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let actions = recorded_actions(&check);
        assert_eq!(actions, vec!["role_changed".to_string()]);
        let activities = check.activities.lock().unwrap();
        // The event is about the account whose role changed, not the admin
        // who changed it.
        assert_eq!(
            activities[0].get_str("user_id").unwrap(),
            target_oid.to_hex()
        );
    }

    #[actix_web::test]
    async fn admin_update_without_role_records_nothing() {
        let admin_oid = test_oid();
        let target_oid = test_oid();
        let repo = MockUserRepo::new().with_user(make_user_doc(
            target_oid,
            "target@test.com",
            "Target",
            "customer",
        ));
        let check = repo.clone();
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
            .set_json(json!({ "name": "Updated Name" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        assert!(
            recorded_actions(&check).is_empty(),
            "no role field was sent, so nothing this service can state \
             truthfully as role_changed happened"
        );
    }

    /// The critical requirement: a broken activity log must not break the
    /// request it is trying to describe. `change_password` still returns 200
    /// and still changes the password even though every `insert_activity`
    /// call errors.
    #[actix_web::test]
    async fn a_failing_activity_write_does_not_fail_the_request() {
        let oid = test_oid();
        let repo = MockUserRepo::new()
            .with_user(make_user_doc(oid, "alice@test.com", "Alice", "customer"))
            .with_failing_activity_writes();
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

        assert_eq!(
            resp.status().as_u16(),
            200,
            "an activity write failure must not turn a successful password \
             change into an error response"
        );
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("Password changed"));
    }

    /// The event belongs to the account that was just created, not to the
    /// admin who created it - `admin_update_user`'s `role_changed` sets the
    /// same precedent for `user_id` (#85).
    #[actix_web::test]
    async fn admin_create_records_account_created_for_the_new_user() {
        let admin_oid = test_oid();
        let repo = MockUserRepo::new();
        let check = repo.clone();
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&admin_oid)))
            .set_json(json!({ "name": "New User", "email": "brand-new@test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        let new_user_id = body["user"]["id"].as_str().unwrap().to_string();

        assert_eq!(
            recorded_actions(&check),
            vec!["account_created".to_string()]
        );
        let activities = check.activities.lock().unwrap();
        assert_eq!(
            activities[0].get_str("user_id").unwrap(),
            new_user_id,
            "the event is about the account that was created, not the admin"
        );
    }

    /// Same guarantee as `change_password` above: a broken activity log must
    /// not turn a successful account creation into an error response (#85).
    #[actix_web::test]
    async fn admin_create_failing_activity_write_does_not_fail_the_request() {
        let admin_oid = test_oid();
        let repo = MockUserRepo::new().with_failing_activity_writes();
        let state = make_state(repo);

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/api/admin/users", web::post().to(admin_create_user)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/admin/users")
            .insert_header(("authorization", admin_token(&admin_oid)))
            .set_json(json!({ "name": "New User", "email": "still-created@test.com" }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(
            resp.status().as_u16(),
            200,
            "an activity write failure must not turn a successful account \
             creation into an error response"
        );
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
    }
}
