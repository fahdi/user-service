//! Handler functions that accept injected dependencies via `AppState`.
//!
//! These are the production handler implementations using trait-based DI.


//! Each handler takes `web::Data<AppState>` instead of calling global singletons.

use actix_web::{web, HttpRequest, HttpResponse, Result};
use actix_multipart::Multipart;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, DateTime};
use bcrypt::{hash, verify as bcrypt_verify};
use validator::Validate;

use crate::models::user::{
    UserProfileResponse, SettingsResponse, UserSettings,
    SettingsUpdateRequest, ProfilePictureResponse,
    PasswordChangeRequest, PasswordChangeResponse, UserSearchQuery,
    UserSearchResponse, AdminUserUpdateRequest,
    UserRolesResponse, RoleUpdateRequest,
    UserActivityResponse, ActivityQuery,
    DataExportResponse, UserDataExport, DataImportRequest, DataImportResponse,
};
use crate::models::response::{ErrorResponse, SuccessResponse};
use crate::utils::security::{generate_secure_password, validate_email};
use crate::traits::AppState;

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

// ============================================================================
// GET /api/users/profile
// ============================================================================

pub async fn get_profile(
    req: HttpRequest,
    query: web::Query<serde_json::Value>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let email = query.get("email").and_then(|v| v.as_str());
    let user_id = query.get("userId").and_then(|v| v.as_str());

    let target_user_id = determine_target_user_id(
        user_id, email, &claims.user_id, &claims.role, &claims.role_type,
    );

    // Try cache first
    let cache_key = profile_cache_key(&target_user_id);
    if let Some(cached_profile) = state.cache.get_cached_profile(&cache_key).await {
        return Ok(HttpResponse::Ok().json(UserProfileResponse {
            success: true,
            user: Some(cached_profile),
            message: None,
        }));
    }

    // Build filter
    let projection = doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 };
    let filter = if (user_id.is_some() || email.is_some()) && is_admin(&claims.role, &claims.role_type) {
        match build_admin_lookup_filter(user_id, email, &claims.user_id) {
            Ok(f) => f,
            Err(e) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: e,
                }));
            }
        }
    } else {
        let oid = match parse_object_id(&claims.user_id) {
            Ok(oid) => oid,
            Err(e) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: e,
                }));
            }
        };
        doc! { "_id": oid }
    };

    let user = match state.repo.find_user(filter, Some(projection)).await {
        Ok(u) => u,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Database error: {}", e),
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

    let standardized_user = match standardize_user_doc(&user) {
        Ok(u) => u,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Internal error: malformed document".to_string(),
            }));
        }
    };

    state.cache.cache_profile(&cache_key, &standardized_user, 900).await;

    Ok(HttpResponse::Ok().json(UserProfileResponse {
        success: true,
        user: Some(standardized_user),
        message: None,
    }))
}

// ============================================================================
// GET /api/users/settings
// ============================================================================

pub async fn get_settings(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let cache_key = settings_cache_key(&claims.user_id);
    if let Some(cached_settings) = state.cache.get_cached_settings(&cache_key).await {
        return Ok(HttpResponse::Ok().json(cached_settings));
    }

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    let projection = doc! {
        "settings": 1, "email": 1, "name": 1,
        "role": 1, "profilePicture": 1, "useGravatar": 1, "location": 1
    };

    let user = match state.repo.find_user(doc! { "_id": oid }, Some(projection)).await {
        Ok(u) => u,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Database error: {}", e),
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

    let settings = if let Ok(settings_doc) = user.get_document("settings") {
        serde_json::from_str(&settings_doc.to_string()).unwrap_or_else(|_| UserSettings::default())
    } else {
        UserSettings::default()
    };

    let mut response_settings = settings;
    let user_info = match extract_user_basic_info(&user) {
        Ok(info) => info,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Internal error: malformed document".to_string(),
            }));
        }
    };
    response_settings.user = Some(user_info);

    let response_data = SettingsResponse {
        success: true,
        settings: Some(response_settings),
        message: None,
    };

    state.cache.cache_settings(&cache_key, &response_data, 1800).await;

    Ok(HttpResponse::Ok().json(response_data))
}

// ============================================================================
// PUT /api/users/settings
// ============================================================================

