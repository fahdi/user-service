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

/// The production route table, defined in exactly one place.
///
/// It used to be built inline in `main.rs`, where nothing could mount it: the
/// tests rebuilt fragments route by route, so they only covered the routes
/// someone remembered to write a test for (#44).
///
/// Authentication here is a per-handler convention rather than a middleware -
/// each handler calls `state.auth.extract_claims(&req)`. Sharing the table is
/// what lets a test mount all of it with a rejecting extractor and require
/// every protected route to answer 401, so a handler that forgets fails on the
/// day it is written.
///
/// `/health` sits outside that convention deliberately: the container probe
/// sends no credentials (#42).
pub fn configure_routes(cfg: &mut actix_web::web::ServiceConfig) {
    use actix_web::web;
    use handlers::di_handlers::*;

    cfg.route("/health", web::get().to(health))
        .service(
            web::scope("/api/users")
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
                .route("/import", web::post().to(import_user_data)),
        )
        .service(
            web::scope("/api/admin/users")
                .route("", web::get().to(admin_search_users))
                .route("", web::post().to(admin_create_user))
                .route("/{id}", web::put().to(admin_update_user)),
        );
}

#[cfg(test)]
mod routed_handlers_authenticate {
    //! Every routed handler must authenticate itself.
    //!
    //! Neither `web::scope("/api/users")` nor `web::scope("/api/admin/users")`
    //! is wrapped, so there is no middleware enforcing anything. Each handler
    //! calls `extract_claims_from_request`, and the admin ones additionally
    //! call `is_admin`. All 13 protected handlers do so correctly today, which
    //! is why this is written now rather than after the fourteenth (#57).
    //!
    //! Source-level because the property is the presence of a call inside a
    //! handler, and driving it behaviourally would need a live Mongo. Comments
    //! are stripped and needles assembled at runtime, so neither this doc nor
    //! the assertions below can satisfy the check.
    use std::collections::HashMap;

    /// Public by design: the container probe sends no credentials.
    const PUBLIC_BY_DESIGN: [&str; 1] = ["health"];

    fn without_comments(source: &str) -> String {
        source
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn read(relative: &str) -> String {
        let path = format!("{}/src/{}", env!("CARGO_MANIFEST_DIR"), relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path} must be readable: {e}"))
    }

    /// `(handler name, is it under the admin scope)` for every routed handler.
    fn routed_handlers() -> Vec<(String, bool)> {
        let lib = without_comments(&read("lib.rs"));
        let admin_scope_at = lib.find("/api/admin/users");

        let mut out = Vec::new();
        let mut rest = lib.as_str();
        let mut consumed = 0usize;

        while let Some(at) = rest.find(".route(") {
            // A fixed window rather than "up to the first `)`": the first
            // paren in `.route("/x", web::get().to(handler))` closes
            // `web::get()`, so cutting there finds no `.to(` at all. The
            // non-empty assertion below caught that immediately.
            let after = &rest[at..];
            let call = &after[..after.len().min(160)];

            if let Some(to_at) = call.find(".to(") {
                let name: String = call[to_at + 4..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    let position = consumed + at;
                    let is_admin_route = admin_scope_at.is_some_and(|s| position > s);
                    out.push((name, is_admin_route));
                }
            }
            consumed += at + 7;
            rest = &rest[at + 7..];
        }
        out
    }

    fn handler_bodies() -> HashMap<String, String> {
        let source = without_comments(&read("handlers/di_handlers.rs"));
        let marker = "pub async fn ";
        let mut bodies = HashMap::new();

        let positions: Vec<usize> = source.match_indices(marker).map(|(i, _)| i).collect();
        for (index, &start) in positions.iter().enumerate() {
            let end = positions.get(index + 1).copied().unwrap_or(source.len());
            let name: String = source[start + marker.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            bodies.insert(name, source[start..end].to_string());
        }
        bodies
    }

    #[test]
    fn every_protected_handler_extracts_claims() {
        let routed = routed_handlers();

        // A route table that parsed to nothing would make everything below
        // vacuous, which is the shape this repository keeps finding.
        assert!(
            routed.len() >= 12,
            "only parsed {} routed handlers; the route table shape changed",
            routed.len()
        );

        let bodies = handler_bodies();
        // The handlers call the trait method `state.auth.extract_claims(&req)`,
        // not the free `extract_claims_from_request`. An earlier draft of this
        // guard used the latter and flagged all 13 correct handlers.
        let needle = format!("extract{}", "_claims");

        let unprotected: Vec<&String> = routed
            .iter()
            .filter(|(name, _)| !PUBLIC_BY_DESIGN.contains(&name.as_str()))
            .filter_map(|(name, _)| bodies.get(name).map(|body| (name, body)))
            .filter(|(_, body)| !body.contains(&needle))
            .map(|(name, _)| name)
            .collect();

        assert!(
            unprotected.is_empty(),
            "routed handlers that never extract claims: {unprotected:?}. \
             Neither scope is wrapped, so a handler that does not check is open."
        );
    }

    #[test]
    fn every_admin_handler_checks_the_role() {
        let routed = routed_handlers();
        let bodies = handler_bodies();
        let needle = format!("is{}", "_admin");

        let admin_routes: Vec<&String> = routed
            .iter()
            .filter(|(_, is_admin_route)| *is_admin_route)
            .map(|(name, _)| name)
            .collect();

        // Same reason as above: an empty set would pass while checking nothing,
        // and the admin scope is exactly where that matters most.
        assert!(
            !admin_routes.is_empty(),
            "found no routes under the admin scope; the scope layout changed"
        );

        let unchecked: Vec<&&String> = admin_routes
            .iter()
            .filter_map(|name| bodies.get(name.as_str()).map(|body| (name, body)))
            .filter(|(_, body)| !body.contains(&needle))
            .map(|(name, _)| name)
            .collect();

        assert!(
            unchecked.is_empty(),
            "admin-scope handlers with no role check: {unchecked:?}. The scope \
             is not wrapped, so membership alone grants nothing."
        );
    }
}
