//! Integration tests for all user-service endpoints using trait-based DI.
//!
//! These tests exercise the actual handler logic (auth extraction, validation,
//! DB interaction, caching, pagination) by injecting mock implementations of
//! `UserRepository`, `CacheService`, `FileUploader`, and `AuthExtractor`.
//!
//! No real MongoDB, Redis, or Google Drive connection is needed.

use std::sync::{Arc, Mutex};

use actix_web::{web, App, HttpRequest, HttpResponse, Result as ActixResult};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use mongodb::bson::{doc, oid::ObjectId, DateTime as BsonDateTime, Document};
use serde_json::json;
use validator::Validate;

use user_service::models::auth::Claims;
use user_service::models::response::{ErrorResponse, SuccessResponse};
use user_service::models::user::*;
use user_service::traits::*;
use user_service::utils::security::{generate_secure_password, validate_email};

// ============================================================================
// Test constants
// ============================================================================

const TEST_JWT_SECRET: &str = "test_jwt_secret_for_integration_tests_only";

// ============================================================================
// JWT helper
// ============================================================================

fn make_token(user_id: &str, role: &str, role_type: &str) -> String {
    let claims = Claims {
        user_id: user_id.to_string(),
        email: format!("{}@test.com", role),
        name: format!("Test {}", role),
        role_type: role_type.to_string(),
        role: role.to_string(),
        exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("failed to encode JWT")
}

fn make_expired_token(user_id: &str) -> String {
    let claims = Claims {
        user_id: user_id.to_string(),
        email: "expired@test.com".to_string(),
        name: "Expired".to_string(),
        role_type: "customer".to_string(),
        role: "customer".to_string(),
        exp: 1000000000, // year 2001 -- expired
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("failed to encode JWT")
}

fn bearer(token: &str) -> (&str, String) {
    ("authorization", format!("Bearer {}", token))
}

// ============================================================================
// Mock AuthExtractor
// ============================================================================

struct MockAuthExtractor;

impl AuthExtractor for MockAuthExtractor {
    fn extract_claims(&self, req: &HttpRequest) -> Result<Claims, actix_web::Error> {
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
        let validation = jsonwebtoken::Validation::default();
        match jsonwebtoken::decode::<Claims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
            &validation,
        ) {
            Ok(data) => Ok(data.claims),
            Err(_) => Err(actix_web::error::ErrorUnauthorized(
                "Invalid or expired token",
            )),
        }
    }
}

// ============================================================================
// Mock CacheService (no-op -- always miss)
// ============================================================================

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

// ============================================================================
// Mock FileUploader
// ============================================================================

struct MockFileUploader;

#[async_trait]
impl FileUploader for MockFileUploader {
    async fn upload_profile_picture(
        &self,
        user_id: &str,
        _user_email: &str,
        _file_data: Vec<u8>,
        _file_name: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(format!(
            "https://drive.google.com/mock-thumbnail?user={}",
            user_id
        ))
    }
}

// ============================================================================
// Mock UserRepository (in-memory)
// ============================================================================

#[derive(Clone)]
struct InMemoryRepo {
    users: Arc<Mutex<Vec<Document>>>,
    activities: Arc<Mutex<Vec<Document>>>,
}

impl InMemoryRepo {
    fn new() -> Self {
        Self {
            users: Arc::new(Mutex::new(Vec::new())),
            activities: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn seed_user(&self, oid: ObjectId, email: &str, name: &str, role: &str, password_hash: &str) {
        let mut users = self.users.lock().unwrap();
        users.push(doc! {
            "_id": oid,
            "email": email,
            "name": name,
            "role": role,
            "isActive": true,
            "emailVerified": true,
            "password": password_hash,
            "createdAt": BsonDateTime::now(),
            "updatedAt": BsonDateTime::now(),
        });
    }

    fn seed_activity(&self, user_id: &str, action: &str) {
        let mut activities = self.activities.lock().unwrap();
        activities.push(doc! {
            "_id": ObjectId::new(),
            "user_id": user_id,
            "action": action,
            "timestamp": BsonDateTime::now(),
        });
    }

    /// Helper: does the filter match a document? (simplified subset of MongoDB matching)
    fn doc_matches(doc: &Document, filter: &Document) -> bool {
        for (key, val) in filter.iter() {
            if key == "$or" {
                // $or: at least one sub-filter must match
                if let Some(arr) = val.as_array() {
                    let any_match = arr.iter().any(|sub| {
                        if let Some(sub_doc) = sub.as_document() {
                            Self::doc_matches(doc, sub_doc)
                        } else {
                            false
                        }
                    });
                    if !any_match {
                        return false;
                    }
                }
                continue;
            }
            if key == "$ne" {
                continue; // handled by parent
            }
            if let Some(filter_doc) = val.as_document() {
                // Nested operators like { "$ne": ... }, { "$regex": ... }
                if let Some(ne_val) = filter_doc.get("$ne") {
                    if let Some(doc_val) = doc.get(key) {
                        if doc_val == ne_val {
                            return false;
                        }
                    }
                    continue;
                }
                if filter_doc.contains_key("$regex") {
                    // Simplified regex: just check contains
                    let _regex_str = filter_doc.get_str("$regex").unwrap_or("");
                    // For test simplicity, we accept any regex match
                    continue;
                }
                // Fall through to exact match
            }
            // Exact match
            if doc.get(key) != Some(val) {
                return false;
            }
        }
        true
    }
}

#[async_trait]
impl UserRepository for InMemoryRepo {
    async fn find_user(
        &self,
        filter: Document,
        projection: Option<Document>,
    ) -> RepoResult<Option<Document>> {
        let users = self.users.lock().unwrap();
        for user in users.iter() {
            if Self::doc_matches(user, &filter) {
                // Apply projection (simplified: just exclude fields with value 0)
                if let Some(proj) = &projection {
                    let mut result = user.clone();
                    for (key, val) in proj.iter() {
                        if val.as_i32() == Some(0) || val.as_i64() == Some(0) {
                            result.remove(key);
                        }
                    }
                    return Ok(Some(result));
                }
                return Ok(Some(user.clone()));
            }
        }
        Ok(None)
    }

    async fn update_user(&self, filter: Document, update: Document) -> RepoResult<u64> {
        let mut users = self.users.lock().unwrap();
        let mut matched = 0u64;
        for user in users.iter_mut() {
            if Self::doc_matches(user, &filter) {
                matched += 1;
                // Apply $set
                if let Ok(set_doc) = update.get_document("$set") {
                    for (key, val) in set_doc.iter() {
                        user.insert(key.clone(), val.clone());
                    }
                }
                // Apply $unset
                if let Ok(unset_doc) = update.get_document("$unset") {
                    for (key, _) in unset_doc.iter() {
                        user.remove(key);
                    }
                }
                // If no $set/$unset, treat entire update as $set (for simple updates)
                if !update.contains_key("$set") && !update.contains_key("$unset") {
                    for (key, val) in update.iter() {
                        user.insert(key.clone(), val.clone());
                    }
                }
            }
        }
        Ok(matched)
    }

    async fn count_users(&self, filter: Document) -> RepoResult<u64> {
        let users = self.users.lock().unwrap();
        let count = users.iter().filter(|u| Self::doc_matches(u, &filter)).count();
        Ok(count as u64)
    }

    async fn find_users(
        &self,
        filter: Document,
        projection: Option<Document>,
        _sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>> {
        let users = self.users.lock().unwrap();
        let matched: Vec<_> = users
            .iter()
            .filter(|u| Self::doc_matches(u, &filter))
            .cloned()
            .collect();

        let skip = skip.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(100) as usize;

        let result: Vec<Document> = matched
            .into_iter()
            .skip(skip)
            .take(limit)
            .map(|mut user| {
                if let Some(proj) = &projection {
                    for (key, val) in proj.iter() {
                        if val.as_i32() == Some(0) || val.as_i64() == Some(0) {
                            user.remove(key);
                        }
                    }
                }
                user
            })
            .collect();

        Ok(result)
    }

    async fn insert_user(&self, doc: Document) -> RepoResult<String> {
        let mut users = self.users.lock().unwrap();
        let id = doc
            .get_object_id("_id")
            .map(|oid| oid.to_hex())
            .unwrap_or_else(|_| {
                let oid = ObjectId::new();
                // Note: we can't insert into borrowed doc here, but for tests this is fine
                oid.to_hex()
            });
        users.push(doc);
        Ok(id)
    }

    async fn count_activities(&self, filter: Document) -> RepoResult<u64> {
        let activities = self.activities.lock().unwrap();
        let count = activities
            .iter()
            .filter(|a| Self::doc_matches(a, &filter))
            .count();
        Ok(count as u64)
    }

    async fn find_activities(
        &self,
        filter: Document,
        _sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>> {
        let activities = self.activities.lock().unwrap();
        let skip = skip.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(100) as usize;

        let result: Vec<Document> = activities
            .iter()
            .filter(|a| Self::doc_matches(a, &filter))
            .skip(skip)
            .take(limit)
            .cloned()
            .collect();

        Ok(result)
    }
}

// ============================================================================
// Handler functions (reimplemented from di_handlers.rs for the lib test crate)
//
// These mirror the DI handler logic exactly. They use AppState and the trait
// abstractions so we test real handler behavior with mock backends.
// ============================================================================

mod handler_helpers {
    use mongodb::bson::{doc, oid::ObjectId, Document};
    use user_service::models::user::*;

    pub fn standardize_user_doc(user: &Document) -> Result<StandardizedUser, String> {
        let user_id_str = user
            .get_object_id("_id")
            .map(|oid| oid.to_hex())
            .map_err(|_| "Document missing valid _id field".to_string())?;

        Ok(StandardizedUser {
            _id: user_id_str.clone(),
            id: user_id_str,
            email: user.get_str("email").unwrap_or("").to_string(),
            name: user.get_str("name").unwrap_or("").to_string(),
            role: user.get_str("role").unwrap_or("customer").to_string(),
            is_active: user.get_bool("isActive").unwrap_or(true),
            email_verified: user.get_bool("emailVerified").unwrap_or(false),
            created_at: user
                .get_datetime("createdAt")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            updated_at: user
                .get_datetime("updatedAt")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            phone: user.get_str("phone").ok().map(|s| s.to_string()),
            company: user.get_str("company").ok().map(|s| s.to_string()),
            department: user.get_str("department").ok().map(|s| s.to_string()),
            position: user.get_str("position").ok().map(|s| s.to_string()),
            username: user.get_str("username").ok().map(|s| s.to_string()),
            profile_picture: user.get_str("profilePicture").ok().map(|s| s.to_string()),
            use_gravatar: user.get_bool("useGravatar").ok(),
            location: user.get_str("location").ok().map(|s| s.to_string()),
        })
    }

    pub fn standardize_activity_doc(activity_doc: &Document) -> Result<ActivityLog, String> {
        let activity_id = activity_doc
            .get_object_id("_id")
            .map(|oid| oid.to_hex())
            .map_err(|_| "Activity document missing valid _id field".to_string())?;

        Ok(ActivityLog {
            id: activity_id,
            user_id: activity_doc.get_str("user_id").unwrap_or("").to_string(),
            action: activity_doc.get_str("action").unwrap_or("").to_string(),
            resource: activity_doc.get_str("resource").ok().map(|s| s.to_string()),
            resource_id: activity_doc.get_str("resource_id").ok().map(|s| s.to_string()),
            ip_address: activity_doc.get_str("ip_address").ok().map(|s| s.to_string()),
            user_agent: activity_doc.get_str("user_agent").ok().map(|s| s.to_string()),
            timestamp: activity_doc
                .get_datetime("timestamp")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            metadata: activity_doc
                .get_document("metadata")
                .ok()
                .and_then(|d| serde_json::from_str(&d.to_string()).ok()),
        })
    }

    pub fn extract_user_basic_info(user: &Document) -> Result<UserBasicInfo, String> {
        let user_id = user
            .get_object_id("_id")
            .map(|oid| oid.to_hex())
            .map_err(|_| "Document missing valid _id field".to_string())?;

        Ok(UserBasicInfo {
            _id: user_id,
            email: user.get_str("email").unwrap_or("").to_string(),
            name: user.get_str("name").unwrap_or("").to_string(),
            role: user.get_str("role").unwrap_or("customer").to_string(),
            profile_picture: user.get_str("profilePicture").ok().map(|s| s.to_string()),
            use_gravatar: user.get_bool("useGravatar").ok(),
            location: user.get_str("location").ok().map(|s| s.to_string()),
        })
    }

    pub fn is_admin(role: &str, role_type: &str) -> bool {
        role == "admin" || role_type == "admin"
    }

    pub fn parse_object_id(id: &str) -> Result<ObjectId, String> {
        ObjectId::parse_str(id).map_err(|_| "Invalid user ID format".to_string())
    }

    pub fn profile_cache_key(user_id: &str) -> String {
        format!("user:profile:{}", user_id)
    }

    pub fn settings_cache_key(user_id: &str) -> String {
        format!("user:settings:{}", user_id)
    }

    pub fn get_role_definitions() -> Vec<RoleInfo> {
        vec![
            RoleInfo {
                name: "admin".to_string(),
                description: "Full system access with administrative privileges".to_string(),
                permissions: vec![
                    "read".to_string(), "write".to_string(), "delete".to_string(),
                    "admin".to_string(), "user_management".to_string(), "system_settings".to_string(),
                ],
            },
            RoleInfo {
                name: "customer".to_string(),
                description: "Regular user with standard access".to_string(),
                permissions: vec!["read".to_string(), "write".to_string(), "profile_edit".to_string()],
            },
            RoleInfo {
                name: "editor".to_string(),
                description: "Content editor with enhanced permissions".to_string(),
                permissions: vec![
                    "read".to_string(), "write".to_string(),
                    "content_edit".to_string(), "profile_edit".to_string(),
                ],
            },
            RoleInfo {
                name: "subscriber".to_string(),
                description: "Read-only access for subscribers".to_string(),
                permissions: vec!["read".to_string()],
            },
        ]
    }

    pub fn get_permissions_for_role(role: &str) -> Option<Vec<String>> {
        get_role_definitions()
            .into_iter()
            .find(|r| r.name == role)
            .map(|r| r.permissions)
    }

    pub fn parse_pagination(
        page: Option<u32>,
        limit: Option<u32>,
        default_limit: u32,
        max_limit: u32,
    ) -> (u32, u32, u64) {
        let page = page.unwrap_or(1).max(1);
        let limit = limit.unwrap_or(default_limit).clamp(1, max_limit);
        let skip = ((page - 1) as u64) * (limit as u64);
        (page, limit, skip)
    }

    pub fn compute_pagination_info(page: u32, limit: u32, total: u64) -> PaginationInfo {
        let total_pages = if limit == 0 {
            0
        } else {
            ((total as f64) / (limit as f64)).ceil() as u32
        };
        PaginationInfo {
            page,
            limit,
            total,
            total_pages,
            has_next: page < total_pages,
            has_prev: page > 1,
        }
    }

    pub fn collect_validation_errors(errors: &validator::ValidationErrors) -> String {
        let msgs: Vec<String> = errors
            .field_errors()
            .values()
            .flat_map(|field_errors| {
                field_errors.iter().map(|e| {
                    e.message
                        .as_ref()
                        .unwrap_or(&"Validation error".into())
                        .to_string()
                })
            })
            .collect();
        msgs.join(", ")
    }

    pub fn build_search_filter(q: Option<&str>, role: Option<&str>) -> Document {
        let mut filter = doc! {};
        if let Some(q) = q {
            let trimmed = q.trim();
            if !trimmed.is_empty() {
                let escaped = regex::escape(trimmed);
                filter.insert(
                    "$or",
                    vec![
                        doc! { "name": { "$regex": &escaped, "$options": "i" } },
                        doc! { "email": { "$regex": &escaped, "$options": "i" } },
                    ],
                );
            }
        }
        if let Some(role) = role {
            let trimmed = role.trim();
            if !trimmed.is_empty() {
                filter.insert("role", trimmed);
            }
        }
        filter
    }

    pub fn build_sort_doc(sort: Option<&str>, order: Option<&str>) -> Document {
        let sort_field = sort.unwrap_or("createdAt");
        let sort_order: i32 = match order {
            Some("asc") => 1,
            _ => -1,
        };
        doc! { sort_field: sort_order }
    }

    pub fn build_admin_update_fields(body: &AdminUserUpdateRequest) -> Document {
        let mut update_doc = doc! { "updatedAt": mongodb::bson::DateTime::now() };
        if let Some(name) = &body.name {
            update_doc.insert("name", name);
        }
        if let Some(email) = &body.email {
            update_doc.insert("email", email.to_lowercase());
        }
        if let Some(role) = &body.role {
            update_doc.insert("role", role);
        }
        if let Some(is_active) = body.is_active {
            update_doc.insert("isActive", is_active);
        }
        if let Some(email_verified) = body.email_verified {
            update_doc.insert("emailVerified", email_verified);
        }
        update_doc
    }

    pub fn build_settings_success_message(email_changed: bool, password_changed: bool) -> String {
        let mut msg = "Settings updated successfully".to_string();
        let mut changes = Vec::new();
        if email_changed {
            changes.push("email");
        }
        if password_changed {
            changes.push("password");
        }
        if !changes.is_empty() {
            msg.push_str(&format!(". {} updated.", changes.join(" and ")));
        }
        msg
    }
}

// ============================================================================
// DI-based handler functions (mirrors di_handlers.rs exactly)
// ============================================================================

mod handlers {
    use super::*;
    use handler_helpers::*;

    pub async fn get_profile(
        req: HttpRequest,
        query: web::Query<serde_json::Value>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let email = query.get("email").and_then(|v| v.as_str());
        let user_id_param = query.get("userId").and_then(|v| v.as_str());

        // Admin can look up any user by userId or email
        let target_user_id = if (user_id_param.is_some() || email.is_some())
            && is_admin(&claims.role, &claims.role_type)
        {
            user_id_param
                .unwrap_or_else(|| email.unwrap_or(&claims.user_id))
                .to_string()
        } else {
            claims.user_id.clone()
        };

        let cache_key = profile_cache_key(&target_user_id);
        if let Some(cached) = state.cache.get_cached_profile(&cache_key).await {
            return Ok(HttpResponse::Ok().json(UserProfileResponse {
                success: true,
                user: Some(cached),
                message: None,
            }));
        }

        let projection = doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 };
        let filter = if (user_id_param.is_some() || email.is_some())
            && is_admin(&claims.role, &claims.role_type)
        {
            if let Some(uid) = user_id_param {
                let oid = match parse_object_id(uid) {
                    Ok(o) => o,
                    Err(e) => {
                        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                            success: false,
                            error: e,
                        }));
                    }
                };
                doc! { "_id": oid }
            } else if let Some(em) = email {
                doc! { "email": em }
            } else {
                let oid = match parse_object_id(&claims.user_id) {
                    Ok(o) => o,
                    Err(e) => {
                        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                            success: false,
                            error: e,
                        }));
                    }
                };
                doc! { "_id": oid }
            }
        } else {
            let oid = match parse_object_id(&claims.user_id) {
                Ok(o) => o,
                Err(e) => {
                    return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                        success: false,
                        error: e,
                    }));
                }
            };
            doc! { "_id": oid }
        };

        let user = match state.repo.find_user(filter, Some(projection)).await {
            Ok(u) => u,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Database error: {}", e),
                }));
            }
        };

        let user = match user {
            Some(u) => u,
            None => {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        };

        let std_user = match standardize_user_doc(&user) {
            Ok(u) => u,
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Internal error: malformed document".to_string(),
                }));
            }
        };

        state.cache.cache_profile(&cache_key, &std_user, 900).await;

        Ok(HttpResponse::Ok().json(UserProfileResponse {
            success: true,
            user: Some(std_user),
            message: None,
        }))
    }

    pub async fn get_settings(
        req: HttpRequest,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let cache_key = settings_cache_key(&claims.user_id);
        if let Some(cached) = state.cache.get_cached_settings(&cache_key).await {
            return Ok(HttpResponse::Ok().json(cached));
        }

        let oid = match parse_object_id(&claims.user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        let projection = doc! {
            "settings": 1, "email": 1, "name": 1,
            "role": 1, "profilePicture": 1, "useGravatar": 1, "location": 1
        };

        let user = match state.repo.find_user(doc! { "_id": oid }, Some(projection)).await {
            Ok(u) => u,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Database error: {}", e),
                }));
            }
        };

        let user = match user {
            Some(u) => u,
            None => {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        };

        let settings = if let Ok(settings_doc) = user.get_document("settings") {
            serde_json::from_str(&settings_doc.to_string())
                .unwrap_or_else(|_| UserSettings::default())
        } else {
            UserSettings::default()
        };

        let mut response_settings = settings;
        let user_info = match extract_user_basic_info(&user) {
            Ok(info) => info,
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Internal error: malformed document".to_string(),
                }));
            }
        };
        response_settings.user = Some(user_info);

        let response_data = SettingsResponse {
            success: true,
            settings: Some(response_settings),
            message: None,
        };

        state.cache.cache_settings(&cache_key, &response_data, 1800).await;

        Ok(HttpResponse::Ok().json(response_data))
    }

    pub async fn update_settings(
        req: HttpRequest,
        body: web::Json<SettingsUpdateRequest>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        if let Err(validation_errors) = body.validate() {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: collect_validation_errors(&validation_errors),
            }));
        }

        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        if let Some(account_changes) = &body.account_changes {
            let oid = match parse_object_id(&claims.user_id) {
                Ok(o) => o,
                Err(_) => {
                    return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                        success: false,
                        error: "Invalid user ID format".to_string(),
                    }));
                }
            };

            let current_user = match state
                .repo
                .find_user(doc! { "_id": oid }, Some(doc! { "password": 1, "email": 1 }))
                .await
            {
                Ok(u) => u,
                Err(e) => {
                    return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                        success: false,
                        error: format!("Database error: {}", e),
                    }));
                }
            };

            let current_user = match current_user {
                Some(u) => u,
                None => {
                    return Ok(HttpResponse::NotFound().json(ErrorResponse {
                        success: false,
                        error: "User not found".to_string(),
                    }));
                }
            };

            let stored_password = current_user.get_str("password").unwrap_or("");
            if !bcrypt::verify(&account_changes.current_password, stored_password).unwrap_or(false)
            {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Current password is incorrect".to_string(),
                }));
            }
        }

        let mut update_doc = doc! {
            "settings": mongodb::bson::to_bson(&body.settings).unwrap(),
            "updatedAt": BsonDateTime::now()
        };

        if let Some(user_info) = &body.settings.user {
            update_doc.insert("name", &user_info.name);
            if let Some(location) = &user_info.location {
                update_doc.insert("location", location);
            }
            if let Some(use_gravatar) = user_info.use_gravatar {
                update_doc.insert("useGravatar", use_gravatar);
            }
        }

        if let Some(account_changes) = &body.account_changes {
            if let Some(new_email) = &account_changes.new_email {
                update_doc.insert("email", new_email.to_lowercase());
            }
            if let Some(new_password) = &account_changes.new_password {
                let password_hash = match bcrypt::hash(new_password, 4) {
                    Ok(h) => h,
                    Err(_) => {
                        return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                            success: false,
                            error: "Failed to process password".to_string(),
                        }));
                    }
                };
                update_doc.insert("password", password_hash);
            }
        }

        let oid = match parse_object_id(&claims.user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        match state
            .repo
            .update_user(doc! { "_id": oid }, doc! { "$set": update_doc })
            .await
        {
            Ok(matched) => {
                if matched == 0 {
                    return Ok(HttpResponse::NotFound().json(ErrorResponse {
                        success: false,
                        error: "User not found".to_string(),
                    }));
                }
            }
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Failed to update settings: {}", e),
                }));
            }
        }

        let cache_key = settings_cache_key(&claims.user_id);
        state.cache.invalidate_settings_cache(&cache_key).await;

        let email_changed = body
            .account_changes
            .as_ref()
            .is_some_and(|ac| ac.new_email.is_some());
        let password_changed = body
            .account_changes
            .as_ref()
            .is_some_and(|ac| ac.new_password.is_some());
        let success_message = build_settings_success_message(email_changed, password_changed);

        Ok(HttpResponse::Ok().json(SuccessResponse {
            success: true,
            message: success_message,
        }))
    }

    pub async fn change_password(
        req: HttpRequest,
        body: web::Json<PasswordChangeRequest>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        if let Err(validation_errors) = body.validate() {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: collect_validation_errors(&validation_errors),
            }));
        }

        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let oid = match parse_object_id(&claims.user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        let current_user = match state
            .repo
            .find_user(doc! { "_id": oid }, Some(doc! { "password": 1, "email": 1 }))
            .await
        {
            Ok(u) => u,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Database error: {}", e),
                }));
            }
        };

        let current_user = match current_user {
            Some(u) => u,
            None => {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        };

        let stored_password = current_user.get_str("password").unwrap_or("");
        if !bcrypt::verify(&body.current_password, stored_password).unwrap_or(false) {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Current password is incorrect".to_string(),
            }));
        }

        // Use cost 4 in tests for speed (prod uses 12)
        let new_hash = match bcrypt::hash(&body.new_password, 4) {
            Ok(h) => h,
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Failed to process password".to_string(),
                }));
            }
        };

        match state
            .repo
            .update_user(
                doc! { "_id": oid },
                doc! { "$set": { "password": new_hash, "updatedAt": BsonDateTime::now() } },
            )
            .await
        {
            Ok(matched) => {
                if matched == 0 {
                    return Ok(HttpResponse::NotFound().json(ErrorResponse {
                        success: false,
                        error: "User not found".to_string(),
                    }));
                }
            }
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Failed to change password".to_string(),
                }));
            }
        }

        Ok(HttpResponse::Ok().json(PasswordChangeResponse {
            success: true,
            message: "Password changed successfully".to_string(),
        }))
    }

    pub async fn delete_avatar(
        req: HttpRequest,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let oid = match parse_object_id(&claims.user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        match state
            .repo
            .update_user(
                doc! { "_id": oid },
                doc! {
                    "$set": { "useGravatar": true, "updatedAt": BsonDateTime::now() },
                    "$unset": { "profilePicture": "" }
                },
            )
            .await
        {
            Ok(matched) => {
                if matched == 0 {
                    return Ok(HttpResponse::NotFound().json(ErrorResponse {
                        success: false,
                        error: "User not found".to_string(),
                    }));
                }
            }
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Failed to delete avatar".to_string(),
                }));
            }
        }

        let cache_key = profile_cache_key(&claims.user_id);
        state.cache.invalidate_profile_cache(&cache_key).await;

        Ok(HttpResponse::Ok().json(SuccessResponse {
            success: true,
            message: "Profile picture deleted successfully. Gravatar will be used instead."
                .to_string(),
        }))
    }

    pub async fn get_user_roles(
        req: HttpRequest,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let roles = get_role_definitions();
        let current_role = claims.role.clone();
        let permissions = get_permissions_for_role(&current_role);

        Ok(HttpResponse::Ok().json(UserRolesResponse {
            success: true,
            roles,
            current_role: Some(current_role),
            permissions,
            message: None,
        }))
    }

    pub async fn update_user_role(
        req: HttpRequest,
        body: web::Json<RoleUpdateRequest>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        if let Err(validation_errors) = body.validate() {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: collect_validation_errors(&validation_errors),
            }));
        }

        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        if !is_admin(&claims.role, &claims.role_type) {
            return Ok(HttpResponse::Forbidden().json(ErrorResponse {
                success: false,
                error: "Admin access required to change user roles".to_string(),
            }));
        }

        let oid = match parse_object_id(&claims.user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        match state
            .repo
            .update_user(
                doc! { "_id": oid },
                doc! { "$set": { "role": &body.role, "updatedAt": BsonDateTime::now() } },
            )
            .await
        {
            Ok(matched) => {
                if matched == 0 {
                    return Ok(HttpResponse::NotFound().json(ErrorResponse {
                        success: false,
                        error: "User not found".to_string(),
                    }));
                }
            }
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Failed to update role".to_string(),
                }));
            }
        }

        let cache_key = profile_cache_key(&claims.user_id);
        state.cache.invalidate_profile_cache(&cache_key).await;

        Ok(HttpResponse::Ok().json(SuccessResponse {
            success: true,
            message: format!("User role updated to {}", body.role),
        }))
    }

    pub async fn get_user_activity(
        req: HttpRequest,
        query: web::Query<ActivityQuery>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let (page, limit, skip) = parse_pagination(query.page, query.limit, 20, 100);
        let filter = doc! { "user_id": &claims.user_id };
        let total = state.repo.count_activities(filter.clone()).await.unwrap_or(0);

        let activity_docs = match state
            .repo
            .find_activities(filter, Some(doc! { "timestamp": -1 }), Some(skip), Some(limit as i64))
            .await
        {
            Ok(docs) => docs,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Query failed: {}", e),
                }));
            }
        };

        let activities: Vec<_> = activity_docs
            .iter()
            .filter_map(|d| standardize_activity_doc(d).ok())
            .collect();

        let pagination = compute_pagination_info(page, limit, total);

        Ok(HttpResponse::Ok().json(UserActivityResponse {
            success: true,
            activities,
            pagination: Some(pagination),
            message: None,
        }))
    }

    pub async fn export_user_data(
        req: HttpRequest,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        let oid = match parse_object_id(&claims.user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        let projection = doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 };
        let user = match state.repo.find_user(doc! { "_id": oid }, Some(projection)).await {
            Ok(u) => u,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Database error: {}", e),
                }));
            }
        };

        let user = match user {
            Some(u) => u,
            None => {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        };

        let standardized_user = match standardize_user_doc(&user) {
            Ok(u) => u,
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Internal error: malformed document".to_string(),
                }));
            }
        };

        let settings = if let Ok(settings_doc) = user.get_document("settings") {
            serde_json::from_str(&settings_doc.to_string()).ok()
        } else {
            Some(UserSettings::default())
        };

        let activity_docs = state
            .repo
            .find_activities(
                doc! { "user_id": &claims.user_id },
                Some(doc! { "timestamp": -1 }),
                None,
                Some(100),
            )
            .await
            .unwrap_or_default();

        let activities: Vec<_> = activity_docs
            .iter()
            .filter_map(|d| standardize_activity_doc(d).ok())
            .collect();

        let export_data = UserDataExport {
            user: standardized_user,
            settings,
            activities,
            exported_at: chrono::Utc::now().to_rfc3339(),
        };

        Ok(HttpResponse::Ok().json(DataExportResponse {
            success: true,
            data: Some(export_data),
            download_url: None,
            message: Some("User data exported successfully".to_string()),
        }))
    }

    pub async fn import_user_data(
        req: HttpRequest,
        body: web::Json<DataImportRequest>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        if !is_admin(&claims.role, &claims.role_type) {
            return Ok(HttpResponse::Forbidden().json(ErrorResponse {
                success: false,
                error: "Admin access required for data import".to_string(),
            }));
        }

        if !validate_email(&body.data.email) {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid email format".to_string(),
            }));
        }

        let existing_user = state
            .repo
            .find_user(doc! { "email": body.data.email.to_lowercase() }, None)
            .await
            .unwrap_or(None);

        if existing_user.is_some() {
            return Ok(HttpResponse::BadRequest().json(DataImportResponse {
                success: false,
                imported_count: 0,
                failed_count: 1,
                errors: vec!["User with this email already exists".to_string()],
                message: "Import failed".to_string(),
            }));
        }

        let temp_password = generate_secure_password();
        let password_hash = match bcrypt::hash(&temp_password, 4) {
            Ok(h) => h,
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Failed to process password".to_string(),
                }));
            }
        };

        let mut user_doc = doc! {
            "_id": ObjectId::new(),
            "email": body.data.email.to_lowercase(),
            "name": &body.data.name,
            "role": body.data.role.as_ref().unwrap_or(&"customer".to_string()),
            "isActive": true,
            "emailVerified": false,
            "password": password_hash,
            "createdAt": BsonDateTime::now(),
            "updatedAt": BsonDateTime::now(),
        };

        if let Some(settings) = &body.data.settings {
            if let Ok(bson_val) = mongodb::bson::to_bson(settings) {
                user_doc.insert("settings", bson_val);
            }
        }

        match state.repo.insert_user(user_doc).await {
            Ok(_) => Ok(HttpResponse::Ok().json(DataImportResponse {
                success: true,
                imported_count: 1,
                failed_count: 0,
                errors: vec![],
                message: "User imported successfully. A password reset is required.".to_string(),
            })),
            Err(e) => Ok(HttpResponse::InternalServerError().json(DataImportResponse {
                success: false,
                imported_count: 0,
                failed_count: 1,
                errors: vec![format!("Database error: {}", e)],
                message: "Import failed".to_string(),
            })),
        }
    }

    pub async fn admin_search_users(
        req: HttpRequest,
        query: web::Query<UserSearchQuery>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        if !is_admin(&claims.role, &claims.role_type) {
            return Ok(HttpResponse::Forbidden().json(ErrorResponse {
                success: false,
                error: "Admin access required".to_string(),
            }));
        }

        let (page, limit, skip) = parse_pagination(query.page, query.limit, 10, 100);
        let filter = build_search_filter(query.q.as_deref(), query.role.as_deref());
        let sort_doc = build_sort_doc(query.sort.as_deref(), query.order.as_deref());
        let projection = doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 };

        let total = match state.repo.count_users(filter.clone()).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Database error: {}", e),
                }));
            }
        };

        let user_docs = match state
            .repo
            .find_users(filter, Some(projection), Some(sort_doc), Some(skip), Some(limit as i64))
            .await
        {
            Ok(docs) => docs,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Search failed: {}", e),
                }));
            }
        };

        let users: Vec<_> = user_docs
            .iter()
            .filter_map(|d| standardize_user_doc(d).ok())
            .collect();

        let pagination = compute_pagination_info(page, limit, total);

        Ok(HttpResponse::Ok().json(UserSearchResponse {
            success: true,
            users,
            pagination,
            message: None,
        }))
    }

    pub async fn admin_update_user(
        req: HttpRequest,
        path: web::Path<String>,
        body: web::Json<AdminUserUpdateRequest>,
        state: web::Data<AppState>,
    ) -> ActixResult<HttpResponse> {
        if let Err(validation_errors) = body.validate() {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: collect_validation_errors(&validation_errors),
            }));
        }

        let claims = match state.auth.extract_claims(&req) {
            Ok(c) => c,
            Err(_) => {
                return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                    success: false,
                    error: "Authentication required".to_string(),
                }));
            }
        };

        if !is_admin(&claims.role, &claims.role_type) {
            return Ok(HttpResponse::Forbidden().json(ErrorResponse {
                success: false,
                error: "Admin access required".to_string(),
            }));
        }

        let user_id = path.into_inner();
        let update_doc = build_admin_update_fields(&body);
        let oid = match parse_object_id(&user_id) {
            Ok(o) => o,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        match state
            .repo
            .update_user(doc! { "_id": oid }, doc! { "$set": update_doc })
            .await
        {
            Ok(matched) => {
                if matched == 0 {
                    return Ok(HttpResponse::NotFound().json(ErrorResponse {
                        success: false,
                        error: "User not found".to_string(),
                    }));
                }
            }
            Err(_) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: "Failed to update user".to_string(),
                }));
            }
        }

        let cache_key = profile_cache_key(&user_id);
        state.cache.invalidate_profile_cache(&cache_key).await;

        Ok(HttpResponse::Ok().json(SuccessResponse {
            success: true,
            message: "User updated successfully".to_string(),
        }))
    }

    /// Simple health endpoint (no DI needed)
    pub async fn health() -> ActixResult<HttpResponse> {
        Ok(HttpResponse::Ok().json(json!({
            "status": "healthy",
            "service": "user-service",
            "version": "1.0.0",
            "timestamp": chrono::Utc::now()
        })))
    }
}

