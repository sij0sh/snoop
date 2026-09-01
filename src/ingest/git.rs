mod align;
mod anchors;
mod emit;
mod history;
#[cfg(test)]
mod tests;

pub use emit::ingest_commit;
pub use history::{
    is_ancestor, is_history_root, list_commits, list_commits_past, reachable_oids, CommitRef,
};

pub const MAX_COMMITS: usize = 500;
pub const MAX_HUNKS_PER_FILE: usize = 64;
