use serde::{Deserialize, Serialize};
use validator::Validate;

// Cached user profile data structure for Redis (following auth-service patterns)
#[derive(Serialize, Deserialize, Clone)]
pub struct CachedUserProfile {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
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