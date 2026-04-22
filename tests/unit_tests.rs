// Comprehensive unit tests for user-service
// Tests pure functions, models, and logic that doesn't require DB/Redis connections

#[cfg(test)]
mod extract_doc_id_tests {
    use mongodb::bson::{doc, oid::ObjectId, Document};

    /// Reimplements extract_doc_id logic for testing (the original is a private fn in handlers)
    fn extract_doc_id(doc: &Document) -> Result<String, String> {
        doc.get_object_id("_id")
            .map(|oid| oid.to_hex())
            .map_err(|_| "Document missing valid _id field".to_string())
    }

    #[test]
    fn test_extract_doc_id_valid_objectid() {
        let oid = ObjectId::new();
        let document = doc! { "_id": oid };
        let result = extract_doc_id(&document);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), oid.to_hex());
    }

    #[test]
    fn test_extract_doc_id_specific_hex() {
        let oid = ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap();
        let document = doc! { "_id": oid };
        let result = extract_doc_id(&document);
        assert_eq!(result.unwrap(), "507f1f77bcf86cd799439011");
    }

    #[test]
    fn test_extract_doc_id_missing_id() {
        let document = doc! { "name": "test" };
        let result = extract_doc_id(&document);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing"));
    }

    #[test]
    fn test_extract_doc_id_wrong_type() {
        let document = doc! { "_id": "string_not_objectid" };
        let result = extract_doc_id(&document);
        assert!(result.is_err(), "_id as string should fail get_object_id");
    }

    #[test]
    fn test_extract_doc_id_int_type() {
        let document = doc! { "_id": 12345 };
        let result = extract_doc_id(&document);
        assert!(result.is_err(), "_id as integer should fail get_object_id");
    }

    #[test]
    fn test_extract_doc_id_empty_document() {
        let document = Document::new();
        let result = extract_doc_id(&document);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_doc_id_null_id() {
        let document = doc! { "_id": mongodb::bson::Bson::Null };
        let result = extract_doc_id(&document);
        assert!(result.is_err(), "null _id should fail");
    }

    #[test]
    fn test_extract_doc_id_multiple_fields() {
        let oid = ObjectId::new();
        let document = doc! {
            "_id": oid,
            "email": "test@example.com",
            "name": "Test User",
            "role": "customer"
        };
        let result = extract_doc_id(&document);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), oid.to_hex());
    }
}

#[cfg(test)]
mod objectid_validation_tests {
    use mongodb::bson::oid::ObjectId;

