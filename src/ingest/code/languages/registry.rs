//! Extension, filename, and shebang registry shared by ingestion and
//! the CLI.

use std::ops::Range;

use tree_sitter::Node;

use super::{
    c_atomic, c_interesting, c_is_import, c_leading_context, c_symbol_info, cpp_atomic,
    cpp_interesting, cpp_is_import, cpp_leading_context, cpp_symbol_info, csharp_atomic,
    csharp_interesting, csharp_is_import, csharp_leading_context, csharp_symbol_info,
    ecmascript_atomic, ecmascript_interesting, ecmascript_is_import, ecmascript_leading_context,
    ecmascript_symbol_info, field_symbol_info, gdscript_atomic, gdscript_interesting,
    gdscript_is_import, gdscript_leading_context, gdscript_symbol_info, go_atomic, go_interesting,
    go_is_import, go_leading_context, go_symbol_info, godot_resource_atomic,
    godot_resource_interesting, godot_resource_is_import, godot_resource_leading_context,
    godot_resource_symbol_info, java_atomic, java_interesting, java_is_import,
    java_leading_context, java_symbol_info, php_atomic, php_interesting, php_is_import,
    php_leading_context, php_symbol_info, python_atomic, python_interesting, python_is_import,
    python_leading_context, ruby_atomic, ruby_interesting, ruby_is_import, ruby_leading_context,
    ruby_symbol_info, rust_atomic, rust_interesting, rust_is_import, rust_leading_context,
    rust_symbol_info, shell_atomic, shell_interesting, shell_is_import, shell_leading_context,
    SymbolInfo,
};
use crate::core::AtomKind;

pub const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "pyw", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "java",
    "cs", "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "ipp", "tpp", "inl", "rb", "rake",
    "gemspec", "php", "phtml", "sh", "bash", "gd", "tscn", "tres",
];

/// One language's recognition rules. Matching order is exact filename,
/// then file extension, then shebang for otherwise unsupported,
/// extensionless files.
pub struct LanguageMatch {
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    pub shebangs: &'static [&'static str],
}

