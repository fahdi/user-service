pub mod handlers;
pub mod impls;
pub mod middleware;
pub mod models;
pub mod services;
pub mod traits;
pub mod utils;

use actix_web::{HttpResponse, Result};
use deadpool_redis::{Config as RedisConfig, Pool as RedisPool, Runtime};
use lazy_static::lazy_static;
use lru::LruCache;
use mongodb::{options::ClientOptions, Client, Database};
use serde::Serialize;
use std::env;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Global optimized pools and caches (following auth-service patterns)
lazy_static! {
    pub static ref REDIS_POOL: Arc<Mutex<Option<RedisPool>>> = Arc::new(Mutex::new(None));
    pub static ref MONGODB_CLIENT: Arc<Mutex<Option<Client>>> = Arc::new(Mutex::new(None));
    pub static ref USER_CACHE: Arc<Mutex<LruCache<String, models::user::CachedUserProfile>>> =
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(1000).unwrap())));
}

// Initialize Redis connection pool (identical to auth-service)
pub async fn init_redis_pool() -> std::result::Result<RedisPool, Box<dyn std::error::Error>> {
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let cfg = RedisConfig::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1))?;

    // Store in global state
    if let Ok(mut global_pool) = REDIS_POOL.lock() {
        *global_pool = Some(pool.clone());
    }

    Ok(pool)
}

// Initialize MongoDB connection pool (identical to auth-service pattern)
pub async fn init_mongodb_client() -> std::result::Result<Client, mongodb::error::Error> {
    let uri = env::var("MONGODB_URI").expect(
        "MONGODB_URI environment variable must be set — refusing to use hardcoded credentials",
    );

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
pub async fn get_database() -> std::result::Result<Database, Box<dyn std::error::Error>> {
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
pub fn optimize_json_response<T: Serialize>(data: &T) -> std::result::Result<Vec<u8>, String> {
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
/// Report whether this service can actually serve.
///
/// It took no state, so it was structurally incapable of observing anything and
/// answered 200 however broken the service was (#42). Docker probes it with
/// `curl -f` and `monitor-containers.sh` does the same, and both read **only
/// the status code**, so an unhealthy report has to be a 503: a 200 carrying
/// `"status": "unhealthy"` is invisible to either.
///
/// MongoDB is fatal. Profiles, settings, roles and activity all live there.
///
/// The cache is not, and unlike utilities-forms#36 and file-management#46 that
/// is not because it goes unused: this service really does read
/// `get_cached_profile` and `get_cached_settings`. It is excluded because
/// `CacheService` returns `Option` and `()` rather than `Result`, so a cache
/// failure is **structurally indistinguishable from a miss** and falls through
/// to MongoDB. A Redis outage costs latency, not correctness, and treating it
/// as fatal would turn that into a restart loop: `monitor-containers.sh`
/// restarts after five consecutive failures, and restarting cannot fix an
/// external Redis.
pub async fn health(state: actix_web::web::Data<crate::traits::AppState>) -> Result<HttpResponse> {
    let database_reachable = state.repo.health_check().await.is_ok();

    let body = serde_json::json!({
        "status": if database_reachable { "healthy" } else { "unhealthy" },
        "service": "user-service",
        "version": "1.0.0",
        "database": if database_reachable { "ok" } else { "unavailable" },
        "timestamp": chrono::Utc::now()
    });

    if database_reachable {
        Ok(HttpResponse::Ok().json(body))
    } else {
        Ok(HttpResponse::ServiceUnavailable().json(body))
    }
}
