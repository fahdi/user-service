use redis::AsyncCommands;
use std::time::{SystemTime, UNIX_EPOCH};
use lru::LruCache;
use std::sync::{Arc, Mutex};
use std::num::NonZeroUsize;
use lazy_static::lazy_static;

use crate::models::user::{StandardizedUser, CachedUserProfile};

// Global LRU cache for user profiles (Phase 4 optimization)
lazy_static! {
    static ref PROFILE_CACHE: Arc<Mutex<LruCache<String, CachedUserProfile>>> = 
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(500).unwrap())));
}

// Get Redis connection (defined in main.rs)
async fn get_redis_connection() -> Result<deadpool_redis::Connection, Box<dyn std::error::Error>> {
    use crate::REDIS_POOL;
    
    if let Ok(pool_guard) = REDIS_POOL.lock() {
        if let Some(pool) = pool_guard.as_ref() {
            return Ok(pool.get().await?);
        }
    }
    
    Err("Redis pool not initialized".into())
}

// Get cached user profile (LRU first, then Redis) - Phase 4 multi-layer caching
pub async fn get_cached_profile(cache_key: &str) -> Option<StandardizedUser> {
    // First check in-memory LRU cache
    if let Ok(mut cache) = PROFILE_CACHE.lock() {
        if let Some(cached_profile) = cache.get(cache_key) {
            let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            if current_time - cached_profile.cached_at < cached_profile.ttl {
                // Convert cached profile to StandardizedUser
                return Some(StandardizedUser {
                    _id: cached_profile.id.clone(),
                    id: cached_profile.id.clone(),
                    email: cached_profile.email.clone(),
                    name: cached_profile.name.clone(),
                    role: cached_profile.role.clone(),
                    is_active: true, // Assume active from cache
                    email_verified: true, // Assume verified from cache
                    created_at: chrono::Utc::now().to_rfc3339(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
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
                    
                    // Convert to StandardizedUser
                    return Some(StandardizedUser {
                        _id: cached_profile.id.clone(),
                        id: cached_profile.id.clone(),
                        email: cached_profile.email.clone(),
                        name: cached_profile.name.clone(),
                        role: cached_profile.role.clone(),
                        is_active: true,
                        email_verified: true,
                        created_at: chrono::Utc::now().to_rfc3339(),
                        updated_at: chrono::Utc::now().to_rfc3339(),
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
pub async fn cache_profile(cache_key: &str, user: &StandardizedUser, ttl: u64) -> Result<(), Box<dyn std::error::Error>> {
    let cached_profile = CachedUserProfile {
        id: user.id.clone(),
        email: user.email.clone(),
        name: user.name.clone(),
        role: user.role.clone(),
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
pub async fn invalidate_profile_cache(cache_key: &str) -> Result<(), Box<dyn std::error::Error>> {
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