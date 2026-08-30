use super::*;

const DEPLOY: &str = "#!/usr/bin/env bash\nset -euo pipefail\n\nsource ./lib/auth.sh\n. ./lib/retry.sh\n\nrefresh() {\n    validate\n}\n\nfunction rotate {\n    persist\n}\n\nparse_args \"$@\"\nmain\n";

#[test]
fn shell_functions_support_both_declaration_styles() {
    let atoms = parse_code(DEPLOY, "scripts/deploy.sh").unwrap();
    assert_eq!(atoms[0].metadata["language"], "shell");
    let names: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, vec!["refresh", "rotate"]);
}

#[test]
fn shell_static_source_commands_are_imports() {
    let atoms = parse_code(DEPLOY, "scripts/deploy.sh").unwrap();
    let imports: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.metadata["is_import"].as_bool() == Some(true))
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(imports, vec!["source", "."]);
}

#[test]
fn shell_dynamic_sourcing_is_not_an_atom() {
    let source = "#!/bin/bash\nsource \"$LIB/auth.sh\"\nmain\n";
    let atoms = parse_code(source, "scripts/dynamic.sh").unwrap();
    assert!(atoms
        .iter()
        .all(|atom| atom.metadata["is_import"].as_bool() != Some(true)));
}

#[test]
fn shell_root_program_becomes_module_overview() {
    let atoms = parse_code(DEPLOY, "scripts/deploy.sh").unwrap();
    let overview = atoms
        .iter()
        .find(|atom| {
            atom.kind == AtomKind::Module && atom.metadata["symbol"].as_str() == Some("program")
        })
        .unwrap();
    // The overview spans the whole script; commands stay visible there.
    assert!(overview.text.contains("set -euo pipefail"));
    assert!(overview.text.contains("source ./lib/auth.sh"));
    assert!(overview.text.contains("main"));
    // Ordinary commands never become atoms.
    assert!(!atoms.iter().any(|atom| atom.kind == AtomKind::Declaration
        && atom.metadata["symbol"].as_str() == Some("parse_args")));
}

#[test]
fn shell_shebang_detects_extensionless_scripts() {
    let atoms = parse_code(DEPLOY, "scripts/deploy").unwrap();
    assert_eq!(atoms[0].metadata["language"], "shell");
    let sh = parse_code("#!/bin/sh\necho hi\n", "scripts/legacy").unwrap();
    assert_eq!(sh[0].metadata["language"], "shell");
}

#[test]
fn shell_known_filenames_recognized_without_extension() {
    assert!(supports_code_path("Gemfile"));
    assert!(supports_code_path("Rakefile"));
    assert!(language_name("Vagrantfile") == Some("ruby"));
    assert!(!supports_code_path("Makefile"));
}

#[test]
fn shell_malformed_input_still_parses() {
    let atoms = parse_code("function broken {\n", "scripts/broken.sh").unwrap();
    assert!(!atoms.is_empty());
}
