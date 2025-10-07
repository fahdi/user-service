use actix_web::{web, App, HttpServer, Result, HttpResponse, middleware::Logger};
use serde::Serialize;
use mongodb::{Client, options::ClientOptions, Database};
use deadpool_redis::{Config as RedisConfig, Pool as RedisPool, Runtime};
use lru::LruCache;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use lazy_static::lazy_static;
use std::num::NonZeroUsize;

mod handlers;
mod middleware;
mod models;
mod services;
mod utils;

use handlers::user_handlers::{
    get_profile, update_profile_picture, get_settings, update_settings, change_password, delete_avatar,
    admin_search_users, admin_update_user, get_user_roles, update_user_role,
    get_user_activity, export_user_data, import_user_data
};
// use middleware::rate_limit::{api_rate_limit, auth_rate_limit, admin_rate_limit};

// Global optimized pools and caches (following auth-service patterns)
lazy_static! {
    static ref REDIS_POOL: Arc<Mutex<Option<RedisPool>>> = Arc::new(Mutex::new(None));
    static ref MONGODB_CLIENT: Arc<Mutex<Option<Client>>> = Arc::new(Mutex::new(None));
    static ref USER_CACHE: Arc<Mutex<LruCache<String, models::user::CachedUserProfile>>> = 
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(1000).unwrap())));
}

// Initialize Redis connection pool (identical to auth-service)
async fn init_redis_pool() -> Result<RedisPool, Box<dyn std::error::Error>> {
    let redis_url = env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    
    let cfg = RedisConfig::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1))?;
    
    // Store in global state
    if let Ok(mut global_pool) = REDIS_POOL.lock() {
        *global_pool = Some(pool.clone());
    }
    
    Ok(pool)
}

// Get Redis connection from pool
async fn get_redis_connection() -> Result<deadpool_redis::Connection, Box<dyn std::error::Error>> {
    if let Ok(pool_guard) = REDIS_POOL.lock() {
        if let Some(pool) = pool_guard.as_ref() {
            return Ok(pool.get().await?);
        }
    }
    
    // Initialize pool if not exists
    let pool = init_redis_pool().await?;
    Ok(pool.get().await?)
}

// Initialize MongoDB connection pool (identical to auth-service pattern)
async fn init_mongodb_client() -> Result<Client, mongodb::error::Error> {
    let uri = env::var("MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://app_user:iSuperCoder_App_2025_Secure_Key_8e7d6c5b4a392817@database:27017/isupercoder?authSource=admin".to_string());
    
    let mut client_options = ClientOptions::parse(&uri).await?;
    
    // Phase 4 optimizations: Advanced connection pool tuning
    client_options.min_pool_size = Some(10);
    client_options.max_pool_size = Some(50);
    client_options.max_idle_time = Some(Duration::from_secs(600));
    client_options.connect_timeout = Some(Duration::from_secs(2));
    client_options.server_selection_timeout = Some(Duration::from_secs(5));
    
    let client = Client::with_options(client_options)?;
    
    // Store in global state
    if let Ok(mut global_client) = MONGODB_CLIENT.lock() {
        *global_client = Some(client.clone());
    }
    
    Ok(client)
}

// Get MongoDB database with connection pooling
pub async fn get_database() -> Result<Database, Box<dyn std::error::Error>> {
    if let Ok(client_guard) = MONGODB_CLIENT.lock() {
        if let Some(client) = client_guard.as_ref() {
            return Ok(client.database("isupercoder"));
        }
    }
    
    // Initialize client if not exists
    let client = init_mongodb_client().await?;
    Ok(client.database("isupercoder"))
}

// Optimized JSON serialization using simd-json (Phase 4 optimization)
pub fn optimize_json_response<T: Serialize>(data: &T) -> Result<Vec<u8>, String> {
    let mut buffer = Vec::with_capacity(1024);
    match simd_json::to_writer(&mut buffer, data) {
        Ok(_) => Ok(buffer),
        Err(_) => {
            // Fallback to serde_json if simd-json fails
            serde_json::to_vec(data).map_err(|e| e.to_string())
        }
    }
}

// Health check endpoint
async fn health() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "service": "user-service",
        "version": "1.0.0",
        "timestamp": chrono::Utc::now()
    })))
}

