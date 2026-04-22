use serde::{Deserialize, Serialize};
use validator::Validate;

// Cached user profile data structure for Redis (following auth-service patterns)
#[derive(Serialize, Deserialize, Clone)]
pub struct CachedUserProfile {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub is_active: bool,
    pub email_verified: bool,
    pub created_at: String,
    pub updated_at: String,
    pub profile_picture: Option<String>,
    pub use_gravatar: Option<bool>,
    pub location: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
    pub department: Option<String>,
    pub position: Option<String>,
    pub settings: Option<UserSettings>,
    pub cached_at: u64,
    pub ttl: u64,
}

// User profile response (matches Node.js API exactly)
#[derive(Serialize, Deserialize)]
pub struct UserProfileResponse {
    pub success: bool,
    pub user: Option<StandardizedUser>,
    pub message: Option<String>,
}

// Standardized user format (following UserUtils.fromDatabase pattern from Node.js)
#[derive(Serialize, Deserialize, Clone)]
pub struct StandardizedUser {
    pub _id: String,
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    #[serde(rename = "isActive")]
    pub is_active: bool,
    #[serde(rename = "emailVerified")]
    pub email_verified: bool,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(rename = "profilePicture", skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<String>,
    #[serde(rename = "useGravatar", skip_serializing_if = "Option::is_none")]
    pub use_gravatar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

// User settings structure (matches Node.js API)
#[derive(Serialize, Deserialize, Clone, Validate)]
pub struct UserSettings {
    #[validate]
    pub notifications: NotificationSettings,
    #[validate(custom(function = "validate_theme", message = "Invalid theme"))]
    pub theme: String,
    #[validate(length(min = 2, max = 5, message = "Invalid language code"))]
    pub language: String,
    #[validate(length(min = 1, message = "Timezone is required"))]
    pub timezone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate]
    pub user: Option<UserBasicInfo>,
}

// Custom validation for theme
fn validate_theme(theme: &str) -> Result<(), validator::ValidationError> {
    let valid_themes = ["light", "dark", "auto"];
    if valid_themes.contains(&theme) {
        Ok(())
    } else {
        Err(validator::ValidationError::new("invalid_theme"))
    }
}

#[derive(Serialize, Deserialize, Clone, Validate)]
pub struct NotificationSettings {
    pub email: bool,
    pub sound: bool,
    pub desktop: bool,
}

#[derive(Serialize, Deserialize, Clone, Validate)]
pub struct UserBasicInfo {
    #[validate(length(min = 1, message = "User ID is required"))]
    pub _id: String,
    #[validate(email(message = "Invalid email format"))]
    pub email: String,
    #[validate(length(min = 1, message = "Name is required"))]
    pub name: String,
    #[validate(custom(function = "validate_user_role", message = "Invalid user role"))]
    pub role: String,
    #[serde(rename = "profilePicture", skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<String>,
    #[serde(rename = "useGravatar", skip_serializing_if = "Option::is_none")]
    pub use_gravatar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

// Settings response (matches Node.js API exactly)
#[derive(Serialize, Deserialize, Clone)]
pub struct SettingsResponse {
    pub success: bool,
    pub settings: Option<UserSettings>,
    pub message: Option<String>,
}

// Settings update request (matches Node.js API)
#[derive(Deserialize, Validate)]
pub struct SettingsUpdateRequest {
    #[validate]
    pub settings: UserSettings,
    #[serde(rename = "accountChanges", skip_serializing_if = "Option::is_none")]
    #[validate]
    pub account_changes: Option<AccountChanges>,
}

#[derive(Deserialize, Validate)]
pub struct AccountChanges {
    #[serde(rename = "currentPassword")]
    #[validate(length(min = 1, message = "Current password is required"))]
    pub current_password: String,
    #[serde(rename = "newEmail", skip_serializing_if = "Option::is_none")]
    #[validate(email(message = "Invalid email format"))]
    pub new_email: Option<String>,
    #[serde(rename = "newPassword", skip_serializing_if = "Option::is_none")]
    #[validate(length(min = 8, message = "Password must be at least 8 characters long"))]
    pub new_password: Option<String>,
}

// Profile picture upload response
#[derive(Serialize)]
pub struct ProfilePictureResponse {
    pub success: bool,
    pub message: String,
    #[serde(rename = "profilePicture", skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<String>,
}

// Password change request (matches Node.js API)
#[derive(Deserialize, Validate)]
pub struct PasswordChangeRequest {
    #[serde(rename = "currentPassword")]
    #[validate(length(min = 1, message = "Current password is required"))]
    pub current_password: String,
    #[serde(rename = "newPassword")]
    #[validate(length(min = 8, message = "New password must be at least 8 characters long"))]
    pub new_password: String,
}

// Password change response
#[derive(Serialize)]
pub struct PasswordChangeResponse {
    pub success: bool,
    pub message: String,
}

// User search query parameters
#[derive(Deserialize)]
pub struct UserSearchQuery {
    pub q: Option<String>,
    pub role: Option<String>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub sort: Option<String>,
    pub order: Option<String>,
}

// User search response
#[derive(Serialize)]
pub struct UserSearchResponse {
    pub success: bool,
    pub users: Vec<StandardizedUser>,
    pub pagination: PaginationInfo,
    pub message: Option<String>,
}

// Pagination information
#[derive(Serialize, Deserialize, Clone)]
pub struct PaginationInfo {
    pub page: u32,
    pub limit: u32,
    pub total: u64,
    #[serde(rename = "totalPages")]
    pub total_pages: u32,
    #[serde(rename = "hasNext")]
    pub has_next: bool,
    #[serde(rename = "hasPrev")]
    pub has_prev: bool,
}

// Admin user update request
#[derive(Deserialize, Validate)]
pub struct AdminUserUpdateRequest {
    #[validate(length(min = 1, message = "Name cannot be empty"))]
    pub name: Option<String>,
    #[validate(email(message = "Invalid email format"))]
    pub email: Option<String>,
    #[validate(custom(function = "validate_user_role", message = "Invalid user role"))]
    pub role: Option<String>,
    #[serde(rename = "isActive")]
    pub is_active: Option<bool>,
    #[serde(rename = "emailVerified")]
    pub email_verified: Option<bool>,
}

// Custom validation function for user roles
fn validate_user_role(role: &str) -> Result<(), validator::ValidationError> {
    let valid_roles = ["admin", "customer", "editor", "subscriber"];
    if valid_roles.contains(&role) {
        Ok(())
    } else {
        Err(validator::ValidationError::new("invalid_role"))
    }
}

// Default settings (matches Node.js implementation)
impl Default for UserSettings {
    fn default() -> Self {
        Self {
            notifications: NotificationSettings {
                email: true,
                sound: true,
                desktop: false,
            },
            theme: "light".to_string(),
            language: "en".to_string(),
            timezone: "UTC".to_string(),
            user: None,
        }
    }
}

// ========== Tests for model types ==========

#[cfg(test)]
mod tests {
    use super::*;
    use validator::Validate;

