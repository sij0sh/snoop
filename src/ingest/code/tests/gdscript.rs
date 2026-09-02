use super::*;

const PLAYER: &str = r#"@tool
## A player-controlled character.
class_name Player
extends CharacterBody2D

## Emitted when health drops.
signal health_changed(amount: int)

enum State { IDLE, RUN }

const SPEED := 300.0

@export var speed_boost := 1.0
@onready var sprite := $Sprite2D

func _ready() -> void:
	pass

## Moves the player.
@rpc("any_peer")
static func move(direction: Vector2, delta: float) -> void:
	var step := SPEED * delta
	move_and_slide()

class Inventory:
	var items: Array[String] = []

	func add_item(item: String) -> void:
		items.append(item)
"#;

#[test]
fn gdscript_symbols_map_to_atom_kinds() {
    let atoms = parse_code(PLAYER, "actors/player.gd").unwrap();
    assert_eq!(atoms[0].metadata["language"], "gdscript");
    for needle in [
        "Player",
        "Player > health_changed",
        "Player > State",
        "Player > SPEED",
        "Player > speed_boost",
        "Player > sprite",
        "Player > _ready",
        "Player > move",
        "Player > Inventory",
        "Player > Inventory > items",
        "Player > Inventory > add_item",
    ] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
    let class_name = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("Player"))
        .unwrap();
    assert_eq!(class_name.kind, AtomKind::Declaration);
    let signal = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("health_changed"))
        .unwrap();
    assert_eq!(signal.kind, AtomKind::Declaration);
    let inner = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("Inventory"))
        .unwrap();
    assert_eq!(inner.kind, AtomKind::Class);
    let function = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("move"))
        .unwrap();
    assert_eq!(function.kind, AtomKind::Function);
}

#[test]
fn gdscript_without_class_name_uses_file_scoped_symbols() {
    let source = "extends Node\n\nfunc wander(target: Vector2) -> void:\\tpass\n";
    let atoms = parse_code(source, "actors/npc.gd").unwrap();
    let wander = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("wander"))
        .unwrap();
    assert_eq!(wander.breadcrumb, "actors/npc.gd > wander");
}

#[test]
fn gdscript_docs_and_annotations_attach_as_leading_context() {
    let atoms = parse_code(PLAYER, "actors/player.gd").unwrap();
    let move_atom = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("move"))
        .unwrap();
    let context = move_atom.metadata["leading_context"]["text"]
        .as_str()
        .unwrap();
    assert!(context.contains("## Moves the player."));
    assert!(context.contains("@rpc"));
    assert!(move_atom.metadata["signature"]
        .as_str()
        .unwrap()
        .starts_with("static func move"));
}

#[test]
fn gdscript_constructor_is_init_and_callbacks_are_functions() {
    let source = "extends Node\n\nfunc _init() -> void:\\tpass\n\nfunc _ready() -> void:\\tpass\n";
    let atoms = parse_code(source, "actors/boot.gd").unwrap();
    let functions: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Function)
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(functions, vec!["_init", "_ready"]);
}

#[test]
fn gdscript_static_res_references_become_imports() {
    let source = concat!(
        "extends \"res://actors/base_actor.gd\"\n",
        "const Weapon = preload(\"res://weapons/sword.tscn\")\n",
        "var data = load(\"res://data/player_stats.tres\")\n",
        "var dynamic = load(variable_path)\n",
        "var plain = load(\"etc/config.cfg\")\n",
    );
    let atoms = parse_code(source, "actors/refs.gd").unwrap();
    let imports: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.metadata["is_import"].as_bool() == Some(true))
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        imports,
        vec![
            "extends res://actors/base_actor.gd",
            "preload res://weapons/sword.tscn",
            "load res://data/player_stats.tres",
        ]
    );
}

#[test]
fn gdscript_local_variables_stay_out_of_the_symbol_table() {
    let atoms = parse_code(PLAYER, "actors/player.gd").unwrap();
    assert!(!atoms
        .iter()
        .any(|atom| atom.metadata["symbol"].as_str() == Some("step")));
    assert!(!atoms
        .iter()
        .any(|atom| atom.metadata["symbol"].as_str() == Some("item")));
}

#[test]
fn gdscript_accessors_stay_inside_the_variable_unit() {
    let source = concat!(
        "var hp := 100:\n",
        "\tget:\n",
        "\t\treturn hp\n",
        "\tset(value):\n",
        "\t\thp = value\n",
    );
    let atoms = parse_code(source, "actors/vitals.gd").unwrap();
    let variables: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Declaration)
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(variables, vec!["hp"]);
    assert!(atoms[1].text.contains("set(value):"));
}

#[test]
fn gdscript_multiline_strings_are_atomic() {
    let source = concat!(
        "func banner() -> String:\n",
        "\treturn \"\"\"\n",
        "line one\n",
        "line two\n",
        "\"\"\"\n",
    );
    let atoms = parse_code(source, "ui/banner.gd").unwrap();
    let function = atoms
        .iter()
        .find(|atom| atom.kind == AtomKind::Function)
        .unwrap();
    assert!(function.text.contains("line two"));
}

#[test]
fn gdscript_large_units_split_on_string_boundaries() {
    let values: Vec<String> = (0..400).map(|index| format!("item number {index} here")).collect();
    let source = format!(
        "const LINES := [\n{}\n]\n",
        values
            .iter()
            .map(|value| format!("\t\"{value}\","))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let atoms = parse_code(&source, "data/lines.gd").unwrap();
    let declaration = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("LINES"))
        .unwrap();
    let segments = declaration.metadata["chunk_segments"]
        .as_array()
        .unwrap();
    assert!(!segments.is_empty());
}

#[test]
fn gdscript_malformed_input_still_parses() {
    let atoms = parse_code("func broken(:\nvar x\n", "actors/broken.gd").unwrap();
    assert!(!atoms.is_empty());
    assert!(atoms
        .iter()
        .any(|atom| atom.metadata["symbol"].as_str() == Some("x")));
}

#[test]
fn gdscript_parsing_is_deterministic() {
    let first = parse_code(PLAYER, "actors/player.gd").unwrap();
    let second = parse_code(PLAYER, "actors/player.gd").unwrap();
    assert_eq!(
        first
            .iter()
            .map(|atom| atom.content_hash.as_str())
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|atom| atom.content_hash.as_str())
            .collect::<Vec<_>>(),
    );
}
