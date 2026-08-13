use lazy_static::lazy_static;
use lru::LruCache;
use redis::AsyncCommands;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::models::user::{CachedUserProfile, SettingsResponse, StandardizedUser};

// Global LRU caches for optimal performance (Phase 4 optimization)
lazy_static! {
    static ref PROFILE_CACHE: Arc<Mutex<LruCache<String, CachedUserProfile>>> =
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(500).unwrap())));
    static ref SETTINGS_CACHE: Arc<Mutex<LruCache<String, SettingsResponse>>> =
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(300).unwrap())));
}

// Get Redis connection (defined in main.rs)
async fn get_redis_connection(
) -> Result<deadpool_redis::Connection, Box<dyn std::error::Error + Send + Sync>> {
    use crate::REDIS_POOL;

    // Clone the pool outside the lock scope to avoid holding MutexGuard across await
    let pool = {
        let pool_guard = REDIS_POOL
            .lock()
            .map_err(|e| format!("Lock poisoned: {}", e))?;
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
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
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
                    last_login: None,
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
                let current_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
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
                        last_login: None,
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
pub async fn cache_profile(
    cache_key: &str,
    user: &StandardizedUser,
    ttl: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

/// Evict `key`, recovering the lock if a panic elsewhere poisoned it (#64).
///
/// The eviction used to sit behind `if let Ok(mut cache) = CACHE.lock()`, so a
/// poisoned lock skipped it while the caller was told the invalidation
/// succeeded. Reads consult this LRU before Redis, so a skipped eviction keeps
/// serving the previous value - and the cached profile carries `role` and
/// `is_active`, which `admin_update_user` and `update_user_role` change.
///
/// Recovering is safe for this value: it is a cache, and dropping an eviction
/// is strictly worse than acting on a map whose last write may have been
/// interrupted. Returns whether an entry was actually removed.
fn pop_recovering<V>(cache: &Mutex<LruCache<String, V>>, key: &str) -> bool {
    let mut guard = cache.lock().unwrap_or_else(|poisoned| {
        log::error!("cache lock poisoned; recovering to complete the eviction of {key}");
        poisoned.into_inner()
    });
    guard.pop(key).is_some()
}

// Invalidate user profile cache
pub async fn invalidate_profile_cache(
    cache_key: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // The LRU is cleared first and unconditionally: it is the layer reads
    // consult first, so a stale entry here outranks a stale entry in Redis.
    pop_recovering(&PROFILE_CACHE, cache_key);

    // Redis failures are reported rather than discarded. warn_if_cache_failed
    // already has the right message for this ("stale data will be served until
    // the entry expires"); it just never received an Err to print.
    let mut conn = get_redis_connection().await?;
    conn.del::<_, i32>(cache_key).await?;

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
pub async fn cache_settings(
    cache_key: &str,
    settings: &SettingsResponse,
    ttl: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
pub async fn invalidate_settings_cache(
    cache_key: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pop_recovering(&SETTINGS_CACHE, cache_key);

    let mut conn = get_redis_connection().await?;
    conn.del::<_, i32>(cache_key).await?;

    Ok(())
}

#[cfg(test)]
mod invalidation_tests {
    //! Both invalidation functions returned Ok(()) whatever happened (#64), so
    //! `warn_if_cache_failed`'s `stale: true` branch - written for exactly this
    //! case, and correct about the consequence - could never be reached.
    //!
    //! The LRU caches are `lazy_static` globals shared by every test in this
    //! binary, and poisoning a global Mutex poisons it for the whole run. So
    //! the recovery is tested through `pop_recovering` against a local mutex
    //! rather than by poisoning PROFILE_CACHE itself.

    use super::*;
    use std::sync::Arc;

    fn poisoned_cache() -> Arc<Mutex<LruCache<String, u32>>> {
        let cache = Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(4).unwrap())));
        cache.lock().unwrap().put("victim".to_string(), 1);

        let clone = Arc::clone(&cache);
        let _ = std::thread::spawn(move || {
            let _guard = clone.lock().unwrap();
            panic!("poisoning the cache lock on purpose");
        })
        .join();

        assert!(cache.lock().is_err(), "expected a poisoned lock");
        cache
    }

    /// An eviction that is silently skipped leaves the stale role or
    /// is_active being served, because reads consult the LRU first.
    #[test]
    fn an_eviction_completes_even_when_the_lock_is_poisoned() {
        let cache = poisoned_cache();
        let evicted = pop_recovering(&cache, "victim");

        assert!(evicted, "the entry should have been present and removed");
        assert!(
            cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .peek("victim")
                .is_none(),
            "the stale entry survived the eviction"
        );
    }

    #[test]
    fn evicting_an_absent_key_reports_that_nothing_was_removed() {
        let cache = poisoned_cache();
        assert!(!pop_recovering(&cache, "never-cached"));
    }

    /// The helper only matters if the invalidation paths use it, and the
    /// Redis result only matters if it is not discarded.
    #[test]
    fn invalidation_neither_skips_the_lru_nor_discards_the_redis_result() {
        let source = include_str!("cache_service.rs");
        let body = source
            .split("#[cfg(test)]")
            .next()
            .expect("the file has a non-test portion");

        // Scoped to the two invalidation bodies. The read and populate paths
        // between them use `if let Ok` on purpose: a poisoned lock there means
        // a miss, which falls through to the database and is the safe
        // direction. The rule is about evictions, not about locks in general.
        let body_of = |name: &str| -> String {
            let start = body.find(name).unwrap_or_else(|| panic!("{name} exists"));
            let rest = &body[start..];
            let end = rest.find("\n}\n").expect("the function terminates");
            rest[..end].to_string()
        };

        for name in [
            "pub async fn invalidate_profile_cache",
            "pub async fn invalidate_settings_cache",
        ] {
            let fn_body = body_of(name);
            assert!(
                !fn_body.contains("if let Ok(mut cache) ="),
                "{name} still skips the LRU eviction when the lock is poisoned"
            );
            assert!(
                !fn_body.contains("let _: Result<i32, redis::RedisError>"),
                "{name} still discards the Redis result, so the caller cannot warn"
            );
            assert!(
                fn_body.contains("pop_recovering"),
                "{name} does not go through the recovering eviction"
            );
        }
    }
}
