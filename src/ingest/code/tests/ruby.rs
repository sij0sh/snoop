use super::*;

#[test]
fn ruby_symbols_map_to_atom_kinds() {
    let source = "# frozen_string_literal: true\nrequire \"json\"\n\nmodule Store\nclass Session\n  def refresh\n    validate\n  end\n\n  def self.validate\n    check\n  end\nend\nend\n";
    let atoms = parse_code(source, "lib/store.rb").unwrap();
    assert_eq!(atoms[0].metadata["language"], "ruby");
    // The root program becomes a Module overview for script languages.
    let overview = atoms
        .iter()
        .find(|atom| {
            atom.kind == AtomKind::Module && atom.metadata["symbol"].as_str() == Some("program")
        })
        .unwrap();
    assert_eq!(overview.start_offset, 0);
    let class = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Class && atom.breadcrumb.ends_with("Store > Session"))
        .unwrap();
    assert_eq!(class.metadata["symbol"].as_str(), Some("Session"));
    let function = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("refresh"))
        .unwrap();
    assert_eq!(function.metadata["symbol"].as_str(), Some("refresh"));
}

#[test]
fn ruby_singleton_methods_anchor_on_bare_names() {
    let source = "class Session\n  def self.validate\n    check\n  end\nend\n";
    let atoms = parse_code(source, "lib/session.rb").unwrap();
    let singleton = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("Session > self.validate"))
        .unwrap();
    assert_eq!(singleton.kind, AtomKind::Function);
    assert_eq!(singleton.metadata["symbol"].as_str(), Some("validate"));
}

#[test]
fn ruby_namespaced_class_names_resolve() {
    let source = "class Auth::Session\n  def load\n  end\nend\n";
    let atoms = parse_code(source, "lib/auth.rb").unwrap();
    let class = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Class)
        .unwrap();
    assert_eq!(class.metadata["symbol"].as_str(), Some("Auth::Session"));
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.ends_with("Auth::Session > load")));
}

#[test]
fn ruby_require_calls_are_imports() {
    let source = "require \"json\"\nrequire_relative \"../auth/token\"\nload \"configuration.rb\"\nputs \"boot\"\n";
    let atoms = parse_code(source, "config/boot.rb").unwrap();
    let imports: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.metadata["is_import"].as_bool() == Some(true))
        .collect();
    assert_eq!(imports.len(), 3);
    assert!(imports
        .iter()
        .all(|atom| atom.kind == AtomKind::Declaration));
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.ends_with("require_relative")));
}

#[test]
fn ruby_dynamic_metaprograming_is_not_inferred() {
    let source = "class Session\ndefine_method(:ghost) { }\nhas_many :tokens\nend\n";
    let atoms = parse_code(source, "lib/session.rb").unwrap();
    assert!(atoms
        .iter()
        .all(|atom| atom.kind != AtomKind::Function
            || atom.metadata["symbol"].as_str() != Some("ghost")));
}

#[test]
fn ruby_heredocs_stay_atomic_and_namespaces_nest() {
    let source = "module API\nclass Client\n  def render\n    text = <<~SQL\n      select 1\n    SQL\n    text\n  end\nend\nend\n";
    let atoms = parse_code(source, "lib/api/client.rb").unwrap();
    assert!(atoms
        .iter()
        .any(|atom| atom.breadcrumb.ends_with("API > Client > render")));
    // Determinism across runs.
    let again = parse_code(source, "lib/api/client.rb").unwrap();
    assert_eq!(atoms.len(), again.len());
}

#[test]
fn ruby_malformed_input_still_parses() {
    let atoms = parse_code("def broken\n", "lib/broken.rb").unwrap();
    assert!(!atoms.is_empty());
}
