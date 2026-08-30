//! Owned metadata contracts.
//!
//! Each cross-module metadata contract has exactly one owner: its key
//! literal and nested shape live here and nowhere else. Writers and readers
//! call the per-contract functions, so a contract change is one edit whose
//! call sites the compiler traces, and any persisted old shape is decoded
//! in exactly one boundary (the contract's read function).
//!
//! Keys consumed only inside their producing module (for example
//! `unit_shape`, `node_kind`, or `symbol_id`) stay inline: no cross-module
//! edge means no owner needed. `tests/architecture.rs` enforces that owned
//! key literals appear only in this module.
//!
//! Upgrade policy: persisted metadata JSON is never transformed in place,
//! not even by schema migrations, which copy it byte-for-byte. When a
//! contract's persisted shape changes incompatibly (rename, removal, type
//! or meaning change), bump `INDEX_FORMAT_VERSION` in `src/ingest/mod.rs`
//! so existing databases rebuild every source on the next index run. Read
//! functions stay total: an unrecognized shape decodes to the documented
//! default instead of reconstructing old semantics.

use serde_json::Value;

pub(crate) mod chunk_segments {
    use super::Value;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChunkSegment {
        pub start_offset: usize,
        pub end_offset: usize,
        pub boundary: String,
    }

    pub fn set(metadata: &mut Value, segments: Vec<ChunkSegment>) {
        metadata["chunk_segments"] = value(&segments);
    }

    /// Encoded entry array. Also used by the code emitter for its
    /// single-module `chunk_alternatives` mirror.
    pub fn value(segments: &[ChunkSegment]) -> Value {
        Value::Array(
            segments
                .iter()
                .map(|segment| {
                    serde_json::json!({
                        "start_offset": segment.start_offset,
                        "end_offset": segment.end_offset,
                        "boundary": segment.boundary,
                    })
                })
                .collect(),
        )
    }

