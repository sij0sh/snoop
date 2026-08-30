use super::*;
mod c;
mod c_declarator;
mod csharp;

#[test]
fn rust_symbols_include_signature_and_references() {
    let atoms = parse_code(
        "pub fn refresh_session(token: Token) { validate_token(token); }",
        "src/auth.rs",
    )
    .unwrap();
    let function = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function)
        .unwrap();
    assert_eq!(function.breadcrumb, "src/auth.rs > refresh_session");
    assert!(function.metadata["signature"]
        .as_str()
        .unwrap()
        .contains("refresh_session"));
    assert!(function.metadata["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "validate_token"));
}

#[test]
fn trait_signatures_and_trait_impls_have_distinct_qualified_names() {
    let source = r#"
trait Build { fn create() -> Self; }
struct Item;
impl Display for Item { fn fmt(&self) {} }
impl Debug for Item { fn fmt(&self) {} }
"#;
    let atoms = parse_code(source, "src/lib.rs").unwrap();
    assert!(atoms.iter().any(|atom| {
        atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("Build > create")
    }));
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.contains("impl Display for Item > fmt")));
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.contains("impl Debug for Item > fmt")));
}

#[test]
fn python_symbols_map_to_atom_kinds() {
    let source = "\"\"\"Docs.\"\"\"\nimport json\n\nclass TokenStore:\n    def refresh(self, token):\n        validate(token)\n\ndef load_config(path):\n    return json.load(open(path))\n";
    let atoms = parse_code(source, "src/store.py").unwrap();
    assert_eq!(atoms[0].metadata["language"], "python");
    let class = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Class)
        .unwrap();
    assert!(class.breadcrumb.ends_with("TokenStore"));
    let function = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("refresh"))
        .expect("method refresh becomes a Function atom");
    assert!(function.breadcrumb.contains("TokenStore > refresh"));
    assert!(function.metadata["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "validate"));
}

#[test]
fn typescript_symbols_map_to_atom_kinds() {
    let source = "import fs from 'fs';\n\ninterface Session { id: string }\n\ntype Token = string;\n\nexport class Auth {\n  refresh(id: string): void {\n    validate(id);\n  }\n}\n\nfunction load(id: string): Token {\n  return id;\n}\n";
    let atoms = parse_code(source, "src/auth.ts").unwrap();
    assert_eq!(atoms[0].metadata["language"], "typescript");
    for needle in ["Auth > refresh", "Session", "Token", "load"] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
}

#[test]
fn javascript_symbols_map_to_atom_kinds() {
    let source = "import fs from 'fs';\n\nconst refresh = () => refresh_token();\nconst load = function () {};\n\nmodule.exports = { refresh };\nexports.validate = validate_input;\n\nclass Auth {\n  login(user) {\n    return user;\n  }\n}\n\nexport function load_config(path) {\n  return fs.readFileSync(path);\n}\n";
    let atoms = parse_code(source, "src/auth.js").unwrap();
    assert_eq!(atoms[0].metadata["language"], "javascript");
    for needle in [
        "refresh",
        "load",
        "module.exports",
        "exports.validate",
        "Auth > login",
        "load_config",
    ] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
    let exports = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("exports.validate"))
        .unwrap();
    assert_eq!(exports.kind, AtomKind::Declaration);
    let refresh = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("refresh"))
        .unwrap();
    assert!(refresh.metadata["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "refresh_token"));
}

#[test]
fn javascript_ignores_ordinary_assignments() {
    let source = "let total = 0;\n\nfunction bump() {\n  total = total + 1;\n}\n";
    let atoms = parse_code(source, "src/sum.js").unwrap();
    let functions: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .collect();
    assert_eq!(
        functions.len(),
        1,
        "only bump is a function: {:?}",
        functions
            .iter()
            .map(|atom| &atom.breadcrumb)
            .collect::<Vec<_>>()
    );
    assert!(atoms.iter().all(|atom| atom.kind != AtomKind::Declaration));
}

#[test]
fn javascript_callable_variables_share_typescript_behavior() {
    let source = "const handler = () => submit();\n\nconst settings = { retries: 2 };\n";
    let atoms = parse_code(source, "src/handler.ts").unwrap();
    let functions: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
    assert!(functions[0].breadcrumb.ends_with("handler"));
}

#[test]
fn jsx_files_parse_with_javascript_symbols() {
    let jsx = "export function App() {\n  return <div className='x'>hi</div>;\n}\n";
    let atoms = parse_code(jsx, "src/app.jsx").unwrap();
    assert_eq!(atoms[0].metadata["language"], "javascript");
    assert!(atoms.iter().any(|atom| atom.breadcrumb.ends_with("App")));
}

#[test]
fn go_symbols_map_to_atom_kinds() {
    let source = "package server\n\nimport \"fmt\"\n\ntype Store struct {\n\titems int\n}\n\ntype Reader interface {\n\tRead() error\n}\n\ntype ID = int\n\n// Refresh reloads the cache.\nfunc Refresh() {\n\tfmt.Println()\n}\n\nconst MaxItems = 10\n";
    let atoms = parse_code(source, "src/server.go").unwrap();
    assert_eq!(atoms[0].metadata["language"], "go");
    for needle in ["Store", "Reader", "ID", "Refresh", "MaxItems"] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
    let store = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("Store"))
        .unwrap();
    assert_eq!(store.kind, AtomKind::Class);
    let reader = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("Reader"))
        .unwrap();
    assert_eq!(reader.kind, AtomKind::Class);
    let alias = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("ID"))
        .unwrap();
    assert_eq!(alias.kind, AtomKind::Declaration);
    let refresh = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("Refresh"))
        .unwrap();
    assert_eq!(refresh.kind, AtomKind::Function);
    assert_eq!(
        refresh.metadata["leading_context"]["text"],
        "// Refresh reloads the cache.\n"
    );
    assert!(refresh.metadata["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "Println"));
}

#[test]
fn go_methods_qualify_their_receiver() {
    let source = "type Server struct{}\n\nfunc (s *Server) Refresh() {}\n\nfunc (s Server) Size() int { return 0 }\n";
    let atoms = parse_code(source, "src/server.go").unwrap();
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.ends_with("Server.Refresh")));
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.ends_with("Server.Size")));
}

