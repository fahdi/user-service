use serde::{Deserialize, Serialize};

// JWT Claims structure (identical to auth-service)
#[derive(Serialize, Deserialize, Clone)]
pub struct Claims {
    #[serde(rename = "userId")]
    pub user_id: String,
    pub email: String,
    pub name: String,
    #[serde(rename = "type")]
    pub role_type: String,
    pub role: String,
    pub exp: usize,
}