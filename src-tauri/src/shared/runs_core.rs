pub(crate) mod capture;
pub(crate) mod recording;
pub(crate) mod store;
pub(crate) mod types;

use std::path::Path;
pub(crate) fn db_path(storage_path: &Path) -> std::path::PathBuf {
    storage_path.with_file_name("runs.sqlite3")
}
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub(crate) fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod contention_tests;