#[test]
fn java_symbols_map_to_atom_kinds() {
    let source = "package app;\n\nimport java.util.List;\n\npublic class AuthService {\n  private final Token token;\n\n  /** Validates the session. */\n  public void refresh(String id) {\n    validate(id);\n  }\n\n  public AuthService() {\n    this.token = new Token();\n  }\n}\n\ninterface Store {\n  void save();\n}\n\nrecord Point(int x, int y) {\n  public Point {\n  }\n}\n\nenum Mode { READ, WRITE }\n\n@interface Marker {}\n";
    let atoms = parse_code(source, "src/AuthService.java").unwrap();
    assert_eq!(atoms[0].metadata["language"], "java");
    for needle in [
        "AuthService",
        "AuthService > refresh",
        "AuthService > AuthService",
        "AuthService > token",
        "Store > save",
        "Point > Point",
        "Mode",
        "Marker",
        "java.util.List",
    ] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
    let refresh = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("AuthService > refresh"))
        .unwrap();
    assert_eq!(
        refresh.metadata["leading_context"]["text"],
        "/** Validates the session. */\n  "
    );
    assert!(refresh.metadata["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "validate"));
}

#[test]
fn java_methods_are_qualified_by_their_class() {
    let source = "class A {\n  void run() {}\n}\n\nclass B {\n  void run() {}\n}\n";
    let atoms = parse_code(source, "src/Dup.java").unwrap();
    let runs: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.breadcrumb.ends_with("run"))
        .collect();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().any(|atom| atom.breadcrumb.ends_with("A > run")));
    assert!(runs.iter().any(|atom| atom.breadcrumb.ends_with("B > run")));
}

#[test]
fn nested_symbols_are_emitted_exactly_once() {
    let rust_source = "impl Store {\n    fn refresh(&self) {}\n    fn validate(&self) {}\n}\n";
    let rust_atoms = parse_code(rust_source, "src/store.rs").unwrap();
    let rust_functions: Vec<_> = rust_atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .collect();
    assert_eq!(
        rust_functions.len(),
        2,
        "methods inside an impl block must not be duplicated: {:?}",
        rust_functions
            .iter()
            .map(|atom| &atom.breadcrumb)
            .collect::<Vec<_>>()
    );

    let python_source =
        "class TokenStore:\n    def refresh(self, token):\n        validate(token)\n";
    let python_atoms = parse_code(python_source, "src/store.py").unwrap();
    let python_functions: Vec<_> = python_atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .collect();
    assert_eq!(
        python_functions.len(),
        1,
        "methods inside a class must not be duplicated"
    );
}

#[test]
fn tsx_files_parse_with_jsx_extension() {
    let tsx = "export function App() {\n  return <div className='x'>hi</div>;\n}\n";
    let atoms = parse_code(tsx, "src/app.tsx").unwrap();
    assert_eq!(atoms[0].metadata["language"], "typescript");
    assert!(atoms.iter().any(|atom| atom.breadcrumb.ends_with("App")));
}