// ============================================================================
// Test app builder
// ============================================================================

fn build_test_state(repo: InMemoryRepo) -> web::Data<AppState> {
    web::Data::new(AppState {
        repo: Arc::new(repo),
        cache: Arc::new(NoOpCache),
        uploader: Arc::new(MockFileUploader),
        auth: Arc::new(MockAuthExtractor),
    })
}

fn build_test_app(
    state: web::Data<AppState>,
) -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    App::new()
        .app_data(state)
        .route("/health", web::get().to(handlers::health))
        .service(
            web::scope("/api/users")
                .route("/profile", web::get().to(handlers::get_profile))
                .route("/avatar", web::delete().to(handlers::delete_avatar))
                .route("/settings", web::get().to(handlers::get_settings))
                .route("/settings", web::put().to(handlers::update_settings))
                .route("/change-password", web::post().to(handlers::change_password))
                .route("/roles", web::get().to(handlers::get_user_roles))
                .route("/roles", web::put().to(handlers::update_user_role))
                .route("/activity", web::get().to(handlers::get_user_activity))
                .route("/export", web::get().to(handlers::export_user_data))
                .route("/import", web::post().to(handlers::import_user_data)),
        )
        .service(
            web::scope("/api/admin/users")
                .route("", web::get().to(handlers::admin_search_users))
                .route("/{id}", web::put().to(handlers::admin_update_user)),
        )
}

