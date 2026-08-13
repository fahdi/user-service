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