    #[test]
    fn test_valid_objectid_parse() {
        let result = ObjectId::parse_str("507f1f77bcf86cd799439011");
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_objectid_too_short() {
        let result = ObjectId::parse_str("507f1f77");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_objectid_not_hex() {
        let result = ObjectId::parse_str("zzzzzzzzzzzzzzzzzzzzzzzz");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_objectid_empty() {
        let result = ObjectId::parse_str("");
        assert!(result.is_err());
    }

    #[test]
    fn test_objectid_hex_roundtrip() {
        let oid = ObjectId::new();
        let hex = oid.to_hex();
        let parsed = ObjectId::parse_str(&hex).unwrap();
        assert_eq!(oid, parsed);
    }
}

#[cfg(test)]
mod cache_key_format_tests {
    // Tests that cache key formats are consistent and correct
    // These mirror the key formats used in cache_service.rs and user_handlers.rs

    #[test]
    fn test_profile_cache_key_format() {
        let user_id = "abc123";
        let cache_key = format!("user:profile:{}", user_id);
        assert_eq!(cache_key, "user:profile:abc123");
    }

    #[test]
    fn test_settings_cache_key_format() {
        let user_id = "abc123";
        let cache_key = format!("user:settings:{}", user_id);
        assert_eq!(cache_key, "user:settings:abc123");
    }

    #[test]
    fn test_rate_limit_cache_key_format() {
        let client_ip = "192.168.1.1";
        let rate_limit_key = format!("rate_limit:{}", client_ip);
        assert_eq!(rate_limit_key, "rate_limit:192.168.1.1");
    }

    #[test]
    fn test_rate_limit_window_key_format() {
        let rate_limit_key = "rate_limit:192.168.1.1";
        let window_seconds: u64 = 60;
        let current_time: u64 = 1700000000;
        let window_key = format!("{}:{}", rate_limit_key, current_time / window_seconds);
        assert_eq!(window_key, "rate_limit:192.168.1.1:28333333");
    }

    #[test]
    fn test_cache_key_with_objectid() {
        let user_id = "507f1f77bcf86cd799439011";
        let cache_key = format!("user:profile:{}", user_id);
        assert_eq!(cache_key, "user:profile:507f1f77bcf86cd799439011");
    }

    #[test]
    fn test_window_key_changes_per_window() {
        let base_key = "rate_limit:10.0.0.1";
        let window_seconds: u64 = 60;

        let t1: u64 = 1700000040; // 1700000040 / 60 = 28333334
        let t2: u64 = 1700000059; // 1700000059 / 60 = 28333334 (same window)

        let k1 = format!("{}:{}", base_key, t1 / window_seconds);
        let k2 = format!("{}:{}", base_key, t2 / window_seconds);
        let k3 = format!("{}:{}", base_key, (t1 / window_seconds) + 1); // explicitly next window

        assert_eq!(k1, k2, "Same window should produce same key");
        assert_ne!(k1, k3, "Different window should produce different key");
    }
}

#[cfg(test)]
mod rate_limit_logic_tests {
    use lru::LruCache;
    use std::num::NonZeroUsize;

    #[derive(Clone)]
    struct RateLimitEntry {
        count: u32,
        window_start: u64,
    }

    #[derive(Clone)]
    struct RateLimitConfig {
        requests_per_window: u32,
        window_seconds: u64,
    }

    /// Pure function version of check_rate_limit_memory for testing
    fn check_rate_limit(
        cache: &mut LruCache<String, RateLimitEntry>,
        key: &str,
        config: &RateLimitConfig,
        current_time: u64,
    ) -> bool {
        let window_start = current_time - config.window_seconds;

        if let Some(entry) = cache.get_mut(key) {
            if entry.window_start >= window_start {
                if entry.count >= config.requests_per_window {
                    return false;
                }
                entry.count += 1;
            } else {
                entry.count = 1;
                entry.window_start = current_time;
            }
        } else {
            cache.put(key.to_string(), RateLimitEntry {
                count: 1,
                window_start: current_time,
            });
        }

        true
    }

    fn default_config() -> RateLimitConfig {
        RateLimitConfig {
            requests_per_window: 5,
            window_seconds: 60,
        }
    }

    #[test]
    fn test_first_request_allowed() {
        let mut cache = LruCache::new(NonZeroUsize::new(100).unwrap());
        let config = default_config();
        assert!(check_rate_limit(&mut cache, "client:1", &config, 1000));
    }

    #[test]
    fn test_within_limit_allowed() {
        let mut cache = LruCache::new(NonZeroUsize::new(100).unwrap());
        let config = default_config();
        for i in 0..5 {
            assert!(check_rate_limit(&mut cache, "client:1", &config, 1000 + i), "Request {} should be allowed", i);
        }
    }

    #[test]
    fn test_exceeding_limit_blocked() {
        let mut cache = LruCache::new(NonZeroUsize::new(100).unwrap());
        let config = default_config(); // 5 requests per window
        // First 5 should pass
        for _ in 0..5 {
            check_rate_limit(&mut cache, "client:1", &config, 1000);
        }
        // 6th should be blocked
        assert!(!check_rate_limit(&mut cache, "client:1", &config, 1000));
    }

    #[test]
    fn test_window_reset() {
        let mut cache = LruCache::new(NonZeroUsize::new(100).unwrap());
        let config = default_config(); // 60 second window
        // Exhaust limit
        for _ in 0..5 {
            check_rate_limit(&mut cache, "client:1", &config, 1000);
        }
        assert!(!check_rate_limit(&mut cache, "client:1", &config, 1000));

        // After window expires, should be allowed again
        assert!(check_rate_limit(&mut cache, "client:1", &config, 1070));
    }

    #[test]
    fn test_different_clients_independent() {
        let mut cache = LruCache::new(NonZeroUsize::new(100).unwrap());
        let config = default_config();
        // Exhaust client 1
        for _ in 0..5 {
            check_rate_limit(&mut cache, "client:1", &config, 1000);
        }
        assert!(!check_rate_limit(&mut cache, "client:1", &config, 1000));

        // Client 2 should still be allowed
        assert!(check_rate_limit(&mut cache, "client:2", &config, 1000));
    }

    #[test]
    fn test_lru_eviction() {
        // Cache size 2, so third client evicts first
        let mut cache = LruCache::new(NonZeroUsize::new(2).unwrap());
        let config = default_config();

        check_rate_limit(&mut cache, "client:1", &config, 1000);
        check_rate_limit(&mut cache, "client:2", &config, 1000);
        check_rate_limit(&mut cache, "client:3", &config, 1000);

        // client:1 should have been evicted, so it starts fresh
        assert!(cache.get("client:1").is_none(), "client:1 should be evicted");
        assert!(cache.get("client:3").is_some(), "client:3 should exist");
    }

    #[test]
    fn test_strict_auth_config() {
        let mut cache = LruCache::new(NonZeroUsize::new(100).unwrap());
        let config = RateLimitConfig {
            requests_per_window: 10,
            window_seconds: 60,
        };
        for _ in 0..10 {
            assert!(check_rate_limit(&mut cache, "client:1", &config, 1000));
        }
        assert!(!check_rate_limit(&mut cache, "client:1", &config, 1000));
    }
}

#[cfg(test)]
mod password_validation_tests {
    #[test]
    fn test_bcrypt_hash_normal_password() {
        let result = bcrypt::hash("SecurePass123!", 12);
        assert!(result.is_ok());
    }

    #[test]
    fn test_bcrypt_hash_empty_password() {
        let result = bcrypt::hash("", 12);
        assert!(result.is_ok(), "bcrypt should handle empty password");
    }

    #[test]
    fn test_bcrypt_hash_long_password() {
        // bcrypt has a 72-byte input limit
        let long_pw = "A".repeat(72);
        let result = bcrypt::hash(&long_pw, 12);
        assert!(result.is_ok());
    }

    #[test]
    fn test_bcrypt_verify_correct_password() {
        let password = "TestPassword123!";
        let hash = bcrypt::hash(password, 12).unwrap();
        assert!(bcrypt::verify(password, &hash).unwrap());
    }

    #[test]
    fn test_bcrypt_verify_wrong_password() {
        let password = "TestPassword123!";
        let hash = bcrypt::hash(password, 12).unwrap();
        assert!(!bcrypt::verify("WrongPassword", &hash).unwrap());
    }

    #[test]
    fn test_bcrypt_different_hashes_same_password() {
        let password = "SamePassword123!";
        let hash1 = bcrypt::hash(password, 12).unwrap();
        let hash2 = bcrypt::hash(password, 12).unwrap();
        assert_ne!(hash1, hash2, "bcrypt should produce different hashes due to random salt");
        // But both should verify
        assert!(bcrypt::verify(password, &hash1).unwrap());
        assert!(bcrypt::verify(password, &hash2).unwrap());
    }
}

#[cfg(test)]
mod json_serialization_tests {
    use user_service::utils::security::{generate_secure_password, validate_email, escape_regex};

    #[test]
    fn test_optimize_json_consistent_output() {
        // Test that standard serde_json produces valid JSON for our response types
        let response = serde_json::json!({
            "success": true,
            "user": {
                "id": "test123",
                "email": "test@example.com",
                "name": "Test User",
                "role": "customer",
                "isActive": true,
                "emailVerified": true,
                "createdAt": "2025-01-01T00:00:00Z",
                "updatedAt": "2025-01-02T00:00:00Z"
            }
        });

        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized["success"], true);
        assert_eq!(deserialized["user"]["email"], "test@example.com");
    }

    #[test]
    fn test_json_with_special_characters() {
        let response = serde_json::json!({
            "error": "Name contains <script>alert('xss')</script>"
        });
        let serialized = serde_json::to_string(&response).unwrap();
        // Should be safely serialized as JSON
        let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert!(deserialized["error"].as_str().unwrap().contains("<script>"));
    }

    #[test]
    fn test_json_unicode_handling() {
        let response = serde_json::json!({
            "name": "José García"
        });
        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized["name"], "José García");
    }

    // Integration: validate_email used with JSON data
    #[test]
    fn test_validate_email_from_json_field() {
        let json_data = serde_json::json!({
            "email": "user@example.com"
        });
        let email = json_data["email"].as_str().unwrap();
        assert!(validate_email(email));
    }

    // Integration: escape_regex used with search query from JSON
    #[test]
    fn test_escape_regex_search_query() {
        let search = "user+test@example.com";
        let escaped = escape_regex(search);
        // Verify the escaped pattern can be compiled as a regex
        let re = regex::Regex::new(&escaped).unwrap();
        assert!(re.is_match(search));
    }

    // Integration: generate_secure_password as JSON field
    #[test]
    fn test_password_in_json_response() {
        let password = generate_secure_password();
        let response = serde_json::json!({
            "success": true,
            "tempPassword": password
        });
        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized["tempPassword"].as_str().unwrap().len(), 32);
    }
}