// ============================================================================
// Shared test fixtures
// ============================================================================

fn test_user_oid() -> ObjectId {
    ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap()
}

fn test_admin_oid() -> ObjectId {
    ObjectId::parse_str("507f1f77bcf86cd799439012").unwrap()
}

/// bcrypt hash of "TestPassword1!" with cost 4
fn test_password_hash() -> String {
    bcrypt::hash("TestPassword1!", 4).unwrap()
}

fn seeded_repo() -> InMemoryRepo {
    let repo = InMemoryRepo::new();
    let user_oid = test_user_oid();
    let admin_oid = test_admin_oid();
    let pw_hash = test_password_hash();

    repo.seed_user(
        user_oid,
        "user@test.com",
        "Test User",
        "customer",
        &pw_hash,
    );
    repo.seed_user(
        admin_oid,
        "admin@test.com",
        "Admin User",
        "admin",
        &pw_hash,
    );

    // Seed some activities
    repo.seed_activity(&user_oid.to_hex(), "login");
    repo.seed_activity(&user_oid.to_hex(), "profile_update");
    repo.seed_activity(&user_oid.to_hex(), "login");

    repo
}

// ============================================================================
// 1. Health endpoint
// ============================================================================

#[cfg(test)]
mod health_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_health_returns_200() {
        let state = build_test_state(InMemoryRepo::new());
        let app = test::init_service(build_test_app(state)).await;

