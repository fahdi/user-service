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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_claims() -> Claims {
        Claims {
            user_id: "user_abc123".to_string(),
            email: "admin@example.com".to_string(),
            name: "Admin User".to_string(),
            role_type: "admin".to_string(),
            role: "admin".to_string(),
            exp: 9999999999,
        }
    }

    #[test]
    fn test_claims_serialization_renames_user_id() {
        let claims = sample_claims();
        let json = serde_json::to_string(&claims).unwrap();
        assert!(json.contains("\"userId\""), "user_id should serialize as userId");
        assert!(!json.contains("\"user_id\""), "user_id in snake_case should not appear");
    }

    #[test]
    fn test_claims_serialization_renames_type() {
        let claims = sample_claims();
        let json = serde_json::to_string(&claims).unwrap();
        assert!(json.contains("\"type\""), "role_type should serialize as type");
        assert!(!json.contains("\"role_type\""), "role_type in snake_case should not appear");
    }

    #[test]
    fn test_claims_deserialization_from_camelcase() {
        let json = r#"{
            "userId": "user_xyz",
            "email": "test@test.com",
            "name": "Test",
            "type": "customer",
            "role": "customer",
            "exp": 1234567890
        }"#;
        let claims: Claims = serde_json::from_str(json).unwrap();
        assert_eq!(claims.user_id, "user_xyz");
        assert_eq!(claims.role_type, "customer");
        assert_eq!(claims.exp, 1234567890);
    }

    #[test]
    fn test_claims_roundtrip() {
        let original = sample_claims();
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Claims = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.user_id, original.user_id);
        assert_eq!(deserialized.email, original.email);
        assert_eq!(deserialized.name, original.name);
        assert_eq!(deserialized.role_type, original.role_type);
        assert_eq!(deserialized.role, original.role);
        assert_eq!(deserialized.exp, original.exp);
    }

    #[test]
    fn test_claims_clone() {
        let original = sample_claims();
        let cloned = original.clone();
        assert_eq!(cloned.user_id, original.user_id);
        assert_eq!(cloned.email, original.email);
    }
}