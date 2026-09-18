use super::rows::{decode_f32, encode_f32, match_expression, retrieval_unit_from_row, UNIT_SELECT};
use super::{LastIndexRun, Store, StoreStats};
use crate::core::RetrievalUnit;
use rusqlite::{params, Connection, OptionalExtension};

impl Store {
    pub fn unit_ids(&self) -> rusqlite::Result<Vec<i64>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM retrieval_units ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// Keyset page of units still missing a vector for (kind, model_version),
    /// ordered by unit id. Bounded by `limit` so callers never materialize a
    /// full backlog (audit fix-c6: peak embed-phase heap = one chunk, not N).
    pub fn units_missing_vectors_page(
        &self,
        kind: &str,
        model_version: &str,
        after_id: i64,
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, String)>> {
        let column = if kind == "routing" {
            "routing_text"
        } else {
            "evidence_text"
        };
        let sql = format!(
            "SELECT u.id,u.{column} FROM retrieval_units u
             WHERE u.id > ?1
             AND NOT EXISTS (SELECT 1 FROM vectors v WHERE v.unit_id=u.id
             AND v.kind=?2 AND v.model_version=?3) ORDER BY u.id LIMIT ?4"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(
            params![after_id, kind, model_version, limit as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        rows.collect()
    }
    pub fn put_vector(
        &self,
        unit_id: i64,
        kind: &str,
        model_version: &str,
        vector: &[f32],
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vectors(unit_id,kind,model_version,dimensions,vector)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                unit_id,
                kind,
                model_version,
                vector.len() as i64,
                encode_f32(vector)
            ],
        )?;
        Ok(())
    }

    pub fn get_vector(
        &self,
        unit_id: i64,
        kind: &str,
        model_version: &str,
    ) -> rusqlite::Result<Option<Vec<f32>>> {
        self.conn
            .query_row(
                "SELECT vector FROM vectors WHERE unit_id=?1 AND kind=?2 AND model_version=?3",
                params![unit_id, kind, model_version],
                |row| Ok(decode_f32(&row.get::<_, Vec<u8>>(0)?)),
            )
            .optional()
    }

    pub fn fts_search(
        &self,
        column: &str,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, f64)>> {
        self.fts_search_hindsight(column, query, limit, &["default", "history", "review"])
    }

