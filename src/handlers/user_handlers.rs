use actix_web::{web, HttpRequest, HttpResponse, Result};
use actix_multipart::Multipart;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, oid::ObjectId, DateTime};
use bcrypt::{hash, verify as bcrypt_verify};

use crate::models::user::{
    UserProfileResponse, StandardizedUser, SettingsResponse, UserSettings, 
    SettingsUpdateRequest, ProfilePictureResponse, UserBasicInfo
};
use crate::models::response::{ErrorResponse, SuccessResponse};
use crate::services::cache_service::{get_cached_profile, cache_profile, invalidate_profile_cache};
use crate::services::google_drive_service::upload_profile_picture;
use crate::middleware::auth::extract_claims_from_request;
use crate::get_database;

// Get user profile endpoint (matches Node.js /api/users/profile exactly)
pub async fn get_profile(req: HttpRequest, query: web::Query<serde_json::Value>) -> Result<HttpResponse> {
    // Extract JWT claims from request
    let claims = match extract_claims_from_request(&req) {
        Ok(claims) => claims,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let email = query.get("email").and_then(|v| v.as_str());
    let user_id = query.get("userId").and_then(|v| v.as_str());

    // Determine target user (admin can lookup any user, regular users only themselves)
    let target_user_id = if (user_id.is_some() || email.is_some()) && 
                            (claims.role == "admin" || claims.role_type == "admin") {
        // Admin lookup by userId (preferred) or email (legacy)
        user_id.unwrap_or_else(|| email.unwrap_or(&claims.user_id)).to_string()
    } else {
        // Regular user can only see their own profile
        claims.user_id.clone()
    };

    // Try cache first (15-minute cache like Node.js)
    let cache_key = format!("user:profile:{}", target_user_id);
    
    if let Some(cached_profile) = get_cached_profile(&cache_key).await {
        log::info!("📦 Cache HIT for user profile: {}", target_user_id);
        return Ok(HttpResponse::Ok().json(UserProfileResponse {
            success: true,
            user: Some(cached_profile),
            message: None,
        }));
    }

    log::info!("🔍 Cache MISS for user profile: {} - fetching from database", target_user_id);

    // Connect to MongoDB
    let db = match get_database().await {
        Ok(db) => db,
        Err(e) => {
            log::error!("Database connection failed: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Database connection failed".to_string(),
            }));
        }
    };

    let users_collection = db.collection::<mongodb::bson::Document>("users");

    // Find user (admin lookup logic matches Node.js exactly)
    let user = if (user_id.is_some() || email.is_some()) && 
                  (claims.role == "admin" || claims.role_type == "admin") {
        
        let filter = if let Some(uid) = user_id {
            match ObjectId::parse_str(uid) {
                Ok(oid) => doc! { "_id": oid },
                Err(_) => {
                    return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                        success: false,
                        error: "Invalid user ID format".to_string(),
                    }));
                }
            }
        } else if let Some(em) = email {
            doc! { "email": em }
        } else {
            match ObjectId::parse_str(&claims.user_id) {
                Ok(oid) => doc! { "_id": oid },
                Err(_) => {
                    return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                        success: false,
                        error: "Invalid user ID format".to_string(),
                    }));
                }
            }
        };

        users_collection.find_one(
            filter,
            mongodb::options::FindOneOptions::builder()
                .projection(doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 })
                .build()
        ).await.unwrap_or(None)
    } else {
        // Regular user can only see their own profile
        match ObjectId::parse_str(&claims.user_id) {
            Ok(oid) => {
                users_collection.find_one(
                    doc! { "_id": oid },
                    mongodb::options::FindOneOptions::builder()
                        .projection(doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 })
                        .build()
                ).await.unwrap_or(None)
            }
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        }
    };

    let user = match user {
        Some(u) => u,
        None => {
            return Ok(HttpResponse::NotFound().json(ErrorResponse {
                success: false,
                error: "User not found".to_string(),
            }));
        }
    };

    // Transform to standardized format (following UserUtils.fromDatabase from Node.js)
    let user_id_str = user.get_object_id("_id").unwrap().to_hex();
    let standardized_user = StandardizedUser {
        _id: user_id_str.clone(),
        id: user_id_str.clone(),
        email: user.get_str("email").unwrap_or("").to_string(),
        name: user.get_str("name").unwrap_or("").to_string(),
        role: user.get_str("role").unwrap_or("customer").to_string(),
        is_active: user.get_bool("isActive").unwrap_or(true),
        email_verified: user.get_bool("emailVerified").unwrap_or(false),
        created_at: user.get_datetime("createdAt")
            .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
            .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
        updated_at: user.get_datetime("updatedAt")
            .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
            .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
        phone: user.get_str("phone").ok().map(|s| s.to_string()),
        company: user.get_str("company").ok().map(|s| s.to_string()),
        department: user.get_str("department").ok().map(|s| s.to_string()),
        position: user.get_str("position").ok().map(|s| s.to_string()),
        username: user.get_str("username").ok().map(|s| s.to_string()),
        profile_picture: user.get_str("profilePicture").ok().map(|s| s.to_string()),
        use_gravatar: user.get_bool("useGravatar").ok(),
        location: user.get_str("location").ok().map(|s| s.to_string()),
    };

    // Cache the result for 15 minutes (900 seconds) like Node.js
    let _ = cache_profile(&cache_key, &standardized_user, 900).await;
    log::info!("💾 Cached user profile: {} for 15 minutes", target_user_id);

    Ok(HttpResponse::Ok().json(UserProfileResponse {
        success: true,
        user: Some(standardized_user),
        message: None,
    }))
}

