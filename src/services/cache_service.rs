use redis::AsyncCommands;
use std::time::{SystemTime, UNIX_EPOCH};
use lru::LruCache;
use std::sync::{Arc, Mutex};
use std::num::NonZeroUsize;
use lazy_static::lazy_static;

use crate::models::user::{StandardizedUser, CachedUserProfile, SettingsResponse};

// Global LRU caches for optimal performance (Phase 4 optimization)
lazy_static! {
    static ref PROFILE_CACHE: Arc<Mutex<LruCache<String, CachedUserProfile>>> = 
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(500).unwrap())));
    static ref SETTINGS_CACHE: Arc<Mutex<LruCache<String, SettingsResponse>>> = 
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(300).unwrap())));
}

// Get Redis connection (defined in main.rs)
async fn get_redis_connection() -> Result<deadpool_redis::Connection, Box<dyn std::error::Error + Send + Sync>> {
    use crate::REDIS_POOL;

    // Clone the pool outside the lock scope to avoid holding MutexGuard across await
    let pool = {
        let pool_guard = REDIS_POOL.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
        pool_guard.as_ref().cloned()
    };

    match pool {
        Some(p) => Ok(p.get().await?),
        None => Err("Redis pool not initialized".into()),
    }
}

// Get cached user profile (LRU first, then Redis) - Phase 4 multi-layer caching
pub async fn get_cached_profile(cache_key: &str) -> Option<StandardizedUser> {
    // First check in-memory LRU cache
    if let Ok(mut cache) = PROFILE_CACHE.lock() {
        if let Some(cached_profile) = cache.get(cache_key) {
            let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            if current_time - cached_profile.cached_at < cached_profile.ttl {
                // Convert cached profile to StandardizedUser (using actual stored values)
                return Some(StandardizedUser {
                    _id: cached_profile.id.clone(),
                    id: cached_profile.id.clone(),
                    email: cached_profile.email.clone(),
                    name: cached_profile.name.clone(),
                    role: cached_profile.role.clone(),
                    is_active: cached_profile.is_active,
                    email_verified: cached_profile.email_verified,
                    created_at: cached_profile.created_at.clone(),
                    updated_at: cached_profile.updated_at.clone(),
                    phone: cached_profile.phone.clone(),
                    company: cached_profile.company.clone(),
                    department: cached_profile.department.clone(),
                    position: cached_profile.position.clone(),
                    username: None,
                    profile_picture: cached_profile.profile_picture.clone(),
                    use_gravatar: cached_profile.use_gravatar,
                    location: cached_profile.location.clone(),
                });
            } else {
                // Remove expired entry
                cache.pop(cache_key);
            }
        }
    }
    
    // Then check Redis with pooled connection
    if let Ok(mut conn) = get_redis_connection().await {
        if let Ok(cached_data) = conn.get::<_, String>(cache_key).await {
            if let Ok(cached_profile) = serde_json::from_str::<CachedUserProfile>(&cached_data) {
                let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                if current_time - cached_profile.cached_at < cached_profile.ttl {
                    // Store in LRU for even faster access next time
                    if let Ok(mut cache) = PROFILE_CACHE.lock() {
                        cache.put(cache_key.to_string(), cached_profile.clone());
                    }
                    
                    // Convert to StandardizedUser (using actual stored values)
                    return Some(StandardizedUser {
                        _id: cached_profile.id.clone(),
                        id: cached_profile.id.clone(),
                        email: cached_profile.email.clone(),
                        name: cached_profile.name.clone(),
                        role: cached_profile.role.clone(),
                        is_active: cached_profile.is_active,
                        email_verified: cached_profile.email_verified,
                        created_at: cached_profile.created_at.clone(),
                        updated_at: cached_profile.updated_at.clone(),
                        phone: cached_profile.phone.clone(),
                        company: cached_profile.company.clone(),
                        department: cached_profile.department.clone(),
                        position: cached_profile.position.clone(),
                        username: None,
                        profile_picture: cached_profile.profile_picture.clone(),
                        use_gravatar: cached_profile.use_gravatar,
                        location: cached_profile.location.clone(),
                    });
                }
            }
        }
    }
    
    None
}