#[test]
fn code_extensions_cover_the_shared_registry() {
    for locator in [
        "a.rs", "a.py", "a.pyi", "a.pyw", "a.ts", "a.tsx", "a.mts", "a.cts", "a.js", "a.jsx",
        "a.mjs", "a.cjs", "a.go", "a.java", "a.cs", "a.c", "a.cc", "a.cpp", "a.cxx", "a.h", "a.hh",
        "a.hpp", "a.hxx", "a.ipp", "a.tpp", "a.inl",
    ] {
        assert!(supports_code_path(locator), "{locator} must be supported");
    }
    for locator in [
        "a.md",
        "a.txt",
        "README",
        "a.json",
        ".gitignore",
        "a.php",
        "a.sh",
        "a.tf",
    ] {
        assert!(!supports_code_path(locator), "{locator} must not be code");
    }
    assert_eq!(language_name("src/app.tsx"), Some("typescript"));
    assert_eq!(language_name("src/store.pyi"), Some("python"));
    assert_eq!(language_name("src/app.jsx"), Some("javascript"));
    assert_eq!(language_name("src/server.go"), Some("go"));
    assert_eq!(language_name("src/Auth.java"), Some("java"));
}

#[test]
fn analyze_code_projects_qualified_boundaries() {
    let source = "class TokenStore:\n    def refresh(self, token):\n        validate(token)\n";
    let boundaries = analyze_code("src/store.py", source).unwrap();
    let refresh = boundaries
        .iter()
        .find(|boundary| boundary.display_name == "refresh")
        .unwrap();
    assert_eq!(refresh.symbol_id, "src/store.py > TokenStore > refresh");
    assert_eq!(refresh.qualified_name, "TokenStore > refresh");
    assert_eq!(
        refresh.parent_symbol_id.as_deref(),
        Some("src/store.py > TokenStore")
    );
    assert_eq!(refresh.language, "python");
    assert_eq!(refresh.kind, AtomKind::Function);
    assert!(refresh
        .references
        .iter()
        .any(|reference| reference == "validate"));
    assert_eq!(*refresh.line_range.start(), 2);
    assert_eq!(*refresh.line_range.end(), 3);
    assert_eq!(
        refresh.byte_range.start,
        source.find("def refresh").unwrap()
    );
}

#[test]
fn leading_context_attaches_to_declarations() {
    let rust = "/// Refresh the token.\nfn refresh() {}\n";
    let rust_boundaries = analyze_code("src/auth.rs", rust).unwrap();
    let rust_refresh = rust_boundaries
        .iter()
        .find(|boundary| boundary.display_name == "refresh")
        .unwrap();
    assert_eq!(
        &rust[rust_refresh.leading_context.clone().unwrap()],
        "/// Refresh the token.\n"
    );

    let python = "@cached\nclass Store:\n    pass\n";
    let python_boundaries = analyze_code("src/store.py", python).unwrap();
    let store = python_boundaries
        .iter()
        .find(|boundary| boundary.display_name == "Store")
        .unwrap();
    assert_eq!(&python[store.leading_context.clone().unwrap()], "@cached\n");

    let typescript = "/** Loads config. */\nfunction load() {}\n";
    let ts_boundaries = analyze_code("src/load.ts", typescript).unwrap();
    let load = ts_boundaries
        .iter()
        .find(|boundary| boundary.display_name == "load")
        .unwrap();
    assert_eq!(
        &typescript[load.leading_context.clone().unwrap()],
        "/** Loads config. */\n"
    );

    let java = "public class Auth {\n  /** Validates. */\n  void validate() {}\n}\n";
    let java_boundaries = analyze_code("src/Auth.java", java).unwrap();
    let validate = java_boundaries
        .iter()
        .find(|boundary| boundary.display_name == "validate")
        .unwrap();
    assert_eq!(
        &java[validate.leading_context.clone().unwrap()],
        "/** Validates. */\n  "
    );
}

#[test]
fn oversized_splitting_avoids_atomic_interiors() {
    let long = "x".repeat(2_000);
    let filler = "y".repeat(2_000);
    let source =
        format!("fn big() {{\n    let first = \"{long}\";\n    let second = \"{filler}\";\n}}\n");
    let atoms = parse_code(&source, "src/big.rs").unwrap();
    let function = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function)
        .unwrap();
    let segments = function.metadata["chunk_segments"].as_array().unwrap();
    assert!(!segments.is_empty(), "the function must be segmented");
    let first_start = source.find(&long).unwrap();
    let first_end = first_start + long.len();
    let second_start = source.rfind(&filler).unwrap();
    let second_end = second_start + filler.len();
    for segment in segments {
        let start = segment["start_offset"].as_u64().unwrap() as usize;
        let end = segment["end_offset"].as_u64().unwrap() as usize;
        let cuts_first = start < first_end && end > first_start;
        let cuts_second = start < second_end && end > second_start;
        if cuts_first {
            assert!(
                start <= first_start && end >= first_end,
                "segment {start}..{end} cuts the first string"
            );
        }
        if cuts_second {
            assert!(
                start <= second_start && end >= second_end,
                "segment {start}..{end} cuts the second string"
            );
        }
    }
}
