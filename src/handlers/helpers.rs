//! Pure business logic extracted from user_handlers.rs for testability.
//!
//! Every function here is synchronous, takes simple inputs, and returns simple outputs.
//! No database connections, no HTTP requests, no async — just logic.

use mongodb::bson::{doc, oid::ObjectId, Document};

use crate::models::user::{
    StandardizedUser, UserBasicInfo, PaginationInfo, RoleInfo, ActivityLog,
    AdminUserUpdateRequest,
};

// ---------------------------------------------------------------------------
// Document → Model conversions
// ---------------------------------------------------------------------------

/// Convert a BSON user document into a StandardizedUser.
/// Extracts `_id` as hex string and maps all fields with safe defaults.
pub fn standardize_user_doc(user: &Document) -> Result<StandardizedUser, String> {
    let user_id_str = user
        .get_object_id("_id")
        .map(|oid| oid.to_hex())
        .map_err(|_| "Document missing valid _id field".to_string())?;

    Ok(StandardizedUser {
        _id: user_id_str.clone(),
        id: user_id_str,
        email: user.get_str("email").unwrap_or("").to_string(),
        name: user.get_str("name").unwrap_or("").to_string(),
        role: user.get_str("role").unwrap_or("customer").to_string(),
        is_active: user.get_bool("isActive").unwrap_or(true),
        email_verified: user.get_bool("emailVerified").unwrap_or(false),
        created_at: user
            .get_datetime("createdAt")
            .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
            .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
        updated_at: user
            .get_datetime("updatedAt")
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
    })
}

/// Convert a BSON activity document into an ActivityLog.
pub fn standardize_activity_doc(activity_doc: &Document) -> Result<ActivityLog, String> {
    let activity_id = activity_doc
        .get_object_id("_id")
        .map(|oid| oid.to_hex())
        .map_err(|_| "Activity document missing valid _id field".to_string())?;

    Ok(ActivityLog {
        id: activity_id,
        user_id: activity_doc.get_str("user_id").unwrap_or("").to_string(),
        action: activity_doc.get_str("action").unwrap_or("").to_string(),
        resource: activity_doc.get_str("resource").ok().map(|s| s.to_string()),
        resource_id: activity_doc.get_str("resource_id").ok().map(|s| s.to_string()),
        ip_address: activity_doc.get_str("ip_address").ok().map(|s| s.to_string()),
        user_agent: activity_doc.get_str("user_agent").ok().map(|s| s.to_string()),
        timestamp: activity_doc
            .get_datetime("timestamp")
            .map(|dt| dt.try_to_rfc3339_string().unwrap_or_default())
            .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
        metadata: activity_doc
            .get_document("metadata")
            .ok()
            .and_then(|d| serde_json::from_str(&d.to_string()).ok()),
    })
}