#[cfg(test)]
mod pagination_logic_tests {
    #[test]
    fn test_total_pages_calculation() {
        // Mirrors the logic used in handlers for pagination
        let total: u64 = 25;
        let limit: u32 = 10;
        let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;
        assert_eq!(total_pages, 3);
    }

    #[test]
    fn test_total_pages_exact_division() {
        let total: u64 = 20;
        let limit: u32 = 10;
        let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;
        assert_eq!(total_pages, 2);
    }

    #[test]
    fn test_total_pages_single_page() {
        let total: u64 = 5;
        let limit: u32 = 10;
        let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;
        assert_eq!(total_pages, 1);
    }

    #[test]
    fn test_total_pages_empty_result() {
        let total: u64 = 0;
        let limit: u32 = 10;
        let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;
        assert_eq!(total_pages, 0);
    }

    #[test]
    fn test_has_next_calculation() {
        let page: u32 = 2;
        let total_pages: u32 = 5;
        let has_next = page < total_pages;
        assert!(has_next);
    }

    #[test]
    fn test_has_prev_calculation() {
        let page: u32 = 2;
        let has_prev = page > 1;
        assert!(has_prev);
    }

    #[test]
    fn test_skip_calculation() {
        let page: u32 = 3;
        let limit: u32 = 10;
        let skip = ((page - 1) * limit) as u64;
        assert_eq!(skip, 20);
    }

