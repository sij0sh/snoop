use super::*;

#[test]
fn c_symbols_map_to_atom_kinds() {
    let source = "#include <stdio.h>\n#include \"util.h\"\n\n#define MAX_LIMIT 100\n\nstruct Point { int x; int y; };\n\nunion Blob { int raw; char bytes[4]; };\n\nenum Color { RED, GREEN };\n\ntypedef struct { int size; } Buffer;\n\nstatic void helper(void) { }\n";
    let atoms = parse_code(source, "src/util.c").unwrap();
    assert_eq!(atoms[0].metadata["language"], "c");
    let expected = [
        ("<stdio.h>", AtomKind::Declaration),
        ("\"util.h\"", AtomKind::Declaration),
        ("MAX_LIMIT", AtomKind::Declaration),
        ("Point", AtomKind::Class),
        ("Blob", AtomKind::Class),
        ("Color", AtomKind::Class),
        ("Buffer", AtomKind::Declaration),
        ("helper", AtomKind::Function),
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
fn c_declarations_and_definitions_share_one_identity() {
    let source = "int add(int a, int b);\n\nint add(int a, int b)\n{\n    return a + b;\n}\n";
    let atoms = parse_code(source, "src/add.c").unwrap();
    let adds: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("add"))
        .collect();
    assert_eq!(adds.len(), 2);
    assert!(adds.iter().all(|atom| atom.breadcrumb.ends_with(" > add")));
}

#[test]
fn c_function_pointers_stay_declarations() {
    let source = "int (*handler)(Token *);\n";
    let atoms = parse_code(source, "src/handler.c").unwrap();
    let handler = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("handler"))
        .unwrap();
    assert_eq!(handler.kind, AtomKind::Declaration);
    assert!(
        !atoms.iter().any(|atom| atom.kind == AtomKind::Function),
        "no atom may be classified as a function"
    );
}

#[test]
fn c_locals_are_not_emitted() {
    let source = "void run(void) {\n    int local = 1;\n    struct Inner { int x; } inner;\n    use(local, inner);\n}\n";
    let atoms = parse_code(source, "src/run.c").unwrap();
    let names: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind != AtomKind::File)
        .map(|atom| atom.breadcrumb.clone())
        .collect();
    assert_eq!(names.len(), 1, "only the function is emitted: {names:?}");
    assert!(names[0].ends_with("run"));
}

#[test]
fn c_multiline_macros_and_conditional_compilation() {
    let source = "#define SQUARE(x) \\\n    ((x) * (x))\n\n#if USE_FAST\nvoid fast_path(void) { }\n#else\nvoid slow_path(void) { }\n#endif\n";
    let atoms = parse_code(source, "src/paths.c").unwrap();
    assert!(
        atoms
            .iter()
            .any(|atom| atom.breadcrumb.ends_with("SQUARE") && atom.kind == AtomKind::Declaration),
        "the multiline macro must be one declaration"
    );
    for needle in ["fast_path", "slow_path"] {
        assert!(
            atoms
                .iter()
                .any(|atom| atom.breadcrumb.ends_with(needle) && atom.kind == AtomKind::Function),
            "conditional compilation must still emit {needle}"
        );
    }
}

#[test]
fn c_doxygen_comments_attach_as_leading_context() {
    let source = "/** Adds two values. */\nint add(int a, int b);\n";
    let boundaries = analyze_code("src/add.c", source).unwrap();
    let add = boundaries
        .iter()
        .find(|boundary| boundary.display_name == "add")
        .unwrap();
    assert_eq!(
        &source[add.leading_context.clone().unwrap()],
        "/** Adds two values. */\n"
    );
}

#[test]
fn c_recovers_from_truncated_input() {
    let source = "int broken(int a, {\n";
    let atoms = parse_code(source, "src/broken.c");
    assert!(atoms.is_ok(), "parse recovery must not fail the file");
}