    /// Single decode boundary: entries missing any field are dropped.
    pub fn read(metadata: &Value) -> Vec<ChunkSegment> {
        metadata["chunk_segments"]
            .as_array()
            .map(|segments| {
                segments
                    .iter()
                    .filter_map(|segment| {
                        Some(ChunkSegment {
                            start_offset: segment["start_offset"].as_u64()? as usize,
                            end_offset: segment["end_offset"].as_u64()? as usize,
                            boundary: segment["boundary"].as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub(crate) mod code_symbol {
    use super::Value;

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct CodeSymbol {
        pub display_name: String,
        pub signature: String,
        pub references: Vec<String>,
    }

    pub fn write(metadata: &mut Value, display_name: &str, signature: &str, references: &[String]) {
        metadata["symbol"] = serde_json::json!(display_name);
        metadata["signature"] = serde_json::json!(signature);
        metadata["references"] = serde_json::json!(references);
    }

    /// Stores only the display name: git span atoms carry no signature or
    /// references.
    pub fn set_symbol(metadata: &mut Value, display_name: &str) {
        metadata["symbol"] = serde_json::json!(display_name);
    }

    /// Single decode boundary. `symbol` is required; `signature` and
    /// `references` default to empty when absent, matching the historical
    /// readers of those fields.
    pub fn read(metadata: &Value) -> Option<CodeSymbol> {
        Some(CodeSymbol {
            display_name: metadata["symbol"].as_str()?.to_string(),
            signature: metadata["signature"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            references: metadata["references"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

pub(crate) mod git_tip {
    use super::Value;

    pub fn set(metadata: &mut Value, tip: &str) {
        metadata["git_tip"] = serde_json::json!(tip);
    }

    pub fn read(metadata: &Value) -> Option<&str> {
        metadata["git_tip"].as_str()
    }
}

pub(crate) mod is_import {
    use super::Value;

    pub fn set(metadata: &mut Value, is_import: bool) {
        metadata["is_import"] = serde_json::json!(is_import);
    }

    /// `None` when the key is absent (the reader falls back to node kinds).
    pub fn read(metadata: &Value) -> Option<bool> {
        metadata["is_import"].as_bool()
    }
}

pub(crate) mod leading_context {
    use super::Value;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct LeadingContext {
        pub start_offset: usize,
        pub end_offset: usize,
        pub text: String,
    }

    /// Stores the contract. `None` writes JSON `null` (the atom has no
    /// leading context), matching the historical writer shape.
    pub fn set(metadata: &mut Value, context: Option<LeadingContext>) {
        metadata["leading_context"] = match context {
            Some(context) => serde_json::json!({
                "start_offset": context.start_offset,
                "end_offset": context.end_offset,
                "text": context.text,
            }),
            None => Value::Null,
        };
    }

    /// Single decode boundary: `None` when the key is absent, null, or
    /// missing any of its three fields. A future restructure that must
    /// recognize earlier persisted shapes belongs here.
    pub fn read(metadata: &Value) -> Option<LeadingContext> {
        let context = &metadata["leading_context"];
        Some(LeadingContext {
            start_offset: context["start_offset"].as_u64()? as usize,
            end_offset: context["end_offset"].as_u64()? as usize,
            text: context["text"].as_str()?.to_string(),
        })
    }
}

pub(crate) mod source_slices {
    use super::Value;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SourceSlice {
        pub atom_hash: String,
        pub start_offset: usize,
        pub end_offset: usize,
        pub boundary: Option<String>,
    }

    pub fn set(metadata: &mut Value, slices: Vec<SourceSlice>) {
        metadata["source_slices"] = value(&slices);
    }

    fn value(slices: &[SourceSlice]) -> Value {
        Value::Array(
            slices
                .iter()
                .map(|slice| {
                    let mut entry = serde_json::json!({
                        "atom_hash": slice.atom_hash,
                        "start_offset": slice.start_offset,
                        "end_offset": slice.end_offset,
                    });
                    if let Some(boundary) = &slice.boundary {
                        entry["boundary"] = serde_json::json!(boundary);
                    }
                    entry
                })
                .collect(),
        )
    }

    /// Passthrough read for consumers that forward entries verbatim
    /// (diagnostics). Any reader that interprets the entry shape belongs
    /// here too, next to the shape.
    pub fn read(metadata: &Value) -> Vec<Value> {
        metadata["source_slices"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) mod timestamp {
    use super::Value;

    /// `None` leaves the key absent, matching the historical writer.
    pub fn set(metadata: &mut Value, timestamp: Option<i64>) {
        if let Some(timestamp) = timestamp {
            metadata["timestamp"] = serde_json::json!(timestamp);
        }
    }

    pub fn read(metadata: &Value) -> Option<i64> {
        metadata["timestamp"].as_i64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_segments_round_trip_and_drop_malformed_entries() {
        let segments = vec![
            chunk_segments::ChunkSegment {
                start_offset: 0,
                end_offset: 120,
                boundary: "ast".into(),
            },
            chunk_segments::ChunkSegment {
                start_offset: 120,
                end_offset: 300,
                boundary: "lexical_fallback".into(),
            },
        ];
        let mut metadata = serde_json::json!({});
        chunk_segments::set(&mut metadata, segments.clone());
        assert_eq!(chunk_segments::read(&metadata), segments);
        assert_eq!(
            chunk_segments::read(&serde_json::json!({"chunk_segments": [{"start_offset": 1}]})),
            Vec::<chunk_segments::ChunkSegment>::new()
        );
        assert_eq!(chunk_segments::read(&serde_json::json!({})), Vec::new());
    }

    #[test]
    fn code_symbol_round_trips_and_defaults_partial_records() {
        let mut metadata = serde_json::json!({});
        code_symbol::write(&mut metadata, "Session", "class Session", &["Auth".into()]);
        assert_eq!(
            code_symbol::read(&metadata),
            Some(code_symbol::CodeSymbol {
                display_name: "Session".into(),
                signature: "class Session".into(),
                references: vec!["Auth".into()],
            })
        );

        // Git span shape: symbol only; signature/references degrade to empty.
        let mut metadata = serde_json::json!({});
        code_symbol::set_symbol(&mut metadata, "refresh");
        assert_eq!(
            code_symbol::read(&metadata),
            Some(code_symbol::CodeSymbol {
                display_name: "refresh".into(),
                ..code_symbol::CodeSymbol::default()
            })
        );
        assert_eq!(code_symbol::read(&serde_json::json!({})), None);
    }

    #[test]
    fn git_tip_round_trips() {
        let mut metadata = serde_json::json!({});
        git_tip::set(&mut metadata, "abc123");
        assert_eq!(git_tip::read(&metadata), Some("abc123"));
        assert_eq!(git_tip::read(&serde_json::json!({})), None);
    }

    #[test]
    fn is_import_round_trips_with_absent_fallback() {
        let mut metadata = serde_json::json!({});
        is_import::set(&mut metadata, true);
        assert_eq!(is_import::read(&metadata), Some(true));
        is_import::set(&mut metadata, false);
        assert_eq!(is_import::read(&metadata), Some(false));
        assert_eq!(is_import::read(&serde_json::json!({})), None);
    }

    #[test]
    fn leading_context_round_trips_null_and_missing_shapes() {
        let mut metadata = serde_json::json!({});
        leading_context::set(
            &mut metadata,
            Some(leading_context::LeadingContext {
                start_offset: 3,
                end_offset: 9,
                text: "// hi".into(),
            }),
        );
        assert_eq!(
            leading_context::read(&metadata),
            Some(leading_context::LeadingContext {
                start_offset: 3,
                end_offset: 9,
                text: "// hi".into(),
            })
        );

        leading_context::set(&mut metadata, None);
        assert!(metadata["leading_context"].is_null());
        assert_eq!(leading_context::read(&metadata), None);
        assert_eq!(leading_context::read(&serde_json::json!({})), None);
    }

    #[test]
    fn source_slices_round_trips_and_omits_absent_boundary() {
        let slices = vec![
            source_slices::SourceSlice {
                atom_hash: "h1".into(),
                start_offset: 0,
                end_offset: 10,
                boundary: None,
            },
            source_slices::SourceSlice {
                atom_hash: "h2".into(),
                start_offset: 10,
                end_offset: 20,
                boundary: Some("prose".into()),
            },
        ];
        let mut metadata = serde_json::json!({});
        source_slices::set(&mut metadata, slices);
        assert_eq!(
            metadata["source_slices"][0].as_object().unwrap().len(),
            3,
            "entries without a boundary must not grow a null boundary field"
        );
        assert_eq!(metadata["source_slices"][1]["boundary"], "prose");
        assert_eq!(
            source_slices::read(&metadata),
            metadata["source_slices"].as_array().unwrap().clone()
        );
        assert_eq!(
            source_slices::read(&serde_json::json!({})),
            Vec::<serde_json::Value>::new()
        );
    }

    #[test]
    fn timestamp_sets_only_when_present() {
        let mut metadata = serde_json::json!({});
        timestamp::set(&mut metadata, None);
        assert!(metadata.get("timestamp").is_none());
        timestamp::set(&mut metadata, Some(1_700_000_000));
        assert_eq!(timestamp::read(&metadata), Some(1_700_000_000));
        assert_eq!(timestamp::read(&serde_json::json!({})), None);
    }
}