    /// Lexical channel with Hindsight lifecycle filtering applied inside
    /// the candidate query, before the limit. Filtering after a top-N
    /// truncation would let ineligible retired memories occupy the window
    /// and starve eligible active records of fusion. Non-memory sources
    /// carry no visibility tier and always pass.
    pub fn fts_search_hindsight(
        &self,
        column: &str,
        query: &str,
        limit: usize,
        allowed: &[&str],
    ) -> rusqlite::Result<Vec<(i64, f64)>> {
        let Some(expression) = match_expression(column, query) else {
            return Ok(Vec::new());
        };
        let placeholders = vec!["?"; allowed.len()].join(",");
        let visibility_path = crate::metadata::hindsight::VISIBILITY_PATH;
        // All positional: match expression, one slot per allowed tier,
        // then the limit.
        let sql = format!(
            "SELECT units_fts.rowid,bm25(units_fts) AS score FROM units_fts
             JOIN retrieval_units u ON u.id=units_fts.rowid
             JOIN sources s ON s.id=u.source_id
             WHERE units_fts MATCH ?
             AND (s.kind != 'hindsight_memory'
                  OR COALESCE(json_extract(u.metadata,'{visibility_path}'),'default')
                     IN ({placeholders}))
             ORDER BY score,units_fts.rowid LIMIT ?"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let tier_params: Vec<String> = allowed.iter().map(|tier| (*tier).to_string()).collect();
        let limit_value = limit as i64;
        let mut ordered: Vec<&dyn rusqlite::ToSql> = vec![&expression];
        for tier in &tier_params {
            ordered.push(tier);
        }
        ordered.push(&limit_value);
        let rows = statement.query_map(rusqlite::params_from_iter(ordered), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    pub fn top_k_cosine(
        &self,
        kind: &str,
        model_version: &str,
        query: &[f32],
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, f32)>> {
        self.top_k_cosine_hindsight(
            kind,
            model_version,
            query,
            limit,
            &["default", "history", "review"],
        )
    }

    /// Vector channel with the same pre-truncation lifecycle filtering as
    /// [`Self::fts_search_hindsight`].
    pub fn top_k_cosine_hindsight(
        &self,
        kind: &str,
        model_version: &str,
        query: &[f32],
        limit: usize,
        allowed: &[&str],
    ) -> rusqlite::Result<Vec<(i64, f32)>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; allowed.len()].join(",");
        let visibility_path = crate::metadata::hindsight::VISIBILITY_PATH;
        // All positional: kind, model version, query vector, one slot per
        // allowed tier, then dimensions and limit.
        let sql = format!(
            "SELECT v.unit_id,vec_distance_cosine(v.vector,?) AS distance FROM vectors v
             JOIN retrieval_units u ON u.id=v.unit_id
             JOIN sources s ON s.id=u.source_id
             WHERE v.kind=?
             AND v.model_version=? AND v.dimensions=?
             AND (s.kind != 'hindsight_memory'
                  OR COALESCE(json_extract(u.metadata,'{visibility_path}'),'default')
                     IN ({placeholders}))
             ORDER BY distance ASC,v.unit_id ASC LIMIT ?"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let encoded = encode_f32(query);
        let dimensions = query.len() as i64;
        let limit_value = limit as i64;
        let tier_params: Vec<String> = allowed.iter().map(|tier| (*tier).to_string()).collect();
        // Textual `?` order: vector, kind, model, dimensions, tiers…, limit.
        let mut positional: Vec<&dyn rusqlite::ToSql> = vec![&encoded, &kind, &model_version];
        positional.push(&dimensions);
        for tier in &tier_params {
            positional.push(tier);
        }
        positional.push(&limit_value);
        let rows = statement.query_map(rusqlite::params_from_iter(positional), |row| {
            let distance: f32 = row.get(1)?;
            Ok((row.get(0)?, 1.0 - distance))
        })?;
        rows.collect()
    }

    pub fn unit_by_id(&self, unit_id: i64) -> rusqlite::Result<Option<RetrievalUnit>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {UNIT_SELECT} FROM retrieval_units u
                     JOIN sources s ON s.id=u.source_id WHERE u.id=?1"
                ),
                params![unit_id],
                retrieval_unit_from_row,
            )
            .optional()
    }

    pub fn units_for_source(&self, locator: &str) -> rusqlite::Result<Vec<RetrievalUnit>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {UNIT_SELECT} FROM retrieval_units u
             JOIN sources s ON s.id=u.source_id WHERE s.locator=?1
             ORDER BY u.id"
        ))?;
        let rows = statement.query_map(params![locator], retrieval_unit_from_row)?;
        rows.collect()
    }

    pub fn stats(&self) -> rusqlite::Result<StoreStats> {
        fn count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
        }
        let last_index_run = self
            .conn
            .query_row(
                "SELECT finished_at,duration_ms,changed_sources,unchanged_sources,embedded,status
                 FROM index_runs ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok(LastIndexRun {
                        finished_at: row.get(0)?,
                        duration_ms: row.get(1)?,
                        changed_sources: row.get(2)?,
                        unchanged_sources: row.get(3)?,
                        embedded: row.get(4)?,
                        status: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(StoreStats {
            sources: count(&self.conn, "sources")?,
            units: count(&self.conn, "retrieval_units")?,
            vectors: count(&self.conn, "vectors")?,
            index_runs: count(&self.conn, "index_runs")?,
            last_index_run,
        })
    }

    pub fn vector_models(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut statement = self.conn.prepare(
            "SELECT model_version, count(*) FROM vectors
             GROUP BY model_version ORDER BY model_version",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    #[cfg(test)]
    pub(super) fn connection(&self) -> &Connection {
        &self.conn
    }
}
