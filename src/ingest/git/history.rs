use std::path::Path;

use crate::core::hash_segments;

#[derive(Debug, Clone)]
pub struct CommitRef {
    pub oid: String,
    pub timestamp: i64,
    pub message: String,
    pub content_hash: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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
    let out = git(
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
    let mut files = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
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
            if old_path.is_empty() || new_path.is_empty() {
                continue;
            }
            let patch = git(
                root,
                &[
                    "diff-tree",
                    "--root",
                    "-p",
                    "-M",
                    "--no-ext-diff",
                    "-U3",
                    oid,
                    "--",
                    old_path,
                    new_path,
                ],
            )?;
            if patch.trim().is_empty() {
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
            if path.is_empty() {
                continue;
            }
            let patch = git(
                root,
                &[
                    "diff-tree",
                    "--root",
                    "-p",
                    "--no-ext-diff",
                    "-U3",
                    oid,
                    "--",
                    path,
                ],
            )?;
            if patch.trim().is_empty() {
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

pub(super) fn blob(root: &Path, rev: &str, path: &str) -> Option<String> {
    let rev_path = format!("{rev}:{path}");
    git(root, &["show", &rev_path])
        .ok()
        .filter(|content| !content.contains('\0'))
}

pub(super) fn parent_oid(root: &Path, oid: &str) -> Option<String> {
    let out = git(root, &["show", "-s", "--format=%P", oid]).ok()?;
    out.lines()
        .next()?
        .split_whitespace()
        .next()
        .map(String::from)
}
