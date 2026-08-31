use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::core::SourceKind;

pub const MAX_SOURCE_BYTES: u64 = 10 * 1024 * 1024;

const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    ".git",
];

#[derive(Debug, Clone)]
pub struct ScannedSource {
    pub path: PathBuf,
    pub locator: String,
    pub kind: SourceKind,
    pub content_hash: String,
    pub modified_at: Option<i64>,
}

pub fn repository_root(start: &Path) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let canonical = start.canonicalize()?;
    let base = if canonical.is_file() {
        canonical
            .parent()
            .ok_or("path has no parent")?
            .to_path_buf()
    } else {
        canonical
    };
    for ancestor in base.ancestors() {
        if ancestor.join(".git").exists() {
            return Ok(ancestor.to_path_buf());
        }
    }
    Ok(base)
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn classify(path: &Path) -> Option<SourceKind> {
    if crate::ingest::code::code_extension(&path.to_string_lossy()).is_some() {
        return Some(SourceKind::Code);
    }
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" | "mdown" => Some(SourceKind::Markdown),
        "txt" => Some(SourceKind::Text),
        _ => None,
    }
}

/// Walk `root` and return `(sources, skipped)` where `skipped` counts
/// entries that vanished, became unreadable, or raced a size/permission
/// change mid-walk. Transient per-file IO failures are skipped with a stderr
/// warning instead of aborting the whole index run (defect-audit
/// 20260831023057-8ecdc8ca c3); only walk-setup errors (bad root) abort.
pub fn scan(
    root: &Path,
) -> Result<(Vec<ScannedSource>, usize), Box<dyn std::error::Error + Send + Sync>> {
    let mut sources = Vec::new();
    let mut skipped = 0_usize;
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|entry| {
            !entry.file_type().is_some_and(|kind| kind.is_dir())
                || entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !SKIP_DIRS.contains(&name))
        });
    let walker = builder.build();
    for entry in walker {
        // A vanished or unreadable entry races the walk; skip it and keep
        // going (c3). It is picked up again on the next ensure.
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                skipped += 1;
                let path = match &error {
                    ignore::Error::WithPath { path, .. } => path.display().to_string(),
                    _ => "<unknown>".to_string(),
                };
                eprintln!("warning: skipped unreadable scan entry {path}: {error}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let locator = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if locator
            .split('/')
            .any(|segment| SKIP_DIRS.contains(&segment))
        {
            continue;
        }
        let Some(kind) = classify(entry.path()) else {
            continue;
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                skipped += 1;
                eprintln!(
                    "warning: skipped unreadable file {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        if metadata.len() > MAX_SOURCE_BYTES {
            continue;
        }
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64);
        let content_hash = match hash_file(entry.path()) {
            Ok(content_hash) => content_hash,
            Err(error) => {
                skipped += 1;
                eprintln!(
                    "warning: skipped unreadable file {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        sources.push(ScannedSource {
            path: entry.into_path(),
            locator,
            kind,
            content_hash,
            modified_at,
        });
    }
    sources.sort_by(|a, b| a.locator.cmp(&b.locator));
    Ok((sources, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_is_stable_and_skips_hidden_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("README.md"), "# Hello").unwrap();
        std::fs::write(directory.path().join("ignored.md"), "ignored").unwrap();
        std::fs::write(directory.path().join(".gitignore"), "ignored.md\n").unwrap();
        std::fs::create_dir(directory.path().join(".hidden")).unwrap();
        std::fs::write(directory.path().join(".hidden/no.md"), "ignored").unwrap();
        std::fs::create_dir(directory.path().join("target")).unwrap();
        std::fs::write(
            directory.path().join("target/generated.rs"),
            "fn ignored() {}",
        )
        .unwrap();
        let (first, first_skipped) = scan(directory.path()).unwrap();
        let (second, second_skipped) = scan(directory.path()).unwrap();
        assert_eq!(first_skipped, 0);
        assert_eq!(second_skipped, 0);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].locator, second[0].locator);
        assert_eq!(first[0].content_hash, second[0].content_hash);
    }
}
