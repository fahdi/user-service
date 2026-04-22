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
