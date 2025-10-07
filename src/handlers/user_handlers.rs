use actix_web::{web, HttpRequest, HttpResponse, Result};
use actix_multipart::Multipart;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, oid::ObjectId, DateTime};
use bcrypt::{hash, verify as bcrypt_verify};
use validator::Validate;

use crate::models::user::{
    UserProfileResponse, StandardizedUser, SettingsResponse, UserSettings,
    SettingsUpdateRequest, ProfilePictureResponse, UserBasicInfo,
    PasswordChangeRequest, PasswordChangeResponse, UserSearchQuery,
    UserSearchResponse, PaginationInfo, AdminUserUpdateRequest,
    UserRolesResponse, RoleInfo, RoleUpdateRequest,
    UserActivityResponse, ActivityLog, ActivityQuery,
    DataExportResponse, UserDataExport, DataImportRequest, DataImportResponse
};
use crate::models::response::{ErrorResponse, SuccessResponse};
use crate::services::cache_service::{
    get_cached_profile, cache_profile, invalidate_profile_cache,
    get_cached_settings, cache_settings, invalidate_settings_cache
};
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
    // Validate input data
    if let Err(validation_errors) = body.validate() {
        let error_messages: Vec<String> = validation_errors
            .field_errors()
            .values()
            .flat_map(|errors| errors.iter().map(|e| e.message.as_ref().unwrap_or(&"Validation error".into()).to_string()))
            .collect();
        
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: error_messages.join(", "),
        }));
    }

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

// Change password endpoint (matches Node.js /api/users/change-password exactly)
pub async fn change_password(req: HttpRequest, body: web::Json<PasswordChangeRequest>) -> Result<HttpResponse> {
    // Validate input data
    if let Err(validation_errors) = body.validate() {
        let error_messages: Vec<String> = validation_errors
            .field_errors()
            .values()
            .flat_map(|errors| errors.iter().map(|e| e.message.as_ref().unwrap_or(&"Validation error".into()).to_string()))
            .collect();
        
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: error_messages.join(", "),
        }));
    }

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
    if !bcrypt_verify(&body.current_password, stored_password).unwrap_or(false) {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: "Current password is incorrect".to_string(),
        }));
    }

    // Hash new password (using bcrypt cost of 12 like Node.js)
    let new_password_hash = match hash(&body.new_password, 12) {
        Ok(hash) => hash,
        Err(e) => {
            log::error!("Failed to hash password: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to process password".to_string(),
            }));
        }
    };

    // Update user password
    let result = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.update_one(
                doc! { "_id": oid },
                doc! { 
                    "$set": { 
                        "password": new_password_hash,
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
            log::error!("Failed to update password: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to change password".to_string(),
            }));
        }
    }

    log::info!("Password changed successfully for user: {}", claims.user_id);

    Ok(HttpResponse::Ok().json(PasswordChangeResponse {
        success: true,
        message: "Password changed successfully".to_string(),
    }))
}

// Delete profile picture endpoint (matches Node.js /api/users/avatar DELETE exactly)
pub async fn delete_avatar(req: HttpRequest) -> Result<HttpResponse> {
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

    // Update user to remove profile picture and enable Gravatar
    let result = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.update_one(
                doc! { "_id": oid },
                doc! { 
                    "$set": { 
                        "useGravatar": true, // Fallback to Gravatar when custom avatar is deleted
                        "updatedAt": DateTime::now()
                    },
                    "$unset": {
                        "profilePicture": "" // Remove custom profile picture
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
            log::error!("Failed to delete avatar: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to delete avatar".to_string(),
            }));
        }
    }

    // Bust cache for this user since they deleted their avatar
    let cache_key = format!("user:profile:{}", claims.user_id);
    let _ = invalidate_profile_cache(&cache_key).await;
    log::info!("🗑️ Invalidated profile cache for user: {} (avatar deleted)", claims.user_id);

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: "Profile picture deleted successfully. Gravatar will be used instead.".to_string(),
    }))
}