pub async fn update_settings(
    req: HttpRequest,
    body: web::Json<SettingsUpdateRequest>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(validation_errors) = body.validate() {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
        }));
    }

    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    // Handle account changes (email/password) if provided
    if let Some(account_changes) = &body.account_changes {
        let oid = match parse_object_id(&claims.user_id) {
            Ok(oid) => oid,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };

        let current_user = match state.repo.find_user(
            doc! { "_id": oid },
            Some(doc! { "password": 1, "email": 1 }),
        ).await {
            Ok(u) => u,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                    success: false,
                    error: format!("Database error: {}", e),
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

        let stored_password = current_user.get_str("password").unwrap_or("");
        if !bcrypt_verify(&account_changes.current_password, stored_password).unwrap_or(false) {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Current password is incorrect".to_string(),
            }));
        }

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
                let email_exists = state.repo.find_user(
                    doc! {
                        "email": new_email.to_lowercase(),
                        "_id": { "$ne": current_oid }
                    },
                    None,
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

    // Build update document
    let mut update_doc = doc! {
        "settings": mongodb::bson::to_bson(&body.settings).unwrap(),
        "updatedAt": DateTime::now()
    };

    if let Some(user_info) = &body.settings.user {
        update_doc.insert("name", &user_info.name);
        if let Some(location) = &user_info.location {
            update_doc.insert("location", location);
        }
        if let Some(use_gravatar) = user_info.use_gravatar {
            update_doc.insert("useGravatar", use_gravatar);
        }
    }

    if let Some(account_changes) = &body.account_changes {
        if let Some(new_email) = &account_changes.new_email {
            update_doc.insert("email", new_email.to_lowercase());
        }
        if let Some(new_password) = &account_changes.new_password {
            let password_hash = match hash(new_password, 12) {
                Ok(h) => h,
                Err(_) => {
                    return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                        success: false,
                        error: "Failed to process password".to_string(),
                    }));
                }
            };
            update_doc.insert("password", password_hash);
        }
    }

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match state.repo.update_user(
        doc! { "_id": oid },
        doc! { "$set": update_doc },
    ).await {
        Ok(matched) => {
            if matched == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Failed to update settings: {}", e),
            }));
        }
    }

    let cache_key = settings_cache_key(&claims.user_id);
    state.cache.invalidate_settings_cache(&cache_key).await;

    let email_changed = body.account_changes.as_ref().is_some_and(|ac| ac.new_email.is_some());
    let password_changed = body.account_changes.as_ref().is_some_and(|ac| ac.new_password.is_some());
    let success_message = build_settings_success_message(email_changed, password_changed);

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: success_message,
    }))
}

// ============================================================================
// POST /api/users/profile-picture
// ============================================================================

pub async fn update_profile_picture(
    req: HttpRequest,
    mut payload: Multipart,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut content_type: Option<String> = None;

    while let Some(mut field) = payload.try_next().await.unwrap_or(None) {
        let field_name = field.name().unwrap_or("unknown").to_string();
        if field_name == "profilePicture" {
            let mut data = Vec::new();
            while let Some(chunk) = field.try_next().await.unwrap_or(None) {
                data.extend_from_slice(&chunk);
            }
            if let Err(e) = validate_file_size(data.len(), 5 * 1024 * 1024) {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: e,
                }));
            }
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

    if let Err(e) = validate_image_content_type(content_type.as_deref()) {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: e,
        }));
    }

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    let user = match state.repo.find_user(
        doc! { "_id": oid },
        Some(doc! { "email": 1 }),
    ).await {
        Ok(u) => u,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Database error: {}", e),
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

    let profile_picture_url = match state.uploader.upload_profile_picture(
        &claims.user_id,
        user.get_str("email").unwrap_or(""),
        file_data,
        &file_name,
    ).await {
        Ok(url) => url,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to upload profile picture".to_string(),
            }));
        }
    };

    let oid2 = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match state.repo.update_user(
        doc! { "_id": oid2 },
        doc! {
            "$set": {
                "profilePicture": &profile_picture_url,
                "useGravatar": false,
                "updatedAt": DateTime::now()
            }
        },
    ).await {
        Ok(matched) => {
            if matched == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update profile picture".to_string(),
            }));
        }
    }

    let cache_key = profile_cache_key(&claims.user_id);
    state.cache.invalidate_profile_cache(&cache_key).await;

    Ok(HttpResponse::Ok().json(ProfilePictureResponse {
        success: true,
        message: "Profile picture updated successfully".to_string(),
        profile_picture: Some(profile_picture_url),
    }))
}

// ============================================================================
// POST /api/users/change-password
// ============================================================================

pub async fn change_password(
    req: HttpRequest,
    body: web::Json<PasswordChangeRequest>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(validation_errors) = body.validate() {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
        }));
    }

    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    let current_user = match state.repo.find_user(
        doc! { "_id": oid },
        Some(doc! { "password": 1, "email": 1 }),
    ).await {
        Ok(u) => u,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Database error: {}", e),
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

    let stored_password = current_user.get_str("password").unwrap_or("");
    if !bcrypt_verify(&body.current_password, stored_password).unwrap_or(false) {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: "Current password is incorrect".to_string(),
        }));
    }

    let new_password_hash = match hash(&body.new_password, 12) {
        Ok(h) => h,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to process password".to_string(),
            }));
        }
    };

    match state.repo.update_user(
        doc! { "_id": oid },
        doc! { "$set": { "password": new_password_hash, "updatedAt": DateTime::now() } },
    ).await {
        Ok(matched) => {
            if matched == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to change password".to_string(),
            }));
        }
    }

    Ok(HttpResponse::Ok().json(PasswordChangeResponse {
        success: true,
        message: "Password changed successfully".to_string(),
    }))
}

