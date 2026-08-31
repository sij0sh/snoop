//! Embedder installation (`snoop install embedder`), config fallback, and
//! embed-server spawn helpers for `snoop embed`.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

const GITHUB_API_LATEST: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";
const GITHUB_API_TAG: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/";
const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/main/Qwen3-Embedding-0.6B-Q8_0.gguf";
const DEFAULT_MODEL_VERSION: &str = "Qwen3-Embedding-0.6B-Q8_0";
const DEFAULT_PORT: u16 = 8097;
const TAG_ASSET_SUFFIX: &str = "nightly-tag.txt";
const EXCLUDED_ASSET_TOKENS: &[&str] = &["vulkan", "rocm", "sycl", "openvino"];

pub const CONFIG_FILE_NAME: &str = "config.json";

pub struct EmbedderOptions {
    pub dir: Option<PathBuf>,
    pub force: bool,
    pub model_url: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    LinuxX64,
    LinuxArm64,
    MacosArm64,
    MacosX64,
    WindowsX64,
    WindowsArm64,
}

pub fn current_platform() -> Result<Platform, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(Platform::LinuxX64),
        ("linux", "aarch64") => Ok(Platform::LinuxArm64),
        ("macos", "aarch64") => Ok(Platform::MacosArm64),
        ("macos", "x86_64") => Ok(Platform::MacosX64),
        ("windows", "x86_64") => Ok(Platform::WindowsX64),
        ("windows", "aarch64") => Ok(Platform::WindowsArm64),
        (os, arch) => Err(format!(
            "unsupported platform for embedder install: {os}-{arch}"
        )),
    }
}

fn asset_pattern(platform: Platform) -> &'static str {
    match platform {
        Platform::LinuxX64 => "bin-ubuntu-x64",
        Platform::LinuxArm64 => "bin-ubuntu-arm64",
        Platform::MacosArm64 => "bin-macos-arm64",
        Platform::MacosX64 => "bin-macos-x64",
        Platform::WindowsX64 => "bin-win-cpu-x64",
        Platform::WindowsArm64 => "bin-win-cpu-arm64",
    }
}

/// First asset name matching the platform pattern, skipping accelerator
/// build variants (vulkan/rocm/sycl/openvino).
pub fn select_llama_asset<'a>(
    platform: Platform,
    names: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let pattern = asset_pattern(platform);
    names
        .into_iter()
        .find(|name| {
            name.contains(pattern)
                && !EXCLUDED_ASSET_TOKENS
                    .iter()
                    .any(|token| name.contains(token))
        })
        .map(str::to_string)
}

pub fn install_embedder(options: &EmbedderOptions) -> Result<(), String> {
    let dir = options
        .dir
        .clone()
        .map(Ok)
        .unwrap_or_else(default_embed_dir)?;
    let bin = dir.join("bin");
    let models = dir.join("models");
    fs::create_dir_all(&bin).map_err(|error| format!("create {}: {error}", bin.display()))?;
    fs::create_dir_all(&models).map_err(|error| format!("create {}: {error}", models.display()))?;

    let (version, model_file, model_url) =
        resolve_model(options.version.as_deref(), options.model_url.as_deref());
    let model_path = models.join(&model_file);

    install_llama_cpp(&dir, &bin, options.force)?;
    install_model(&model_url, &model_path)?;
    write_embed_config(&dir, &bin.join(server_binary_name()), &model_path, &version)?;

    println!("next steps:");
    println!("  1. start the embedding server in another terminal: snoop embed");
    println!("     (or run it under any service manager)");
    println!("  2. build vectors in a project: snoop index .");
    println!("SNOOP_EMBED_URL and SNOOP_EMBED_VERSION env vars still override {CONFIG_FILE_NAME}.");
    Ok(())
}