    // ---- CachedUserProfile serialization/deserialization ----

    #[test]
    fn test_cached_user_profile_roundtrip() {
        let profile = CachedUserProfile {
            id: "abc123".to_string(),
            email: "test@example.com".to_string(),
            name: "Test User".to_string(),
            role: "customer".to_string(),
            is_active: true,
            email_verified: true,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-06-01T00:00:00Z".to_string(),
            profile_picture: Some("https://example.com/pic.jpg".to_string()),
            use_gravatar: Some(false),
            location: Some("Grand Rapids, MI".to_string()),
            phone: Some("+1234567890".to_string()),
            company: Some("ACME Corp".to_string()),
            department: Some("Engineering".to_string()),
            position: Some("Developer".to_string()),
            settings: None,
            cached_at: 1700000000,
            ttl: 900,
        };

        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: CachedUserProfile = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "abc123");
        assert_eq!(deserialized.email, "test@example.com");
        assert_eq!(deserialized.name, "Test User");
        assert_eq!(deserialized.role, "customer");
        assert!(deserialized.is_active);
        assert!(deserialized.email_verified);
        assert_eq!(deserialized.profile_picture, Some("https://example.com/pic.jpg".to_string()));
        assert_eq!(deserialized.use_gravatar, Some(false));
        assert_eq!(deserialized.location, Some("Grand Rapids, MI".to_string()));
        assert_eq!(deserialized.cached_at, 1700000000);
        assert_eq!(deserialized.ttl, 900);
    }