// ============================================================================
// DELETE /api/users/avatar
// ============================================================================

pub async fn delete_avatar(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match state.repo.update_user(
        doc! { "_id": oid },
        doc! {
            "$set": { "useGravatar": true, "updatedAt": DateTime::now() },
            "$unset": { "profilePicture": "" }
        },
    ).await {
        Ok(matched) => {
            if matched == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to delete avatar".to_string(),
            }));
        }
    }

    let cache_key = profile_cache_key(&claims.user_id);
    state.cache.invalidate_profile_cache(&cache_key).await;

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: "Profile picture deleted successfully. Gravatar will be used instead.".to_string(),
    }))
}

// ============================================================================
// GET /api/admin/users (search)
// ============================================================================

pub async fn admin_search_users(
    req: HttpRequest,
    query: web::Query<UserSearchQuery>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    if !is_admin(&claims.role, &claims.role_type) {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required".to_string(),
        }));
    }

    let (page, limit, skip) = parse_pagination(query.page, query.limit, 10, 100);
    let filter = build_search_filter(query.q.as_deref(), query.role.as_deref());
    let sort_doc = build_sort_doc(query.sort.as_deref(), query.order.as_deref());
    let projection = doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 };

    let total = match state.repo.count_users(filter.clone()).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Database error: {}", e),
            }));
        }
    };

    let user_docs = match state.repo.find_users(
        filter,
        Some(projection),
        Some(sort_doc),
        Some(skip),
        Some(limit as i64),
    ).await {
        Ok(docs) => docs,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Search failed: {}", e),
            }));
        }
    };

    let users: Vec<_> = user_docs
        .iter()
        .filter_map(|d| standardize_user_doc(d).ok())
        .collect();

    let pagination = compute_pagination_info(page, limit, total);

    Ok(HttpResponse::Ok().json(UserSearchResponse {
        success: true,
        users,
        pagination,
        message: None,
    }))
}

// ============================================================================
// PUT /api/admin/users/{id}
// ============================================================================

pub async fn admin_update_user(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AdminUserUpdateRequest>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(validation_errors) = body.validate() {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
        }));
    }

    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    if !is_admin(&claims.role, &claims.role_type) {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required".to_string(),
        }));
    }

    let user_id = path.into_inner();

    // Check email uniqueness
    if let Some(new_email) = &body.email {
        let user_id_obj = match parse_object_id(&user_id) {
            Ok(oid) => oid,
            Err(_) => {
                return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                    success: false,
                    error: "Invalid user ID format".to_string(),
                }));
            }
        };
        let email_exists = state.repo.find_user(
            doc! {
                "email": new_email.to_lowercase(),
                "_id": { "$ne": user_id_obj }
            },
            None,
        ).await.unwrap_or(None);

        if email_exists.is_some() {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Email address is already in use".to_string(),
            }));
        }
    }

    let update_doc = build_admin_update_fields(&body);
    let oid = match parse_object_id(&user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match state.repo.update_user(
        doc! { "_id": oid },
        doc! { "$set": update_doc },
    ).await {
        Ok(matched) => {
            if matched == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update user".to_string(),
            }));
        }
    }

    let cache_key = profile_cache_key(&user_id);
    state.cache.invalidate_profile_cache(&cache_key).await;

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: "User updated successfully".to_string(),
    }))
}

// ============================================================================
// GET /api/users/roles
// ============================================================================

pub async fn get_user_roles(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let roles = get_role_definitions();
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

// ============================================================================
// PUT /api/users/roles
// ============================================================================

pub async fn update_user_role(
    req: HttpRequest,
    body: web::Json<RoleUpdateRequest>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(validation_errors) = body.validate() {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: collect_validation_errors(&validation_errors),
        }));
    }

    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    if !is_admin(&claims.role, &claims.role_type) {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required to change user roles".to_string(),
        }));
    }

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    match state.repo.update_user(
        doc! { "_id": oid },
        doc! { "$set": { "role": &body.role, "updatedAt": DateTime::now() } },
    ).await {
        Ok(matched) => {
            if matched == 0 {
                return Ok(HttpResponse::NotFound().json(ErrorResponse {
                    success: false,
                    error: "User not found".to_string(),
                }));
            }
        }
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to update role".to_string(),
            }));
        }
    }

    let cache_key = profile_cache_key(&claims.user_id);
    state.cache.invalidate_profile_cache(&cache_key).await;

    Ok(HttpResponse::Ok().json(SuccessResponse {
        success: true,
        message: format!("User role updated to {}", body.role),
    }))
}