// Admin user search endpoint (matches Node.js /api/admin/users GET exactly)
pub async fn admin_search_users(req: HttpRequest, query: web::Query<UserSearchQuery>) -> Result<HttpResponse> {
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

    // Check admin permissions
    if claims.role != "admin" && claims.role_type != "admin" {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required".to_string(),
        }));
    }

    // Parse pagination parameters
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(10).clamp(1, 100); // Max 100 per page
    let skip = (page - 1) * limit;

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

    // Build search filter
    let mut filter = doc! {};
    
    // Search by query string (name or email)
    if let Some(q) = &query.q {
        if !q.trim().is_empty() {
            filter.insert("$or", vec![
                doc! { "name": { "$regex": q, "$options": "i" } },
                doc! { "email": { "$regex": q, "$options": "i" } }
            ]);
        }
    }

    // Filter by role
    if let Some(role) = &query.role {
        if !role.trim().is_empty() {
            filter.insert("role", role);
        }
    }

    // Build sort criteria
    let sort_field = query.sort.as_deref().unwrap_or("createdAt");
    let sort_order = match query.order.as_deref() {
        Some("asc") => 1,
        _ => -1, // Default to descending
    };
    let sort_doc = doc! { sort_field: sort_order };

    // Get total count for pagination
    let total = users_collection.count_documents(filter.clone(), None).await.unwrap_or(0);
    let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;

    // Execute search with pagination
    let mut cursor = users_collection
        .find(filter, mongodb::options::FindOptions::builder()
            .projection(doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 })
            .sort(sort_doc)
            .skip(skip as u64)
            .limit(limit as i64)
            .build())
        .await
        .map_err(|e| {
            log::error!("User search query failed: {}", e);
            actix_web::error::ErrorInternalServerError("Search failed")
        })?;

    let mut users = Vec::new();
    while let Some(user_doc) = cursor.try_next().await.unwrap_or(None) {
        // Transform to standardized format
        let user_id_str = user_doc.get_object_id("_id").unwrap().to_hex();
        let standardized_user = StandardizedUser {
            _id: user_id_str.clone(),
            id: user_id_str.clone(),
            email: user_doc.get_str("email").unwrap_or("").to_string(),
            name: user_doc.get_str("name").unwrap_or("").to_string(),
            role: user_doc.get_str("role").unwrap_or("customer").to_string(),
            is_active: user_doc.get_bool("isActive").unwrap_or(true),
            email_verified: user_doc.get_bool("emailVerified").unwrap_or(false),
            created_at: user_doc.get_datetime("createdAt")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            updated_at: user_doc.get_datetime("updatedAt")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            phone: user_doc.get_str("phone").ok().map(|s| s.to_string()),
            company: user_doc.get_str("company").ok().map(|s| s.to_string()),
            department: user_doc.get_str("department").ok().map(|s| s.to_string()),
            position: user_doc.get_str("position").ok().map(|s| s.to_string()),
            username: user_doc.get_str("username").ok().map(|s| s.to_string()),
            profile_picture: user_doc.get_str("profilePicture").ok().map(|s| s.to_string()),
            use_gravatar: user_doc.get_bool("useGravatar").ok(),
            location: user_doc.get_str("location").ok().map(|s| s.to_string()),
        };
        users.push(standardized_user);
    }

    let pagination = PaginationInfo {
        page,
        limit,
        total,
        total_pages,
        has_next: page < total_pages,
        has_prev: page > 1,
    };

    log::info!("Admin user search completed: {} users found (page {}/{})", users.len(), page, total_pages);

    Ok(HttpResponse::Ok().json(UserSearchResponse {
        success: true,
        users,
        pagination,
        message: None,
    }))
}

