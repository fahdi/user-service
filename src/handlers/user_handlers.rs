use actix_web::{web, HttpRequest, HttpResponse, Result};
use actix_multipart::Multipart;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, oid::ObjectId, DateTime};
use bcrypt::{hash, verify as bcrypt_verify};
use validator::Validate;

use crate::models::user::{
    UserProfileResponse, SettingsResponse, UserSettings,
    SettingsUpdateRequest, ProfilePictureResponse,
    PasswordChangeRequest, PasswordChangeResponse, UserSearchQuery,
    UserSearchResponse, AdminUserUpdateRequest,
    UserRolesResponse, RoleUpdateRequest,
    UserActivityResponse, ActivityQuery,
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
use crate::utils::security::{generate_secure_password, validate_email};

use super::helpers::{
    standardize_user_doc, standardize_activity_doc, extract_user_basic_info,
    is_admin, determine_target_user_id, get_role_definitions, get_permissions_for_role,
    parse_pagination, compute_pagination_info,
    build_search_filter, build_sort_doc, build_admin_lookup_filter,
    build_activity_filter, build_admin_update_fields, build_settings_success_message,
    validate_file_size, validate_image_content_type,
    profile_cache_key, settings_cache_key,
    collect_validation_errors, parse_object_id,
};

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
    let target_user_id = determine_target_user_id(
        user_id, email, &claims.user_id, &claims.role, &claims.role_type,
    );

    // Try cache first (15-minute cache like Node.js)
    let cache_key = profile_cache_key(&target_user_id);
    
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
    let user = if (user_id.is_some() || email.is_some()) && is_admin(&claims.role, &claims.role_type) {
        let filter = match build_admin_lookup_filter(user_id, email, &claims.user_id) {
            Ok(f) => f,
            Err(e) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: e,
                }));
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
        let oid = match parse_object_id(&claims.user_id) {
            Ok(oid) => oid,
            Err(e) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: e,
                }));
            }
        };
        users_collection.find_one(
            doc! { "_id": oid },
            mongodb::options::FindOneOptions::builder()
                .projection(doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 })
                .build()
        ).await.unwrap_or(None)
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
    let standardized_user = match standardize_user_doc(&user) {
        Ok(u) => u,
        Err(_) => {
            log::error!("Document missing valid _id field");
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Internal error: malformed document".to_string(),
            }));
        }
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
    let cache_key = settings_cache_key(&claims.user_id);
    
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
    let user_info = match extract_user_basic_info(&user) {
        Ok(info) => info,
        Err(_) => {
            log::error!("Document missing valid _id field");
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Internal error: malformed document".to_string(),
            }));
        }
    };
    response_settings.user = Some(user_info);

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
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
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
                let current_oid = match current_user.get_object_id("_id") {
                    Ok(oid) => oid,
                    Err(_) => {
                        return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                            success: false,
                            error: "Internal error: malformed document".to_string(),
                        }));
                    }
                };
                let email_exists = users_collection.find_one(
                    doc! {
                        "email": new_email.to_lowercase(),
                        "_id": { "$ne": current_oid }
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
            let password_hash = match hash(new_password, 12) {
                Ok(h) => h,
                Err(e) => {
                    log::error!("Failed to hash password: {}", e);
                    return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                        success: false,
                        error: "Failed to process password".to_string(),
                    }));
                }
            };
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
    let cache_key = settings_cache_key(&claims.user_id);
    let _ = invalidate_settings_cache(&cache_key).await;
    log::info!("🗑️ Invalidated settings cache for user: {}", claims.user_id);

    // Build success message (matches Node.js logic)
    let email_changed = body.account_changes.as_ref().is_some_and(|ac| ac.new_email.is_some());
    let password_changed = body.account_changes.as_ref().is_some_and(|ac| ac.new_password.is_some());
    let success_message = build_settings_success_message(email_changed, password_changed);

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
            if let Err(e) = validate_file_size(data.len(), 5 * 1024 * 1024) {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: e,
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
    if let Err(e) = validate_image_content_type(content_type.as_deref()) {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: e,
        }));
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
    let cache_key = profile_cache_key(&claims.user_id);
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
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
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
    let cache_key = profile_cache_key(&claims.user_id);
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
    if !is_admin(&claims.role, &claims.role_type) {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required".to_string(),
        }));
    }

    // Parse pagination parameters
    let (page, limit, skip) = parse_pagination(query.page, query.limit, 10, 100);

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
    let filter = build_search_filter(query.q.as_deref(), query.role.as_deref());

    // Build sort criteria
    let sort_doc = build_sort_doc(query.sort.as_deref(), query.order.as_deref());

    // Get total count for pagination
    let total = users_collection.count_documents(filter.clone(), None).await.unwrap_or(0);

    // Execute search with pagination
    let mut cursor = users_collection
        .find(filter, mongodb::options::FindOptions::builder()
            .projection(doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 })
            .sort(sort_doc)
            .skip(skip)
            .limit(limit as i64)
            .build())
        .await
        .map_err(|e| {
            log::error!("User search query failed: {}", e);
            actix_web::error::ErrorInternalServerError("Search failed")
        })?;

    let mut users = Vec::new();
    while let Some(user_doc) = cursor.try_next().await.unwrap_or(None) {
        // Transform to standardized format — skip docs with missing _id
        if let Ok(standardized_user) = standardize_user_doc(&user_doc) {
            users.push(standardized_user);
        }
    }

    let pagination = compute_pagination_info(page, limit, total);

    log::info!("Admin user search completed: {} users found (page {}/{})", users.len(), page, pagination.total_pages);

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
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
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
    if !is_admin(&claims.role, &claims.role_type) {
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
    let update_doc = build_admin_update_fields(&body);

    // Update user
    let result = match parse_object_id(&user_id) {
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
    let cache_key = profile_cache_key(&user_id);
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
    let roles = get_role_definitions();

    // Get current user's role permissions
    let current_role = claims.role.clone();
    let permissions = get_permissions_for_role(&current_role);

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
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
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
    if !is_admin(&claims.role, &claims.role_type) {
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
    let cache_key = profile_cache_key(&claims.user_id);
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
    let (page, limit, skip) = parse_pagination(query.page, query.limit, 20, 100);

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
    let filter = build_activity_filter(
        &claims.user_id,
        query.action.as_deref(),
        query.start_date.as_deref(),
        query.end_date.as_deref(),
    );

    // Get total count for pagination
    let total = activity_collection.count_documents(filter.clone(), None).await.unwrap_or(0);

    // Execute query with pagination
    let mut cursor = activity_collection
        .find(filter, mongodb::options::FindOptions::builder()
            .sort(doc! { "timestamp": -1 })
            .skip(skip)
            .limit(limit as i64)
            .build())
        .await
        .map_err(|e| {
            log::error!("Activity query failed: {}", e);
            actix_web::error::ErrorInternalServerError("Query failed")
        })?;

    let mut activities = Vec::new();
    while let Some(activity_doc) = cursor.try_next().await.unwrap_or(None) {
        if let Ok(activity) = standardize_activity_doc(&activity_doc) {
            activities.push(activity);
        }
    }

    let pagination = compute_pagination_info(page, limit, total);

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
    let standardized_user = match standardize_user_doc(&user) {
        Ok(u) => u,
        Err(_) => {
            log::error!("Document missing valid _id field");
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Internal error: malformed document".to_string(),
            }));
        }
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
        if let Ok(activity) = standardize_activity_doc(&activity_doc) {
            activities.push(activity);
        }
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
    if !is_admin(&claims.role, &claims.role_type) {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required for data import".to_string(),
        }));
    }

    // Validate email format (proper structural validation, not just contains('@'))
    if !validate_email(&body.data.email) {
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

    // Generate a cryptographically random temporary password (never expose it in response)
    let temp_password = generate_secure_password();
    let password_hash = match hash(&temp_password, 12) {
        Ok(h) => h,
        Err(e) => {
            log::error!("Failed to hash temporary password: {}", e);
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to process password".to_string(),
            }));
        }
    };

    // Create new user document
    let mut user_doc = doc! {
        "email": body.data.email.to_lowercase(),
        "name": &body.data.name,
        "role": body.data.role.as_ref().unwrap_or(&"customer".to_string()),
        "isActive": true,
        "emailVerified": false,
        "password": password_hash,
        "createdAt": DateTime::now(),
        "updatedAt": DateTime::now(),
    };

    // Add settings if provided
    if let Some(settings) = &body.data.settings {
        if let Ok(bson_val) = mongodb::bson::to_bson(settings) {
            user_doc.insert("settings", bson_val);
        }
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
                message: "User imported successfully. A password reset is required.".to_string(),
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