// ============================================================================
// GET /api/users/activity
// ============================================================================

pub async fn get_user_activity(
    req: HttpRequest,
    query: web::Query<ActivityQuery>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let (page, limit, skip) = parse_pagination(query.page, query.limit, 20, 100);

    let filter = build_activity_filter(
        &claims.user_id,
        query.action.as_deref(),
        query.start_date.as_deref(),
        query.end_date.as_deref(),
    );

    let total = state.repo.count_activities(filter.clone()).await.unwrap_or(0);

    let activity_docs = match state.repo.find_activities(
        filter,
        Some(doc! { "timestamp": -1 }),
        Some(skip),
        Some(limit as i64),
    ).await {
        Ok(docs) => docs,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Query failed: {}", e),
            }));
        }
    };

    let activities: Vec<_> = activity_docs
        .iter()
        .filter_map(|d| standardize_activity_doc(d).ok())
        .collect();

    let pagination = compute_pagination_info(page, limit, total);

    Ok(HttpResponse::Ok().json(UserActivityResponse {
        success: true,
        activities,
        pagination: Some(pagination),
        message: None,
    }))
}

// ============================================================================
// GET /api/users/export
// ============================================================================

pub async fn export_user_data(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    let oid = match parse_object_id(&claims.user_id) {
        Ok(oid) => oid,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(ErrorResponse {
                success: false,
                error: "Invalid user ID format".to_string(),
            }));
        }
    };

    let projection = doc! { "password": 0, "resetToken": 0, "resetTokenExpiry": 0 };
    let user = match state.repo.find_user(doc! { "_id": oid }, Some(projection)).await {
        Ok(u) => u,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: format!("Database error: {}", e),
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

    let standardized_user = match standardize_user_doc(&user) {
        Ok(u) => u,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Internal error: malformed document".to_string(),
            }));
        }
    };

    let settings = if let Ok(settings_doc) = user.get_document("settings") {
        serde_json::from_str(&settings_doc.to_string()).ok()
    } else {
        Some(UserSettings::default())
    };

    let activity_docs = state.repo.find_activities(
        doc! { "user_id": &claims.user_id },
        Some(doc! { "timestamp": -1 }),
        None,
        Some(100),
    ).await.unwrap_or_default();

    let activities: Vec<_> = activity_docs
        .iter()
        .filter_map(|d| standardize_activity_doc(d).ok())
        .collect();

    let export_data = UserDataExport {
        user: standardized_user,
        settings,
        activities,
        exported_at: chrono::Utc::now().to_rfc3339(),
    };

    Ok(HttpResponse::Ok().json(DataExportResponse {
        success: true,
        data: Some(export_data),
        download_url: None,
        message: Some("User data exported successfully".to_string()),
    }))
}

// ============================================================================
// POST /api/users/import
// ============================================================================

pub async fn import_user_data(
    req: HttpRequest,
    body: web::Json<DataImportRequest>,
    state: web::Data<AppState>,
) -> Result<HttpResponse> {
    let claims = match state.auth.extract_claims(&req) {
        Ok(c) => c,
        Err(_) => {
            return Ok(HttpResponse::Unauthorized().json(ErrorResponse {
                success: false,
                error: "Authentication required".to_string(),
            }));
        }
    };

    if !is_admin(&claims.role, &claims.role_type) {
        return Ok(HttpResponse::Forbidden().json(ErrorResponse {
            success: false,
            error: "Admin access required for data import".to_string(),
        }));
    }

    if !validate_email(&body.data.email) {
        return Ok(HttpResponse::BadRequest().json(ErrorResponse {
            success: false,
            error: "Invalid email format".to_string(),
        }));
    }

    let existing_user = state.repo.find_user(
        doc! { "email": body.data.email.to_lowercase() },
        None,
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

    let temp_password = generate_secure_password();
    let password_hash = match hash(&temp_password, 12) {
        Ok(h) => h,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(ErrorResponse {
                success: false,
                error: "Failed to process password".to_string(),
            }));
        }
    };

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

    if let Some(settings) = &body.data.settings {
        if let Ok(bson_val) = mongodb::bson::to_bson(settings) {
            user_doc.insert("settings", bson_val);
        }
    }

    match state.repo.insert_user(user_doc).await {
        Ok(_) => {
            Ok(HttpResponse::Ok().json(DataImportResponse {
                success: true,
                imported_count: 1,
                failed_count: 0,
                errors: vec![],
                message: "User imported successfully. A password reset is required.".to_string(),
            }))
        }
        Err(e) => {
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