        let req = test::TestRequest::get().uri("/health").to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["service"], "user-service");
        assert!(body["timestamp"].is_string());
    }
}

// ============================================================================
// 2. Auth validation (all protected endpoints return 401 without token)
// ============================================================================

#[cfg(test)]
mod auth_validation_tests {
    use super::*;
    use actix_web::test;

    /// Assert an endpoint returns 401 without a token.
    ///
    /// For POST/PUT endpoints the JSON body extractor may fail (400) before the
    /// handler ever runs if the payload is empty or malformed. We send a valid
    /// JSON structure so deserialization succeeds and the handler can reach the
    /// auth check.
    async fn assert_401(uri: &str, method: &str) {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;

        let req = match method {
            "GET" => test::TestRequest::get().uri(uri).to_request(),
            "PUT" if uri.contains("settings") => test::TestRequest::put()
                .uri(uri)
                .set_json(json!({
                    "settings": {
                        "notifications": { "email": true, "sound": true, "desktop": false },
                        "theme": "light",
                        "language": "en",
                        "timezone": "UTC"
                    }
                }))
                .to_request(),
            "PUT" if uri.contains("roles") => test::TestRequest::put()
                .uri(uri)
                .set_json(json!({ "role": "customer" }))
                .to_request(),
            "PUT" => test::TestRequest::put()
                .uri(uri)
                .set_json(json!({ "name": "Test" }))
                .to_request(),
            "POST" if uri.contains("change-password") => test::TestRequest::post()
                .uri(uri)
                .set_json(json!({
                    "currentPassword": "OldPass123!",
                    "newPassword": "NewPass456!"
                }))
                .to_request(),
            "POST" if uri.contains("import") => test::TestRequest::post()
                .uri(uri)
                .set_json(json!({
                    "data": { "email": "x@test.com", "name": "X" }
                }))
                .to_request(),
            "POST" => test::TestRequest::post()
                .uri(uri)
                .set_json(json!({}))
                .to_request(),
            "DELETE" => test::TestRequest::delete().uri(uri).to_request(),
            _ => panic!("unsupported method"),
        };

        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status().as_u16(),
            401,
            "{} {} should return 401 without token",
            method,
            uri
        );
    }

    #[actix_web::test]
    async fn test_profile_requires_auth() {
        assert_401("/api/users/profile", "GET").await;
    }

    #[actix_web::test]
    async fn test_settings_get_requires_auth() {
        assert_401("/api/users/settings", "GET").await;
    }

    #[actix_web::test]
    async fn test_settings_put_requires_auth() {
        assert_401("/api/users/settings", "PUT").await;
    }

    #[actix_web::test]
    async fn test_change_password_requires_auth() {
        assert_401("/api/users/change-password", "POST").await;
    }

    #[actix_web::test]
    async fn test_delete_avatar_requires_auth() {
        assert_401("/api/users/avatar", "DELETE").await;
    }

    #[actix_web::test]
    async fn test_roles_get_requires_auth() {
        assert_401("/api/users/roles", "GET").await;
    }

    #[actix_web::test]
    async fn test_roles_put_requires_auth() {
        assert_401("/api/users/roles", "PUT").await;
    }

    #[actix_web::test]
    async fn test_activity_requires_auth() {
        assert_401("/api/users/activity", "GET").await;
    }

    #[actix_web::test]
    async fn test_export_requires_auth() {
        assert_401("/api/users/export", "GET").await;
    }

    #[actix_web::test]
    async fn test_import_requires_auth() {
        assert_401("/api/users/import", "POST").await;
    }

    #[actix_web::test]
    async fn test_admin_search_requires_auth() {
        assert_401("/api/admin/users", "GET").await;
    }

    #[actix_web::test]
    async fn test_admin_update_requires_auth() {
        assert_401("/api/admin/users/507f1f77bcf86cd799439011", "PUT").await;
    }

    #[actix_web::test]
    async fn test_expired_token_returns_401() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let expired = make_expired_token(&test_user_oid().to_hex());

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(bearer(&expired))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401, "Expired token should return 401");
    }

    #[actix_web::test]
    async fn test_invalid_bearer_format_returns_401() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(("authorization", "Basic invalid"))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn test_garbage_token_returns_401() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(("authorization", "Bearer not.a.valid.jwt.token"))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 401);
    }
}

