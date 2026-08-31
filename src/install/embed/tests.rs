use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn selects_linux_asset_and_skips_variants() {
    let names = [
        "llama-b10621-bin-ubuntu-x64.tar.gz",
        "llama-b10621-bin-ubuntu-x64-vulkan.tar.gz",
        "llama-b10621-bin-ubuntu-x64-sycl.tar.gz",
    ];
    assert_eq!(
        select_llama_asset(Platform::LinuxX64, names).as_deref(),
        Some("llama-b10621-bin-ubuntu-x64.tar.gz")
    );
}

#[test]
fn excluded_variant_first_still_selects_clean_asset() {
    let names = [
        "llama-b10621-bin-ubuntu-x64-vulkan.tar.gz",
        "llama-b10621-bin-ubuntu-x64-openvino.tar.gz",
        "llama-b10621-bin-ubuntu-x64-rocm.tar.gz",
        "llama-b10621-bin-ubuntu-x64.tar.gz",
    ];
    assert_eq!(
        select_llama_asset(Platform::LinuxX64, names).as_deref(),
        Some("llama-b10621-bin-ubuntu-x64.tar.gz")
    );
}

#[test]
fn selects_windows_and_macos_assets() {
    assert_eq!(
        select_llama_asset(
            Platform::WindowsX64,
            [
                "llama-b10621-bin-win-cpu-x64.zip",
                "llama-b10621-bin-win-cuda-x64.zip"
            ]
        )
        .as_deref(),
        Some("llama-b10621-bin-win-cpu-x64.zip")
    );
    assert_eq!(
        select_llama_asset(
            Platform::MacosArm64,
            ["llama-b10621-bin-macos-arm64.tar.gz"]
        )
        .as_deref(),
        Some("llama-b10621-bin-macos-arm64.tar.gz")
    );
    assert_eq!(
        select_llama_asset(Platform::MacosX64, ["llama-b10621-bin-macos-x64.tar.gz"]).as_deref(),
        Some("llama-b10621-bin-macos-x64.tar.gz")
    );
    assert_eq!(
        select_llama_asset(
            Platform::WindowsArm64,
            ["llama-b10621-bin-win-cpu-arm64.zip"]
        )
        .as_deref(),
        Some("llama-b10621-bin-win-cpu-arm64.zip")
    );
}

#[test]
fn accelerator_variants_are_rejected() {
    for name in [
        "llama-b10621-bin-ubuntu-x64-vulkan.tar.gz",
        "llama-b10621-bin-ubuntu-x64-rocm.tar.gz",
        "llama-b10621-bin-ubuntu-x64-sycl.tar.gz",
        "llama-b10621-bin-ubuntu-x64-openvino.tar.gz",
    ] {
        assert_eq!(
            select_llama_asset(Platform::LinuxX64, [name]),
            None,
            "variant must be rejected: {name}"
        );
    }
}

#[test]
fn wrong_platform_name_returns_none() {
    assert_eq!(
        select_llama_asset(Platform::MacosArm64, ["llama-b10621-bin-ubuntu-x64.tar.gz"]),
        None
    );
    assert_eq!(
        select_llama_asset(Platform::WindowsArm64, ["llama-b10621-bin-win-cpu-x64.zip"]),
        None
    );
    assert_eq!(
        select_llama_asset(Platform::LinuxX64, ["llama-b10621-bin-win-cpu-x64.zip"]),
        None
    );
}

#[test]
fn config_roundtrip_via_explicit_dir() {
    let dir = tempdir();
    let server = dir.path().join("bin").join("llama-server");
    let model = dir.path().join("models").join("m.gguf");
    write_embed_config(dir.path(), &server, &model, "v9").unwrap();
    let (url, version) = file_embed_config_in(dir.path()).expect("config parses");
    assert_eq!(url, format!("http://127.0.0.1:{DEFAULT_PORT}"));
    assert_eq!(version, "v9");
    let server_config = embed_server_config_in(dir.path()).expect("server config parses");
    assert_eq!(server_config.port, DEFAULT_PORT);
    assert_eq!(server_config.version, "v9");
    assert_eq!(server_config.server, server);
    assert_eq!(server_config.model, model);
    let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE_NAME)).unwrap();
    assert!(text.ends_with('\n'), "config written with trailing newline");
}

