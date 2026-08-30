use super::Store;
use crate::core::{AnchorKind, ResolvedAnchor};
use rusqlite::params;

pub(super) fn ensure_anchor(
    transaction: &rusqlite::Transaction<'_>,
    kind: AnchorKind,
    value: &str,
) -> rusqlite::Result<i64> {
    transaction.execute(
        "INSERT INTO anchors(kind,value) VALUES (?1,?2)
         ON CONFLICT(kind,value) DO NOTHING",
        params![kind.as_str(), value],
    )?;
    transaction.query_row(
        "SELECT id FROM anchors WHERE kind=?1 AND value=?2",
        params![kind.as_str(), value],
        |row| row.get(0),
    )
}

impl Store {
    /// Resolved anchors for one unit, ordered by kind then value.
    pub fn anchors_for_unit(&self, unit_id: i64) -> rusqlite::Result<Vec<ResolvedAnchor>> {
        let mut statement = self.conn.prepare(
            "SELECT a.kind, a.value, ua.relationship
             FROM unit_anchors ua
             JOIN anchors a ON a.id=ua.anchor_id
             WHERE ua.unit_id=?1
             ORDER BY a.kind, a.value, ua.relationship",
        )?;
        let rows = statement.query_map(params![unit_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut output = Vec::new();
        for row in rows {
            let (kind, value, relationship) = row?;
            let Some(kind) = AnchorKind::parse(&kind) else {
                continue;
            };
            output.push(ResolvedAnchor {
                kind,
                value,
                relationship,
            });
        }
        Ok(output)
    }

    /// Units linked to one anchor, oldest first.
    pub fn units_for_anchor(
        &self,
        kind: &str,
        value: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<i64>> {
        let Some(kind) = AnchorKind::parse(kind) else {
            return Ok(Vec::new());
        };
        let sql = "SELECT DISTINCT ua.unit_id FROM unit_anchors ua
             JOIN anchors a ON a.id=ua.anchor_id
             JOIN retrieval_units u ON u.id=ua.unit_id
             JOIN sources s ON s.id=u.source_id
             WHERE a.kind=?1 AND a.value=?2
             ORDER BY ua.unit_id LIMIT ?3";
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(params![kind.as_str(), value, limit as i64], |row| {
            row.get(0)
        })?;
        rows.collect()
    }
}
