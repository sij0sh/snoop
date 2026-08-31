//! CLI-facing coding-agent wiring and glue for `snoop install`.

pub mod embed;
mod merge;

use std::fs;
use std::path::{Path, PathBuf};

pub use embed::file_embed_config;

const PI_EXTENSION_TS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/extensions/snoop-pi.ts"
));

pub const AGENT_NAMES: &[&str] = &[
    "pi",
    "claude-code",
    "cursor",
    "codex",
    "opencode",
    "gemini",
    "vscode",
    "windsurf",
    "kiro",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireOutcome {
    Wired,
    Updated,
    AlreadyConfigured,
}

impl WireOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Wired => "wired",
            Self::Updated => "updated",
            Self::AlreadyConfigured => "already-configured",
        }
    }
}

#[derive(clap::Args)]
pub struct InstallOptions {
    /// install target, e.g. "embedder"; omit to wire detected coding agents
    #[arg(value_name = "TARGET")]
    pub target: Option<String>,
    /// print agent detection status without writing anything
    #[arg(long)]
    pub list: bool,
    /// wire exactly these agents, ignoring detection (repeatable)
    #[arg(long = "agent", value_name = "NAME")]
    pub agents: Vec<String>,
    /// embedder install directory (default: ~/.snoop)
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// re-download even if llama.cpp and the model already exist
    #[arg(long)]
    pub force: bool,
    /// model download URL (filename is derived from it)
    #[arg(long, value_name = "URL")]
    pub model_url: Option<String>,
    /// embedding model version (default: Qwen3-Embedding-0.6B-Q8_0)
    #[arg(long, value_name = "NAME")]
    pub version: Option<String>,
}

struct AgentSpec {
    name: &'static str,
    detect: fn() -> bool,
    config_path: fn() -> Result<PathBuf, String>,
    wire: fn() -> Result<WireOutcome, String>,
}

const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        name: "pi",
        detect: detect_pi,
        config_path: pi_config_path,
        wire: wire_pi,
    },
    AgentSpec {
        name: "claude-code",
        detect: detect_claude_code,
        config_path: claude_config_path,
        wire: wire_claude_code,
    },
    AgentSpec {
        name: "cursor",
        detect: detect_cursor,
        config_path: cursor_config_path,
        wire: wire_cursor,
    },
    AgentSpec {
        name: "codex",
        detect: detect_codex,
        config_path: codex_config_path,
        wire: wire_codex,
    },
    AgentSpec {
        name: "opencode",
        detect: detect_opencode,
        config_path: opencode_config_path,
        wire: wire_opencode,
    },
    AgentSpec {
        name: "gemini",
        detect: detect_gemini,
        config_path: gemini_config_path,
        wire: wire_gemini,
    },
    AgentSpec {
        name: "vscode",
        detect: detect_vscode,
        config_path: vscode_config_path,
        wire: wire_vscode,
    },
    AgentSpec {
        name: "windsurf",
        detect: detect_windsurf,
        config_path: windsurf_config_path,
        wire: wire_windsurf,
    },
    AgentSpec {
        name: "kiro",
        detect: detect_kiro,
        config_path: kiro_config_path,
        wire: wire_kiro,
    },
];

pub fn run_install(options: InstallOptions) -> Result<(), String> {
    match options.target.as_deref() {
        Some("embedder") => {
            if !options.agents.is_empty() || options.list {
                return Err(
                    "`snoop install embedder` cannot be combined with --agent or --list".into(),
                );
            }
            embed::install_embedder(&embed::EmbedderOptions {
                dir: options.dir,
                force: options.force,
                model_url: options.model_url,
                version: options.version,
            })
        }
        Some(other) => Err(format!(
            "unknown install target: {other} (expected: embedder)"
        )),
        None if options.list => {
            print_agent_status();
            Ok(())
        }
        None => install_agents(&options.agents),
    }
}

fn print_agent_status() {
    for agent in AGENTS {
        let path = match (agent.config_path)() {
            Ok(path) => path.display().to_string(),
            Err(error) => error,
        };
        let status = if (agent.detect)() {
            "detected"
        } else {
            "not-detected"
        };
        println!("{:<12} {:<19} {}", agent.name, status, path);
    }
}

fn install_agents(requested: &[String]) -> Result<(), String> {
    let names: Vec<&'static str> = if requested.is_empty() {
        AGENTS.iter().map(|agent| agent.name).collect()
    } else {
        validate_agent_names(requested)?
    };
    let mut failures = Vec::new();
    for name in names {
        let agent = agent_by_name(name).expect("validated agent name");
        // Explicit --agent names skip detection: the user asked for them.
        if requested.is_empty() && !(agent.detect)() {
            let path = config_path_text(agent);
            println!("{:<12} {:<19} {}", name, "not-detected", path);
            continue;
        }
        match (agent.wire)() {
            Ok(outcome) => println!(
                "{:<12} {:<19} {}",
                name,
                outcome.label(),
                config_path_text(agent)
            ),
            Err(error) => {
                println!("{:<12} error: {error}", name);
                failures.push(name);
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("failed to wire: {}", failures.join(", ")))
    }
}

fn config_path_text(agent: &AgentSpec) -> String {
    (agent.config_path)()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

pub fn validate_agent_names(requested: &[String]) -> Result<Vec<&'static str>, String> {
    requested
        .iter()
        .map(|name| {
            agent_by_name(name)
                .map(|agent| agent.name)
                .ok_or_else(|| format!("unknown agent: {name} (valid: {})", AGENT_NAMES.join(", ")))
        })
        .collect()
}

