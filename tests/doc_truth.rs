//! Guard: the tracked CLAUDE.md must not document features the binary does
//! not contain (issue #26). Docs claiming live rate limiting while the
//! module is commented out sent readers (and reviewers) chasing behavior
//! that does not exist.

use std::fs;
use std::path::Path;

#[test]
fn claude_md_does_not_claim_unwired_rate_limiting() {
    let root = env!("CARGO_MANIFEST_DIR");
    let mod_rs = fs::read_to_string(Path::new(root).join("src/middleware/mod.rs"))
        .expect("middleware/mod.rs exists");

    let rate_limiting_wired = mod_rs
        .lines()
        .any(|l| l.trim_start().starts_with("pub mod rate_limit"));

    if rate_limiting_wired {
        return; // claims would be legitimate
    }

    let claude_md =
        fs::read_to_string(Path::new(root).join("CLAUDE.md")).expect("CLAUDE.md exists");

    for claim in [
        "rate_limit.rs",
        "Rate limiting: Actix Transform",
        "Rate limit middleware tests",
    ] {
        assert!(
            !claude_md.contains(claim),
            "CLAUDE.md claims '{claim}' but no rate_limit module is declared"
        );
    }
}