// Get user settings endpoint (matches Node.js /api/users/settings GET exactly)
pub async fn get_settings(req: HttpRequest) -> Result<HttpResponse> {
    // Extract JWT claims from request
    let claims = match extract_claims_from_request(&req) {
        Ok(claims) => claims,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    // Generate cache key for user settings
    let cache_key = format!("user:settings:{}", claims.user_id);
    
    // Try cache first (30-minute cache like Node.js)
    if let Some(cached_settings) = get_cached_settings(&cache_key).await {
        log::info!("📦 Cache HIT for user settings: {}", claims.user_id);
        return Ok(HttpResponse::Ok().json(cached_settings));
    }
    
    log::info!("🔍 Cache MISS for user settings: {} - fetching from database", claims.user_id);

    // Connect to MongoDB
    let db = match get_database().await {
        Ok(db) => db,
        Err(e) => {
            log::error!("Database connection failed: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Database connection failed".to_string(),
            }));
        }
    };

    let users_collection = db.collection::<mongodb::bson::Document>("users");

    // Get user with settings
    let user = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.find_one(
                doc! { "_id": oid },
                mongodb::options::FindOneOptions::builder()
                    .projection(doc! { 
                        "settings": 1, 
                        "email": 1, 
                        "name": 1, 
                        "role": 1, 
                        "profilePicture": 1, 
                        "useGravatar": 1, 
                        "location": 1 
                    })
                    .build()
            ).await.unwrap_or(None)
        }
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    let user = match user {
        Some(u) => u,
        None => {
            return Ok(HttpResponse::NotFound().json(ErrorResponse {
                success: false,
                error: "User not found".to_string(),
            }));
        }
    };

    // Return default settings if none exist (matches Node.js)
    let settings = if let Ok(settings_doc) = user.get_document("settings") {
        serde_json::from_str(&settings_doc.to_string()).unwrap_or_else(|_| UserSettings::default())
    } else {
        UserSettings::default()
    };

    let mut response_settings = settings;
    response_settings.user = Some(UserBasicInfo {
        _id: user.get_object_id("_id").unwrap().to_hex(),
        email: user.get_str("email").unwrap_or("").to_string(),
        name: user.get_str("name").unwrap_or("").to_string(),
        role: user.get_str("role").unwrap_or("customer").to_string(),
        profile_picture: user.get_str("profilePicture").ok().map(|s| s.to_string()),
        use_gravatar: user.get_bool("useGravatar").ok(),
        location: user.get_str("location").ok().map(|s| s.to_string()),
    });

    let response_data = SettingsResponse {
        success: true,
        settings: Some(response_settings.clone()),
        message: None,
    };

    // Cache the result for 30 minutes (1800 seconds) like Node.js
    let _ = cache_settings(&cache_key, &response_data, 1800).await;
    log::info!("💾 Cached user settings: {} for 30 minutes", claims.user_id);

    Ok(HttpResponse::Ok().json(response_data))
}