#[test]
fn missing_or_corrupt_config_returns_none() {
    let dir = tempdir();
    assert_eq!(file_embed_config_in(dir.path()), None);
    assert_eq!(embed_server_config_in(dir.path()), None);
    std::fs::write(dir.path().join(CONFIG_FILE_NAME), "{ not json").unwrap();
    assert_eq!(file_embed_config_in(dir.path()), None);
    std::fs::write(dir.path().join(CONFIG_FILE_NAME), r#"{"embed":{}}"#).unwrap();
    assert_eq!(file_embed_config_in(dir.path()), None);
}

#[test]
fn home_override_wrapper_reads_env() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    let original = std::env::var_os(key).map(std::path::PathBuf::from);
    let home = tempdir();
    let embed_dir = home.path().join(".snoop");
    let server = embed_dir.join("bin").join("llama-server");
    let model = embed_dir.join("models").join("m.gguf");
    std::fs::create_dir_all(&embed_dir).unwrap();
    write_embed_config(&embed_dir, &server, &model, "env-version").unwrap();
    std::env::set_var(key, home.path());
    let config = file_embed_config();
    if let Some(original) = original {
        std::env::set_var(key, original);
    }
    let (url, version) = config.expect("wrapper reads the overridden home");
    assert_eq!(version, "env-version");
    assert_eq!(url, format!("http://127.0.0.1:{DEFAULT_PORT}"));
}

#[test]
fn resolve_model_matrix() {
    let (version, file, url) = resolve_model(None, None);
    assert_eq!(version, DEFAULT_MODEL_VERSION);
    assert_eq!(file, format!("{DEFAULT_MODEL_VERSION}.gguf"));
    assert_eq!(url, DEFAULT_MODEL_URL);

    let (version, file, url) = resolve_model(Some("custom-v2"), None);
    assert_eq!(version, "custom-v2");
    assert_eq!(file, "custom-v2.gguf");
    assert_eq!(url, DEFAULT_MODEL_URL);

    let (version, file, _) = resolve_model(
        None,
        Some("https://example.com/models/My-Embed-Q8.gguf?download=true"),
    );
    assert_eq!(version, "My-Embed-Q8");
    assert_eq!(file, "My-Embed-Q8.gguf");

    let (version, file, _) = resolve_model(
        Some("explicit-v"),
        Some("https://example.com/models/My-Embed-Q8.gguf"),
    );
    assert_eq!(version, "explicit-v");
    assert_eq!(file, "My-Embed-Q8.gguf");
}

#[test]
fn archive_copy_flattens_top_dir_and_keeps_nested_layout() {
    let dir = tempdir();
    let extract = dir.path().join(".extract");
    let top = extract.join("llama-b10621-bin-ubuntu-x64");
    let nested = top.join("bin");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("llama-server"), "server").unwrap();
    std::fs::write(nested.join("llama-cli"), "cli").unwrap();
    std::fs::write(top.join("README"), "docs").unwrap();

    let bin = dir.path().join("bin");
    copy_archive_contents(&extract, "b10621-bin-ubuntu-x64", &bin).expect("copy succeeds");
    assert_eq!(std::fs::read(bin.join("llama-server")).unwrap(), b"server");
    assert_eq!(std::fs::read(bin.join("llama-cli")).unwrap(), b"cli");
    assert_eq!(std::fs::read(bin.join("README")).unwrap(), b"docs");
    assert!(!bin.join("bin").exists(), "nested bin flattened away");
}

#[test]
fn archive_copy_reports_missing_server() {
    let dir = tempdir();
    let extract = dir.path().join(".extract");
    let top = extract.join("llama-b10621");
    std::fs::create_dir_all(&top).unwrap();
    let bin = dir.path().join("bin");
    let error = copy_archive_contents(&extract, "b10621", &bin).unwrap_err();
    assert!(error.contains("llama-server"), "{error}");
}
