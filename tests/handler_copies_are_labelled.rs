//! A test file that reimplements handlers must say so (#82).
//!
//! `handler_integration_tests.rs` opened with "These tests exercise the actual
//! handler logic ... by injecting mock implementations", and 660 lines later
//! defined 13 handlers locally and mounted those. Its own body called them
//! "reimplemented from di_handlers.rs"; only the header disagreed.
//!
//! The duplication itself is redundancy rather than a gap - every production
//! equivalent is exercised for real in `di_integration_tests.rs` - and what to
//! do with 3038 lines stays an owner decision (#75). The claim is the defect:
//! someone auditing whether `change_password` authenticates before validating
//! can open that file, find a thorough-looking test, and conclude the behaviour
//! is pinned. It is pinned, in a copy.
//!
//! Source-level because the property is about what a file *says* relative to
//! what it *defines*, which is not observable at runtime.

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Handler names as production defines them.
///
/// Read from `di_handlers.rs` rather than hardcoded, so a handler added or
/// renamed there is covered without anyone remembering to update this list.
fn production_handler_names() -> Vec<String> {
    let src = fs::read_to_string(manifest_dir().join("src/handlers/di_handlers.rs"))
        .expect("di_handlers.rs is readable");
    src.lines()
        .filter_map(|line| line.trim().strip_prefix("pub async fn "))
        .filter_map(|rest| rest.split('(').next())
        .map(|name| name.trim().to_string())
        .collect()
}

/// Phrases asserting the file drives production handlers. Assembled at runtime
/// so this doc comment cannot satisfy the search against its own file.
fn claim_phrases() -> Vec<String> {
    vec![
        format!("exercise the actual {} logic", "handler"),
        format!("exercises the actual {} logic", "handler"),
        format!("{} the actual handlers", "exercise"),
    ]
}

fn test_files() -> Vec<PathBuf> {
    fs::read_dir(manifest_dir().join("tests"))
        .expect("tests directory is readable")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect()
}

#[test]
fn a_file_that_reimplements_handlers_does_not_claim_to_drive_production() {
    let names = production_handler_names();
    assert!(
        names.len() >= 10,
        "parsed {} handler names from di_handlers.rs - the parse is broken, and \
         an empty list would make this test pass over nothing",
        names.len()
    );

    let files = test_files();
    assert!(!files.is_empty(), "found no test files to check");

    let mut offenders = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).expect("test file is readable");

        // Does it define its own copies? Two or more, so a single helper that
        // happens to share a name is not enough.
        let copies = names
            .iter()
            .filter(|n| text.contains(&format!("pub async fn {n}(")))
            .count();
        if copies < 2 {
            continue;
        }

        if let Some(claim) = claim_phrases().into_iter().find(|p| text.contains(p)) {
            offenders.push(format!(
                "{}: defines {copies} local copies of production handlers while claiming {claim:?}",
                path.file_name().unwrap().to_string_lossy()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "a test file must not claim to drive production handlers while \
         reimplementing them (#82). di_integration_tests.rs is the file that \
         exercises production code:\n  {}",
        offenders.join("\n  ")
    );
}
