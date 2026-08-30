//! Shared harness for integration tests: repo fixtures and an MCP child
//! process with timeout-bounded line reading.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use std::io::BufRead;
use std::io::BufReader;

pub fn simple_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("README.md"),
        "# Auth\n\n`refresh_session` rotates the session token after validation.\n",
    )
    .unwrap();
}

/// Indexes the simple fixture with the mock embedder; returns (dir, db path).
pub fn indexed_fixture() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    simple_fixture(&repo);
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");
    for args in [
        vec!["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()],
        vec![
            "index",
            repo.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ],
    ] {
        let output = Command::new(binary)
            .args(&args)
            .env("SNOOP_EMBED_URL", "mock")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    (directory, db)
}

/// A TCP server that accepts connections and never replies: embed requests
/// connect successfully and then hang until the client-side deadline.
pub struct HangingEmbedServer {
    listener: std::net::TcpListener,
}

impl HangingEmbedServer {
    pub fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let handle = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            for stream in handle.incoming() {
                // Hold each connection open without reading or writing.
                let _ = stream;
                std::thread::sleep(Duration::from_secs(3600));
            }
        });
        Self { listener }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.listener.local_addr().unwrap())
    }
}

pub struct McpChild {
    child: Child,
    responses: mpsc::Receiver<String>,
}

impl McpChild {
    pub fn start(db: &Path, env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_snoop"));
        command
            .args(["mcp", "--db"])
            .arg(db)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let stdout: ChildStdout = child.stdout.take().expect("piped stdout");
        let (sender, receiver) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if sender.send(line.unwrap_or_default()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            responses: receiver,
        }
    }

    pub fn send(&mut self, value: &serde_json::Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, value).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// Reads one JSON response line, waiting at most `timeout`.
    pub fn read_response(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        let line = self.responses.recv_timeout(timeout).ok()?;
        serde_json::from_str(&line).ok()
    }
}

impl Drop for McpChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