    #[test]
    fn test_cached_user_profile_optional_fields_none() {
        let profile = CachedUserProfile {
            id: "id1".to_string(),
            email: "e@e.com".to_string(),
            name: "N".to_string(),
            role: "customer".to_string(),
            is_active: true,
            email_verified: false,
            created_at: "".to_string(),
            updated_at: "".to_string(),
            profile_picture: None,
            use_gravatar: None,
            location: None,
            phone: None,
            company: None,
            department: None,
            position: None,
            settings: None,
            cached_at: 0,
            ttl: 0,
        };

        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: CachedUserProfile = serde_json::from_str(&json).unwrap();
        assert!(deserialized.profile_picture.is_none());
        assert!(deserialized.use_gravatar.is_none());
        assert!(deserialized.location.is_none());
        assert!(deserialized.phone.is_none());
        assert!(deserialized.company.is_none());
    }

    // ---- StandardizedUser serde field renaming ----

    #[test]
    fn test_standardized_user_json_field_names() {
        let user = StandardizedUser {
            _id: "abc".to_string(),
            id: "abc".to_string(),
            email: "u@e.com".to_string(),
            name: "User".to_string(),
            role: "customer".to_string(),
            is_active: true,
            email_verified: true,
            created_at: "2025-01-01".to_string(),
            updated_at: "2025-01-02".to_string(),
            phone: None,
            company: None,
            department: None,
            position: None,
            username: None,
            profile_picture: Some("url".to_string()),
            use_gravatar: Some(true),
            location: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        // Check camelCase field names in serialized JSON
        assert!(json.contains("\"isActive\""), "is_active should serialize as isActive");
        assert!(json.contains("\"emailVerified\""), "email_verified should serialize as emailVerified");
        assert!(json.contains("\"createdAt\""), "created_at should serialize as createdAt");
        assert!(json.contains("\"updatedAt\""), "updated_at should serialize as updatedAt");
        assert!(json.contains("\"profilePicture\""), "profile_picture should serialize as profilePicture");
        assert!(json.contains("\"useGravatar\""), "use_gravatar should serialize as useGravatar");
    }

    #[test]
    fn test_standardized_user_skips_none_optional_fields() {
        let user = StandardizedUser {
            _id: "id".to_string(),
            id: "id".to_string(),
            email: "u@e.com".to_string(),
            name: "U".to_string(),
            role: "customer".to_string(),
            is_active: true,
            email_verified: false,
            created_at: "".to_string(),
            updated_at: "".to_string(),
            phone: None,
            company: None,
            department: None,
            position: None,
            username: None,
            profile_picture: None,
            use_gravatar: None,
            location: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(!json.contains("phone"), "None phone should be skipped");
        assert!(!json.contains("company"), "None company should be skipped");
        assert!(!json.contains("department"), "None department should be skipped");
        assert!(!json.contains("position"), "None position should be skipped");
        assert!(!json.contains("username"), "None username should be skipped");
        assert!(!json.contains("profilePicture"), "None profilePicture should be skipped");
        assert!(!json.contains("useGravatar"), "None useGravatar should be skipped");
        assert!(!json.contains("location"), "None location should be skipped");
    }

    // ---- UserSettings defaults and validation ----

    #[test]
    fn test_user_settings_default_values() {
        let settings = UserSettings::default();
        assert_eq!(settings.theme, "light");
        assert_eq!(settings.language, "en");
        assert_eq!(settings.timezone, "UTC");
        assert!(settings.notifications.email);
        assert!(settings.notifications.sound);
        assert!(!settings.notifications.desktop);
        assert!(settings.user.is_none());
    }

    #[test]
    fn test_user_settings_valid_themes() {
        for theme in &["light", "dark", "auto"] {
            let settings = UserSettings {
                theme: theme.to_string(),
                ..UserSettings::default()
            };
            assert!(settings.validate().is_ok(), "Theme '{}' should be valid", theme);
        }
    }

    #[test]
    fn test_user_settings_invalid_theme() {
        let settings = UserSettings {
            theme: "neon".to_string(),
            ..UserSettings::default()
        };
        assert!(settings.validate().is_err(), "Theme 'neon' should be invalid");
    }

    #[test]
    fn test_user_settings_language_too_short() {
        let settings = UserSettings {
            language: "x".to_string(),
            ..UserSettings::default()
        };
        assert!(settings.validate().is_err(), "1-char language code should fail");
    }

    #[test]
    fn test_user_settings_language_too_long() {
        let settings = UserSettings {
            language: "englsh".to_string(),
            ..UserSettings::default()
        };
        assert!(settings.validate().is_err(), "6-char language code should fail");
    }

    #[test]
    fn test_user_settings_empty_timezone() {
        let settings = UserSettings {
            timezone: "".to_string(),
            ..UserSettings::default()
        };
        assert!(settings.validate().is_err(), "Empty timezone should fail");
    }

    // ---- PaginationInfo ----

    #[test]
    fn test_pagination_info_serialization() {
        let pagination = PaginationInfo {
            page: 2,
            limit: 20,
            total: 100,
            total_pages: 5,
            has_next: true,
            has_prev: true,
        };

        let json = serde_json::to_string(&pagination).unwrap();
        assert!(json.contains("\"totalPages\""));
        assert!(json.contains("\"hasNext\""));
        assert!(json.contains("\"hasPrev\""));

        let deserialized: PaginationInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.page, 2);
        assert_eq!(deserialized.total_pages, 5);
        assert!(deserialized.has_next);
        assert!(deserialized.has_prev);
    }

    #[test]
    fn test_pagination_first_page() {
        let pagination = PaginationInfo {
            page: 1,
            limit: 10,
            total: 50,
            total_pages: 5,
            has_next: true,
            has_prev: false,
        };
        assert!(!pagination.has_prev);
        assert!(pagination.has_next);
    }

    #[test]
    fn test_pagination_last_page() {
        let pagination = PaginationInfo {
            page: 5,
            limit: 10,
            total: 50,
            total_pages: 5,
            has_next: false,
            has_prev: true,
        };
        assert!(!pagination.has_next);
        assert!(pagination.has_prev);
    }

    // ---- PasswordChangeRequest validation ----

    #[test]
    fn test_password_change_request_valid() {
        let req = PasswordChangeRequest {
            current_password: "OldPass123!".to_string(),
            new_password: "NewPass456!".to_string(),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_password_change_request_empty_current() {
        let req = PasswordChangeRequest {
            current_password: "".to_string(),
            new_password: "NewPass456!".to_string(),
        };
        assert!(req.validate().is_err(), "Empty current password should fail");
    }

    #[test]
    fn test_password_change_request_short_new_password() {
        let req = PasswordChangeRequest {
            current_password: "OldPass123!".to_string(),
            new_password: "short".to_string(),
        };
        assert!(req.validate().is_err(), "New password under 8 chars should fail");
    }

    #[test]
    fn test_password_change_request_exactly_8_chars() {
        let req = PasswordChangeRequest {
            current_password: "OldPass123!".to_string(),
            new_password: "12345678".to_string(),
        };
        assert!(req.validate().is_ok(), "Exactly 8 char password should pass");
    }

    // ---- AdminUserUpdateRequest validation ----

    #[test]
    fn test_admin_update_valid_role() {
        let req = AdminUserUpdateRequest {
            name: None,
            email: None,
            role: Some("admin".to_string()),
            is_active: None,
            email_verified: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_admin_update_invalid_role() {
        let req = AdminUserUpdateRequest {
            name: None,
            email: None,
            role: Some("superuser".to_string()),
            is_active: None,
            email_verified: None,
        };
        assert!(req.validate().is_err(), "Role 'superuser' should be invalid");
    }

    #[test]
    fn test_admin_update_all_valid_roles() {
        for role in &["admin", "customer", "editor", "subscriber"] {
            let req = AdminUserUpdateRequest {
                name: None,
                email: None,
                role: Some(role.to_string()),
                is_active: None,
                email_verified: None,
            };
            assert!(req.validate().is_ok(), "Role '{}' should be valid", role);
        }
    }

    #[test]
    fn test_admin_update_invalid_email() {
        let req = AdminUserUpdateRequest {
            name: None,
            email: Some("not-an-email".to_string()),
            role: None,
            is_active: None,
            email_verified: None,
        };
        assert!(req.validate().is_err(), "Invalid email should fail validation");
    }

    #[test]
    fn test_admin_update_empty_name() {
        let req = AdminUserUpdateRequest {
            name: Some("".to_string()),
            email: None,
            role: None,
            is_active: None,
            email_verified: None,
        };
        assert!(req.validate().is_err(), "Empty name should fail validation");
    }

    // ---- AccountChanges validation ----

    #[test]
    fn test_account_changes_valid_email_change() {
        let changes = AccountChanges {
            current_password: "MyPassword1!".to_string(),
            new_email: Some("newemail@example.com".to_string()),
            new_password: None,
        };
        assert!(changes.validate().is_ok());
    }

    #[test]
    fn test_account_changes_invalid_new_email() {
        let changes = AccountChanges {
            current_password: "MyPassword1!".to_string(),
            new_email: Some("bad-email".to_string()),
            new_password: None,
        };
        assert!(changes.validate().is_err());
    }

    #[test]
    fn test_account_changes_short_new_password() {
        let changes = AccountChanges {
            current_password: "MyPassword1!".to_string(),
            new_email: None,
            new_password: Some("short".to_string()),
        };
        assert!(changes.validate().is_err(), "New password under 8 chars should fail");
    }

    #[test]
    fn test_account_changes_empty_current_password() {
        let changes = AccountChanges {
            current_password: "".to_string(),
            new_email: None,
            new_password: None,
        };
        assert!(changes.validate().is_err(), "Empty current password should fail");
    }

    // ---- RoleUpdateRequest validation ----

    #[test]
    fn test_role_update_valid() {
        let req = RoleUpdateRequest { role: "editor".to_string() };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_role_update_invalid() {
        let req = RoleUpdateRequest { role: "moderator".to_string() };
        assert!(req.validate().is_err());
    }

    // ---- UserBasicInfo validation ----

    #[test]
    fn test_user_basic_info_valid() {
        let info = UserBasicInfo {
            _id: "abc123".to_string(),
            email: "user@test.com".to_string(),
            name: "John".to_string(),
            role: "customer".to_string(),
            profile_picture: None,
            use_gravatar: None,
            location: None,
        };
        assert!(info.validate().is_ok());
    }

    #[test]
    fn test_user_basic_info_empty_id() {
        let info = UserBasicInfo {
            _id: "".to_string(),
            email: "user@test.com".to_string(),
            name: "John".to_string(),
            role: "customer".to_string(),
            profile_picture: None,
            use_gravatar: None,
            location: None,
        };
        assert!(info.validate().is_err(), "Empty _id should fail");
    }

    #[test]
    fn test_user_basic_info_invalid_email() {
        let info = UserBasicInfo {
            _id: "abc".to_string(),
            email: "not-email".to_string(),
            name: "John".to_string(),
            role: "customer".to_string(),
            profile_picture: None,
            use_gravatar: None,
            location: None,
        };
        assert!(info.validate().is_err(), "Invalid email should fail");
    }

    #[test]
    fn test_user_basic_info_empty_name() {
        let info = UserBasicInfo {
            _id: "abc".to_string(),
            email: "user@test.com".to_string(),
            name: "".to_string(),
            role: "customer".to_string(),
            profile_picture: None,
            use_gravatar: None,
            location: None,
        };
        assert!(info.validate().is_err(), "Empty name should fail");
    }

    #[test]
    fn test_user_basic_info_invalid_role() {
        let info = UserBasicInfo {
            _id: "abc".to_string(),
            email: "user@test.com".to_string(),
            name: "John".to_string(),
            role: "superadmin".to_string(),
            profile_picture: None,
            use_gravatar: None,
            location: None,
        };
        assert!(info.validate().is_err(), "Invalid role should fail");
    }

    // ---- UserProfileResponse ----

    #[test]
    fn test_user_profile_response_success() {
        let resp = UserProfileResponse {
            success: true,
            user: Some(StandardizedUser {
                _id: "id".to_string(),
                id: "id".to_string(),
                email: "u@e.com".to_string(),
                name: "N".to_string(),
                role: "customer".to_string(),
                is_active: true,
                email_verified: true,
                created_at: "".to_string(),
                updated_at: "".to_string(),
                phone: None, company: None, department: None,
                position: None, username: None, profile_picture: None,
                use_gravatar: None, location: None,
            }),
            message: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
    }

    #[test]
    fn test_user_profile_response_not_found() {
        let resp = UserProfileResponse {
            success: false,
            user: None,
            message: Some("User not found".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("User not found"));
    }

    // ---- SettingsResponse ----

    #[test]
    fn test_settings_response_roundtrip() {
        let resp = SettingsResponse {
            success: true,
            settings: Some(UserSettings::default()),
            message: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: SettingsResponse = serde_json::from_str(&json).unwrap();
        assert!(deserialized.success);
        assert!(deserialized.settings.is_some());
        assert_eq!(deserialized.settings.unwrap().theme, "light");
    }

    // ---- UserSearchQuery deserialization ----

    #[test]
    fn test_user_search_query_all_fields() {
        let json = r#"{"q":"john","role":"admin","page":2,"limit":25,"sort":"name","order":"asc"}"#;
        let query: UserSearchQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.q, Some("john".to_string()));
        assert_eq!(query.role, Some("admin".to_string()));
        assert_eq!(query.page, Some(2));
        assert_eq!(query.limit, Some(25));
        assert_eq!(query.sort, Some("name".to_string()));
        assert_eq!(query.order, Some("asc".to_string()));
    }

    #[test]
    fn test_user_search_query_empty() {
        let json = "{}";
        let query: UserSearchQuery = serde_json::from_str(json).unwrap();
        assert!(query.q.is_none());
        assert!(query.role.is_none());
        assert!(query.page.is_none());
        assert!(query.limit.is_none());
    }

    // ---- ActivityLog serialization ----

    #[test]
    fn test_activity_log_roundtrip() {
        let log = ActivityLog {
            id: "act1".to_string(),
            user_id: "user1".to_string(),
            action: "login".to_string(),
            resource: Some("session".to_string()),
            resource_id: Some("sess123".to_string()),
            ip_address: Some("192.168.1.1".to_string()),
            user_agent: Some("Mozilla/5.0".to_string()),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            metadata: Some(serde_json::json!({"browser": "Chrome"})),
        };

        let json = serde_json::to_string(&log).unwrap();
        let deserialized: ActivityLog = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.action, "login");
        assert_eq!(deserialized.ip_address, Some("192.168.1.1".to_string()));
    }

    // ---- DataImportResponse ----

    #[test]
    fn test_data_import_response_serialization() {
        let resp = DataImportResponse {
            success: true,
            imported_count: 5,
            failed_count: 1,
            errors: vec!["Row 3: invalid email".to_string()],
            message: "Import completed with errors".to_string(),
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"imported_count\":5"));
        assert!(json.contains("\"failed_count\":1"));
        assert!(json.contains("Row 3: invalid email"));
    }

    // ---- ProfilePictureResponse ----

    #[test]
    fn test_profile_picture_response_with_url() {
        let resp = ProfilePictureResponse {
            success: true,
            message: "Uploaded".to_string(),
            profile_picture: Some("https://drive.google.com/thumbnail?id=abc".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("profilePicture"));
        assert!(json.contains("drive.google.com"));
    }

    #[test]
    fn test_profile_picture_response_without_url() {
        let resp = ProfilePictureResponse {
            success: false,
            message: "Upload failed".to_string(),
            profile_picture: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("profilePicture"), "None profilePicture should be skipped");
    }
}

// User roles and permissions structures
#[derive(Serialize, Deserialize, Clone)]
pub struct UserRolesResponse {
    pub success: bool,
    pub roles: Vec<RoleInfo>,
    pub current_role: Option<String>,
    pub permissions: Option<Vec<String>>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RoleInfo {
    pub name: String,
    pub description: String,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Validate)]
pub struct RoleUpdateRequest {
    #[validate(custom(function = "validate_user_role", message = "Invalid user role"))]
    pub role: String,
}

// User activity tracking structures
#[derive(Serialize, Deserialize, Clone)]
pub struct UserActivityResponse {
    pub success: bool,
    pub activities: Vec<ActivityLog>,
    pub pagination: Option<PaginationInfo>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ActivityLog {
    pub id: String,
    pub user_id: String,
    pub action: String,
    pub resource: Option<String>,
    pub resource_id: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub timestamp: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub action: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

// Data export/import structures
#[derive(Serialize)]
pub struct DataExportResponse {
    pub success: bool,
    pub data: Option<UserDataExport>,
    pub download_url: Option<String>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UserDataExport {
    pub user: StandardizedUser,
    pub settings: Option<UserSettings>,
    pub activities: Vec<ActivityLog>,
    pub exported_at: String,
}

#[derive(Deserialize)]
pub struct DataImportRequest {
    pub data: UserDataImport,
}

#[derive(Deserialize, Clone)]
pub struct UserDataImport {
    pub email: String,
    pub name: String,
    pub role: Option<String>,
    pub settings: Option<UserSettings>,
}

#[derive(Serialize)]
pub struct DataImportResponse {
    pub success: bool,
    pub imported_count: u32,
    pub failed_count: u32,
    pub errors: Vec<String>,
    pub message: String,
}