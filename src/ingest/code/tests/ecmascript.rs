use super::*;

#[test]
fn tauri_bridge_calls_become_named_declarations() {
    let source = concat!(
        "import { invoke } from '@tauri-apps/api/core';\n",
        "import { listen } from '@tauri-apps/api/event';\n\n",
        "export function save(path: string) {\n",
        "  return invoke<string>('save_file', { path });\n",
        "}\n\n",
        "export function watch() {\n",
        "  return listen('file-saved', (event) => event.payload);\n",
        "}\n\n",
        "export function notify(message: string) {\n",
        "  return window.__TAURI__.emit('status', message);\n",
        "}\n"
    );
    let atoms = parse_code(source, "src/api.ts").unwrap();
    let bridges: Vec<_> = atoms
        .iter()
        .filter(|atom| {
            atom.kind == AtomKind::Declaration
                && (atom.breadcrumb.contains(" > invoke ")
                    || atom.breadcrumb.contains(" > listen ")
                    || atom.breadcrumb.contains(" > emit "))
        })
        .collect();
    let mut names: Vec<&str> = bridges
        .iter()
        .map(|atom| atom.metadata["symbol"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["file-saved", "save_file", "status"]);
    for atom in &bridges {
        let breadcrumb = atom.breadcrumb.rsplit(" > ").next().unwrap();
        let symbol = atom.metadata["symbol"].as_str().unwrap();
        assert!(
            breadcrumb.ends_with(symbol),
            "breadcrumb {breadcrumb} must end with anchor name {symbol}"
        );
    }
}

#[test]
fn tauri_bridge_ignores_dynamic_and_non_bridge_calls() {
    let source = concat!(
        "import { invoke as rpc } from '@tauri-apps/api/core';\n\n",
        "const channel = 'status';\n\n",
        "export function run(name: string) {\n",
        "  rpc(name);\n",
        "  rpc(`save_${name}`);\n",
        "  rpc('save_' + name);\n",
        "  rpc('');\n",
        "  fetch('save_file');\n",
        "  return invoke_nothing('save_file');\n",
        "}\n"
    );
    let atoms = parse_code(source, "src/dynamic.ts").unwrap();
    assert!(
        !atoms
            .iter()
            .any(|atom| atom.breadcrumb.contains(" > invoke ")
                || atom.breadcrumb.contains(" > listen ")
                || atom.breadcrumb.contains(" > emit ")),
        "no bridge declaration may be extracted: {:?}",
        atoms
            .iter()
            .map(|atom| &atom.breadcrumb)
            .collect::<Vec<_>>()
    );
}
