use super::super::languages::{declarator_name, SymbolInfo};
use tree_sitter::Parser;

/// Parses one top-level C construct and walks its `declarator` field.
fn declarator_identity(source: &str) -> Option<SymbolInfo> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node().named_child(0)?;
    let declarator = root.child_by_field_name("declarator")?;
    declarator_name(declarator, source)
}

#[test]
fn function_declaration_and_definition_share_one_identity() {
    for source in [
        "int add(int a, int b);",
        "int add(int a, int b) { return a + b; }",
    ] {
        let info = declarator_identity(source).expect("declarator resolves");
        assert_eq!(info.display_name, "add", "source: {source}");
        assert_eq!(info.qualified_component, "add");
    }
}

#[test]
fn function_pointer_yields_the_variable_identity() {
    let info = declarator_identity("int (*handler)(Token *);").unwrap();
    assert_eq!(info.display_name, "handler");
}

#[test]
fn pointer_array_and_parenthesized_declarators_reach_the_identifier() {
    for (source, expected) in [
        ("char *name;", "name"),
        ("char *argv[4];", "argv"),
        ("int grid[8][8];", "grid"),
        ("void (callback)(void);", "callback"),
    ] {
        let info = declarator_identity(source).expect("declarator resolves");
        assert_eq!(info.display_name, expected, "source: {source}");
    }
}

#[test]
fn init_declarator_yields_the_declared_name() {
    let info = declarator_identity("int port = 8080;").unwrap();
    assert_eq!(info.display_name, "port");
}

#[test]
fn attributed_declarator_keeps_the_name() {
    let info = declarator_identity("int __attribute__((unused)) attempts;").unwrap();
    assert_eq!(info.display_name, "attempts");
}

#[test]
fn typedef_yields_the_type_name() {
    let info = declarator_identity("typedef struct { int x; } Point;").unwrap();
    assert_eq!(info.display_name, "Point");
}

#[test]
fn non_declarator_nodes_resolve_to_nothing() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .unwrap();
    let source = "int add(int a, int b);";
    let tree = parser.parse(source, None).unwrap();
    let declaration = tree.root_node().named_child(0).unwrap();
    assert_eq!(declaration.kind(), "declaration");
    assert!(declarator_name(declaration, source).is_none());
}