// Update user settings endpoint (matches Node.js /api/users/settings PUT exactly)
pub async fn update_settings(req: HttpRequest, body: web::Json<SettingsUpdateRequest>) -> Result<HttpResponse> {
    // Extract JWT claims from request
    let claims = match extract_claims_from_request(&req) {
        Ok(claims) => claims,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    // Connect to MongoDB
    let db = match get_database().await {
        Ok(db) => db,
        Err(e) => {
            log::error!("Database connection failed: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Database connection failed".to_string(),
            }));
        }
    };

    let users_collection = db.collection::<mongodb::bson::Document>("users");

    // Handle account changes (email/password) if provided
    if let Some(account_changes) = &body.account_changes {
        // Get current user to verify password
        let current_user = match ObjectId::parse_str(&claims.user_id) {
            Ok(oid) => {
                users_collection.find_one(
                    doc! { "_id": oid },
                    mongodb::options::FindOneOptions::builder()
                        .projection(doc! { "password": 1, "email": 1 })
                        .build()
                ).await.unwrap_or(None)
            }
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        let current_user = match current_user {
            Some(u) => u,
            None => {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        };

        // Verify current password
        let stored_password = current_user.get_str("password").unwrap_or("");
        if !bcrypt_verify(&account_changes.current_password, stored_password).unwrap_or(false) {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Current password is incorrect".to_string(),
            }));
        }

        // Check if email is already taken by another user
        if let Some(new_email) = &account_changes.new_email {
            let current_email = current_user.get_str("email").unwrap_or("");
            if new_email != current_email {
                let email_exists = users_collection.find_one(
                    doc! { 
                        "email": new_email.to_lowercase(),
                        "_id": { "$ne": current_user.get_object_id("_id").unwrap() }
                    },
                    None
                ).await.unwrap_or(None);
                
                if email_exists.is_some() {
                    return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                        success: false,
                        error: "Email address is already in use".to_string(),
                    }));
                }
            }
        }
    }

    // Prepare update document
    let mut update_doc = doc! {
        "settings": mongodb::bson::to_bson(&body.settings).unwrap(),
        "updatedAt": DateTime::now()
    };

    // Update user name if provided in settings
    if let Some(user_info) = &body.settings.user {
        update_doc.insert("name", &user_info.name);
        
        if let Some(location) = &user_info.location {
            update_doc.insert("location", location);
        }
        
        if let Some(use_gravatar) = user_info.use_gravatar {
            update_doc.insert("useGravatar", use_gravatar);
        }
    }

    // Add account changes to update
    if let Some(account_changes) = &body.account_changes {
        if let Some(new_email) = &account_changes.new_email {
            update_doc.insert("email", new_email.to_lowercase());
        }
        if let Some(new_password) = &account_changes.new_password {
            let password_hash = hash(new_password, 12).unwrap();
            update_doc.insert("password", password_hash);
        }
    }

    // Update user settings
    let result = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.update_one(
                doc! { "_id": oid },
                doc! { "$set": update_doc },
                None
            ).await
        }
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match result {
        Ok(update_result) => {
            if update_result.matched_count == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(e) => {
            log::error!("Failed to update user settings: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update settings".to_string(),
            }));
        }
    }

    // Invalidate cached settings for this user
    let cache_key = format!("user:settings:{}", claims.user_id);
    let _ = invalidate_settings_cache(&cache_key).await;
    log::info!("🗑️ Invalidated settings cache for user: {}", claims.user_id);

    // Build success message (matches Node.js logic)
    let mut success_message = "Settings updated successfully".to_string();
    if let Some(account_changes) = &body.account_changes {
        let mut changes = Vec::new();
        if account_changes.new_email.is_some() {
            changes.push("email");
        }
        if account_changes.new_password.is_some() {
            changes.push("password");
        }
        if !changes.is_empty() {
            success_message.push_str(&format!(". {} updated.", changes.join(" and ")));
        }
    }

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: success_message,
    }))
}