// Cache user profile in Redis and LRU (Phase 4 multi-layer caching)
pub async fn cache_profile(cache_key: &str, user: &StandardizedUser, ttl: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cached_profile = CachedUserProfile {
        id: user.id.clone(),
        email: user.email.clone(),
        name: user.name.clone(),
        role: user.role.clone(),
        is_active: user.is_active,
        email_verified: user.email_verified,
        created_at: user.created_at.clone(),
        updated_at: user.updated_at.clone(),
        profile_picture: user.profile_picture.clone(),
        use_gravatar: user.use_gravatar,
        location: user.location.clone(),
        phone: user.phone.clone(),
        company: user.company.clone(),
        department: user.department.clone(),
        position: user.position.clone(),
        settings: None, // Settings cached separately
        cached_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        ttl,
    };
    
    // Cache in Redis
    if let Ok(mut conn) = get_redis_connection().await {
        let serialized = serde_json::to_string(&cached_profile)?;
        let _: () = conn.set_ex(cache_key, serialized, ttl).await?;
    }
    
    // Cache in LRU for fastest access
    if let Ok(mut cache) = PROFILE_CACHE.lock() {
        cache.put(cache_key.to_string(), cached_profile);
    }
    
    Ok(())
}

// Invalidate user profile cache
pub async fn invalidate_profile_cache(cache_key: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Remove from Redis
    if let Ok(mut conn) = get_redis_connection().await {
        let _: Result<i32, redis::RedisError> = conn.del(cache_key).await;
    }
    
    // Remove from LRU cache
    if let Ok(mut cache) = PROFILE_CACHE.lock() {
        cache.pop(cache_key);
    }
    
    Ok(())
}

// Get cached user settings (LRU first, then Redis) - Phase 4 multi-layer caching
pub async fn get_cached_settings(cache_key: &str) -> Option<SettingsResponse> {
    // First check in-memory LRU cache
    if let Ok(mut cache) = SETTINGS_CACHE.lock() {
        if let Some(cached_settings) = cache.get(cache_key) {
            return Some(cached_settings.clone());
        }
    }
    
    // Then check Redis with pooled connection
    if let Ok(mut conn) = get_redis_connection().await {
        if let Ok(cached_data) = conn.get::<_, String>(cache_key).await {
            if let Ok(cached_settings) = serde_json::from_str::<SettingsResponse>(&cached_data) {
                // Store in LRU for even faster access next time
                if let Ok(mut cache) = SETTINGS_CACHE.lock() {
                    cache.put(cache_key.to_string(), cached_settings.clone());
                }
                return Some(cached_settings);
            }
        }
    }
    
    None
}

// Cache user settings in Redis and LRU (Phase 4 multi-layer caching)
pub async fn cache_settings(cache_key: &str, settings: &SettingsResponse, ttl: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Cache in Redis
    if let Ok(mut conn) = get_redis_connection().await {
        let serialized = serde_json::to_string(settings)?;
        let _: () = conn.set_ex(cache_key, serialized, ttl).await?;
    }
    
    // Cache in LRU for fastest access
    if let Ok(mut cache) = SETTINGS_CACHE.lock() {
        cache.put(cache_key.to_string(), settings.clone());
    }
    
    Ok(())
}

// Invalidate user settings cache
pub async fn invalidate_settings_cache(cache_key: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Remove from Redis
    if let Ok(mut conn) = get_redis_connection().await {
        let _: Result<i32, redis::RedisError> = conn.del(cache_key).await;
    }
    
    // Remove from LRU cache
    if let Ok(mut cache) = SETTINGS_CACHE.lock() {
        cache.pop(cache_key);
    }
    
    Ok(())
}