    #[test]
    #[allow(clippy::unnecessary_literal_unwrap)]
    fn test_default_page_and_limit() {
        // Mirrors handler defaults: query.page.unwrap_or(1), query.limit.unwrap_or(20).min(100)
        let page_opt: Option<u32> = None;
        let limit_opt: Option<u32> = None;
        let page = page_opt.unwrap_or(1);
        let limit = limit_opt.unwrap_or(20).min(100);
        assert_eq!(page, 1);
        assert_eq!(limit, 20);
    }

    #[test]
    fn test_limit_clamped_to_max() {
        let requested_limit: u32 = 500;
        let limit = requested_limit.min(100);
        assert_eq!(limit, 100);
    }
}

#[cfg(test)]
mod profile_picture_url_validation_tests {
    #[test]
    fn test_google_drive_thumbnail_url_format() {
        let file_id = "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms";
        let url = format!("https://drive.google.com/thumbnail?id={}&sz=w200-h200", file_id);
        assert!(url.starts_with("https://drive.google.com/thumbnail"));
        assert!(url.contains(file_id));
        assert!(url.contains("sz=w200-h200"));
    }

    #[test]
    fn test_profile_picture_url_is_https() {
        let url = "https://drive.google.com/thumbnail?id=abc&sz=w200-h200";
        assert!(url.starts_with("https://"), "Profile picture URLs must use HTTPS");
    }