fn install_llama_cpp(dir: &Path, bin: &Path, force: bool) -> Result<(), String> {
    let server = bin.join(server_binary_name());
    if server.is_file() && !force {
        println!("llama.cpp already installed: {}", server.display());
        return Ok(());
    }
    let tag = fetch_latest_tag()?;
    println!("fetching llama.cpp release {tag}");
    let release = fetch_json(&format!("{GITHUB_API_TAG}{tag}"))?;
    let names: Vec<&str> = release["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| asset["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let asset_name = select_llama_asset(current_platform()?, names)
        .ok_or_else(|| format!("no llama.cpp asset for this platform in release {tag}"))?;
    let url = asset_url(&release, &asset_name)?;
    let archive = dir.join(&asset_name);
    println!("downloading {asset_name}");
    let bytes = download(&url, &archive)?;
    println!(
        "downloaded {asset_name} ({:.1} MiB)",
        bytes as f64 / (1024.0 * 1024.0)
    );

    let extract_dir = dir.join(".extract");
    let _ = fs::remove_dir_all(&extract_dir);
    extract(&archive, &extract_dir)?;
    copy_archive_contents(&extract_dir, &tag, bin)?;
    let _ = fs::remove_dir_all(&extract_dir);
    let _ = fs::remove_file(&archive);
    println!("llama.cpp installed: {}", server.display());
    Ok(())
}

fn install_model(url: &str, model_path: &Path) -> Result<(), String> {
    if model_path.is_file() {
        println!("model already installed: {}", model_path.display());
        return Ok(());
    }
    println!("downloading model {url}");
    let bytes = download(url, model_path)?;
    println!(
        "model saved: {} ({:.1} MiB)",
        model_path.display(),
        bytes as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

fn fetch_latest_tag() -> Result<String, String> {
    let release = fetch_json(GITHUB_API_LATEST)?;
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| "llama.cpp release JSON has no assets array".to_string())?;
    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("");
        if name.ends_with(TAG_ASSET_SUFFIX) {
            let url = asset["browser_download_url"]
                .as_str()
                .ok_or_else(|| format!("{name} asset is missing a download url"))?;
            let tag = fetch_text(url)?.trim().to_string();
            return if tag.is_empty() {
                Err(format!("{name} asset is empty"))
            } else {
                Ok(tag)
            };
        }
    }
    Err(format!(
        "no asset ending with {TAG_ASSET_SUFFIX} in the latest llama.cpp release"
    ))
}

fn asset_url(release: &serde_json::Value, name: &str) -> Result<String, String> {
    release["assets"]
        .as_array()
        .and_then(|assets| {
            assets.iter().find_map(|asset| {
                (asset["name"].as_str() == Some(name))
                    .then(|| asset["browser_download_url"].as_str().map(str::to_string))
                    .flatten()
            })
        })
        .ok_or_else(|| format!("asset {name} is missing a download url"))
}

fn extract(archive: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|error| format!("create {}: {error}", dest.display()))?;
    #[cfg(unix)]
    {
        run_command(
            std::process::Command::new("tar")
                .arg("-xzf")
                .arg(archive)
                .arg("-C")
                .arg(dest),
        )
    }
    #[cfg(windows)]
    {
        let bsdtar = std::process::Command::new("tar")
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(dest)
            .status();
        if bsdtar.map(|status| status.success()).unwrap_or(false) {
            return Ok(());
        }
        run_command(std::process::Command::new("powershell").args([
            "-NoProfile",
            "-Command",
            &format!(
                "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
                archive.display(),
                dest.display()
            ),
        ]))
    }
}

fn run_command(command: &mut std::process::Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("spawn {:?}: {error}", command))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {status}"))
    }
}

fn copy_archive_contents(extract_dir: &Path, tag: &str, bin: &Path) -> Result<(), String> {
    let top = extract_dir.join(format!("llama-{tag}"));
    if !top.is_dir() {
        return Err(format!(
            "expected extracted directory {} not found",
            top.display()
        ));
    }
    copy_dir_contents(&top, bin)?;
    // Some release archives nest the binaries one level deeper in bin/.
    let server = bin.join(server_binary_name());
    let nested = bin.join("bin");
    if !server.is_file() && nested.join(server_binary_name()).is_file() {
        copy_dir_contents(&nested, bin)?;
        let _ = fs::remove_dir_all(&nested);
    }
    if !server.is_file() {
        return Err(format!("{} not found after extraction", server.display()));
    }
    set_executable(&server);
    let cli = bin.join(cli_binary_name());
    if cli.is_file() {
        set_executable(&cli);
    }
    Ok(())
}

fn copy_dir_contents(source: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|error| format!("create {}: {error}", dest.display()))?;
    let entries =
        fs::read_dir(source).map_err(|error| format!("read {}: {error}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {error}", source.display()))?;
        let target = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_contents(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|error| {
                format!(
                    "copy {} -> {}: {error}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[cfg(windows)]
fn server_binary_name() -> &'static str {
    "llama-server.exe"
}

#[cfg(not(windows))]
fn server_binary_name() -> &'static str {
    "llama-server"
}

#[cfg(windows)]
fn cli_binary_name() -> &'static str {
    "llama-cli.exe"
}

#[cfg(not(windows))]
fn cli_binary_name() -> &'static str {
    "llama-cli"
}

