use super::*;

#[test]
fn csharp_symbols_map_to_atom_kinds() {
    let source = "using System;\n\nnamespace Demo\n{\n    public class Store\n    {\n        private int count;\n\n        /// Loads the count.\n        public void Refresh()\n        {\n            count = Compute();\n        }\n\n        public int Count\n        {\n            get { return count; }\n            set { count = value; }\n        }\n\n        public int Double(int v) => v * 2;\n\n        public Store()\n        {\n        }\n\n        public static int operator +(Store a, Store b) => 0;\n\n        public static implicit operator int(Store s) => 0;\n\n        public event Handler Changed;\n    }\n\n    public struct Pair { }\n    public interface Repo { void Save(); }\n    public enum Mode { Read, Write }\n    public record Point(int X, int Y);\n    public delegate void Handler(object sender);\n}\n";
    let atoms = parse_code(source, "src/Store.cs").unwrap();
    assert_eq!(atoms[0].metadata["language"], "csharp");
    for needle in [
        "Demo > Store > Refresh",
        "Demo > Store > Store",
        "Demo > Store > operator +",
        "Demo > Store > operator int",
        "Demo > Store > Count",
        "Demo > Store > count",
        "Demo > Store > Double",
        "Demo > Store > Changed",
        "Demo > Pair",
        "Demo > Repo > Save",
        "Demo > Mode",
        "Demo > Point",
        "Demo > Handler",
        "using System;",
    ] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
    let refresh = atoms
        .iter()
        .find(|atom| atom.breadcrumb.ends_with("Demo > Store > Refresh"))
        .unwrap();
    assert!(refresh.metadata["leading_context"]["text"]
        .as_str()
        .unwrap()
        .contains("/// Loads the count."));
    assert!(refresh.metadata["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "Compute"));
}

#[test]
fn csharp_file_scoped_namespaces_and_generics_qualify_breadcrumbs() {
    let source = "namespace App;\n\npublic class Worker\n{\n    public void Run<T>(T item)\n    {\n        Handle(item);\n    }\n}\n";
    let atoms = parse_code(source, "src/Worker.cs").unwrap();
    let runs: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.breadcrumb.ends_with("Run"))
        .collect();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].breadcrumb.ends_with("App > Worker > Run"));
}

#[test]
fn csharp_attributes_and_docs_attach_as_leading_context() {
    let source = "public class Guard\n{\n    /// Checks access.\n    [Authorize]\n    public void Check() { }\n}\n";
    let boundaries = analyze_code("src/Guard.cs", source).unwrap();
    let check = boundaries
        .iter()
        .find(|boundary| boundary.display_name == "Check")
        .unwrap();
    let text = &source[check.leading_context.clone().unwrap()];
    assert!(text.contains("/// Checks access."));
    // Attribute lists are children of the declaration node in this grammar,
    // so they travel with the atom text instead of the leading context.
    assert!(source[check.byte_range.clone()].contains("[Authorize]"));
}

#[test]
fn csharp_oversized_raw_strings_stay_atomic() {
    let long = "z".repeat(2_000);
    let filler = "y".repeat(2_000);
    let source = format!(
        "class Big {{\n    void Run() {{\n        var first = \"\"\"\n{long}\n\"\"\";\n        var second = \"\"\"\n{filler}\n\"\"\";\n    }}\n}}\n"
    );
    let atoms = parse_code(&source, "src/Big.cs").unwrap();
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
        if start < first_end && end > first_start {
            assert!(
                start <= first_start && end >= first_end,
                "segment {start}..{end} cuts the first raw string"
            );
        }
        if start < second_end && end > second_start {
            assert!(
                start <= second_start && end >= second_end,
                "segment {start}..{end} cuts the second raw string"
            );
        }
    }
}