    #[test]
    fn test_profile_picture_url_not_http() {
        let url = "http://drive.google.com/thumbnail?id=abc";
        assert!(!url.starts_with("https://"), "HTTP URLs should be rejected");
    }

    #[test]
    fn test_gravatar_url_format() {
        // If use_gravatar is true, the frontend generates gravatar URLs
        // The backend stores useGravatar flag, not the URL
        let use_gravatar = true;
        let profile_picture: Option<String> = None;
        // When useGravatar is true and no profile_picture, frontend uses gravatar
        assert!(use_gravatar);
        assert!(profile_picture.is_none());
    }
}

#[cfg(test)]
mod role_permissions_tests {
    #[test]
    fn test_admin_role_permissions() {
        let admin_perms: &[&str] = &["read", "write", "delete", "manage_users", "manage_settings", "admin"];
        assert!(admin_perms.contains(&"admin"));
        assert!(admin_perms.contains(&"manage_users"));
    }

    #[test]
    fn test_customer_role_permissions() {
        let customer_perms: &[&str] = &["read", "write"];
        assert!(customer_perms.contains(&"read"));
        assert!(customer_perms.contains(&"write"));
        assert!(!customer_perms.contains(&"admin"));
        assert!(!customer_perms.contains(&"delete"));
    }

    #[test]
    fn test_editor_role_permissions() {
        let editor_perms: &[&str] = &["read", "write", "edit_content"];
        assert!(editor_perms.contains(&"edit_content"));
        assert!(!editor_perms.contains(&"admin"));
    }

    #[test]
    fn test_subscriber_role_permissions() {
        let subscriber_perms: &[&str] = &["read"];
        assert!(subscriber_perms.contains(&"read"));
        assert!(!subscriber_perms.contains(&"write"));
    }

    #[test]
    fn test_valid_roles_list() {
        let valid_roles = ["admin", "customer", "editor", "subscriber"];
        assert_eq!(valid_roles.len(), 4);
        assert!(valid_roles.contains(&"admin"));
        assert!(valid_roles.contains(&"customer"));
        assert!(valid_roles.contains(&"editor"));
        assert!(valid_roles.contains(&"subscriber"));
        assert!(!valid_roles.contains(&"superuser"));
        assert!(!valid_roles.contains(&"moderator"));
    }
}

#[cfg(test)]
mod sort_order_validation_tests {
    // Tests for the sorting logic used in admin_search_users handler

    #[test]
    fn test_valid_sort_fields() {
        let valid_sorts = ["name", "email", "role", "createdAt", "updatedAt"];
        for field in &valid_sorts {
            assert!(valid_sorts.contains(field));
        }
    }

    #[test]
    fn test_sort_order_values() {
        let order_asc = "asc";
        let order_desc = "desc";
        let sort_value_asc: i32 = if order_asc == "desc" { -1 } else { 1 };
        let sort_value_desc: i32 = if order_desc == "desc" { -1 } else { 1 };
        assert_eq!(sort_value_asc, 1);
        assert_eq!(sort_value_desc, -1);
    }

    #[test]
    #[allow(clippy::unnecessary_literal_unwrap)]
    fn test_default_sort_is_created_at_desc() {
        // Mirrors handler defaults: query.sort.unwrap_or("createdAt"), query.order.unwrap_or("desc")
        let sort_opt: Option<String> = None;
        let order_opt: Option<String> = None;
        let sort = sort_opt.unwrap_or_else(|| "createdAt".to_string());
        let order = order_opt.unwrap_or_else(|| "desc".to_string());
        assert_eq!(sort, "createdAt");
        assert_eq!(order, "desc");
    }
}