/// Extract UserBasicInfo from a BSON user document.
pub fn extract_user_basic_info(user: &Document) -> Result<UserBasicInfo, String> {
    let user_id = user
        .get_object_id("_id")
        .map(|oid| oid.to_hex())
        .map_err(|_| "Document missing valid _id field".to_string())?;

    Ok(UserBasicInfo {
        _id: user_id,
        email: user.get_str("email").unwrap_or("").to_string(),
        name: user.get_str("name").unwrap_or("").to_string(),
        role: user.get_str("role").unwrap_or("customer").to_string(),
        profile_picture: user.get_str("profilePicture").ok().map(|s| s.to_string()),
        use_gravatar: user.get_bool("useGravatar").ok(),
        location: user.get_str("location").ok().map(|s| s.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Permission / role logic
// ---------------------------------------------------------------------------

/// Check whether the given role and role_type indicate admin privileges.
pub fn is_admin(role: &str, role_type: &str) -> bool {
    role == "admin" || role_type == "admin"
}

/// Determine the target user ID for profile lookup.
///
/// Admins can look up any user by `user_id` or `email` query params.
/// Regular users always see their own profile.
pub fn determine_target_user_id(
    query_user_id: Option<&str>,
    query_email: Option<&str>,
    claims_user_id: &str,
    claims_role: &str,
    claims_role_type: &str,
) -> String {
    if (query_user_id.is_some() || query_email.is_some())
        && is_admin(claims_role, claims_role_type)
    {
        query_user_id
            .unwrap_or_else(|| query_email.unwrap_or(claims_user_id))
            .to_string()
    } else {
        claims_user_id.to_string()
    }
}

/// Return the canonical list of system roles with their permissions.
pub fn get_role_definitions() -> Vec<RoleInfo> {
    vec![
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
    ]
}

/// Find the permissions for a given role name from the canonical role list.
pub fn get_permissions_for_role(role: &str) -> Option<Vec<String>> {
    get_role_definitions()
        .into_iter()
        .find(|r| r.name == role)
        .map(|r| r.permissions)
}

// ---------------------------------------------------------------------------
// Pagination helpers
// ---------------------------------------------------------------------------

/// Parse page/limit from optional query parameters with clamping.
///
/// Returns `(page, limit, skip)`.
/// - `page` defaults to 1, minimum 1.
/// - `limit` defaults to `default_limit`, clamped to `[1, max_limit]`.
/// - `skip` = `(page - 1) * limit`.
pub fn parse_pagination(
    page: Option<u32>,
    limit: Option<u32>,
    default_limit: u32,
    max_limit: u32,
) -> (u32, u32, u64) {
    let page = page.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(default_limit).clamp(1, max_limit);
    let skip = ((page - 1) as u64) * (limit as u64);
    (page, limit, skip)
}

/// Compute total pages and build a PaginationInfo struct.
pub fn compute_pagination_info(page: u32, limit: u32, total: u64) -> PaginationInfo {
    let total_pages = if limit == 0 {
        0
    } else {
        ((total as f64) / (limit as f64)).ceil() as u32
    };
    PaginationInfo {
        page,
        limit,
        total,
        total_pages,
        has_next: page < total_pages,
        has_prev: page > 1,
    }
}

// ---------------------------------------------------------------------------
// Search / filter builders
// ---------------------------------------------------------------------------

/// Build a MongoDB search filter document from admin search query parameters.
///
/// - `q`: searches name and email with case-insensitive regex.
/// - `role`: exact match on role field.
pub fn build_search_filter(q: Option<&str>, role: Option<&str>) -> Document {
    let mut filter = doc! {};

    if let Some(q) = q {
        let trimmed = q.trim();
        if !trimmed.is_empty() {
            let escaped = crate::utils::security::escape_regex(trimmed);
            filter.insert(
                "$or",
                vec![
                    doc! { "name": { "$regex": &escaped, "$options": "i" } },
                    doc! { "email": { "$regex": &escaped, "$options": "i" } },
                ],
            );
        }
    }

    if let Some(role) = role {
        let trimmed = role.trim();
        if !trimmed.is_empty() {
            filter.insert("role", trimmed);
        }
    }

    filter
}

/// Build a sort document from optional sort field and order.
///
/// Defaults to `createdAt` descending.
pub fn build_sort_doc(sort: Option<&str>, order: Option<&str>) -> Document {
    let sort_field = sort.unwrap_or("createdAt");
    let sort_order: i32 = match order {
        Some("asc") => 1,
        _ => -1,
    };
    doc! { sort_field: sort_order }
}

/// Build a MongoDB filter for the user admin lookup.
///
/// Returns a filter document or an error string if the ObjectId is invalid.
pub fn build_admin_lookup_filter(
    user_id: Option<&str>,
    email: Option<&str>,
    claims_user_id: &str,
) -> Result<Document, String> {
    if let Some(uid) = user_id {
        let oid = ObjectId::parse_str(uid).map_err(|_| "Invalid user ID format".to_string())?;
        Ok(doc! { "_id": oid })
    } else if let Some(em) = email {
        Ok(doc! { "email": em })
    } else {
        let oid = ObjectId::parse_str(claims_user_id)
            .map_err(|_| "Invalid user ID format".to_string())?;
        Ok(doc! { "_id": oid })
    }
}

/// Build a MongoDB filter for activity logs.
///
/// Supports filtering by action type and date range (RFC 3339).
pub fn build_activity_filter(
    user_id: &str,
    action: Option<&str>,
    start_date: Option<&str>,
    end_date: Option<&str>,
) -> Document {
    let mut filter = doc! { "user_id": user_id };

    if let Some(action) = action {
        let trimmed = action.trim();
        if !trimmed.is_empty() {
            filter.insert("action", trimmed);
        }
    }

    if start_date.is_some() || end_date.is_some() {
        let mut date_filter = doc! {};

        if let Some(sd) = start_date {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(sd) {
                date_filter
                    .insert("$gte", mongodb::bson::DateTime::from_millis(dt.timestamp_millis()));
            }
        }

        if let Some(ed) = end_date {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ed) {
                date_filter
                    .insert("$lte", mongodb::bson::DateTime::from_millis(dt.timestamp_millis()));
            }
        }

        if !date_filter.is_empty() {
            filter.insert("timestamp", date_filter);
        }
    }

    filter
}

// ---------------------------------------------------------------------------
// Update document builders
// ---------------------------------------------------------------------------

/// Build a BSON update document from an AdminUserUpdateRequest.
///
/// Only includes fields that are `Some` in the request.
/// Always includes `updatedAt`.
pub fn build_admin_update_fields(body: &AdminUserUpdateRequest) -> Document {
    let mut update_doc = doc! {
        "updatedAt": mongodb::bson::DateTime::now()
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

    update_doc
}

// ---------------------------------------------------------------------------
// Settings update helpers
// ---------------------------------------------------------------------------

/// Build a success message for settings updates.
///
/// If account changes include email and/or password changes, appends them.
pub fn build_settings_success_message(
    email_changed: bool,
    password_changed: bool,
) -> String {
    let mut msg = "Settings updated successfully".to_string();
    let mut changes = Vec::new();
    if email_changed {
        changes.push("email");
    }
    if password_changed {
        changes.push("password");
    }
    if !changes.is_empty() {
        msg.push_str(&format!(". {} updated.", changes.join(" and ")));
    }
    msg
}

// ---------------------------------------------------------------------------
// File upload validation
// ---------------------------------------------------------------------------

/// Validate that a file's size is within the allowed maximum (in bytes).
pub fn validate_file_size(size: usize, max_bytes: usize) -> Result<(), String> {
    if size > max_bytes {
        Err(format!(
            "File size too large. Maximum size is {}MB.",
            max_bytes / (1024 * 1024)
        ))
    } else {
        Ok(())
    }
}

/// Validate that a content type string represents an image.
pub fn validate_image_content_type(content_type: Option<&str>) -> Result<(), String> {
    match content_type {
        Some(ct) if ct.starts_with("image/") => Ok(()),
        Some(_) => Err("Only image files are allowed".to_string()),
        None => Ok(()), // If no content type provided, allow it (will be checked elsewhere)
    }
}

/// Format a cache key for user profiles.
pub fn profile_cache_key(user_id: &str) -> String {
    format!("user:profile:{}", user_id)
}

/// Format a cache key for user settings.
pub fn settings_cache_key(user_id: &str) -> String {
    format!("user:settings:{}", user_id)
}

// ---------------------------------------------------------------------------
// Validation helpers used in handlers
// ---------------------------------------------------------------------------

/// Collect validation error messages from a `validator::ValidationErrors`.
pub fn collect_validation_errors(errors: &validator::ValidationErrors) -> String {
    let msgs: Vec<String> = errors
        .field_errors()
        .values()
        .flat_map(|field_errors| {
            field_errors.iter().map(|e| {
                e.message
                    .as_ref()
                    .unwrap_or(&"Validation error".into())
                    .to_string()
            })
        })
        .collect();
    msgs.join(", ")
}

/// Parse an ObjectId from a string, returning a user-friendly error.
pub fn parse_object_id(id: &str) -> Result<ObjectId, String> {
    ObjectId::parse_str(id).map_err(|_| "Invalid user ID format".to_string())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{doc, oid::ObjectId, DateTime as BsonDateTime};

    // -----------------------------------------------------------------------
    // standardize_user_doc
    // -----------------------------------------------------------------------

    #[test]
    fn test_standardize_user_doc_full_fields() {
        let oid = ObjectId::new();
        let now = BsonDateTime::now();
        let user_doc = doc! {
            "_id": oid,
            "email": "alice@example.com",
            "name": "Alice",
            "role": "admin",
            "isActive": true,
            "emailVerified": true,
            "createdAt": now,
            "updatedAt": now,
            "phone": "+1234567890",
            "company": "ACME",
            "department": "Engineering",
            "position": "CTO",
            "username": "alice42",
            "profilePicture": "https://example.com/pic.jpg",
            "useGravatar": false,
            "location": "NYC",
        };

        let user = standardize_user_doc(&user_doc).unwrap();
        assert_eq!(user._id, oid.to_hex());
        assert_eq!(user.id, oid.to_hex());
        assert_eq!(user.email, "alice@example.com");
        assert_eq!(user.name, "Alice");
        assert_eq!(user.role, "admin");
        assert!(user.is_active);
        assert!(user.email_verified);
        assert_eq!(user.phone, Some("+1234567890".to_string()));
        assert_eq!(user.company, Some("ACME".to_string()));
        assert_eq!(user.department, Some("Engineering".to_string()));
        assert_eq!(user.position, Some("CTO".to_string()));
        assert_eq!(user.username, Some("alice42".to_string()));
        assert_eq!(user.profile_picture, Some("https://example.com/pic.jpg".to_string()));
        assert_eq!(user.use_gravatar, Some(false));
        assert_eq!(user.location, Some("NYC".to_string()));
    }

    #[test]
    fn test_standardize_user_doc_minimal_fields() {
        let oid = ObjectId::new();
        let user_doc = doc! { "_id": oid };

        let user = standardize_user_doc(&user_doc).unwrap();
        assert_eq!(user.email, "");
        assert_eq!(user.name, "");
        assert_eq!(user.role, "customer"); // default
        assert!(user.is_active); // default true
        assert!(!user.email_verified); // default false
        assert!(user.phone.is_none());
        assert!(user.company.is_none());
        assert!(user.department.is_none());
        assert!(user.position.is_none());
        assert!(user.username.is_none());
        assert!(user.profile_picture.is_none());
        assert!(user.use_gravatar.is_none());
        assert!(user.location.is_none());
    }

    #[test]
    fn test_standardize_user_doc_missing_id() {
        let user_doc = doc! { "email": "noid@example.com" };
        let result = standardize_user_doc(&user_doc);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing"));
    }

    #[test]
    fn test_standardize_user_doc_string_id_fails() {
        let user_doc = doc! { "_id": "not_an_objectid" };
        let result = standardize_user_doc(&user_doc);
        assert!(result.is_err());
    }

    #[test]
    fn test_standardize_user_doc_null_id_fails() {
        let user_doc = doc! { "_id": mongodb::bson::Bson::Null };
        let result = standardize_user_doc(&user_doc);
        assert!(result.is_err());
    }

    #[test]
    fn test_standardize_user_doc_int_id_fails() {
        let user_doc = doc! { "_id": 12345 };
        let result = standardize_user_doc(&user_doc);
        assert!(result.is_err());
    }

    #[test]
    fn test_standardize_user_doc_defaults_role_to_customer() {
        let oid = ObjectId::new();
        let user_doc = doc! { "_id": oid, "email": "x@y.com" };
        let user = standardize_user_doc(&user_doc).unwrap();
        assert_eq!(user.role, "customer");
    }

    #[test]
    fn test_standardize_user_doc_defaults_is_active_to_true() {
        let oid = ObjectId::new();
        let user_doc = doc! { "_id": oid };
        let user = standardize_user_doc(&user_doc).unwrap();
        assert!(user.is_active);
    }

    #[test]
    fn test_standardize_user_doc_is_active_false() {
        let oid = ObjectId::new();
        let user_doc = doc! { "_id": oid, "isActive": false };
        let user = standardize_user_doc(&user_doc).unwrap();
        assert!(!user.is_active);
    }

    #[test]
    fn test_standardize_user_doc_email_verified_true() {
        let oid = ObjectId::new();
        let user_doc = doc! { "_id": oid, "emailVerified": true };
        let user = standardize_user_doc(&user_doc).unwrap();
        assert!(user.email_verified);
    }

    // -----------------------------------------------------------------------
    // standardize_activity_doc
    // -----------------------------------------------------------------------

    #[test]
    fn test_standardize_activity_doc_full() {
        let oid = ObjectId::new();
        let ts = BsonDateTime::now();
        let activity_doc = doc! {
            "_id": oid,
            "user_id": "user_abc",
            "action": "login",
            "resource": "session",
            "resource_id": "sess_123",
            "ip_address": "10.0.0.1",
            "user_agent": "Mozilla/5.0",
            "timestamp": ts,
            "metadata": { "browser": "Chrome" },
        };

        let activity = standardize_activity_doc(&activity_doc).unwrap();
        assert_eq!(activity.id, oid.to_hex());
        assert_eq!(activity.user_id, "user_abc");
        assert_eq!(activity.action, "login");
        assert_eq!(activity.resource, Some("session".to_string()));
        assert_eq!(activity.resource_id, Some("sess_123".to_string()));
        assert_eq!(activity.ip_address, Some("10.0.0.1".to_string()));
        assert_eq!(activity.user_agent, Some("Mozilla/5.0".to_string()));
        assert!(activity.metadata.is_some());
    }

    #[test]
    fn test_standardize_activity_doc_minimal() {
        let oid = ObjectId::new();
        let activity_doc = doc! { "_id": oid };

        let activity = standardize_activity_doc(&activity_doc).unwrap();
        assert_eq!(activity.user_id, "");
        assert_eq!(activity.action, "");
        assert!(activity.resource.is_none());
        assert!(activity.resource_id.is_none());
        assert!(activity.ip_address.is_none());
        assert!(activity.user_agent.is_none());
        assert!(activity.metadata.is_none());
    }

    #[test]
    fn test_standardize_activity_doc_missing_id() {
        let activity_doc = doc! { "action": "login" };
        let result = standardize_activity_doc(&activity_doc);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // extract_user_basic_info
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_user_basic_info_full() {
        let oid = ObjectId::new();
        let user_doc = doc! {
            "_id": oid,
            "email": "bob@example.com",
            "name": "Bob",
            "role": "editor",
            "profilePicture": "https://example.com/bob.jpg",
            "useGravatar": true,
            "location": "London",
        };

        let info = extract_user_basic_info(&user_doc).unwrap();
        assert_eq!(info._id, oid.to_hex());
        assert_eq!(info.email, "bob@example.com");
        assert_eq!(info.name, "Bob");
        assert_eq!(info.role, "editor");
        assert_eq!(info.profile_picture, Some("https://example.com/bob.jpg".to_string()));
        assert_eq!(info.use_gravatar, Some(true));
        assert_eq!(info.location, Some("London".to_string()));
    }

    #[test]
    fn test_extract_user_basic_info_minimal() {
        let oid = ObjectId::new();
        let user_doc = doc! { "_id": oid };

        let info = extract_user_basic_info(&user_doc).unwrap();
        assert_eq!(info.email, "");
        assert_eq!(info.name, "");
        assert_eq!(info.role, "customer");
        assert!(info.profile_picture.is_none());
        assert!(info.use_gravatar.is_none());
        assert!(info.location.is_none());
    }

    #[test]
    fn test_extract_user_basic_info_missing_id() {
        let user_doc = doc! { "email": "test@test.com" };
        let result = extract_user_basic_info(&user_doc);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // is_admin
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_admin_by_role() {
        assert!(is_admin("admin", "customer"));
    }

    #[test]
    fn test_is_admin_by_role_type() {
        assert!(is_admin("customer", "admin"));
    }

    #[test]
    fn test_is_admin_both() {
        assert!(is_admin("admin", "admin"));
    }

    #[test]
    fn test_is_not_admin_customer() {
        assert!(!is_admin("customer", "customer"));
    }

    #[test]
    fn test_is_not_admin_editor() {
        assert!(!is_admin("editor", "editor"));
    }

    #[test]
    fn test_is_not_admin_subscriber() {
        assert!(!is_admin("subscriber", "subscriber"));
    }

    #[test]
    fn test_is_not_admin_empty() {
        assert!(!is_admin("", ""));
    }

    // -----------------------------------------------------------------------
    // determine_target_user_id
    // -----------------------------------------------------------------------

    #[test]
    fn test_target_user_id_regular_user_sees_self() {
        let result = determine_target_user_id(None, None, "my_id", "customer", "customer");
        assert_eq!(result, "my_id");
    }

    #[test]
    fn test_target_user_id_regular_user_ignores_query() {
        // Non-admin providing user_id query param should still see their own profile
        let result =
            determine_target_user_id(Some("other_id"), None, "my_id", "customer", "customer");
        assert_eq!(result, "my_id");
    }

    #[test]
    fn test_target_user_id_admin_with_user_id() {
        let result =
            determine_target_user_id(Some("target_id"), None, "admin_id", "admin", "admin");
        assert_eq!(result, "target_id");
    }

    #[test]
    fn test_target_user_id_admin_with_email_fallback() {
        let result =
            determine_target_user_id(None, Some("target@e.com"), "admin_id", "admin", "admin");
        assert_eq!(result, "target@e.com");
    }

    #[test]
    fn test_target_user_id_admin_user_id_preferred_over_email() {
        let result = determine_target_user_id(
            Some("uid123"),
            Some("email@e.com"),
            "admin_id",
            "admin",
            "admin",
        );
        assert_eq!(result, "uid123");
    }

    #[test]
    fn test_target_user_id_admin_no_query_sees_self() {
        let result = determine_target_user_id(None, None, "admin_id", "admin", "admin");
        assert_eq!(result, "admin_id");
    }

    #[test]
    fn test_target_user_id_admin_by_role_type_only() {
        let result = determine_target_user_id(
            Some("target_id"),
            None,
            "admin_id",
            "customer", // role is customer
            "admin",    // but role_type is admin
        );
        assert_eq!(result, "target_id");
    }

    // -----------------------------------------------------------------------
    // get_role_definitions / get_permissions_for_role
    // -----------------------------------------------------------------------

    #[test]
    fn test_role_definitions_has_four_roles() {
        let roles = get_role_definitions();
        assert_eq!(roles.len(), 4);
    }

    #[test]
    fn test_role_definitions_names() {
        let roles = get_role_definitions();
        let names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"admin"));
        assert!(names.contains(&"customer"));
        assert!(names.contains(&"editor"));
        assert!(names.contains(&"subscriber"));
    }

    #[test]
    fn test_admin_has_user_management_permission() {
        let perms = get_permissions_for_role("admin").unwrap();
        assert!(perms.contains(&"user_management".to_string()));
    }

    #[test]
    fn test_admin_has_system_settings_permission() {
        let perms = get_permissions_for_role("admin").unwrap();
        assert!(perms.contains(&"system_settings".to_string()));
    }

    #[test]
    fn test_customer_has_profile_edit_permission() {
        let perms = get_permissions_for_role("customer").unwrap();
        assert!(perms.contains(&"profile_edit".to_string()));
    }

    #[test]
    fn test_customer_no_admin_permission() {
        let perms = get_permissions_for_role("customer").unwrap();
        assert!(!perms.contains(&"admin".to_string()));
    }

    #[test]
    fn test_editor_has_content_edit() {
        let perms = get_permissions_for_role("editor").unwrap();
        assert!(perms.contains(&"content_edit".to_string()));
    }

    #[test]
    fn test_subscriber_read_only() {
        let perms = get_permissions_for_role("subscriber").unwrap();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0], "read");
    }

    #[test]
    fn test_unknown_role_returns_none() {
        assert!(get_permissions_for_role("superuser").is_none());
    }

    #[test]
    fn test_empty_role_returns_none() {
        assert!(get_permissions_for_role("").is_none());
    }

    // -----------------------------------------------------------------------
    // parse_pagination
    // -----------------------------------------------------------------------

    #[test]
    fn test_pagination_defaults() {
        let (page, limit, skip) = parse_pagination(None, None, 10, 100);
        assert_eq!(page, 1);
        assert_eq!(limit, 10);
        assert_eq!(skip, 0);
    }

    #[test]
    fn test_pagination_explicit_values() {
        let (page, limit, skip) = parse_pagination(Some(3), Some(25), 10, 100);
        assert_eq!(page, 3);
        assert_eq!(limit, 25);
        assert_eq!(skip, 50);
    }

    #[test]
    fn test_pagination_page_zero_becomes_one() {
        let (page, _, _) = parse_pagination(Some(0), None, 10, 100);
        assert_eq!(page, 1);
    }

    #[test]
    fn test_pagination_limit_clamped_to_max() {
        let (_, limit, _) = parse_pagination(None, Some(500), 10, 100);
        assert_eq!(limit, 100);
    }

    #[test]
    fn test_pagination_limit_clamped_to_min() {
        let (_, limit, _) = parse_pagination(None, Some(0), 10, 100);
        assert_eq!(limit, 1);
    }

    #[test]
    fn test_pagination_skip_page_2() {
        let (_, _, skip) = parse_pagination(Some(2), Some(20), 10, 100);
        assert_eq!(skip, 20);
    }

    #[test]
    fn test_pagination_skip_page_1() {
        let (_, _, skip) = parse_pagination(Some(1), Some(10), 10, 100);
        assert_eq!(skip, 0);
    }

    #[test]
    fn test_pagination_large_page() {
        let (page, limit, skip) = parse_pagination(Some(100), Some(10), 10, 100);
        assert_eq!(page, 100);
        assert_eq!(limit, 10);
        assert_eq!(skip, 990);
    }

    // -----------------------------------------------------------------------
    // compute_pagination_info
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_pagination_basic() {
        let info = compute_pagination_info(1, 10, 25);
        assert_eq!(info.page, 1);
        assert_eq!(info.limit, 10);
        assert_eq!(info.total, 25);
        assert_eq!(info.total_pages, 3);
        assert!(info.has_next);
        assert!(!info.has_prev);
    }

    #[test]
    fn test_compute_pagination_last_page() {
        let info = compute_pagination_info(3, 10, 25);
        assert_eq!(info.total_pages, 3);
        assert!(!info.has_next);
        assert!(info.has_prev);
    }

    #[test]
    fn test_compute_pagination_middle_page() {
        let info = compute_pagination_info(2, 10, 50);
        assert!(info.has_next);
        assert!(info.has_prev);
    }

    #[test]
    fn test_compute_pagination_single_page() {
        let info = compute_pagination_info(1, 10, 5);
        assert_eq!(info.total_pages, 1);
        assert!(!info.has_next);
        assert!(!info.has_prev);
    }

    #[test]
    fn test_compute_pagination_empty_result() {
        let info = compute_pagination_info(1, 10, 0);
        assert_eq!(info.total_pages, 0);
        assert!(!info.has_next);
        assert!(!info.has_prev);
    }

    #[test]
    fn test_compute_pagination_exact_division() {
        let info = compute_pagination_info(1, 10, 30);
        assert_eq!(info.total_pages, 3);
    }

    #[test]
    fn test_compute_pagination_zero_limit() {
        let info = compute_pagination_info(1, 0, 10);
        assert_eq!(info.total_pages, 0);
    }

    // -----------------------------------------------------------------------
    // build_search_filter
    // -----------------------------------------------------------------------

    #[test]
    fn test_search_filter_empty() {
        let filter = build_search_filter(None, None);
        assert!(filter.is_empty());
    }

    #[test]
    fn test_search_filter_with_query() {
        let filter = build_search_filter(Some("john"), None);
        assert!(filter.contains_key("$or"));
    }

    #[test]
    fn test_search_filter_with_empty_query() {
        let filter = build_search_filter(Some("  "), None);
        assert!(!filter.contains_key("$or"), "Whitespace-only query should be ignored");
    }

    #[test]
    fn test_search_filter_with_role() {
        let filter = build_search_filter(None, Some("admin"));
        assert_eq!(filter.get_str("role").unwrap(), "admin");
    }

    #[test]
    fn test_search_filter_with_empty_role() {
        let filter = build_search_filter(None, Some("  "));
        assert!(!filter.contains_key("role"), "Whitespace-only role should be ignored");
    }

    #[test]
    fn test_search_filter_with_both() {
        let filter = build_search_filter(Some("test"), Some("editor"));
        assert!(filter.contains_key("$or"));
        assert_eq!(filter.get_str("role").unwrap(), "editor");
    }

    #[test]
    fn test_search_filter_escapes_regex_chars() {
        // The query "user.*" should be escaped so the dot and star are literal
        let filter = build_search_filter(Some("user.*"), None);
        assert!(filter.contains_key("$or"));
        // The filter document should contain escaped regex
        let or_array = filter.get_array("$or").unwrap();
        let first = or_array[0].as_document().unwrap();
        let name_regex = first.get_document("name").unwrap();
        let regex_str = name_regex.get_str("$regex").unwrap();
        assert!(regex_str.contains(r"\."), "Dot should be escaped");
        assert!(regex_str.contains(r"\*"), "Star should be escaped");
    }

    // -----------------------------------------------------------------------
    // build_sort_doc
    // -----------------------------------------------------------------------

    #[test]
    fn test_sort_doc_defaults() {
        let sort = build_sort_doc(None, None);
        assert_eq!(sort.get_i32("createdAt").unwrap(), -1);
    }

    #[test]
    fn test_sort_doc_asc() {
        let sort = build_sort_doc(Some("name"), Some("asc"));
        assert_eq!(sort.get_i32("name").unwrap(), 1);
    }

    #[test]
    fn test_sort_doc_desc() {
        let sort = build_sort_doc(Some("email"), Some("desc"));
        assert_eq!(sort.get_i32("email").unwrap(), -1);
    }

    #[test]
    fn test_sort_doc_unknown_order_defaults_desc() {
        let sort = build_sort_doc(Some("name"), Some("random"));
        assert_eq!(sort.get_i32("name").unwrap(), -1);
    }

    #[test]
    fn test_sort_doc_custom_field_default_order() {
        let sort = build_sort_doc(Some("updatedAt"), None);
        assert_eq!(sort.get_i32("updatedAt").unwrap(), -1);
    }

    // -----------------------------------------------------------------------
    // build_admin_lookup_filter
    // -----------------------------------------------------------------------

    #[test]
    fn test_admin_lookup_by_user_id() {
        let oid_str = "507f1f77bcf86cd799439011";
        let filter = build_admin_lookup_filter(Some(oid_str), None, "claims_id").unwrap();
        assert!(filter.contains_key("_id"));
    }

    #[test]
    fn test_admin_lookup_by_email() {
        let filter = build_admin_lookup_filter(None, Some("alice@e.com"), "claims_id").unwrap();
        assert_eq!(filter.get_str("email").unwrap(), "alice@e.com");
    }

    #[test]
    fn test_admin_lookup_fallback_to_claims() {
        let claims_id = "507f1f77bcf86cd799439011";
        let filter = build_admin_lookup_filter(None, None, claims_id).unwrap();
        assert!(filter.contains_key("_id"));
    }

    #[test]
    fn test_admin_lookup_invalid_user_id() {
        let result = build_admin_lookup_filter(Some("not_valid"), None, "claims");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid user ID"));
    }

    #[test]
    fn test_admin_lookup_invalid_claims_id() {
        let result = build_admin_lookup_filter(None, None, "bad_id");
        assert!(result.is_err());
    }

    #[test]
    fn test_admin_lookup_user_id_takes_priority() {
        let oid_str = "507f1f77bcf86cd799439011";
        let filter =
            build_admin_lookup_filter(Some(oid_str), Some("email@e.com"), "claims_id").unwrap();
        // Should have _id, not email, because user_id takes priority
        assert!(filter.contains_key("_id"));
        assert!(!filter.contains_key("email"));
    }

    // -----------------------------------------------------------------------
    // build_activity_filter
    // -----------------------------------------------------------------------

    #[test]
    fn test_activity_filter_basic() {
        let filter = build_activity_filter("user_123", None, None, None);
        assert_eq!(filter.get_str("user_id").unwrap(), "user_123");
        assert!(!filter.contains_key("action"));
        assert!(!filter.contains_key("timestamp"));
    }

    #[test]
    fn test_activity_filter_with_action() {
        let filter = build_activity_filter("user_123", Some("login"), None, None);
        assert_eq!(filter.get_str("action").unwrap(), "login");
    }

    #[test]
    fn test_activity_filter_empty_action_ignored() {
        let filter = build_activity_filter("user_123", Some("  "), None, None);
        assert!(!filter.contains_key("action"));
    }

    #[test]
    fn test_activity_filter_with_start_date() {
        let filter = build_activity_filter(
            "user_123",
            None,
            Some("2025-01-01T00:00:00Z"),
            None,
        );
        assert!(filter.contains_key("timestamp"));
        let ts = filter.get_document("timestamp").unwrap();
        assert!(ts.contains_key("$gte"));
        assert!(!ts.contains_key("$lte"));
    }

    #[test]
    fn test_activity_filter_with_end_date() {
        let filter = build_activity_filter(
            "user_123",
            None,
            None,
            Some("2025-12-31T23:59:59Z"),
        );
        assert!(filter.contains_key("timestamp"));
        let ts = filter.get_document("timestamp").unwrap();
        assert!(!ts.contains_key("$gte"));
        assert!(ts.contains_key("$lte"));
    }

    #[test]
    fn test_activity_filter_with_date_range() {
        let filter = build_activity_filter(
            "user_123",
            None,
            Some("2025-01-01T00:00:00Z"),
            Some("2025-12-31T23:59:59Z"),
        );
        let ts = filter.get_document("timestamp").unwrap();
        assert!(ts.contains_key("$gte"));
        assert!(ts.contains_key("$lte"));
    }

    #[test]
    fn test_activity_filter_invalid_date_ignored() {
        let filter = build_activity_filter("user_123", None, Some("not-a-date"), None);
        // Invalid dates should just not be inserted
        assert!(!filter.contains_key("timestamp"));
    }

    #[test]
    fn test_activity_filter_with_action_and_dates() {
        let filter = build_activity_filter(
            "user_123",
            Some("profile_update"),
            Some("2025-06-01T00:00:00Z"),
            Some("2025-06-30T23:59:59Z"),
        );
        assert_eq!(filter.get_str("action").unwrap(), "profile_update");
        assert!(filter.contains_key("timestamp"));
    }

    // -----------------------------------------------------------------------
    // build_admin_update_fields
    // -----------------------------------------------------------------------

    #[test]
    fn test_admin_update_fields_all() {
        let body = AdminUserUpdateRequest {
            name: Some("New Name".to_string()),
            email: Some("New@Example.com".to_string()),
            role: Some("editor".to_string()),
            is_active: Some(false),
            email_verified: Some(true),
        };
        let doc = build_admin_update_fields(&body);
        assert_eq!(doc.get_str("name").unwrap(), "New Name");
        assert_eq!(doc.get_str("email").unwrap(), "new@example.com"); // lowercased
        assert_eq!(doc.get_str("role").unwrap(), "editor");
        assert!(!doc.get_bool("isActive").unwrap());
        assert!(doc.get_bool("emailVerified").unwrap());
        assert!(doc.contains_key("updatedAt"));
    }

    #[test]
    fn test_admin_update_fields_name_only() {
        let body = AdminUserUpdateRequest {
            name: Some("Just Name".to_string()),
            email: None,
            role: None,
            is_active: None,
            email_verified: None,
        };
        let doc = build_admin_update_fields(&body);
        assert!(doc.contains_key("name"));
        assert!(!doc.contains_key("email"));
        assert!(!doc.contains_key("role"));
        assert!(!doc.contains_key("isActive"));
        assert!(!doc.contains_key("emailVerified"));
    }

    #[test]
    fn test_admin_update_fields_none() {
        let body = AdminUserUpdateRequest {
            name: None,
            email: None,
            role: None,
            is_active: None,
            email_verified: None,
        };
        let doc = build_admin_update_fields(&body);
        // Only updatedAt should be present
        assert!(doc.contains_key("updatedAt"));
        assert!(!doc.contains_key("name"));
        assert!(!doc.contains_key("email"));
    }

    #[test]
    fn test_admin_update_fields_email_lowercased() {
        let body = AdminUserUpdateRequest {
            name: None,
            email: Some("UPPER@CASE.COM".to_string()),
            role: None,
            is_active: None,
            email_verified: None,
        };
        let doc = build_admin_update_fields(&body);
        assert_eq!(doc.get_str("email").unwrap(), "upper@case.com");
    }

    // -----------------------------------------------------------------------
    // build_settings_success_message
    // -----------------------------------------------------------------------

    #[test]
    fn test_settings_message_no_changes() {
        let msg = build_settings_success_message(false, false);
        assert_eq!(msg, "Settings updated successfully");
    }

    #[test]
    fn test_settings_message_email_changed() {
        let msg = build_settings_success_message(true, false);
        assert_eq!(msg, "Settings updated successfully. email updated.");
    }

    #[test]
    fn test_settings_message_password_changed() {
        let msg = build_settings_success_message(false, true);
        assert_eq!(msg, "Settings updated successfully. password updated.");
    }

    #[test]
    fn test_settings_message_both_changed() {
        let msg = build_settings_success_message(true, true);
        assert_eq!(msg, "Settings updated successfully. email and password updated.");
    }

    // -----------------------------------------------------------------------
    // validate_file_size
    // -----------------------------------------------------------------------

    #[test]
    fn test_file_size_within_limit() {
        assert!(validate_file_size(1024, 5 * 1024 * 1024).is_ok());
    }

    #[test]
    fn test_file_size_at_limit() {
        let max = 5 * 1024 * 1024;
        assert!(validate_file_size(max, max).is_ok());
    }

    #[test]
    fn test_file_size_over_limit() {
        let max = 5 * 1024 * 1024;
        let result = validate_file_size(max + 1, max);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("5MB"));
    }

    #[test]
    fn test_file_size_zero() {
        assert!(validate_file_size(0, 5 * 1024 * 1024).is_ok());
    }

    #[test]
    fn test_file_size_much_larger() {
        let result = validate_file_size(100 * 1024 * 1024, 5 * 1024 * 1024);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // validate_image_content_type
    // -----------------------------------------------------------------------

    #[test]
    fn test_valid_image_jpeg() {
        assert!(validate_image_content_type(Some("image/jpeg")).is_ok());
    }

    #[test]
    fn test_valid_image_png() {
        assert!(validate_image_content_type(Some("image/png")).is_ok());
    }

    #[test]
    fn test_valid_image_gif() {
        assert!(validate_image_content_type(Some("image/gif")).is_ok());
    }

    #[test]
    fn test_valid_image_webp() {
        assert!(validate_image_content_type(Some("image/webp")).is_ok());
    }

    #[test]
    fn test_invalid_content_type_text() {
        let result = validate_image_content_type(Some("text/plain"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Only image files"));
    }

    #[test]
    fn test_invalid_content_type_pdf() {
        assert!(validate_image_content_type(Some("application/pdf")).is_err());
    }

    #[test]
    fn test_invalid_content_type_json() {
        assert!(validate_image_content_type(Some("application/json")).is_err());
    }

    #[test]
    fn test_none_content_type_allowed() {
        assert!(validate_image_content_type(None).is_ok());
    }

    // -----------------------------------------------------------------------
    // cache key helpers
    // -----------------------------------------------------------------------

    #[test]
    fn test_profile_cache_key_format() {
        assert_eq!(profile_cache_key("abc123"), "user:profile:abc123");
    }

    #[test]
    fn test_settings_cache_key_format() {
        assert_eq!(settings_cache_key("abc123"), "user:settings:abc123");
    }

    #[test]
    fn test_cache_key_with_objectid() {
        let key = profile_cache_key("507f1f77bcf86cd799439011");
        assert_eq!(key, "user:profile:507f1f77bcf86cd799439011");
    }

    // -----------------------------------------------------------------------
    // collect_validation_errors
    // -----------------------------------------------------------------------

    #[test]
    fn test_collect_validation_errors_with_errors() {
        use validator::Validate;
        use crate::models::user::PasswordChangeRequest;

        let req = PasswordChangeRequest {
            current_password: "".to_string(),
            new_password: "short".to_string(),
        };
        let errs = req.validate().unwrap_err();
        let msg = collect_validation_errors(&errs);
        // Should contain at least one message
        assert!(!msg.is_empty());
    }

    #[test]
    fn test_collect_validation_errors_multiple() {
        use validator::Validate;
        use crate::models::user::PasswordChangeRequest;

        let req = PasswordChangeRequest {
            current_password: "".to_string(), // fails min length 1
            new_password: "short".to_string(), // fails min length 8
        };
        let errs = req.validate().unwrap_err();
        let msg = collect_validation_errors(&errs);
        // Should be comma-separated
        // Both errors should be present (order may vary)
        assert!(msg.contains("password") || msg.contains("Validation error"));
    }

    // -----------------------------------------------------------------------
    // parse_object_id
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_object_id_valid() {
        let result = parse_object_id("507f1f77bcf86cd799439011");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().to_hex(), "507f1f77bcf86cd799439011");
    }

    #[test]
    fn test_parse_object_id_invalid() {
        let result = parse_object_id("not_valid");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid user ID"));
    }

    #[test]
    fn test_parse_object_id_empty() {
        assert!(parse_object_id("").is_err());
    }

    #[test]
    fn test_parse_object_id_too_short() {
        assert!(parse_object_id("507f1f77").is_err());
    }

    #[test]
    fn test_parse_object_id_non_hex() {
        assert!(parse_object_id("zzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }
}