// Admin update user endpoint (matches Node.js /api/admin/users/:id PUT exactly)
pub async fn admin_update_user(req: HttpRequest, path: web::Path<String>, body: web::Json<AdminUserUpdateRequest>) -> Result<HttpResponse> {
    // Validate input data
    if let Err(validation_errors) = body.validate() {
        let error_messages: Vec<String> = validation_errors
            .field_errors()
            .values()
            .flat_map(|errors| errors.iter().map(|e| e.message.as_ref().unwrap_or(&"Validation error".into()).to_string()))
            .collect();
        
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: error_messages.join(", "),
        }));
    }

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

    // Check admin permissions
    if claims.role != "admin" && claims.role_type != "admin" {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required".to_string(),
        }));
    }

    let user_id = path.into_inner();

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

    // Check if email is already taken by another user
    if let Some(new_email) = &body.email {
        let user_id_obj = match ObjectId::parse_str(&user_id) {
            Ok(oid) => oid,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        let email_exists = users_collection.find_one(
            doc! { 
                "email": new_email.to_lowercase(),
                "_id": { "$ne": user_id_obj }
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

    // Build update document
    let mut update_doc = doc! {
        "updatedAt": DateTime::now()
    };

    if let Some(name) = &body.name {
        update_doc.insert("name", name);
    }
    if let Some(email) = &body.email {
        update_doc.insert("email", email.to_lowercase());
    }
    if let Some(role) = &body.role {
        update_doc.insert("role", role);
    }
    if let Some(is_active) = body.is_active {
        update_doc.insert("isActive", is_active);
    }
    if let Some(email_verified) = body.email_verified {
        update_doc.insert("emailVerified", email_verified);
    }

    // Update user
    let result = match ObjectId::parse_str(&user_id) {
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
            log::error!("Failed to update user: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update user".to_string(),
            }));
        }
    }

    // Invalidate cache for the updated user
    let cache_key = format!("user:profile:{}", user_id);
    let _ = invalidate_profile_cache(&cache_key).await;
    log::info!("🗑️ Invalidated profile cache for user: {} (admin update)", user_id);

    log::info!("Admin updated user: {} by admin: {}", user_id, claims.user_id);

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: "User updated successfully".to_string(),
    }))
}

// Get user roles and permissions (GET /api/users/roles)
pub async fn get_user_roles(req: HttpRequest) -> Result<HttpResponse> {
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

    // Define available roles with permissions (matches system roles)
    let roles = vec![
        RoleInfo {
            name: "admin".to_string(),
            description: "Full system access with administrative privileges".to_string(),
            permissions: vec![
                "read".to_string(),
                "write".to_string(),
                "delete".to_string(),
                "admin".to_string(),
                "user_management".to_string(),
                "system_settings".to_string(),
            ],
        },
        RoleInfo {
            name: "customer".to_string(),
            description: "Regular user with standard access".to_string(),
            permissions: vec![
                "read".to_string(),
                "write".to_string(),
                "profile_edit".to_string(),
            ],
        },
        RoleInfo {
            name: "editor".to_string(),
            description: "Content editor with enhanced permissions".to_string(),
            permissions: vec![
                "read".to_string(),
                "write".to_string(),
                "content_edit".to_string(),
                "profile_edit".to_string(),
            ],
        },
        RoleInfo {
            name: "subscriber".to_string(),
            description: "Read-only access for subscribers".to_string(),
            permissions: vec!["read".to_string()],
        },
    ];

    // Get current user's role permissions
    let current_role = claims.role.clone();
    let permissions = roles
        .iter()
        .find(|r| r.name == current_role)
        .map(|r| r.permissions.clone());

    Ok(HttpResponse::Ok().json(UserRolesResponse {
        success: true,
        roles,
        current_role: Some(current_role),
        permissions,
        message: None,
    }))
}

// Update user role (PUT /api/users/roles)
pub async fn update_user_role(req: HttpRequest, body: web::Json<RoleUpdateRequest>) -> Result<HttpResponse> {
    // Validate input data
    if let Err(validation_errors) = body.validate() {
        let error_messages: Vec<String> = validation_errors
            .field_errors()
            .values()
            .flat_map(|errors| errors.iter().map(|e| e.message.as_ref().unwrap_or(&"Validation error".into()).to_string()))
            .collect();

        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: error_messages.join(", "),
        }));
    }

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

    // Check admin permissions (only admins can change roles)
    if claims.role != "admin" && claims.role_type != "admin" {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required to change user roles".to_string(),
        }));
    }

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

    // Update user role
    let result = match ObjectId::parse_str(&claims.user_id) {
        Ok(oid) => {
            users_collection.update_one(
                doc! { "_id": oid },
                doc! {
                    "$set": {
                        "role": &body.role,
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
            log::error!("Failed to update role: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update role".to_string(),
            }));
        }
    }

    // Invalidate cache
    let cache_key = format!("user:profile:{}", claims.user_id);
    let _ = invalidate_profile_cache(&cache_key).await;

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: format!("User role updated to {}", body.role),
    }))
}

