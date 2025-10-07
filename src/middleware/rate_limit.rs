use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error, HttpResponse, Result,
};
use futures_util::future::{ready, Ready};
use redis::AsyncCommands;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use lru::LruCache;
use std::num::NonZeroUsize;
use lazy_static::lazy_static;

use crate::models::response::ErrorResponse;

// In-memory rate limiter for fallback when Redis is unavailable
lazy_static! {
    static ref RATE_LIMIT_CACHE: Arc<Mutex<LruCache<String, RateLimitEntry>>> = 
        Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(1000).unwrap())));
}

#[derive(Clone)]
struct RateLimitEntry {
    count: u32,
    window_start: u64,
}

// Rate limit configuration
#[derive(Clone)]
pub struct RateLimitConfig {
    pub requests_per_window: u32,
    pub window_seconds: u64,
    pub enabled: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_window: 100, // 100 requests per window
            window_seconds: 60,       // 1 minute window
            enabled: true,
        }
    }
}

pub struct RateLimit {
    config: RateLimitConfig,
}

impl RateLimit {
    pub fn new(config: RateLimitConfig) -> Self {
        Self { config }
    }
}

impl<S, B> Transform<S, ServiceRequest> for RateLimit
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + Clone,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = RateLimitMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RateLimitMiddleware {
            service,
            config: self.config.clone(),
        }))
    }
}

pub struct RateLimitMiddleware<S> {
    service: S,
    config: RateLimitConfig,
}

impl<S, B> Service<ServiceRequest> for RateLimitMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + Clone,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        if !self.config.enabled {
            let service = &self.service;
            return Box::pin(async move { service.call(req).await });
        }

        let config = self.config.clone();
        let service = self.service.clone();

        Box::pin(async move {
            // Extract client IP (prefer X-Forwarded-For if available)
            let client_ip = req
                .headers()
                .get("x-forwarded-for")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.split(',').next())
                .or_else(|| req.peer_addr().map(|addr| addr.ip().to_string().as_str()))
                .unwrap_or("unknown")
                .to_string();

            let rate_limit_key = format!("rate_limit:{}", client_ip);
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Try Redis first, fallback to in-memory cache
            let is_allowed = if let Ok(allowed) = check_rate_limit_redis(&rate_limit_key, &config, current_time).await {
                allowed
            } else {
                // Fallback to in-memory rate limiting
                check_rate_limit_memory(&rate_limit_key, &config, current_time)
            };

            if !is_allowed {
                let response = HttpResponse::TooManyRequests()
                    .json(ErrorResponse {
                        success: false,
                        error: format!(
                            "Rate limit exceeded. Maximum {} requests per {} seconds allowed.",
                            config.requests_per_window, config.window_seconds
                        ),
                    });
                
                return Ok(req.into_response(response));
            }

            // Rate limit passed, continue with request
            service.call(req).await
        })
    }
}

// Redis-based rate limiting using simple counter approach
async fn check_rate_limit_redis(
    key: &str,
    config: &RateLimitConfig,
    current_time: u64,
) -> Result<bool, Box<dyn std::error::Error>> {
    use crate::REDIS_POOL;
    
    if let Ok(pool_guard) = REDIS_POOL.lock() {
        if let Some(pool) = pool_guard.as_ref() {
            let mut conn = pool.get().await?;
            
            // Simple counter-based rate limiting
            let window_key = format!("{}:{}", key, current_time / config.window_seconds);
            
            // Get current count
            let count: i32 = conn.get(&window_key).await.unwrap_or(0);
            
            if count >= config.requests_per_window as i32 {
                return Ok(false); // Rate limit exceeded
            }
            
            // Increment counter
            let _: Result<i32, redis::RedisError> = conn.incr(&window_key, 1).await;
            
            // Set expiry on the key (expire after window + some buffer)
            let _: Result<bool, redis::RedisError> = conn.expire(&window_key, (config.window_seconds + 10) as i64).await;
            
            return Ok(true);
        }
    }
    
    Err("Redis not available".into())
}

// In-memory fallback rate limiting
fn check_rate_limit_memory(
    key: &str,
    config: &RateLimitConfig,
    current_time: u64,
) -> bool {
    if let Ok(mut cache) = RATE_LIMIT_CACHE.lock() {
        let window_start = current_time - config.window_seconds;
        
        if let Some(entry) = cache.get_mut(key) {
            // Check if we're still in the same window
            if entry.window_start >= window_start {
                if entry.count >= config.requests_per_window {
                    return false; // Rate limit exceeded
                }
                entry.count += 1;
            } else {
                // New window, reset counter
                entry.count = 1;
                entry.window_start = current_time;
            }
        } else {
            // First request from this client
            cache.put(key.to_string(), RateLimitEntry {
                count: 1,
                window_start: current_time,
            });
        }
        
        return true;
    }
    
    // If cache is locked, allow the request (fail open)
    true
}

// Pre-configured rate limiters for different endpoints
pub fn auth_rate_limit() -> RateLimit {
    RateLimit::new(RateLimitConfig {
        requests_per_window: 10, // Stricter for auth endpoints
        window_seconds: 60,
        enabled: true,
    })
}

pub fn api_rate_limit() -> RateLimit {
    RateLimit::new(RateLimitConfig {
        requests_per_window: 100, // Standard API rate limit
        window_seconds: 60,
        enabled: true,
    })
}

pub fn admin_rate_limit() -> RateLimit {
    RateLimit::new(RateLimitConfig {
        requests_per_window: 200, // Higher limit for admin operations
        window_seconds: 60,
        enabled: true,
    })
}