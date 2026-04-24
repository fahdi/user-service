//! Trait abstractions for dependency injection.
//!
//! These traits decouple handler logic from concrete MongoDB, Redis, and
//! external-service implementations so the handlers can be tested with
//! lightweight mocks.

use async_trait::async_trait;
use mongodb::bson::Document;

use crate::models::user::{StandardizedUser, SettingsResponse};

// ---------------------------------------------------------------------------
// User repository (MongoDB abstraction)
// ---------------------------------------------------------------------------

/// Result type for repository operations.
pub type RepoResult<T> = Result<T, RepoError>;

/// Minimal error type surfaced by the repository layer.
#[derive(Debug, Clone)]
pub struct RepoError(pub String);

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RepoError {}

/// Abstracts all database operations the user handlers need.
#[async_trait]
pub trait UserRepository: Send + Sync {
    /// Find a single user by MongoDB filter, applying the given projection.
    async fn find_user(
        &self,
        filter: Document,
        projection: Option<Document>,
    ) -> RepoResult<Option<Document>>;

    /// Update a single user matching `filter` with an update document.
    /// Returns the number of matched documents.
    async fn update_user(
        &self,
        filter: Document,
        update: Document,
    ) -> RepoResult<u64>;

    /// Count documents matching `filter` in the users collection.
    async fn count_users(&self, filter: Document) -> RepoResult<u64>;

    /// Search users with filter, projection, sort, skip, limit.
    /// Returns the matching documents.
    async fn find_users(
        &self,
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>>;

    /// Insert a single user document. Returns the inserted id as hex string.
    async fn insert_user(&self, doc: Document) -> RepoResult<String>;

    // ---- Activity collection ----

    /// Count activity documents matching `filter`.
    async fn count_activities(&self, filter: Document) -> RepoResult<u64>;

    /// Find activity documents with filter, sort, skip, limit.
    async fn find_activities(
        &self,
        filter: Document,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>>;
}

// ---------------------------------------------------------------------------
// Cache service abstraction
// ---------------------------------------------------------------------------

/// Abstracts the multi-layer (LRU + Redis) cache used by handlers.
#[async_trait]
pub trait CacheService: Send + Sync {
    async fn get_cached_profile(&self, key: &str) -> Option<StandardizedUser>;
    async fn cache_profile(&self, key: &str, user: &StandardizedUser, ttl: u64);
    async fn invalidate_profile_cache(&self, key: &str);

    async fn get_cached_settings(&self, key: &str) -> Option<SettingsResponse>;
    async fn cache_settings(&self, key: &str, settings: &SettingsResponse, ttl: u64);
    async fn invalidate_settings_cache(&self, key: &str);
}

// ---------------------------------------------------------------------------
// File uploader abstraction (Google Drive)
// ---------------------------------------------------------------------------

/// Abstracts the profile picture upload so handlers can be tested
/// without hitting the real Google Drive API.
#[async_trait]
pub trait FileUploader: Send + Sync {
    /// Upload a profile picture and return the public URL.
    async fn upload_profile_picture(
        &self,
        user_id: &str,
        user_email: &str,
        file_data: Vec<u8>,
        file_name: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

// ---------------------------------------------------------------------------
// Auth extractor abstraction
// ---------------------------------------------------------------------------

use crate::models::auth::Claims;

/// Abstracts JWT extraction from HTTP requests.
pub trait AuthExtractor: Send + Sync {
    fn extract_claims(&self, req: &actix_web::HttpRequest) -> Result<Claims, actix_web::Error>;
}

// ---------------------------------------------------------------------------
// AppState bundles all injectable dependencies for handlers
// ---------------------------------------------------------------------------

use std::sync::Arc;

/// Application state passed to all handlers via `web::Data`.
pub struct AppState {
    pub repo: Arc<dyn UserRepository>,
    pub cache: Arc<dyn CacheService>,
    pub uploader: Arc<dyn FileUploader>,
    pub auth: Arc<dyn AuthExtractor>,
}