// Get user activity logs (GET /api/users/activity)
pub async fn get_user_activity(req: HttpRequest, query: web::Query<ActivityQuery>) -> Result<HttpResponse> {
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

    // Parse pagination parameters
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let skip = (page - 1) * limit;

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

    let activity_collection = db.collection::<mongodb::bson::Document>("user_activities");

    // Build activity filter
    let mut filter = doc! { "user_id": &claims.user_id };

    // Filter by action type
    if let Some(action) = &query.action {
        if !action.trim().is_empty() {
            filter.insert("action", action);
        }
    }

    // Filter by date range
    if query.start_date.is_some() || query.end_date.is_some() {
        let mut date_filter = doc! {};

        if let Some(start_date) = &query.start_date {
            if let Ok(start_dt) = chrono::DateTime::parse_from_rfc3339(start_date) {
                date_filter.insert("$gte", DateTime::from_millis(start_dt.timestamp_millis()));
            }
        }

        if let Some(end_date) = &query.end_date {
            if let Ok(end_dt) = chrono::DateTime::parse_from_rfc3339(end_date) {
                date_filter.insert("$lte", DateTime::from_millis(end_dt.timestamp_millis()));
            }
        }

        if !date_filter.is_empty() {
            filter.insert("timestamp", date_filter);
        }
    }

    // Get total count for pagination
    let total = activity_collection.count_documents(filter.clone(), None).await.unwrap_or(0);
    let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;

    // Execute query with pagination
    let mut cursor = activity_collection
        .find(filter, mongodb::options::FindOptions::builder()
            .sort(doc! { "timestamp": -1 })
            .skip(skip as u64)
            .limit(limit as i64)
            .build())
        .await
        .map_err(|e| {
            log::error!("Activity query failed: {}", e);
            actix_web::error::ErrorInternalServerError("Query failed")
        })?;

    let mut activities = Vec::new();
    while let Some(activity_doc) = cursor.try_next().await.unwrap_or(None) {
        let activity_id = activity_doc.get_object_id("_id").unwrap().to_hex();
        let activity = ActivityLog {
            id: activity_id,
            user_id: activity_doc.get_str("user_id").unwrap_or("").to_string(),
            action: activity_doc.get_str("action").unwrap_or("").to_string(),
            resource: activity_doc.get_str("resource").ok().map(|s| s.to_string()),
            resource_id: activity_doc.get_str("resource_id").ok().map(|s| s.to_string()),
            ip_address: activity_doc.get_str("ip_address").ok().map(|s| s.to_string()),
            user_agent: activity_doc.get_str("user_agent").ok().map(|s| s.to_string()),
            timestamp: activity_doc.get_datetime("timestamp")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            metadata: activity_doc.get_document("metadata").ok().and_then(|d| {
                serde_json::from_str(&d.to_string()).ok()
            }),
        };
        activities.push(activity);
    }

    let pagination = PaginationInfo {
        page,
        limit,
        total,
        total_pages,
        has_next: page < total_pages,
        has_prev: page > 1,
    };

    log::info!("Retrieved {} activities for user: {}", activities.len(), claims.user_id);

    Ok(HttpResponse::Ok().json(UserActivityResponse {
        success: true,
        activities,
        pagination: Some(pagination),
        message: None,
    }))
}

