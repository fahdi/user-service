//! Documentation may not claim an optimisation the code does not implement (#66).
//!
//! `optimize_json_response` used simd-json and was called by nothing - no
//! handler, no test, no feature flag - while `CLAUDE.md` listed it under Key
//! Design Decisions and described the dependency as this service's JSON
//! serializer. Every response has always gone through actix-web's `Json`.
//!
//! Checked in both directions, because either half can rot on its own: a claim
//! with no implementation misleads, and an implementation nothing references is
//! dependency surface carried for free.

use std::fs;
use std::path::Path;

fn production_sources() -> String {
    fn walk(dir: &Path, out: &mut String) {
        for entry in fs::read_dir(dir).expect("src/ is readable") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push_str(&fs::read_to_string(&path).expect("a readable source file"));
            }
        }
    }
    let mut out = String::new();
    walk(Path::new("src"), &mut out);
    out
}

#[test]
fn the_check_actually_read_the_sources_and_the_doc() {
    // Comparing two empty strings would satisfy every assertion below.
    assert!(
        production_sources().len() > 10_000,
        "source walk found almost nothing; the check is blind"
    );
    assert!(
        fs::read_to_string("CLAUDE.md").expect("CLAUDE.md is readable").len() > 1_000,
        "CLAUDE.md not readable; the check is blind"
    );
}

#[test]
fn no_public_function_in_lib_is_orphaned() {
    // The defect was not "simd-json is absent from the source" - it was there,
    // inside a function nothing called. Presence is not reachability, so the
    // rule has to be about references, not about the identifier appearing.
    let lib = fs::read_to_string("src/lib.rs").expect("src/lib.rs is readable");
    let sources = production_sources();

    let mut orphans = Vec::new();
    for line in lib.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("pub fn ")
            .or_else(|| trimmed.strip_prefix("pub async fn "))
        else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        // Its own definition is one occurrence; anything reachable has more.
        let references = sources.matches(&name).count();
        if references <= 1 {
            orphans.push(name);
        }
    }

    assert!(
        orphans.is_empty(),
        "these are defined in lib.rs and referenced nowhere, so they ship as \
         dead weight and can carry dependencies with them: {orphans:?}"
    );
}

#[test]
fn simd_json_is_not_claimed_unless_it_is_declared() {
    // Guards the other half: re-adding the documentation claim without the
    // dependency, or vice versa, puts the two back out of step.
    let doc = fs::read_to_string("CLAUDE.md").expect("CLAUDE.md is readable");
    let manifest = fs::read_to_string("Cargo.toml").expect("Cargo.toml is readable");

    let claimed = doc.contains("simd-json") || doc.contains("simd_json");
    let declared = manifest
        .lines()
        .any(|l| l.trim_start().starts_with("simd-json"));

    assert_eq!(
        claimed, declared,
        "CLAUDE.md and Cargo.toml disagree about whether simd-json is part of \
         this service (claimed={claimed}, declared={declared})"
    );
}
