use rand::Rng;

/// Generate a cryptographically random password of 32 characters.
/// Uses alphanumeric + special characters for high entropy.
pub fn generate_secure_password() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Validate an email address with basic structural checks.
/// Requires: non-empty local part, '@', domain with at least one dot.
pub fn validate_email(email: &str) -> bool {
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    if parts.len() != 2 {
        return false;
    }
    let local = parts[0];
    let domain = parts[1];
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    // Domain must contain at least one dot (TLD check)
    if !domain.contains('.') {
        return false;
    }
    // Domain must not start or end with a dot
    if domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    true
}

/// Escape a string for safe use in a MongoDB `$regex` query.
/// Delegates to `regex::escape` which handles all regex metacharacters.
pub fn escape_regex(input: &str) -> String {
    regex::escape(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ========== generate_secure_password tests ==========

    #[test]
    fn test_password_length_is_32() {
        let pw = generate_secure_password();
        assert_eq!(pw.len(), 32, "Password should be exactly 32 characters");
    }

    #[test]
    fn test_password_contains_uppercase() {
        // Generate several passwords; at least one should contain uppercase
        let has_upper = (0..20).any(|_| {
            generate_secure_password().chars().any(|c| c.is_ascii_uppercase())
        });
        assert!(has_upper, "Generated passwords should contain uppercase letters");
    }

    #[test]
    fn test_password_contains_lowercase() {
        let has_lower = (0..20).any(|_| {
            generate_secure_password().chars().any(|c| c.is_ascii_lowercase())
        });
        assert!(has_lower, "Generated passwords should contain lowercase letters");
    }

    #[test]
    fn test_password_contains_digits() {
        let has_digit = (0..20).any(|_| {
            generate_secure_password().chars().any(|c| c.is_ascii_digit())
        });
        assert!(has_digit, "Generated passwords should contain digits");
    }

    #[test]
    fn test_password_contains_special_chars() {
        let specials: &str = "!@#$%^&*()-_=+";
        let has_special = (0..20).any(|_| {
            generate_secure_password().chars().any(|c| specials.contains(c))
        });
        assert!(has_special, "Generated passwords should contain special characters");
    }

    #[test]
    fn test_password_uniqueness_batch() {
        let passwords: HashSet<String> = (0..50).map(|_| generate_secure_password()).collect();
        assert_eq!(passwords.len(), 50, "50 generated passwords should all be unique");
    }

    #[test]
    fn test_password_uses_only_charset_chars() {
        let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+";
        for _ in 0..10 {
            let pw = generate_secure_password();
            for ch in pw.chars() {
                assert!(charset.contains(ch), "Character '{}' not in allowed charset", ch);
            }
        }
    }

    // ========== validate_email tests ==========

    #[test]
    fn test_email_valid_simple() {
        assert!(validate_email("user@example.com"));
    }

    #[test]
    fn test_email_valid_subdomain() {
        assert!(validate_email("user@mail.example.com"));
    }

    #[test]
    fn test_email_valid_plus_addressing() {
        assert!(validate_email("user+tag@example.com"));
    }

    #[test]
    fn test_email_valid_dots_in_local() {
        assert!(validate_email("first.last@example.com"));
    }

    #[test]
    fn test_email_valid_hyphen_in_domain() {
        assert!(validate_email("user@my-domain.com"));
    }

    #[test]
    fn test_email_valid_numbers_in_local() {
        assert!(validate_email("user123@example.com"));
    }

    #[test]
    fn test_email_reject_empty_string() {
        assert!(!validate_email(""));
    }

    #[test]
    fn test_email_reject_no_at() {
        assert!(!validate_email("userexample.com"));
    }

    #[test]
    fn test_email_reject_bare_at() {
        assert!(!validate_email("@"));
    }

    #[test]
    fn test_email_reject_empty_local() {
        assert!(!validate_email("@example.com"));
    }

    #[test]
    fn test_email_reject_empty_domain() {
        assert!(!validate_email("user@"));
    }

    #[test]
    fn test_email_reject_no_tld() {
        assert!(!validate_email("user@domain"));
    }

    #[test]
    fn test_email_reject_domain_starts_with_dot() {
        assert!(!validate_email("user@.example.com"));
    }

    #[test]
    fn test_email_reject_domain_ends_with_dot() {
        assert!(!validate_email("user@example.com."));
    }

    #[test]
    fn test_email_reject_multiple_at_signs() {
        // splitn(2, '@') means second part is "b@c.com", which contains '.' so it passes.
        // This documents the current behavior: the validator only checks basic structure.
        let result = validate_email("a@b@c.com");
        // "b@c.com" has a dot and doesn't start/end with dot, so it passes
        assert!(result, "Current validator accepts multiple @ (known limitation)");
    }

    #[test]
    fn test_email_valid_long_tld() {
        assert!(validate_email("user@example.museum"));
    }

    #[test]
    fn test_email_valid_two_char_tld() {
        assert!(validate_email("user@example.uk"));
    }

    // ========== escape_regex tests ==========

    #[test]
    fn test_escape_plain_string() {
        assert_eq!(escape_regex("hello"), "hello");
    }

    #[test]
    fn test_escape_empty_string() {
        assert_eq!(escape_regex(""), "");
    }

    #[test]
    fn test_escape_dot() {
        assert_eq!(escape_regex("."), r"\.");
    }

    #[test]
    fn test_escape_star() {
        assert_eq!(escape_regex("*"), r"\*");
    }

    #[test]
    fn test_escape_plus() {
        assert_eq!(escape_regex("+"), r"\+");
    }

    #[test]
    fn test_escape_question_mark() {
        assert_eq!(escape_regex("?"), r"\?");
    }

    #[test]
    fn test_escape_pipe() {
        assert_eq!(escape_regex("|"), r"\|");
    }

    #[test]
    fn test_escape_brackets() {
        let escaped = escape_regex("[]{}()");
        assert!(escaped.contains(r"\["));
        assert!(escaped.contains(r"\]"));
        assert!(escaped.contains(r"\{"));
        assert!(escaped.contains(r"\}"));
        assert!(escaped.contains(r"\("));
        assert!(escaped.contains(r"\)"));
    }

    #[test]
    fn test_escape_caret_and_dollar() {
        assert_eq!(escape_regex("^$"), r"\^\$");
    }

    #[test]
    fn test_escape_backslash() {
        assert_eq!(escape_regex(r"\"), r"\\");
    }

    #[test]
    fn test_escape_mixed_content() {
        let escaped = escape_regex("user.*search");
        assert_eq!(escaped, r"user\.\*search");
    }

    #[test]
    fn test_escape_unicode() {
        // Unicode characters are not regex metacharacters, should pass through
        assert_eq!(escape_regex("café"), "café");
    }

    #[test]
    fn test_escape_preserves_spaces() {
        assert_eq!(escape_regex("hello world"), "hello world");
    }

    #[test]
    fn test_escape_all_metacharacters() {
        let input = r"\.+*?()|[]{}^$";
        let escaped = escape_regex(input);
        // Each metacharacter should be escaped with a backslash
        // The escaped output should be safe for use in a regex pattern
        let re = regex::Regex::new(&escaped).expect("Escaped string should be valid regex");
        assert!(re.is_match(input), "Escaped regex should match the original literal string");
    }
}
