use serde::{Deserialize, Serialize};

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
#[derive(Serialize, Deserialize)]
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
#[derive(Serialize, Deserialize, Clone)]
pub struct UserSettings {
    pub notifications: NotificationSettings,
    pub theme: String,
    pub language: String,
    pub timezone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserBasicInfo>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct NotificationSettings {
    pub email: bool,
    pub sound: bool,
    pub desktop: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UserBasicInfo {
    pub _id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    #[serde(rename = "profilePicture", skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<String>,
    #[serde(rename = "useGravatar", skip_serializing_if = "Option::is_none")]
    pub use_gravatar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

// Settings response (matches Node.js API exactly)
#[derive(Serialize, Deserialize)]
pub struct SettingsResponse {
    pub success: bool,
    pub settings: Option<UserSettings>,
    pub message: Option<String>,
}

// Settings update request (matches Node.js API)
#[derive(Deserialize)]
pub struct SettingsUpdateRequest {
    pub settings: UserSettings,
    #[serde(rename = "accountChanges", skip_serializing_if = "Option::is_none")]
    pub account_changes: Option<AccountChanges>,
}

#[derive(Deserialize)]
pub struct AccountChanges {
    #[serde(rename = "currentPassword")]
    pub current_password: String,
    #[serde(rename = "newEmail", skip_serializing_if = "Option::is_none")]
    pub new_email: Option<String>,
    #[serde(rename = "newPassword", skip_serializing_if = "Option::is_none")]
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