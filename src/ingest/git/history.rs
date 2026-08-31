use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::hash_segments;

// Scaling-guard counter (computational-scaling-audit 20260830195149-6f1a96a5,
// finding 3). Invariant: spawns per ingest run <= 4C + k0, independent of
// files-per-commit F (was exactly 4F + 1 per commit).
pub(crate) static GIT_SPAWNS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct CommitRef {
    pub oid: String,
    pub timestamp: i64,
    pub message: String,
    pub content_hash: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    GIT_SPAWNS.fetch_add(1, Ordering::Relaxed);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.quotepath=false",
            "-c",
            "diff.algorithm=myers",
            "--no-pager",
        ])
        .args(args)
        .output()
        .map_err(|error| format!("git spawn failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn is_history_root(root: &Path) -> bool {
    root.join(".git").exists()
}

pub fn list_commits(
    root: &Path,
    max: usize,
) -> Result<Vec<CommitRef>, Box<dyn std::error::Error + Send + Sync>> {
    if git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let max_arg = format!("--max-count={max}");
    let out = git(
        root,
        &["log", &max_arg, "--format=%H%x1f%ct%x1f%B%x1e", "HEAD"],
    )?;
    parse_log(&out)
}

pub fn list_commits_past(
    root: &Path,
    max: usize,
    boundary_tip: &str,
) -> Result<Vec<CommitRef>, Box<dyn std::error::Error + Send + Sync>> {
    if git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let range = format!("{boundary_tip}..HEAD");
    let max_arg = format!("--max-count={max}");
    match git(
        root,
        &["log", &max_arg, "--format=%H%x1f%ct%x1f%B%x1e", &range],
    ) {
        Ok(out) => parse_log(&out),
        Err(_) => list_commits(root, max),
    }
}

fn parse_log(out: &str) -> Result<Vec<CommitRef>, Box<dyn std::error::Error + Send + Sync>> {
    let mut commits = Vec::new();
    for record in out.split('\x1e') {
        let record = record.trim_matches('\n');
        if record.trim().is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, '\x1f');
        let (Some(oid), Some(timestamp), Some(message)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let oid = oid.trim();
        commits.push(CommitRef {
            oid: oid.to_string(),
            timestamp: timestamp.trim().parse().unwrap_or(0),
            message: message.trim_end().to_string(),
            content_hash: hash_segments(&["git-commit", oid]),
        });
    }
    Ok(commits)
}

#[derive(Debug, Clone)]
pub(super) struct ChangedFile {
    pub(super) path: String,
    pub(super) old_path: Option<String>,
    pub(super) status: char,
    pub(super) patch: String,
}

pub(super) fn changed_files(
    root: &Path,
    oid: &str,
) -> Result<Vec<ChangedFile>, Box<dyn std::error::Error + Send + Sync>> {
    let status_out = git(
        root,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "-r",
            "-M",
            "--name-status",
            oid,
        ],
    )?;
    // Finding 3 (audit 20260830195149-6f1a96a5): one batched patch per
    // commit instead of one diff-tree spawn per changed file. git emits
    // name-status rows and patch headers in the same tree order, so the two
    // outputs pair positionally; patch bodies cannot contain a "diff --git"
    // line at column 0 (body lines start with ' ', '+', '-', or '@').
    let patch_out = git(
        root,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "-p",
            "-M",
            "--no-ext-diff",
            "-U3",
            oid,
        ],
    )?;
    let mut patches: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in patch_out.lines() {
        if line.starts_with("diff --git ") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            current = Some(String::new());
        }
        if let Some(patch) = current.as_mut() {
            patch.push_str(line);
            patch.push('\n');
        }
    }
    if let Some(patch) = current.take() {
        patches.push(patch);
    }

    let status_rows: Vec<&str> = status_out
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if status_rows.len() != patches.len() {
        return Err(format!(
            "git diff-tree {oid}: {} name-status rows but {} patch files",
            status_rows.len(),
            patches.len()
        )
        .into());
    }
    let mut files = Vec::new();
    for (status_field, patch) in status_rows.into_iter().zip(patches) {
        let mut fields = status_field.split('\t');
        let Some(status_field) = fields.next() else {
            continue;
        };
        let status = status_field.trim().chars().next().unwrap_or('M');
        if matches!(status, 'R' | 'C') {
            let score = status_field.trim()[1..].to_string();
            if score.is_empty() || !score.chars().all(|digit| digit.is_ascii_digit()) {
                continue;
            }
            let (Some(old_path), Some(new_path)) = (fields.next(), fields.next()) else {
                continue;
            };
            if old_path.is_empty() || new_path.is_empty() || patch.trim().is_empty() {
                continue;
            }
            files.push(ChangedFile {
                path: new_path.to_string(),
                old_path: Some(old_path.to_string()),
                status,
                patch,
            });
        } else {
            let Some(path) = fields.next() else {
                continue;
            };
            if path.is_empty() || patch.trim().is_empty() {
                continue;
            }
            files.push(ChangedFile {
                path: path.to_string(),
                old_path: None,
                status,
                patch,
            });
        }
    }
    Ok(files)
}

/// Long-lived `git cat-file --batch` session (audit finding 3): one spawned
/// process answers every blob read of a commit instead of two `git show`
/// spawns per supported file. Responses are read sequentially, one request
/// in flight at a time.
pub(super) struct BlobReader {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    broken: bool,
}

impl BlobReader {
    pub(super) fn spawn(root: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        GIT_SPAWNS.fetch_add(1, Ordering::Relaxed);
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "core.quotepath=false", "cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("git cat-file spawn failed: {error}"))?;
        let stdin = child.stdin.take().ok_or("git cat-file stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("git cat-file stdout unavailable")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            broken: false,
        })
    }

    /// Blob contents for `rev:path`; `None` for missing or binary blobs —
    /// the same contract as the `git show` helper it replaces.
    pub(super) fn read(&mut self, rev: &str, path: &str) -> Option<String> {
        if self.broken {
            return None;
        }
        if self
            .stdin
            .write_all(format!("{rev}:{path}\n").as_bytes())
            .is_err()
            || self.stdin.flush().is_err()
        {
            self.broken = true;
            return None;
        }
        let mut header = String::new();
        if self.stdout.read_line(&mut header).is_err() || header.trim().is_empty() {
            self.broken = true;
            return None;
        }
        let mut fields = header.split_whitespace();
        let kind = fields.nth(1);
        let size = fields.next().and_then(|size| size.parse::<usize>().ok());
        let (Some("blob"), Some(size)) = (kind, size) else {
            // "missing" reply or non-blob object: same as the old None.
            return None;
        };
        let mut bytes = vec![0u8; size];
        if self.stdout.read_exact(&mut bytes).is_err() {
            self.broken = true;
            return None;
        }
        let mut trailing = [0u8; 1];
        if self.stdout.read_exact(&mut trailing).is_err() {
            self.broken = true;
            return None;
        }
        if bytes.contains(&0) {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn parent_oid(root: &Path, oid: &str) -> Option<String> {
    let out = git(root, &["show", "-s", "--format=%P", oid]).ok()?;
    out.lines()
        .next()?
        .split_whitespace()
        .next()
        .map(String::from)
}
