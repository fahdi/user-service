use actix_web::{web, App, HttpServer, middleware::Logger};
use std::env;
use mongodb::bson::Document;

use user_service::{
    health, init_mongodb_client, init_redis_pool,
    handlers::di_handlers::{
        get_profile, update_profile_picture, get_settings, update_settings, change_password,
        delete_avatar, admin_search_users, admin_update_user, get_user_roles, update_user_role,
        get_user_activity, export_user_data, import_user_data,
    },
};

// Phase 4: Database index creation for user service optimization
async fn create_database_indexes() -> Result<(), Box<dyn std::error::Error>> {
    let db = user_service::get_database().await?;
    let users = db.collection::<Document>("users");

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
                log::info!("  User service index created: {}", index_name);
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

    let port = env::var("PORT").unwrap_or_else(|_| "8081".to_string());
    log::info!("Starting User Service on port {}", port);

    // Initialize database connection pools at startup
    log::info!("Initializing MongoDB connection pool...");
    if let Err(e) = init_mongodb_client().await {
        log::error!("Failed to initialize MongoDB client: {}", e);
        return Err(std::io::Error::other("Database initialization failed"));
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

    // Build DI-wired AppState with concrete implementations
    let app_state = web::Data::new(user_service::impls::build_app_state());

    HttpServer::new(move || {
        App::new()
            .wrap(Logger::default())
            .app_data(app_state.clone())
            .route("/health", web::get().to(health))
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
                    .route("/import", web::post().to(import_user_data))
            )
            .service(
                web::scope("/api/admin/users")
                    .route("", web::get().to(admin_search_users))
                    .route("/{id}", web::put().to(admin_update_user))
            )
    })
    .bind(format!("0.0.0.0:{}", port))?
    .run()
    .await
}