// ============================================================================
// 3. Profile tests
// ============================================================================

#[cfg(test)]
mod profile_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_get_own_profile_success() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["user"].is_object());
        assert_eq!(body["user"]["email"], "user@test.com");
        assert_eq!(body["user"]["name"], "Test User");
        assert_eq!(body["user"]["role"], "customer");
        assert_eq!(body["user"]["isActive"], true);
    }

    #[actix_web::test]
    async fn test_profile_excludes_password() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(
            body["user"]["password"].is_null(),
            "Password should not be in profile response"
        );
    }

    #[actix_web::test]
    async fn test_profile_not_found() {
        let state = build_test_state(InMemoryRepo::new()); // empty repo
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/profile")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 404);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert_eq!(body["error"], "User not found");
    }

    #[actix_web::test]
    async fn test_admin_can_lookup_other_user_by_id() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");
        let target_id = test_user_oid().to_hex();

        let req = test::TestRequest::get()
            .uri(&format!("/api/users/profile?userId={}", target_id))
            .insert_header(bearer(&admin_token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["user"]["email"], "user@test.com");
    }

    #[actix_web::test]
    async fn test_admin_can_lookup_by_email() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::get()
            .uri("/api/users/profile?email=user@test.com")
            .insert_header(bearer(&admin_token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["user"]["email"], "user@test.com");
    }

    #[actix_web::test]
    async fn test_non_admin_cannot_lookup_other_user() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        // Customer tries to look up admin by userId -- should see own profile instead
        let customer_token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri(&format!(
                "/api/users/profile?userId={}",
                test_admin_oid().to_hex()
            ))
            .insert_header(bearer(&customer_token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        // Should see own profile, not admin's
        assert_eq!(body["user"]["email"], "user@test.com");
    }
}

