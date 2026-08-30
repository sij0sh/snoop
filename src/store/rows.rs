use crate::core::{Repository, RetrievalUnit, Source, SourceId, SourceKind, UnitId, UnitKind};

pub(super) const UNIT_SELECT: &str = "u.id,u.source_id,s.kind,s.locator,u.kind,u.evidence_text,u.routing_text,u.token_count,u.content_hash,COALESCE(u.timestamp,s.modified_at),u.metadata";

pub(super) fn repository_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        root_path: row.get(0)?,
        content_version: row.get(1)?,
        metadata: serde_json::from_str(&row.get::<_, String>(2)?)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn retrieval_unit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RetrievalUnit> {
    Ok(RetrievalUnit {
        id: UnitId(row.get(0)?),
        source_id: SourceId(row.get(1)?),
        source_kind: SourceKind::parse(&row.get::<_, String>(2)?).unwrap_or(SourceKind::Text),
        locator: row.get(3)?,
        kind: UnitKind::parse(&row.get::<_, String>(4)?).unwrap_or(UnitKind::Prose),
        evidence_text: row.get(5)?,
        routing_text: row.get(6)?,
        token_count: row.get::<_, i64>(7)? as usize,
        content_hash: row.get(8)?,
        timestamp: row.get(9)?,
        metadata: serde_json::from_str(&row.get::<_, String>(10)?)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    Ok(Source {
        id: SourceId(row.get(0)?),
        kind: SourceKind::parse(&row.get::<_, String>(1)?).unwrap_or(SourceKind::Text),
        locator: row.get(2)?,
        content_hash: row.get(3)?,
        modified_at: row.get(4)?,
        metadata: serde_json::from_str(&row.get::<_, String>(5)?)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn match_expression(column: &str, query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect();
    (!terms.is_empty()).then(|| format!("{column}:({})", terms.join(" OR ")))
}

pub fn encode_f32(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for index in 0..a.len().min(b.len()) {
        dot += a[index] * b[index];
        norm_a += a[index] * a[index];
        norm_b += b[index] * b[index];
    }
    let denominator = norm_a.sqrt() * norm_b.sqrt();
    if denominator <= f32::EPSILON {
        0.0
    } else {
        dot / denominator
    }
}
