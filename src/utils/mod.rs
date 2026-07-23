// Utility modules for user service
pub mod security;
/// True when an index-creation error just means a conflicting index is
/// already present (benign at startup). Modern MongoDB reports these as
/// IndexOptionsConflict (code 85, "already exists") or IndexKeySpecsConflict
/// (code 86, "same name"); older servers use plain "already exists" text.
pub fn is_index_conflict(msg: &str) -> bool {
    msg.contains("already exists")
        || msg.contains("same name")
        || msg.contains("IndexOptionsConflict")
        || msg.contains("IndexKeySpecsConflict")
}