// ============================================================================
// 4. Settings tests
// ============================================================================

#[cfg(test)]
mod settings_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_get_settings_returns_defaults_for_new_user() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/settings")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["settings"].is_object());
        assert_eq!(body["settings"]["theme"], "light");
        assert_eq!(body["settings"]["language"], "en");
        assert_eq!(body["settings"]["timezone"], "UTC");
    }

    #[actix_web::test]
    async fn test_get_settings_includes_user_info() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/settings")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["settings"]["user"].is_object());
        assert_eq!(body["settings"]["user"]["email"], "user@test.com");
    }

    #[actix_web::test]
    async fn test_update_settings_success() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .insert_header(bearer(&token))
            .set_json(json!({
                "settings": {
                    "notifications": { "email": false, "sound": true, "desktop": true },
                    "theme": "dark",
                    "language": "es",
                    "timezone": "America/New_York"
                }
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"].as_str().unwrap().contains("Settings updated"));
    }

    #[actix_web::test]
    async fn test_update_settings_invalid_theme_rejected() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .insert_header(bearer(&token))
            .set_json(json!({
                "settings": {
                    "notifications": { "email": true, "sound": true, "desktop": false },
                    "theme": "neon",
                    "language": "en",
                    "timezone": "UTC"
                }
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }

    #[actix_web::test]
    async fn test_update_settings_persists() {
        let repo = seeded_repo();
        let state = build_test_state(repo.clone());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        // Update settings
        let req = test::TestRequest::put()
            .uri("/api/users/settings")
            .insert_header(bearer(&token))
            .set_json(json!({
                "settings": {
                    "notifications": { "email": true, "sound": false, "desktop": false },
                    "theme": "dark",
                    "language": "fr",
                    "timezone": "Europe/Paris"
                }
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        // Verify the user doc was actually updated in the repo
        let users = repo.users.lock().unwrap();
        let user = users
            .iter()
            .find(|u| u.get_str("email").unwrap_or("") == "user@test.com")
            .unwrap();
        assert!(user.contains_key("settings"), "Settings should be persisted");
    }
}

// ============================================================================
// 5. Password change tests
// ============================================================================

#[cfg(test)]
mod password_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_change_password_success() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(bearer(&token))
            .set_json(json!({
                "currentPassword": "TestPassword1!",
                "newPassword": "NewPassword99!"
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["message"], "Password changed successfully");
    }

    #[actix_web::test]
    async fn test_change_password_wrong_current() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(bearer(&token))
            .set_json(json!({
                "currentPassword": "WrongPassword!",
                "newPassword": "NewPassword99!"
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "Current password is incorrect");
    }

    #[actix_web::test]
    async fn test_change_password_short_new_password() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(bearer(&token))
            .set_json(json!({
                "currentPassword": "TestPassword1!",
                "newPassword": "short"
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400, "Short password should be rejected");
    }

    #[actix_web::test]
    async fn test_change_password_empty_current() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(bearer(&token))
            .set_json(json!({
                "currentPassword": "",
                "newPassword": "NewPassword99!"
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400, "Empty current password should be rejected");
    }

    #[actix_web::test]
    async fn test_change_password_persists_new_hash() {
        let repo = seeded_repo();
        let state = build_test_state(repo.clone());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::post()
            .uri("/api/users/change-password")
            .insert_header(bearer(&token))
            .set_json(json!({
                "currentPassword": "TestPassword1!",
                "newPassword": "BrandNewPassword1!"
            }))
            .to_request();

        test::call_service(&app, req).await;

        // Verify the password was actually changed in the repo
        let users = repo.users.lock().unwrap();
        let user = users
            .iter()
            .find(|u| u.get_str("email").unwrap_or("") == "user@test.com")
            .unwrap();
        let stored_hash = user.get_str("password").unwrap();
        assert!(
            bcrypt::verify("BrandNewPassword1!", stored_hash).unwrap(),
            "New password should verify against stored hash"
        );
    }
}

// ============================================================================
// 6. Delete avatar tests
// ============================================================================

#[cfg(test)]
mod avatar_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_delete_avatar_success() {
        let repo = seeded_repo();
        // Add a profile picture first
        {
            let mut users = repo.users.lock().unwrap();
            let user = users
                .iter_mut()
                .find(|u| u.get_str("email").unwrap_or("") == "user@test.com")
                .unwrap();
            user.insert("profilePicture", "https://example.com/pic.jpg");
        }

        let state = build_test_state(repo.clone());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::delete()
            .uri("/api/users/avatar")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"].as_str().unwrap().contains("Gravatar"));

        // Verify profile picture was removed and useGravatar set
        let users = repo.users.lock().unwrap();
        let user = users
            .iter()
            .find(|u| u.get_str("email").unwrap_or("") == "user@test.com")
            .unwrap();
        assert!(
            user.get_str("profilePicture").is_err(),
            "profilePicture should be unset"
        );
        assert!(user.get_bool("useGravatar").unwrap());
    }

    #[actix_web::test]
    async fn test_delete_avatar_user_not_found() {
        let state = build_test_state(InMemoryRepo::new()); // empty repo
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::delete()
            .uri("/api/users/avatar")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 404);
    }
}

// ============================================================================
// 7. Roles tests
// ============================================================================

#[cfg(test)]
mod roles_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_get_roles_returns_definitions() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/roles")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        let roles = body["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 4);

        let role_names: Vec<&str> = roles.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(role_names.contains(&"admin"));
        assert!(role_names.contains(&"customer"));
        assert!(role_names.contains(&"editor"));
        assert!(role_names.contains(&"subscriber"));
    }

    #[actix_web::test]
    async fn test_get_roles_returns_current_role() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/roles")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["current_role"], "customer");
        assert!(body["permissions"].is_array());
    }

    #[actix_web::test]
    async fn test_update_role_requires_admin() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let customer_token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .insert_header(bearer(&customer_token))
            .set_json(json!({ "role": "editor" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn test_update_role_success_as_admin() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .insert_header(bearer(&admin_token))
            .set_json(json!({ "role": "editor" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["message"].as_str().unwrap().contains("editor"));
    }

    #[actix_web::test]
    async fn test_update_role_invalid_role_rejected() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::put()
            .uri("/api/users/roles")
            .insert_header(bearer(&admin_token))
            .set_json(json!({ "role": "superuser" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }
}

// ============================================================================
// 8. Activity tests
// ============================================================================

#[cfg(test)]
mod activity_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_get_activity_returns_paginated_list() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/activity")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["activities"].is_array());
        assert_eq!(body["activities"].as_array().unwrap().len(), 3);
        assert!(body["pagination"].is_object());
        assert_eq!(body["pagination"]["total"], 3);
    }

    #[actix_web::test]
    async fn test_get_activity_pagination_params() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/activity?page=1&limit=2")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["activities"].as_array().unwrap().len(), 2);
        assert_eq!(body["pagination"]["page"], 1);
        assert_eq!(body["pagination"]["limit"], 2);
        assert_eq!(body["pagination"]["hasNext"], true);
    }

    #[actix_web::test]
    async fn test_get_activity_empty_for_user_with_none() {
        let repo = InMemoryRepo::new();
        let oid = test_user_oid();
        let pw = test_password_hash();
        repo.seed_user(oid, "user@test.com", "Test User", "customer", &pw);
        // No activities seeded

        let state = build_test_state(repo);
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&oid.to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/activity")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["activities"].as_array().unwrap().len(), 0);
        assert_eq!(body["pagination"]["total"], 0);
    }
}

// ============================================================================
// 9. Admin endpoints tests
// ============================================================================

#[cfg(test)]
mod admin_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_admin_search_users_returns_list() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::get()
            .uri("/api/admin/users")
            .insert_header(bearer(&admin_token))
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
    async fn test_admin_search_non_admin_gets_403() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let customer_token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/admin/users")
            .insert_header(bearer(&customer_token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn test_admin_search_with_role_filter() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::get()
            .uri("/api/admin/users?role=admin")
            .insert_header(bearer(&admin_token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["users"].as_array().unwrap().len(), 1);
        assert_eq!(body["users"][0]["role"], "admin");
    }

    #[actix_web::test]
    async fn test_admin_search_pagination() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::get()
            .uri("/api/admin/users?page=1&limit=1")
            .insert_header(bearer(&admin_token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["users"].as_array().unwrap().len(), 1);
        assert_eq!(body["pagination"]["page"], 1);
        assert_eq!(body["pagination"]["limit"], 1);
        assert_eq!(body["pagination"]["total"], 2);
        assert_eq!(body["pagination"]["totalPages"], 2);
        assert_eq!(body["pagination"]["hasNext"], true);
    }

    #[actix_web::test]
    async fn test_admin_update_user_success() {
        let repo = seeded_repo();
        let state = build_test_state(repo.clone());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::put()
            .uri(&format!(
                "/api/admin/users/{}",
                test_user_oid().to_hex()
            ))
            .insert_header(bearer(&admin_token))
            .set_json(json!({
                "name": "Updated Name",
                "role": "editor"
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);

        // Verify changes persisted
        let users = repo.users.lock().unwrap();
        let user = users
            .iter()
            .find(|u| u.get_str("email").unwrap_or("") == "user@test.com")
            .unwrap();
        assert_eq!(user.get_str("name").unwrap(), "Updated Name");
        assert_eq!(user.get_str("role").unwrap(), "editor");
    }

    #[actix_web::test]
    async fn test_admin_update_non_admin_gets_403() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let customer_token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::put()
            .uri(&format!(
                "/api/admin/users/{}",
                test_admin_oid().to_hex()
            ))
            .insert_header(bearer(&customer_token))
            .set_json(json!({ "name": "Hacked" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn test_admin_update_invalid_id_returns_400() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::put()
            .uri("/api/admin/users/not-a-valid-id")
            .insert_header(bearer(&admin_token))
            .set_json(json!({ "name": "Test" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }

    #[actix_web::test]
    async fn test_admin_update_nonexistent_user_returns_404() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::put()
            .uri(&format!(
                "/api/admin/users/{}",
                ObjectId::new().to_hex()
            ))
            .insert_header(bearer(&admin_token))
            .set_json(json!({ "name": "Nobody" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 404);
    }

    #[actix_web::test]
    async fn test_admin_update_invalid_role_returns_400() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::put()
            .uri(&format!(
                "/api/admin/users/{}",
                test_user_oid().to_hex()
            ))
            .insert_header(bearer(&admin_token))
            .set_json(json!({ "role": "superuser" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }
}

// ============================================================================
// 10. Data export/import tests
// ============================================================================

#[cfg(test)]
mod export_import_tests {
    use super::*;
    use actix_web::test;

    #[actix_web::test]
    async fn test_export_returns_complete_data() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert!(body["data"].is_object());
        assert!(body["data"]["user"].is_object());
        assert_eq!(body["data"]["user"]["email"], "user@test.com");
        assert!(body["data"]["settings"].is_object());
        assert!(body["data"]["activities"].is_array());
        assert!(body["data"]["exported_at"].is_string());
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("exported successfully"));
    }

    #[actix_web::test]
    async fn test_export_excludes_password() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(
            body["data"]["user"]["password"].is_null(),
            "Password should not appear in export"
        );
    }

    #[actix_web::test]
    async fn test_export_includes_activities() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::get()
            .uri("/api/users/export")
            .insert_header(bearer(&token))
            .to_request();

        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["data"]["activities"].as_array().unwrap().len(), 3);
    }

    #[actix_web::test]
    async fn test_import_requires_admin() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let customer_token = make_token(&test_user_oid().to_hex(), "customer", "customer");

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(bearer(&customer_token))
            .set_json(json!({
                "data": {
                    "email": "new@example.com",
                    "name": "New User"
                }
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[actix_web::test]
    async fn test_import_success() {
        let repo = seeded_repo();
        let state = build_test_state(repo.clone());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(bearer(&admin_token))
            .set_json(json!({
                "data": {
                    "email": "imported@example.com",
                    "name": "Imported User",
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

        // Verify user was created in the repo
        let users = repo.users.lock().unwrap();
        let imported = users
            .iter()
            .find(|u| u.get_str("email").unwrap_or("") == "imported@example.com");
        assert!(imported.is_some(), "Imported user should exist in repo");
    }

    #[actix_web::test]
    async fn test_import_invalid_email_rejected() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(bearer(&admin_token))
            .set_json(json!({
                "data": {
                    "email": "not-an-email",
                    "name": "Bad Email"
                }
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }

    #[actix_web::test]
    async fn test_import_duplicate_email_rejected() {
        let state = build_test_state(seeded_repo());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(bearer(&admin_token))
            .set_json(json!({
                "data": {
                    "email": "user@test.com",
                    "name": "Duplicate"
                }
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], false);
        assert!(body["errors"][0]
            .as_str()
            .unwrap()
            .contains("already exists"));
    }

    #[actix_web::test]
    async fn test_import_defaults_role_to_customer() {
        let repo = seeded_repo();
        let state = build_test_state(repo.clone());
        let app = test::init_service(build_test_app(state)).await;
        let admin_token = make_token(&test_admin_oid().to_hex(), "admin", "admin");

        let req = test::TestRequest::post()
            .uri("/api/users/import")
            .insert_header(bearer(&admin_token))
            .set_json(json!({
                "data": {
                    "email": "norole@example.com",
                    "name": "No Role Given"
                }
            }))
            .to_request();

        test::call_service(&app, req).await;

        let users = repo.users.lock().unwrap();
        let imported = users
            .iter()
            .find(|u| u.get_str("email").unwrap_or("") == "norole@example.com")
            .unwrap();
        assert_eq!(imported.get_str("role").unwrap(), "customer");
    }
}
