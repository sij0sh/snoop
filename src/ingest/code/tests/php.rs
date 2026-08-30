use super::*;

#[test]
fn php_symbols_map_to_atom_kinds() {
    let source = "<?php\nnamespace App\\Auth;\n\nuse App\\Support\\retry;\n\nclass Session {\n    public function refresh(): Token {}\n}\n";
    let atoms = parse_code(source, "src/auth.php").unwrap();
    assert_eq!(atoms[0].metadata["language"], "php");
    let class = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Class)
        .unwrap();
    assert_eq!(class.metadata["symbol"].as_str(), Some("Session"));
    let method = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function)
        .unwrap();
    assert!(method.breadcrumb.contains("App\\Auth > Session > refresh"));
    assert_eq!(method.metadata["symbol"].as_str(), Some("refresh"));
}

#[test]
fn php_use_statements_are_imports() {
    let source = "<?php\nuse App\\Auth\\Token;\nuse function App\\Support\\retry;\n";
    let atoms = parse_code(source, "src/imports.php").unwrap();
    let imports: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.metadata["is_import"].as_bool() == Some(true))
        .collect();
    assert_eq!(imports.len(), 2);
    assert!(imports
        .iter()
        .all(|atom| atom.kind == AtomKind::Declaration));
}

#[test]
fn php_traits_interfaces_enums_are_classes() {
    let source =
        "<?php\ninterface Repo {}\ntrait Cache {}\nenum Status {}\nabstract class Base {}\n";
    let atoms = parse_code(source, "src/kinds.php").unwrap();
    let classes: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Class)
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(classes, vec!["Repo", "Cache", "Status", "Base"]);
}

#[test]
fn php_anonymous_functions_are_not_symbols() {
    let source = "<?php\n$handler = function () { return 1; };\nfunction named() {}\n";
    let atoms = parse_code(source, "src/closures.php").unwrap();
    let functions: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(functions, vec!["named"]);
}

#[test]
fn php_html_outside_php_blocks_never_becomes_atoms() {
    let source = "<html><body><?php function render() {} ?></body></html>\n";
    let atoms = parse_code(source, "templates/page.phtml").unwrap();
    assert_eq!(atoms[0].metadata["language"], "php");
    let functions: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
    assert!(functions[0].metadata["symbol"].as_str() == Some("render"));
}

#[test]
fn php_malformed_input_still_parses() {
    let atoms = parse_code("<?php\nfunction broken {\n", "src/broken.php").unwrap();
    assert!(!atoms.is_empty());
}
