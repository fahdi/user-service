//! Concrete implementations of the DI traits in `traits.rs`.
//!
//! These wrap the existing global statics (MongoDB, Redis, LRU) so the
//! DI-based handlers can be wired with real infrastructure while remaining
//! testable with mocks.

use async_trait::async_trait;
use mongodb::bson::Document;
use std::sync::Arc;

use crate::models::auth::Claims;
use crate::models::user::{StandardizedUser, SettingsResponse};
use crate::traits::{
    AuthExtractor, CacheService, FileUploader, RepoError, RepoResult, UserRepository,
};

// ---------------------------------------------------------------------------
// MongoUserRepository
// ---------------------------------------------------------------------------

/// Concrete implementation backed by the global MongoDB connection pool.
pub struct MongoUserRepository;

impl MongoUserRepository {
    async fn get_db(&self) -> Result<mongodb::Database, RepoError> {
        crate::get_database()
            .await
            .map_err(|e| RepoError(format!("Database connection failed: {}", e)))
    }

    fn users_collection(&self, db: &mongodb::Database) -> mongodb::Collection<Document> {
        db.collection::<Document>("users")
    }

    fn activities_collection(&self, db: &mongodb::Database) -> mongodb::Collection<Document> {
        db.collection::<Document>("user_activities")
    }
}

#[async_trait]
impl UserRepository for MongoUserRepository {
    async fn find_user(
        &self,
        filter: Document,
        projection: Option<Document>,
    ) -> RepoResult<Option<Document>> {
        let db = self.get_db().await?;
        let coll = self.users_collection(&db);
        let opts = projection.map(|p| {
            mongodb::options::FindOneOptions::builder()
                .projection(p)
                .build()
        });
        coll.find_one(filter, opts)
            .await
            .map_err(|e| RepoError(e.to_string()))
    }

    async fn update_user(&self, filter: Document, update: Document) -> RepoResult<u64> {
        let db = self.get_db().await?;
        let coll = self.users_collection(&db);
        let result = coll
            .update_one(filter, update, None)
            .await
            .map_err(|e| RepoError(e.to_string()))?;
        Ok(result.matched_count)
    }

    async fn count_users(&self, filter: Document) -> RepoResult<u64> {
        let db = self.get_db().await?;
        let coll = self.users_collection(&db);
        coll.count_documents(filter, None)
            .await
            .map_err(|e| RepoError(e.to_string()))
    }

    async fn find_users(
        &self,
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>> {
        use futures_util::TryStreamExt;
        let db = self.get_db().await?;
        let coll = self.users_collection(&db);
        let opts = mongodb::options::FindOptions::builder()
            .projection(projection)
            .sort(sort)
            .skip(skip)
            .limit(limit)
            .build();
        let mut cursor = coll
            .find(filter, opts)
            .await
            .map_err(|e| RepoError(e.to_string()))?;
        let mut docs = Vec::new();
        while let Some(doc) = cursor.try_next().await.map_err(|e| RepoError(e.to_string()))? {
            docs.push(doc);
        }
        Ok(docs)
    }

    async fn insert_user(&self, doc: Document) -> RepoResult<String> {
        let db = self.get_db().await?;
        let coll = self.users_collection(&db);
        let result = coll
            .insert_one(doc, None)
            .await
            .map_err(|e| RepoError(e.to_string()))?;
        Ok(result
            .inserted_id
            .as_object_id()
            .map(|oid| oid.to_hex())
            .unwrap_or_default())
    }

    async fn count_activities(&self, filter: Document) -> RepoResult<u64> {
        let db = self.get_db().await?;
        let coll = self.activities_collection(&db);
        coll.count_documents(filter, None)
            .await
            .map_err(|e| RepoError(e.to_string()))
    }

    async fn find_activities(
        &self,
        filter: Document,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    ) -> RepoResult<Vec<Document>> {
        use futures_util::TryStreamExt;
        let db = self.get_db().await?;
        let coll = self.activities_collection(&db);
        let opts = mongodb::options::FindOptions::builder()
            .sort(sort)
            .skip(skip)
            .limit(limit)
            .build();
        let mut cursor = coll
            .find(filter, opts)
            .await
            .map_err(|e| RepoError(e.to_string()))?;
        let mut docs = Vec::new();
        while let Some(doc) = cursor.try_next().await.map_err(|e| RepoError(e.to_string()))? {
            docs.push(doc);
        }
        Ok(docs)
    }
}

// ---------------------------------------------------------------------------
// RedisCacheService
// ---------------------------------------------------------------------------

/// Concrete implementation that delegates to the existing cache_service module.
pub struct RedisCacheService;

#[async_trait]
impl CacheService for RedisCacheService {
    async fn get_cached_profile(&self, key: &str) -> Option<StandardizedUser> {
        crate::services::cache_service::get_cached_profile(key).await
    }

    async fn cache_profile(&self, key: &str, user: &StandardizedUser, ttl: u64) {
        let _ = crate::services::cache_service::cache_profile(key, user, ttl).await;
    }

    async fn invalidate_profile_cache(&self, key: &str) {
        let _ = crate::services::cache_service::invalidate_profile_cache(key).await;
    }

    async fn get_cached_settings(&self, key: &str) -> Option<SettingsResponse> {
        crate::services::cache_service::get_cached_settings(key).await
    }

    async fn cache_settings(&self, key: &str, settings: &SettingsResponse, ttl: u64) {
        let _ = crate::services::cache_service::cache_settings(key, settings, ttl).await;
    }

    async fn invalidate_settings_cache(&self, key: &str) {
        let _ = crate::services::cache_service::invalidate_settings_cache(key).await;
    }
}

// ---------------------------------------------------------------------------
// GoogleDriveUploader
// ---------------------------------------------------------------------------

/// Concrete implementation that delegates to the existing google_drive_service.
pub struct GoogleDriveUploader;

#[async_trait]
impl FileUploader for GoogleDriveUploader {
    async fn upload_profile_picture(
        &self,
        user_id: &str,
        user_email: &str,
        file_data: Vec<u8>,
        file_name: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        crate::services::google_drive_service::upload_profile_picture(
            user_id, user_email, file_data, file_name,
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(std::io::Error::other(e.to_string()))
        })
    }
}

// ---------------------------------------------------------------------------
// JwtAuthExtractor
// ---------------------------------------------------------------------------

/// Concrete implementation that delegates to the existing middleware/auth module.
pub struct JwtAuthExtractor;

impl AuthExtractor for JwtAuthExtractor {
    fn extract_claims(&self, req: &actix_web::HttpRequest) -> Result<Claims, actix_web::Error> {
        crate::middleware::auth::extract_claims_from_request(req)
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

use crate::traits::AppState;

/// Build an `AppState` wired with real (production) implementations.
pub fn build_app_state() -> AppState {
    AppState {
        repo: Arc::new(MongoUserRepository),
        cache: Arc::new(RedisCacheService),
        uploader: Arc::new(GoogleDriveUploader),
        auth: Arc::new(JwtAuthExtractor),
    }
}
