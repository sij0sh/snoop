//! Pi session ingestion: one episode unit per nonempty user turn.

use std::path::{Path, PathBuf};

use crate::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, UnitKind};
use crate::ingest::units::{estimate_tokens, split_oversized, MAX_TOKENS};

mod jsonl;
mod tools;

#[cfg(test)]
mod tests;

use jsonl::{parse_pi_episodes, EpisodeTurn};

pub const MAX_SESSION_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_EPISODES_PER_SESSION: usize = 200;

/// Bumped whenever the turn-to-unit policy changes so stored hashes change.
pub const TURN_POLICY_VERSION: &str = "user-turn-v1";

const DEFAULT_SESSIONS_ROOT: &str = ".pi/agent/sessions";

pub fn session_locator(session_id: &str) -> String {
    format!("pi-session:{session_id}")
}

pub fn session_directory_name(cwd: &str) -> String {
    format!(
        "--{}--",
        cwd.trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-")
    )
}

pub fn sessions_root(home: &Path) -> PathBuf {
    if let Some(override_root) = std::env::var_os("SNOOP_SESSIONS_ROOT") {
        return PathBuf::from(override_root);
    }
    home.join(DEFAULT_SESSIONS_ROOT)
}

fn directory_entries_matching(
    root: &Path,
    extension: &str,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }
        if entry
            .metadata()
            .is_ok_and(|meta| meta.len() > MAX_SESSION_BYTES)
        {
            continue;
        }
        files.push(path);
    }
    files.sort();
    Ok(files)
}

#[derive(Debug, Clone)]
pub struct SessionFile {
    pub path: PathBuf,
    pub session_id: String,
}

pub fn discover_sessions(
    home: &Path,
    repo_root: &Path,
) -> Result<Vec<SessionFile>, Box<dyn std::error::Error + Send + Sync>> {
    let directory = sessions_root(home).join(session_directory_name(&repo_root.to_string_lossy()));
    let mut sessions = Vec::new();
    for path in directory_entries_matching(&directory, "jsonl")? {
        let Some(session_id) = jsonl::read_session_id(&path) else {
            continue;
        };
        sessions.push(SessionFile { path, session_id });
    }
    Ok(sessions)
}

/// Splits the turn body only when breadcrumb plus body exceeds the unit
/// token budget; otherwise the turn stays a single piece.
fn turn_pieces(breadcrumb: &str, body: &str) -> Vec<String> {
    if estimate_tokens(&format!("{breadcrumb}\n\n{body}")) <= MAX_TOKENS {
        return vec![body.to_string()];
    }
    // Reserve room for the per-piece breadcrumb suffix so every piece
    // stays within the token budget.
    let max_chars = (MAX_TOKENS * 4)
        .saturating_sub(breadcrumb.chars().count() + 18)
        .max(1);
    split_oversized(body, max_chars)
        .into_iter()
        .map(|(piece, _, _)| piece)
        .collect()
}

fn build_turn_units(turns: &[EpisodeTurn], session_id: &str) -> Vec<BuiltUnit> {
    let locator = session_locator(session_id);
    let mut units = Vec::new();
    let mut episode = 0usize;
    for turn in turns {
        // Turns before the first user message are not indexed.
        if turn.user_text().trim().is_empty() {
            continue;
        }
        episode += 1;
        let breadcrumb = format!("{locator} > episode {episode}");
        let pieces = turn_pieces(&breadcrumb, &turn.body());
        let total = pieces.len();
        let files = turn.files();
        let commands = turn.commands();
        let outcomes = turn.outcomes();
        for (offset, piece) in pieces.iter().enumerate() {
            let piece_ordinal = offset + 1;
            let evidence = if total == 1 {
                format!("{breadcrumb}\n\n{piece}")
            } else {
                format!("{breadcrumb} > piece {piece_ordinal}\n\n{piece}")
            };
            let routing =
                format!("source: agent_episode\nsession: {session_id}\nepisode: {episode}\n");
            let content_hash = hash_segments(&[
                TURN_POLICY_VERSION,
                &locator,
                turn.start_key(),
                &piece_ordinal.to_string(),
                &evidence,
                &routing,
            ]);
            let mut metadata = serde_json::json!({
                "policy_version": TURN_POLICY_VERSION,
                "session": session_id,
                "episode": episode,
                "piece": piece_ordinal,
                "pieces": total,
                "files": files,
                "commands": commands,
                "outcomes": outcomes,
            });
            crate::metadata::timestamp::set(&mut metadata, turn.timestamp);
            let (start_byte, end_byte) = turn.byte_range();
            metadata["source_range"] = serde_json::json!({
                "start_byte": start_byte,
                "end_byte": end_byte,
            });
            let mut anchors = vec![BuiltAnchor {
                kind: AnchorKind::Session,
                value: session_id.to_string(),
                relationship: "part_of".to_string(),
            }];
            for file in &files {
                anchors.push(BuiltAnchor {
                    kind: AnchorKind::File,
                    value: file.clone(),
                    relationship: "touched".to_string(),
                });
            }
            units.push(BuiltUnit {
                kind: UnitKind::Episode,
                token_count: estimate_tokens(&evidence),
                content_hash,
                evidence_text: evidence,
                routing_text: routing,
                metadata,
                anchors,
            });
        }
    }
    units
}

fn finalize_turn_cap(turns: &mut Vec<EpisodeTurn>) {
    if turns.len() > MAX_EPISODES_PER_SESSION {
        let excess = turns.len() - MAX_EPISODES_PER_SESSION;
        turns.drain(..excess);
    }
}

pub fn ingest_pi_session(
    content: &str,
    session_id: &str,
) -> Result<Vec<BuiltUnit>, Box<dyn std::error::Error + Send + Sync>> {
    let mut turns = parse_pi_episodes(content);
    finalize_turn_cap(&mut turns);
    Ok(build_turn_units(&turns, session_id))
}
