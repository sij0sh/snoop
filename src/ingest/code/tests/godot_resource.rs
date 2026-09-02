use super::*;

const SCENE: &str = r#"[gd_scene load_steps=3 format=3 uid="uid://abc123"]

[ext_resource type="Script" path="res://actors/player.gd" id="1_player"]

[sub_resource type="Curve" id="Curve_damage"]
_data = [0.0, 0.0, 1.0, 1.0]

[node name="Player" type="CharacterBody2D" groups=["heroes"]]
script = ExtResource("1_player")
speed = 300.0

[node name="Camera" type="Camera2D" parent="."]

[node name="Hurtbox" type="Area2D" parent="Camera"]

[connection signal="body_entered" from="Camera/Hurtbox" to="." method="_on_hurtbox_body_entered"]
"#;

#[test]
fn godot_scene_nodes_become_class_units_with_path_identity() {
    let atoms = parse_code(SCENE, "actors/player.tscn").unwrap();
    assert_eq!(atoms[0].metadata["language"], "godot_resource");
    for needle in ["Player", "Camera", "Camera/Hurtbox"] {
        assert!(
            atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
            "missing breadcrumb ending in {needle}"
        );
    }
    let root = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("Player"))
        .unwrap();
    assert_eq!(root.kind, AtomKind::Class);
    // Node properties stay inside their node unit.
    assert!(root.text.contains("script = ExtResource(\"1_player\")"));
    assert!(root.text.contains("speed = 300.0"));
    assert!(root.text.contains("groups"));
}

#[test]
fn godot_scene_headers_and_ext_resources_stay_in_the_overview() {
    let atoms = parse_code(SCENE, "actors/player.tscn").unwrap();
    let file = &atoms[0];
    assert_eq!(file.kind, AtomKind::File);
    assert!(file.text.contains("uid=\"uid://abc123\""));
    assert!(file.text.contains("path=\"res://actors/player.gd\""));
    assert!(!atoms.iter().any(|atom| {
        atom.kind != AtomKind::File && atom.text.contains("[ext_resource")
    }));
}

#[test]
fn godot_subresources_become_namespaced_declarations() {
    let atoms = parse_code(SCENE, "actors/player.tscn").unwrap();
    let curve = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("SubResource:Curve:Curve_damage"))
        .unwrap();
    assert_eq!(curve.kind, AtomKind::Declaration);
    assert!(curve.text.contains("_data = [0.0, 0.0, 1.0, 1.0]"));
}

#[test]
fn godot_connections_render_signal_wiring() {
    let atoms = parse_code(SCENE, "actors/player.tscn").unwrap();
    let connection = atoms
        .iter()
        .find(|atom| {
            atom.metadata["symbol"].as_str()
                == Some("Camera/Hurtbox.body_entered -> _on_hurtbox_body_entered")
        })
        .unwrap();
    assert_eq!(connection.kind, AtomKind::Declaration);
}

#[test]
fn godot_resource_file_emits_main_resource_unit() {
    let source = concat!(
        "[gd_resource type=\"WeaponData\" load_steps=2 format=3]\n",
        "\n",
        "[ext_resource type=\"Script\" path=\"res://items/weapon_data.gd\" id=\"1_script\"]\n",
        "\n",
        "[resource]\n",
        "script = ExtResource(\"1_script\")\n",
        "display_name = \"Rusty Sword\"\n",
        "damage = 12\n",
    );
    let atoms = parse_code(source, "items/sword.tres").unwrap();
    assert_eq!(atoms[0].metadata["language"], "godot_resource");
    let resource = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("resource"))
        .unwrap();
    assert_eq!(resource.kind, AtomKind::Declaration);
    assert!(resource.text.contains("display_name = \"Rusty Sword\""));
    assert!(!atoms.iter().any(|atom| atom
        .metadata["symbol"]
        .as_str()
        .is_some_and(|symbol| symbol.starts_with("SubResource"))));
}

#[test]
fn godot_typed_subresources_are_units_but_empty_headers_are_not() {
    let source = concat!(
        "[gd_resource type=\"Loadout\" format=3]\n",
        "\n",
        "[sub_resource type=\"Curve\" id=\"Curve_empty\"]\n",
        "\n",
        "[sub_resource type=\"Gradient\" id=\"Gradient_ramp\"]\n",
        "offsets = PackedFloat32Array(0, 1)\n",
        "\n",
        "[resource]\n",
        "ramp = SubResource(\"Gradient_ramp\")\n",
    );
    let atoms = parse_code(source, "items/loadout.tres").unwrap();
    let symbols: Vec<_> = atoms
        .iter()
        .filter(|atom| atom.kind == AtomKind::Declaration)
        .map(|atom| atom.metadata["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(symbols, vec!["SubResource:Gradient:Gradient_ramp", "resource"]);
}

#[test]
fn godot_oversized_serialized_values_split_on_boundaries() {
    let values: Vec<String> = (0..300).map(|index| format!("{index}.0, 1.0")).collect();
    let source = format!(
        concat!(
            "[gd_scene format=3]\n",
            "\n",
            "[node name=\"Map\" type=\"TileMap\"]\n",
            "tile_map_data = PackedByteArray({})\n",
        ),
        values.join(", ")
    );
    let atoms = parse_code(&source, "levels/map.tscn").unwrap();
    let map = atoms
        .iter()
        .find(|atom| atom.metadata["symbol"].as_str() == Some("Map"))
        .unwrap();
    let segments = map.metadata["chunk_segments"].as_array().unwrap();
    assert!(!segments.is_empty());
    // The serialized blob stays inside the node unit, never split into
    // child units.
    assert!(map.text.contains("tile_map_data"));
}

#[test]
fn godot_resource_ids_never_become_imports() {
    let atoms = parse_code(SCENE, "actors/player.tscn").unwrap();
    assert!(atoms
        .iter()
        .all(|atom| atom.metadata["is_import"].as_bool() != Some(true)));
}

#[test]
fn godot_malformed_input_still_parses() {
    let source = "[gd_scene format=3\n\n[node name=\"Broken\" type=\"Node\"\nspeed = \n";
    let atoms = parse_code(source, "levels/broken.tscn").unwrap();
    // The unparseable section is not emitted as a unit, but the file
    // still indexes through its raw-text overview.
    assert!(!atoms.is_empty());
    assert_eq!(atoms[0].kind, AtomKind::File);
    assert!(atoms[0].text.contains("[node name=\"Broken\""));
    assert!(!atoms
        .iter()
        .any(|atom| atom.metadata["symbol"].as_str() == Some("Broken")));
}

#[test]
fn godot_parsing_is_deterministic() {
    let first = parse_code(SCENE, "actors/player.tscn").unwrap();
    let second = parse_code(SCENE, "actors/player.tscn").unwrap();
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