// Update profile picture endpoint (matches Node.js /api/users/profile-picture exactly)
pub async fn update_profile_picture(req: HttpRequest, mut payload: Multipart) -> Result<HttpResponse> {
    // Extract JWT claims from request
    let claims = match extract_claims_from_request(&req) {
        Ok(claims) => claims,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    // Process multipart upload (similar to Node.js multer logic)
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut content_type: Option<String> = None;

    while let Some(mut field) = payload.try_next().await.unwrap_or(None) {
        let field_name = field.name().unwrap_or("unknown").to_string();
        
        if field_name == "profilePicture" {
            let mut data = Vec::new();
            
            // Read file data
            while let Some(chunk) = field.try_next().await.unwrap_or(None) {
                data.extend_from_slice(&chunk);
            }
            
            // Validate file size (5MB max, same as Node.js)
            if data.len() > 5 * 1024 * 1024 {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "File size too large. Maximum size is 5MB.".to_string(),
                }));
            }

            // Get content disposition for filename
            if let Some(content_disposition) = field.content_disposition() {
                file_name = content_disposition.get_filename().map(|f| f.to_string());
            }
            
            content_type = field.content_type().map(|ct| ct.to_string());
            file_data = Some(data);
            break;
        }
    }

    let (file_data, file_name) = match (file_data, file_name) {
        (Some(data), Some(name)) => (data, name),
        _ => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "No profile picture uploaded".to_string(),
            }));
        }
    };

    // Validate content type (only images allowed, same as Node.js)
    if let Some(ct) = &content_type {
        if !ct.starts_with("image/") {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Only image files are allowed".to_string(),
            }));
        }
    }

    log::info!("File received: {} {} bytes", file_name, file_data.len());

    // Connect to MongoDB
    let db = match get_database().await {
        Ok(db) => db,
        Err(e) => {
            log::error!("Database connection failed: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Database connection failed".to_string(),
            }));
        }
    };

    let users_collection = db.collection::<mongodb::bson::Document>("users");

    // Get user email for Google Drive folder reference
    let user = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.find_one(
                doc! { "_id": oid },
                mongodb::options::FindOneOptions::builder()
                    .projection(doc! { "email": 1 })
                    .build()
            ).await.unwrap_or(None)
        }
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    let user = match user {
        Some(u) => u,
        None => {
            return Ok(HttpResponse::NotFound().json(ErrorResponse {
                success: false,
                error: "User not found".to_string(),
            }));
        }
    };

    // Upload profile picture to Google Drive (same logic as Node.js)
    let profile_picture_url = match upload_profile_picture(
        &claims.user_id,
        user.get_str("email").unwrap_or(""),
        file_data,
        &file_name
    ).await {
        Ok(url) => url,
        Err(e) => {
            log::error!("Profile picture upload error: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to upload profile picture".to_string(),
            }));
        }
    };

    // Update user's profile picture
    let result = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.update_one(
                doc! { "_id": oid },
                doc! { 
                    "$set": { 
                        "profilePicture": &profile_picture_url,
                        "useGravatar": false, // User uploaded a custom picture
                        "updatedAt": DateTime::now()
                    } 
                },
                None
            ).await
        }
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match result {
        Ok(update_result) => {
            if update_result.matched_count == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(e) => {
            log::error!("Failed to update profile picture: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update profile picture".to_string(),
            }));
        }
    }

    // Bust cache for this user since they uploaded a new photo
    let cache_key = format!("user:profile:{}", claims.user_id);
    let _ = invalidate_profile_cache(&cache_key).await;
    log::info!("🗑️ Invalidated profile cache for user: {}", claims.user_id);

    Ok(HttpResponse::Ok().json(ProfilePictureResponse {
        success: true,
        message: "Profile picture updated successfully".to_string(),
        profile_picture: Some(profile_picture_url),
    }))
}

// Helper functions for cache operations (to be implemented in cache service)
async fn get_cached_settings(_cache_key: &str) -> Option<SettingsResponse> {
    // Implementation will be in cache service
    None
}

async fn cache_settings(_cache_key: &str, _data: &SettingsResponse, _ttl: u64) -> Result<(), Box<dyn std::error::Error>> {
    // Implementation will be in cache service
    Ok(())
}

async fn invalidate_settings_cache(_cache_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Implementation will be in cache service  
    Ok(())
}