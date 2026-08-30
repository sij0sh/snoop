//! Extension-to-language registry shared by ingestion and the CLI.

use std::ops::Range;

use tree_sitter::Node;

use super::{
    c_atomic, c_interesting, c_leading_context, c_symbol_info, cpp_atomic, cpp_interesting,
    cpp_leading_context, cpp_symbol_info, csharp_atomic, csharp_interesting,
    csharp_leading_context, csharp_symbol_info, ecmascript_atomic, ecmascript_interesting,
    ecmascript_leading_context, ecmascript_symbol_info, field_symbol_info, go_atomic,
    go_interesting, go_leading_context, go_symbol_info, java_atomic, java_interesting,
    java_leading_context, java_symbol_info, python_atomic, python_interesting,
    python_leading_context, rust_atomic, rust_interesting, rust_leading_context, rust_symbol_info,
    SymbolInfo,
};
use crate::core::AtomKind;

pub const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "pyw", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "java",
    "cs", "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "ipp", "tpp", "inl",
];

pub struct Language {
    pub name: &'static str,
    pub language: fn() -> tree_sitter::Language,
    pub interesting: fn(Node<'_>, &str) -> Option<AtomKind>,
    pub symbol_info: fn(Node<'_>, &str) -> Option<SymbolInfo>,
    pub leading_context: fn(Node<'_>, &str) -> Option<Range<usize>>,
    pub is_atomic: fn(Node<'_>) -> bool,
}

pub fn code_extension(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    CODE_EXTENSIONS
        .iter()
        .copied()
        .find(|candidate| *candidate == extension)
}

pub fn supports_code_path(path: &str) -> bool {
    code_extension(path).is_some()
}

pub fn language_name(path: &str) -> Option<&'static str> {
    language_for(path).map(|language| language.name)
}

pub fn language_for(locator: &str) -> Option<Language> {
    match code_extension(locator)? {
        "rs" => Some(Language {
            name: "rust",
            language: || tree_sitter_rust::LANGUAGE.into(),
            interesting: rust_interesting,
            symbol_info: rust_symbol_info,
            leading_context: rust_leading_context,
            is_atomic: rust_atomic,
        }),
        "py" | "pyi" | "pyw" => Some(Language {
            name: "python",
            language: || tree_sitter_python::LANGUAGE.into(),
            interesting: python_interesting,
            symbol_info: field_symbol_info,
            leading_context: python_leading_context,
            is_atomic: python_atomic,
        }),
        "ts" | "mts" | "cts" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
        }),
        "tsx" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
        }),
        "js" | "jsx" | "mjs" | "cjs" => Some(Language {
            name: "javascript",
            language: || tree_sitter_javascript::LANGUAGE.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
        }),
        "go" => Some(Language {
            name: "go",
            language: || tree_sitter_go::LANGUAGE.into(),
            interesting: go_interesting,
            symbol_info: go_symbol_info,
            leading_context: go_leading_context,
            is_atomic: go_atomic,
        }),
        "java" => Some(Language {
            name: "java",
            language: || tree_sitter_java::LANGUAGE.into(),
            interesting: java_interesting,
            symbol_info: java_symbol_info,
            leading_context: java_leading_context,
            is_atomic: java_atomic,
        }),
        "cs" => Some(Language {
            name: "csharp",
            language: || tree_sitter_c_sharp::LANGUAGE.into(),
            interesting: csharp_interesting,
            symbol_info: csharp_symbol_info,
            leading_context: csharp_leading_context,
            is_atomic: csharp_atomic,
        }),
        "c" => Some(Language {
            name: "c",
            language: || tree_sitter_c::LANGUAGE.into(),
            interesting: c_interesting,
            symbol_info: c_symbol_info,
            leading_context: c_leading_context,
            is_atomic: c_atomic,
        }),
        // Every C++ header extension routes to the cpp adapter; header
        // policy is extension-only with no content sniffing.
        "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" | "ipp" | "tpp" | "inl" => {
            Some(Language {
                name: "cpp",
                language: || tree_sitter_cpp::LANGUAGE.into(),
                interesting: cpp_interesting,
                symbol_info: cpp_symbol_info,
                leading_context: cpp_leading_context,
                is_atomic: cpp_atomic,
            })
        }
        _ => None,
    }
}
