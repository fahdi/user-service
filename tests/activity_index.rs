//! `user_activities` must be indexed for the queries that run against it (#68).
//!
//! Four indexes existed and all were created via `users.create_indexes(...)`,
//! bound to the users collection. `user_activities` - the other collection this
//! service uses, and the one that grows with every recorded action - had none.
//!
//! Both queries against it filter on `user_id` and sort by `timestamp: -1`
//! (the activity endpoint and the GDPR export). Without an index that is a
//! collection scan *and* an in-memory sort, and the in-memory sort has a hard
//! 32 MB limit past which the query fails rather than slows.
//!
//! Asserted against the source: index creation needs a live database, and what
//! matters is the declaration, its key order, and that it is actually created
//! on the right collection.

const MAIN: &str = include_str!("../src/main.rs");
const IMPLS: &str = include_str!("../src/impls.rs");

fn production(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or("")
}

#[test]
fn the_check_reads_the_index_code() {
    assert!(
        production(MAIN).contains("create_indexes"),
        "index creation not found in main.rs; this check is blind"
    );
}

#[test]
fn the_activities_index_is_declared_with_user_id_leading() {
    let src = production(MAIN);
    assert!(
        src.contains(r#"doc! { "user_id": 1, "timestamp": -1 }"#),
        "no index on user_activities leads with user_id, so the activity \
         queries scan the collection"
    );
}

/// Reversing the pair would serve neither query: the filter is an equality on
/// `user_id`, so it has to lead, with the sort field following.
#[test]
fn the_sort_field_follows_the_equality_field() {
    let src = production(MAIN);
    let at = src
        .find(r#"doc! { "user_id": 1, "timestamp": -1 }"#)
        .expect("the index is declared");
    let spec = &src[at..at + 40];
    assert!(spec.find("user_id").unwrap() < spec.find("timestamp").unwrap());
}

/// Declaring it is not enough. The existing call is bound to `users`, which is
/// exactly how this collection went unindexed.
#[test]
fn the_index_is_created_on_the_activities_collection() {
    let src = production(MAIN);
    assert!(
        src.contains("activities.create_indexes("),
        "the index is declared but never created on user_activities"
    );
}

/// The index and the queries must name the same collection.
#[test]
fn the_indexed_collection_is_the_one_the_queries_open() {
    assert!(
        production(MAIN).contains("\"user_activities\""),
        "main.rs does not name the activities collection"
    );
    assert!(
        production(IMPLS).contains("\"user_activities\""),
        "impls.rs no longer opens user_activities; the index may be orphaned"
    );
}

// ── The collection has no writer (#70) ─────────────────────────────────
//
// `GET /api/users/activity` and the GDPR export both read `user_activities`,
// and nothing in the monorepo writes it - no insert, in any service. So both
// return empty for every user, always.
//
// This does not invalidate the index above: it is correctly shaped for the
// queries, and will matter the moment a writer exists. It does mean the
// documentation must not describe a populated log.

/// Writes would appear as an insert against the activities collection. The
/// accessor and the index declaration are reads and setup, not writes.
fn has_an_activity_writer() -> bool {
    let impls = include_str!("../src/impls.rs");
    let production = impls.split("#[cfg(test)]").next().unwrap_or("");

    // Must be an insert against the activities collection, not merely both
    // strings appearing somewhere in the file - `impls.rs` inserts into
    // `users`, and a looser check reported a writer that does not exist.
    production.contains("insert_activity")
        || production.contains("log_activity")
        || production
            .split("activities_collection")
            .skip(1)
            .any(|after| after[..after.len().min(400)].contains("insert_"))
}

#[test]
fn the_docs_do_not_claim_a_populated_activity_log() {
    let doc = std::fs::read_to_string("CLAUDE.md").expect("CLAUDE.md is readable");

    // The claim and the implementation must move together: once a writer
    // exists this assertion stops constraining the wording.
    let claims_populated = doc.contains("Get user activity log (paginated)");

    assert!(
        !claims_populated || has_an_activity_writer(),
        "CLAUDE.md describes a user activity log, but nothing writes \
         user_activities - the endpoint returns empty for every user"
    );
}
