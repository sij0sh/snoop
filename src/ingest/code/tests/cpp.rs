use super::*;

#[test]
fn cpp_symbols_map_to_atom_kinds() {
    let source = "#include <vector>\n\n#define LIMIT 100\n\nnamespace store {\n\nclass Shelf {\n    int count_;\n\n  public:\n    void restock(int amount);\n};\n\nvoid Shelf::restock(int amount) { count_ += amount; }\n\nusing Matrix = std::vector<std::vector<int>>;\n\n}  // namespace store\n";
    let atoms = parse_code(source, "src/shelf.cpp").unwrap();
    assert_eq!(atoms[0].metadata["language"], "cpp");
    let expected = [
        ("<vector>", AtomKind::Declaration),
        ("LIMIT", AtomKind::Declaration),
        ("store", AtomKind::Module),
        ("Shelf", AtomKind::Class),
        ("restock", AtomKind::Function),
        ("Matrix", AtomKind::Declaration),
    ];
    for (needle, kind) in expected {
        assert!(
            atoms
                .iter()
                .any(|atom| atom.breadcrumb.ends_with(needle) && atom.kind == kind),
            "missing {kind:?} atom ending in {needle}"
        );
    }
}

#[test]
fn cpp_nested_namespaces_nest_breadcrumbs() {
    let source = "namespace outer {\nnamespace inner {\n\nvoid run() { step(); }\n\n}  // namespace inner\n}  // namespace outer\n";
    let atoms = parse_code(source, "src/nest.cpp").unwrap();
    let run = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function)
        .unwrap();
    assert!(run.breadcrumb.ends_with("outer > inner > run"));
}

#[test]
fn cpp_out_of_class_definitions_share_one_identity() {
    let source = "class Session {\n  public:\n    void refresh();\n    void refresh();\n};\n\nvoid Session::refresh() { ready_ = true; }\n";
    let boundaries = analyze_code("src/session.cpp", source).unwrap();
    let refreshes: Vec<_> = boundaries
        .iter()
        .filter(|boundary| boundary.display_name == "refresh")
        .collect();
    assert_eq!(refreshes.len(), 2, "declaration and definition both emit");
    assert!(
        refreshes
            .iter()
            .all(|boundary| boundary.qualified_name.ends_with("Session::refresh")),
        "both boundaries use the qualified identity: {refreshes:?}"
    );
    assert_ne!(refreshes[0].byte_range, refreshes[1].byte_range);
}

#[test]
fn cpp_constructors_destructors_and_operators_are_functions() {
    let source = "class Wallet {\n  public:\n    Wallet(int cents);\n    ~Wallet();\n    bool operator==(const Wallet& other) const;\n    operator int() const;\n\n  private:\n    int cents_;\n};\n";
    let boundaries = analyze_code("src/wallet.cpp", source).unwrap();
    let displays: Vec<_> = boundaries
        .iter()
        .filter(|boundary| boundary.kind == AtomKind::Function)
        .map(|boundary| boundary.display_name.clone())
        .collect();
    for expected in ["Wallet", "~Wallet", "operator==", "operator int"] {
        assert!(
            displays.iter().any(|name| name == expected),
            "missing function {expected}: {displays:?}"
        );
    }
}

#[test]
fn cpp_templates_attach_preamble_and_skip_wrapper() {
    let source = "/// Box holds one value.\ntemplate <typename T>\nclass Box {\n  public:\n    T value;\n};\n\ntemplate <typename T>\nT pick(T first, T second) {\n    return first;\n}\n";
    let boundaries = analyze_code("src/boxes.cpp", source).unwrap();
    let box_boundary = boundaries
        .iter()
        .find(|boundary| boundary.display_name == "Box")
        .unwrap();
    assert_eq!(box_boundary.kind, AtomKind::Class);
    let box_context = &source[box_boundary.leading_context.clone().unwrap()];
    assert!(box_context.contains("/// Box holds one value."));
    assert!(box_context.contains("template <typename T>"));
    let pick = boundaries
        .iter()
        .find(|boundary| boundary.display_name == "pick")
        .unwrap();
    assert_eq!(pick.kind, AtomKind::Function);
    let pick_context = &source[pick.leading_context.clone().unwrap()];
    assert!(pick_context.contains("template <typename T>"));
}

#[test]
fn cpp_alias_concepts_and_usings_are_declarations() {
    let source = "using Matrix = std::vector<std::vector<int>>;\n\ntemplate <typename T>\nconcept Numeric = requires(T a) { a + a; };\n\nnamespace fs = std::filesystem;\n\nusing std::vector;\n";
    let boundaries = analyze_code("src/aliases.cpp", source).unwrap();
    let expected = [
        ("Matrix", "Matrix"),
        ("Numeric", "Numeric"),
        ("fs", "fs"),
        ("std::vector", "std::vector"),
    ];
    for (display, needle) in expected {
        let boundary = boundaries
            .iter()
            .find(|boundary| boundary.display_name == display)
            .unwrap_or_else(|| panic!("missing declaration {display}"));
        assert_eq!(boundary.kind, AtomKind::Declaration, "{needle}");
    }
}

#[test]
fn cpp_raw_strings_macros_and_headers_index() {
    let source = "#define BANNER_PREFIX \"[store]\"\n\nconst char* banner = R\"(multi \"quoted\" text)\";\n\ntemplate <typename T>\nT clamp_to(T value, T limit) {\n    return value < limit ? value : limit;\n}\n";
    let header_atoms = parse_code(source, "src/util.h").unwrap();
    assert_eq!(header_atoms[0].metadata["language"], "cpp");
    assert!(
        header_atoms
            .iter()
            .any(|atom| atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("clamp_to")),
        "header-only templates emit functions"
    );
    assert!(
        header_atoms
            .iter()
            .any(|atom| atom.kind == AtomKind::Declaration && atom.breadcrumb.ends_with("banner")),
        "raw string initializer keeps the declaration"
    );
}

#[test]
fn cpp_locals_and_lambdas_are_not_emitted() {
    let source = "void run() {\n    int local = 1;\n    struct Hidden { int x; } hidden;\n    auto closure = [&] { return local; };\n    (void)hidden;\n}\n";
    let atoms = parse_code(source, "src/run.cpp").unwrap();
    let names: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind != AtomKind::File)
        .map(|atom| atom.breadcrumb.clone())
        .collect();
    assert_eq!(names.len(), 1, "only the function is emitted: {names:?}");
    assert!(names[0].ends_with("run"));
}

#[test]
fn cpp_recovers_from_truncated_input() {
    let source = "int broken(int a, {\n";
    let atoms = parse_code(source, "src/broken.cpp");
    assert!(atoms.is_ok(), "parse recovery must not fail the file");
}
