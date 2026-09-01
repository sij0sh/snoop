//! Conditional chunking of `.agents/CHEATCODES.md` corpora: one unit per
//! entry, entries never blended, other markdown untouched.

use snoop::core::RetrievalUnit;
use snoop::ingest::index_repository_bounded;
use snoop::store::Store;

fn corpus() -> String {
    let mut corpus = String::from("# CHEATCODES\n\n");
    for (id, title, body) in [
        ("cc-1", "Alpha entry", "Alpha body mentions `src/auth.rs`."),
        ("cc-2", "Beta entry", "Beta body mentions `src/rotate.rs`."),
    ] {
        corpus.push_str(&format!(
            "<!-- cheatcodes-entry {{\"id\":\"{id}\",\"title\":\"{title}\"}}-->\n\
             ## {title}\n\n{body}\n\n<!-- /cheatcodes-entry -->\n"
        ));
    }
    corpus
}

fn units_for(store: &Store, locator: &str) -> Vec<RetrievalUnit> {
    store
        .unit_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| store.unit_by_id(id).unwrap())
        .filter(|unit| unit.locator == locator)
        .collect()
}

fn index(directory: &tempfile::TempDir) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, directory.path(), None, None).unwrap();
    store
}

#[test]
fn cheatcodes_corpus_yields_one_unit_per_entry() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join(".agents")).unwrap();
    std::fs::write(directory.path().join(".gitignore"), ".agents/\n").unwrap();
    let corpus = corpus();
    std::fs::write(directory.path().join(".agents/CHEATCODES.md"), &corpus).unwrap();
    std::fs::write(
        directory.path().join("README.md"),
        "# Guide\n\nOrdinary prose.\n",
    )
    .unwrap();

    let store = index(&directory);
    assert!(
        store
            .source_by_locator(".agents/CHEATCODES.md")
            .unwrap()
            .is_some(),
        "the corpus is indexed despite the hidden directory and ignore rule"
    );

    let units = units_for(&store, ".agents/CHEATCODES.md");
    assert_eq!(units.len(), 2, "one unit per entry");
    let evidence: Vec<&str> = units.iter().map(|unit| unit.evidence_text.as_str()).collect();
    assert!(evidence
        .iter()
        .any(|text| text.contains("CHEATCODES > Alpha entry") && text.contains("Alpha body")));
    assert!(evidence
        .iter()
        .any(|text| text.contains("CHEATCODES > Beta entry") && text.contains("Beta body")));
    for text in &evidence {
        assert!(
            !(text.contains("Alpha") && text.contains("Beta")),
            "units must not blend entries: {text}"
        );
    }

    for unit in &units {
        let slices = unit.metadata["source_slices"]
            .as_array()
            .expect("source slices provenance");
        assert!(!slices.is_empty());
        for slice in slices {
            let start = slice["start_offset"].as_u64().unwrap() as usize;
            let end = slice["end_offset"].as_u64().unwrap() as usize;
            assert!(end > start && end <= corpus.len());
            assert!(
                unit.evidence_text.contains(corpus[start..end].trim()),
                "slice {start}..{end} maps back onto the original corpus"
            );
        }
    }

    let readme = units_for(&store, "README.md");
    assert!(!readme.is_empty(), "ordinary markdown keeps its units");
    assert!(readme
        .iter()
        .any(|unit| unit.evidence_text.contains("README > Guide")
            && unit.evidence_text.contains("Ordinary prose.")));
}

#[test]
fn unclosed_final_entry_is_captured_to_end_of_file() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join(".agents")).unwrap();
    let corpus =
        "<!-- cheatcodes-entry {\"id\":\"cc-1\"}-->\n## Only entry\n\nOnly body.\n";
    std::fs::write(directory.path().join(".agents/CHEATCODES.md"), corpus).unwrap();

    let store = index(&directory);
    let units = units_for(&store, ".agents/CHEATCODES.md");
    assert_eq!(units.len(), 1);
    assert!(units[0].evidence_text.contains("CHEATCODES > Only entry"));
    assert!(units[0].evidence_text.contains("Only body."));
}