// Export user data (GET /api/users/export)
pub async fn export_user_data(req: HttpRequest) -> Result<HttpResponse> {
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
    let activity_collection = db.collection::<mongodb::bson::Document>("user_activities");

    // Get user data
    let user = match ObjectId::parse_str(&claims.user_id) {
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

    // Transform user to standardized format
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

    // Get user settings
    let settings = if let Ok(settings_doc) = user.get_document("settings") {
        serde_json::from_str(&settings_doc.to_string()).ok()
    } else {
        Some(UserSettings::default())
    };

    // Get user activities (last 100)
    let mut cursor = activity_collection
        .find(
            doc! { "user_id": &claims.user_id },
            mongodb::options::FindOptions::builder()
                .sort(doc! { "timestamp": -1 })
                .limit(100)
                .build()
        )
        .await
        .map_err(|e| {
            log::error!("Activity query failed: {}", e);
            actix_web::error::ErrorInternalServerError("Query failed")
        })?;

    let mut activities = Vec::new();
    while let Some(activity_doc) = cursor.try_next().await.unwrap_or(None) {
        let activity_id = activity_doc.get_object_id("_id").unwrap().to_hex();
        let activity = ActivityLog {
            id: activity_id,
            user_id: activity_doc.get_str("user_id").unwrap_or("").to_string(),
            action: activity_doc.get_str("action").unwrap_or("").to_string(),
            resource: activity_doc.get_str("resource").ok().map(|s| s.to_string()),
            resource_id: activity_doc.get_str("resource_id").ok().map(|s| s.to_string()),
            ip_address: activity_doc.get_str("ip_address").ok().map(|s| s.to_string()),
            user_agent: activity_doc.get_str("user_agent").ok().map(|s| s.to_string()),
            timestamp: activity_doc.get_datetime("timestamp")
                .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
            metadata: activity_doc.get_document("metadata").ok().and_then(|d| {
                serde_json::from_str(&d.to_string()).ok()
            }),
        };
        activities.push(activity);
    }

    let export_data = UserDataExport {
        user: standardized_user,
        settings,
        activities,
        exported_at: chrono::Utc::now().to_rfc3339(),
    };

    log::info!("Exported data for user: {}", claims.user_id);

    Ok(HttpResponse::Ok().json(DataExportResponse {
        success: true,
        data: Some(export_data),
        download_url: None, // Could implement download URL generation if needed
        message: Some("User data exported successfully".to_string()),
    }))
}

// Import user data (POST /api/users/import - Admin only)
pub async fn import_user_data(req: HttpRequest, body: web::Json<DataImportRequest>) -> Result<HttpResponse> {
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

    // Check admin permissions
    if claims.role != "admin" && claims.role_type != "admin" {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required for data import".to_string(),
        }));
    }

    // Validate email format
    if !body.data.email.contains('@') {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: "Invalid email format".to_string(),
        }));
    }

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

    // Check if user already exists
    let existing_user = users_collection.find_one(
        doc! { "email": body.data.email.to_lowercase() },
        None
    ).await.unwrap_or(None);

    if existing_user.is_some() {
        return Ok(HttpResponse::BadRequest().json(DataImportResponse {
            success: false,
            imported_count: 0,
            failed_count: 1,
            errors: vec!["User with this email already exists".to_string()],
            message: "Import failed".to_string(),
        }));
    }

    // Create new user document
    let mut user_doc = doc! {
        "email": body.data.email.to_lowercase(),
        "name": &body.data.name,
        "role": body.data.role.as_ref().unwrap_or(&"customer".to_string()),
        "isActive": true,
        "emailVerified": false,
        "password": hash("ChangeMe123!", 12).unwrap(), // Temporary password
        "createdAt": DateTime::now(),
        "updatedAt": DateTime::now(),
    };

    // Add settings if provided
    if let Some(settings) = &body.data.settings {
        user_doc.insert("settings", mongodb::bson::to_bson(settings).unwrap());
    }

    // Insert user
    match users_collection.insert_one(user_doc, None).await {
        Ok(_) => {
            log::info!("Imported user: {} by admin: {}", body.data.email, claims.user_id);

            Ok(HttpResponse::Ok().json(DataImportResponse {
                success: true,
                imported_count: 1,
                failed_count: 0,
                errors: vec![],
                message: "User imported successfully. Temporary password: ChangeMe123!".to_string(),
            }))
        }
        Err(e) => {
            log::error!("Failed to import user: {}", e);
            Ok(HttpResponse::InternalServerError().json(DataImportResponse {
                success: false,
                imported_count: 0,
                failed_count: 1,
                errors: vec![format!("Database error: {}", e)],
                message: "Import failed".to_string(),
            }))
        }
    }
}