const LANGUAGES: &[(&str, LanguageMatch)] = &[
    (
        "rust",
        LanguageMatch {
            extensions: &["rs"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "python",
        LanguageMatch {
            extensions: &["py", "pyi", "pyw"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "typescript",
        LanguageMatch {
            extensions: &["ts", "tsx", "mts", "cts"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "javascript",
        LanguageMatch {
            extensions: &["js", "jsx", "mjs", "cjs"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "go",
        LanguageMatch {
            extensions: &["go"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "java",
        LanguageMatch {
            extensions: &["java"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "csharp",
        LanguageMatch {
            extensions: &["cs"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "c",
        LanguageMatch {
            extensions: &["c"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        // Header policy is extension-only with no content sniffing.
        "cpp",
        LanguageMatch {
            extensions: &[
                "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "ipp", "tpp", "inl",
            ],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "ruby",
        LanguageMatch {
            extensions: &["rb", "rake", "gemspec"],
            filenames: &["Gemfile", "Rakefile", "Vagrantfile"],
            shebangs: &["ruby"],
        },
    ),
    (
        "php",
        LanguageMatch {
            extensions: &["php", "phtml"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        "shell",
        LanguageMatch {
            extensions: &["sh", "bash"],
            filenames: &[],
            shebangs: &["sh", "bash"],
        },
    ),
    (
        "gdscript",
        LanguageMatch {
            extensions: &["gd"],
            filenames: &[],
            shebangs: &[],
        },
    ),
    (
        // One grammar covers both serialized resource formats.
        "godot_resource",
        LanguageMatch {
            extensions: &["tscn", "tres"],
            filenames: &[],
            shebangs: &[],
        },
    ),
];

pub struct Language {
    pub name: &'static str,
    pub language: fn() -> tree_sitter::Language,
    pub interesting: fn(Node<'_>, &str) -> Option<AtomKind>,
    pub symbol_info: fn(Node<'_>, &str) -> Option<SymbolInfo>,
    pub leading_context: fn(Node<'_>, &str) -> Option<Range<usize>>,
    pub is_atomic: fn(Node<'_>) -> bool,
    pub is_import: fn(Node<'_>, &str) -> bool,
    /// Script languages execute meaningful code at the top level; the
    /// root `program` becomes a `Module` overview unit.
    pub root_overview: bool,
}

pub fn code_extension(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    CODE_EXTENSIONS
        .iter()
        .copied()
        .find(|candidate| *candidate == extension)
}

pub fn supports_code_path(path: &str) -> bool {
    language_key_by_path(path).is_some()
}

pub fn language_name(path: &str) -> Option<&'static str> {
    language_key_by_path(path)
}

pub fn language_for(locator: &str) -> Option<Language> {
    language_key_by_path(locator).and_then(build_language)
}

/// Path matching first, then a shebang probe for otherwise unsupported,
/// extensionless files. The shebang check reads at most the first 256
/// bytes and never executes or fully interprets the line.
pub fn language_for_source(locator: &str, source: &str) -> Option<Language> {
    language_for(locator).or_else(|| {
        if locator.contains('.') {
            return None;
        }
        shebang_key(source).and_then(build_language)
    })
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn language_key_by_path(path: &str) -> Option<&'static str> {
    let base = basename(path);
    for (name, entry) in LANGUAGES {
        if entry.filenames.contains(&base) {
            return Some(name);
        }
    }
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    for (name, entry) in LANGUAGES {
        if entry.extensions.contains(&extension.as_str()) {
            return Some(name);
        }
    }
    None
}

fn shebang_key(source: &str) -> Option<&'static str> {
    let bytes = source.as_bytes();
    let window = &bytes[..bytes.len().min(256)];
    let end = window
        .iter()
        .position(|&byte| byte == b'\n')
        .unwrap_or(window.len());
    let line = std::str::from_utf8(&window[..end]).ok()?;
    let body = line.strip_prefix("#!")?;
    LANGUAGES
        .iter()
        .find(|(_, entry)| {
            entry.shebangs.iter().any(|token| {
                body.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|part| part == *token)
            })
        })
        .map(|(name, _)| *name)
}

fn build_language(key: &'static str) -> Option<Language> {
    match key {
        "rust" => Some(Language {
            name: "rust",
            language: || tree_sitter_rust::LANGUAGE.into(),
            interesting: rust_interesting,
            symbol_info: rust_symbol_info,
            leading_context: rust_leading_context,
            is_atomic: rust_atomic,
            is_import: rust_is_import,
            root_overview: false,
        }),
        "python" => Some(Language {
            name: "python",
            language: || tree_sitter_python::LANGUAGE.into(),
            interesting: python_interesting,
            symbol_info: field_symbol_info,
            leading_context: python_leading_context,
            is_atomic: python_atomic,
            is_import: python_is_import,
            root_overview: false,
        }),
        "typescript" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
            is_import: ecmascript_is_import,
            root_overview: false,
        }),
        "javascript" => Some(Language {
            name: "javascript",
            language: || tree_sitter_javascript::LANGUAGE.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
            is_import: ecmascript_is_import,
            root_overview: false,
        }),
        "go" => Some(Language {
            name: "go",
            language: || tree_sitter_go::LANGUAGE.into(),
            interesting: go_interesting,
            symbol_info: go_symbol_info,
            leading_context: go_leading_context,
            is_atomic: go_atomic,
            is_import: go_is_import,
            root_overview: false,
        }),
        "java" => Some(Language {
            name: "java",
            language: || tree_sitter_java::LANGUAGE.into(),
            interesting: java_interesting,
            symbol_info: java_symbol_info,
            leading_context: java_leading_context,
            is_atomic: java_atomic,
            is_import: java_is_import,
            root_overview: false,
        }),
        "csharp" => Some(Language {
            name: "csharp",
            language: || tree_sitter_c_sharp::LANGUAGE.into(),
            interesting: csharp_interesting,
            symbol_info: csharp_symbol_info,
            leading_context: csharp_leading_context,
            is_atomic: csharp_atomic,
            is_import: csharp_is_import,
            root_overview: false,
        }),
        "c" => Some(Language {
            name: "c",
            language: || tree_sitter_c::LANGUAGE.into(),
            interesting: c_interesting,
            symbol_info: c_symbol_info,
            leading_context: c_leading_context,
            is_atomic: c_atomic,
            is_import: c_is_import,
            root_overview: false,
        }),
        "cpp" => Some(Language {
            name: "cpp",
            language: || tree_sitter_cpp::LANGUAGE.into(),
            interesting: cpp_interesting,
            symbol_info: cpp_symbol_info,
            leading_context: cpp_leading_context,
            is_atomic: cpp_atomic,
            is_import: cpp_is_import,
            root_overview: false,
        }),
        "ruby" => Some(Language {
            name: "ruby",
            language: || tree_sitter_ruby::LANGUAGE.into(),
            interesting: ruby_interesting,
            symbol_info: ruby_symbol_info,
            leading_context: ruby_leading_context,
            is_atomic: ruby_atomic,
            is_import: ruby_is_import,
            root_overview: true,
        }),
        "php" => Some(Language {
            name: "php",
            language: || tree_sitter_php::LANGUAGE_PHP.into(),
            interesting: php_interesting,
            symbol_info: php_symbol_info,
            leading_context: php_leading_context,
            is_atomic: php_atomic,
            is_import: php_is_import,
            root_overview: false,
        }),
        "shell" => Some(Language {
            name: "shell",
            language: || tree_sitter_bash::LANGUAGE.into(),
            interesting: shell_interesting,
            symbol_info: field_symbol_info,
            leading_context: shell_leading_context,
            is_atomic: shell_atomic,
            is_import: shell_is_import,
            root_overview: true,
        }),
        "gdscript" => Some(Language {
            name: "gdscript",
            language: || tree_sitter_gdscript::LANGUAGE.into(),
            interesting: gdscript_interesting,
            symbol_info: gdscript_symbol_info,
            leading_context: gdscript_leading_context,
            is_atomic: gdscript_atomic,
            is_import: gdscript_is_import,
            root_overview: false,
        }),
        "godot_resource" => Some(Language {
            name: "godot_resource",
            language: || tree_sitter_godot_resource::LANGUAGE.into(),
            interesting: godot_resource_interesting,
            symbol_info: godot_resource_symbol_info,
            leading_context: godot_resource_leading_context,
            is_atomic: godot_resource_atomic,
            is_import: godot_resource_is_import,
            root_overview: false,
        }),
        _ => None,
    }
}