fn agent_by_name(name: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|agent| agent.name == name)
}

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| format!("cannot determine home directory ({key} is unset)"))
}

fn home_sub_path(parts: &[&str]) -> Result<PathBuf, String> {
    let mut path = home_dir()?;
    for part in parts {
        path.push(part);
    }
    Ok(path)
}

fn dir_exists(parts: &[&str]) -> bool {
    home_sub_path(parts)
        .map(|path| path.is_dir())
        .unwrap_or(false)
}

fn file_exists(parts: &[&str]) -> bool {
    home_sub_path(parts)
        .map(|path| path.is_file())
        .unwrap_or(false)
}

fn on_path(binary: &str) -> bool {
    let Ok(search_path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&search_path).any(|dir| {
        let candidate = dir.join(binary);
        candidate.is_file() && is_executable(&candidate)
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

fn detect_pi() -> bool {
    on_path("pi") || dir_exists(&[".pi", "agent"])
}

fn pi_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".pi", "agent", "extensions", "snoop-pi.ts"])
}

fn detect_claude_code() -> bool {
    on_path("claude") || file_exists(&[".claude.json"])
}

fn claude_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".claude.json"])
}

fn detect_cursor() -> bool {
    on_path("cursor") || dir_exists(&[".cursor"])
}

fn cursor_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".cursor", "mcp.json"])
}

fn detect_codex() -> bool {
    on_path("codex") || dir_exists(&[".codex"])
}

fn codex_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".codex", "config.toml"])
}

fn detect_opencode() -> bool {
    on_path("opencode") || dir_exists(&[".config", "opencode"])
}

fn opencode_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".config", "opencode", "opencode.json"])
}

fn detect_gemini() -> bool {
    on_path("gemini") || dir_exists(&[".gemini"])
}

fn gemini_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".gemini", "settings.json"])
}

fn detect_vscode() -> bool {
    on_path("code") || vscode_user_dir().map(|dir| dir.is_dir()).unwrap_or(false)
}

fn vscode_user_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        home_sub_path(&["Library", "Application Support", "Code", "User"])
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("Code").join("User"))
            .ok_or_else(|| "cannot determine app data directory (APPDATA is unset)".to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        home_sub_path(&[".config", "Code", "User"])
    }
}

fn vscode_config_path() -> Result<PathBuf, String> {
    Ok(vscode_user_dir()?.join("mcp.json"))
}

fn detect_windsurf() -> bool {
    dir_exists(&[".codeium", "windsurf"]) || on_path("windsurf")
}

fn windsurf_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".codeium", "windsurf", "mcp_config.json"])
}

fn detect_kiro() -> bool {
    dir_exists(&[".kiro"]) || on_path("kiro")
}

fn kiro_config_path() -> Result<PathBuf, String> {
    home_sub_path(&[".kiro", "settings", "mcp.json"])
}

fn snoop_command_entry() -> serde_json::Value {
    serde_json::json!({ "command": "snoop", "args": ["mcp"] })
}

fn opencode_entry() -> serde_json::Value {
    serde_json::json!({
        "type": "local",
        "command": ["snoop", "mcp"],
        "enabled": true,
    })
}

fn wire_pi() -> Result<WireOutcome, String> {
    wire_pi_to(&pi_config_path()?)
}

fn wire_claude_code() -> Result<WireOutcome, String> {
    merge::merge_json_entry(
        &claude_config_path()?,
        "mcpServers",
        "snoop",
        &snoop_command_entry(),
    )
}

fn wire_cursor() -> Result<WireOutcome, String> {
    merge::merge_json_entry(
        &cursor_config_path()?,
        "mcpServers",
        "snoop",
        &snoop_command_entry(),
    )
}

fn wire_codex() -> Result<WireOutcome, String> {
    merge::merge_toml_entry(&codex_config_path()?, "mcp_servers", "snoop")
}

fn wire_opencode() -> Result<WireOutcome, String> {
    merge::merge_json_entry(&opencode_config_path()?, "mcp", "snoop", &opencode_entry())
}

fn wire_gemini() -> Result<WireOutcome, String> {
    merge::merge_json_entry(
        &gemini_config_path()?,
        "mcpServers",
        "snoop",
        &snoop_command_entry(),
    )
}

fn wire_vscode() -> Result<WireOutcome, String> {
    merge::merge_json_entry(
        &vscode_config_path()?,
        "servers",
        "snoop",
        &snoop_command_entry(),
    )
}

fn wire_windsurf() -> Result<WireOutcome, String> {
    merge::merge_json_entry(
        &windsurf_config_path()?,
        "mcpServers",
        "snoop",
        &snoop_command_entry(),
    )
}

fn wire_kiro() -> Result<WireOutcome, String> {
    merge::merge_json_entry(
        &kiro_config_path()?,
        "mcpServers",
        "snoop",
        &snoop_command_entry(),
    )
}

fn wire_pi_to(dest: &Path) -> Result<WireOutcome, String> {
    let bytes = PI_EXTENSION_TS.as_bytes();
    if fs::read(dest)
        .map(|existing| existing == bytes)
        .unwrap_or(false)
    {
        return Ok(WireOutcome::AlreadyConfigured);
    }
    let existed = dest.exists();
    merge::write_atomic(dest, bytes)?;
    Ok(if existed {
        WireOutcome::Updated
    } else {
        WireOutcome::Wired
    })
}

#[cfg(test)]
mod tests;