/// Returns (version, model file name, download url) for the model install.
fn resolve_model(version: Option<&str>, model_url: Option<&str>) -> (String, String, String) {
    match model_url {
        Some(url) => {
            let file = url_file_name(url);
            let version = version
                .map(str::to_string)
                .unwrap_or_else(|| file.trim_end_matches(".gguf").to_string());
            (version, file, url.to_string())
        }
        None => {
            let version = version.unwrap_or(DEFAULT_MODEL_VERSION).to_string();
            let file = format!("{version}.gguf");
            (version.clone(), file, DEFAULT_MODEL_URL.to_string())
        }
    }
}

fn url_file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_string()
}

pub fn download(url: &str, dest: &Path) -> Result<u64, String> {
    let response = ureq::get(url)
        .call()
        .map_err(|error| format!("GET {url}: {error}"))?;
    let mut reader = response.into_reader();
    let mut file =
        fs::File::create(dest).map_err(|error| format!("create {}: {error}", dest.display()))?;
    let count = std::io::copy(&mut reader, &mut file)
        .map_err(|error| format!("download {url} to {}: {error}", dest.display()))?;
    Ok(count)
}

fn fetch_text(url: &str) -> Result<String, String> {
    let mut reader = ureq::get(url)
        .call()
        .map_err(|error| format!("GET {url}: {error}"))?
        .into_reader();
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .map_err(|error| format!("read {url}: {error}"))?;
    Ok(text)
}

fn fetch_json(url: &str) -> Result<serde_json::Value, String> {
    ureq::get(url)
        .call()
        .map_err(|error| format!("GET {url}: {error}"))?
        .into_json::<serde_json::Value>()
        .map_err(|error| format!("decode {url}: {error}"))
}

pub fn write_embed_config(
    dir: &Path,
    server: &Path,
    model: &Path,
    version: &str,
) -> Result<(), String> {
    let config = serde_json::json!({
        "embed": {
            "url": format!("http://127.0.0.1:{DEFAULT_PORT}"),
            "version": version,
            "server": server.display().to_string(),
            "model": model.display().to_string(),
            "port": DEFAULT_PORT,
        }
    });
    let mut text = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("serialize embed config: {error}"))?;
    text.push('\n');
    let path = dir.join(CONFIG_FILE_NAME);
    fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

pub fn default_embed_dir() -> Result<PathBuf, String> {
    Ok(crate::install::home_dir()?.join(".snoop"))
}

#[derive(serde::Deserialize)]
pub struct EmbedConfig {
    pub url: String,
    pub version: String,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(serde::Deserialize)]
struct ConfigFile {
    embed: Option<EmbedConfig>,
}

/// Embedder endpoint from `~/.snoop/config.json`, used when the
/// SNOOP_EMBED_URL env var is unset.
pub fn file_embed_config() -> Option<(String, String)> {
    file_embed_config_in(&default_embed_dir().ok()?)
}

pub fn file_embed_config_in(dir: &Path) -> Option<(String, String)> {
    let text = fs::read_to_string(dir.join(CONFIG_FILE_NAME)).ok()?;
    let file: ConfigFile = serde_json::from_str(&text).ok()?;
    let embed = file.embed?;
    Some((embed.url, embed.version))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedServerConfig {
    pub url: String,
    pub version: String,
    pub server: PathBuf,
    pub model: PathBuf,
    pub port: u16,
}

pub fn embed_server_config_in(dir: &Path) -> Option<EmbedServerConfig> {
    let text = fs::read_to_string(dir.join(CONFIG_FILE_NAME)).ok()?;
    let file: ConfigFile = serde_json::from_str(&text).ok()?;
    let embed = file.embed?;
    Some(EmbedServerConfig {
        url: embed.url,
        version: embed.version,
        server: PathBuf::from(embed.server?),
        model: PathBuf::from(embed.model?),
        port: embed.port.unwrap_or(DEFAULT_PORT),
    })
}

/// `snoop embed`: start the installed llama.cpp embedding server in the
/// foreground and propagate its exit status.
pub fn run_embed(port: Option<u16>) -> Result<(), String> {
    let dir = default_embed_dir()?;
    let config =
        embed_server_config_in(&dir).ok_or_else(|| "run: snoop install embedder".to_string())?;
    let port = port.unwrap_or(config.port);
    let mut command = std::process::Command::new(&config.server);
    command
        .args([
            "--embeddings",
            "--pooling",
            "last",
            "--host",
            "127.0.0.1",
            "--port",
        ])
        .arg(port.to_string())
        .args(["-m"])
        .arg(&config.model);
    println!(
        "starting: {} --embeddings --pooling last --host 127.0.0.1 --port {port} -m {}",
        config.server.display(),
        config.model.display()
    );
    let status = command
        .status()
        .map_err(|error| format!("spawn {}: {error}", config.server.display()))?;
    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests;