// Phase 4: Database index creation for user service optimization
async fn create_database_indexes() -> Result<(), Box<dyn std::error::Error>> {
    let db = get_database().await?;
    let users = db.collection::<mongodb::bson::Document>("users");
    
    log::info!("Creating user service database indexes for optimal performance...");
    
    // Email index (unique) - primary user lookup
    let email_index = mongodb::IndexModel::builder()
        .keys(mongodb::bson::doc! { "email": 1 })
        .options(
            mongodb::options::IndexOptions::builder()
                .unique(true)
                .name("email_unique_idx".to_string())
                .build()
        )
        .build();
    
    // Profile picture index (sparse) - for profile picture queries
    let profile_picture_index = mongodb::IndexModel::builder()
        .keys(mongodb::bson::doc! { "profilePicture": 1 })
        .options(
            mongodb::options::IndexOptions::builder()
                .sparse(true)
                .name("profile_picture_sparse_idx".to_string())
                .build()
        )
        .build();
    
    // Settings index - for user preferences queries
    let settings_index = mongodb::IndexModel::builder()
        .keys(mongodb::bson::doc! { "settings": 1 })
        .options(
            mongodb::options::IndexOptions::builder()
                .sparse(true)
                .name("settings_sparse_idx".to_string())
                .build()
        )
        .build();
    
    // Updated at index for cache invalidation
    let updated_at_index = mongodb::IndexModel::builder()
        .keys(mongodb::bson::doc! { "updatedAt": -1 })
        .options(
            mongodb::options::IndexOptions::builder()
                .name("updated_at_desc_idx".to_string())
                .build()
        )
        .build();
    
    // Create all indexes
    let indexes = vec![
        email_index,
        profile_picture_index,
        settings_index,
        updated_at_index,
    ];
    
    match users.create_indexes(indexes, None).await {
        Ok(result) => {
            log::info!("Successfully created {} user service indexes", result.index_names.len());
            for index_name in result.index_names {
                log::info!("  ✅ User service index created: {}", index_name);
            }
        }
        Err(e) => {
            if e.to_string().contains("already exists") {
                log::info!("User service indexes already exist (this is normal)");
            } else {
                log::warn!("Failed to create some user service indexes: {} (service will still work)", e);
            }
        }
    }
    
    Ok(())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    log::info!("Starting User Service on port 8081");

    // Initialize database connection pools at startup
    log::info!("Initializing MongoDB connection pool...");
    if let Err(e) = init_mongodb_client().await {
        log::error!("Failed to initialize MongoDB client: {}", e);
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "Database initialization failed"));
    }
    log::info!("MongoDB connection pool initialized successfully");

    // Initialize Redis connection pool at startup
    log::info!("Initializing Redis connection pool...");
    if let Err(e) = init_redis_pool().await {
        log::warn!("Failed to initialize Redis pool: {} - Redis caching disabled", e);
    } else {
        log::info!("Redis connection pool initialized successfully");
    }

    // Phase 4: Create database indexes for optimal performance
    log::info!("Setting up user service database indexes...");
    if let Err(e) = create_database_indexes().await {
        log::warn!("Failed to create user service indexes: {} - Performance may be reduced", e);
    } else {
        log::info!("User service database indexes configured successfully");
    }

    HttpServer::new(|| {
        App::new()
            .wrap(Logger::default())
            .route("/health", web::get().to(health))
            .service(
                web::scope("/api/users")
                    // TODO: Add rate limiting middleware (.wrap(api_rate_limit()))
                    .route("/profile", web::get().to(get_profile))
                    .route("/profile-picture", web::post().to(update_profile_picture))
                    .route("/avatar", web::delete().to(delete_avatar))
                    .route("/settings", web::get().to(get_settings))
                    .route("/settings", web::put().to(update_settings))
                    .route("/change-password", web::post().to(change_password))
                    .route("/roles", web::get().to(get_user_roles))
                    .route("/roles", web::put().to(update_user_role))
                    .route("/activity", web::get().to(get_user_activity))
                    .route("/export", web::get().to(export_user_data))
                    .route("/import", web::post().to(import_user_data))
            )
            .service(
                web::scope("/api/admin/users")
                    // TODO: Add rate limiting middleware (.wrap(admin_rate_limit()))
                    .route("", web::get().to(admin_search_users))
                    .route("/{id}", web::put().to(admin_update_user))
            )
    })
    .bind("0.0.0.0:8081")?
    .run()
    .await
}