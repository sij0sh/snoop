use std::collections::HashSet;

#[derive(Debug, Clone, Copy)]
pub struct QueryChannels {
    pub(super) evidence_lexical: bool,
    pub(super) evidence_vector: bool,
    pub(super) routing_lexical: bool,
    pub(super) routing_vector: bool,
}

impl QueryChannels {
    pub const fn evidence_only() -> Self {
        Self {
            evidence_lexical: true,
            evidence_vector: true,
            routing_lexical: false,
            routing_vector: false,
        }
    }

    pub const fn evidence_lexical_only() -> Self {
        Self {
            evidence_lexical: true,
            evidence_vector: false,
            routing_lexical: false,
            routing_vector: false,
        }
    }

    pub fn for_embedder(embedder: Option<&dyn crate::inference::Embedder>) -> Self {
        match embedder {
            Some(_) => Self {
                evidence_lexical: true,
                evidence_vector: true,
                routing_lexical: true,
                routing_vector: true,
            },
            None => Self {
                evidence_lexical: true,
                evidence_vector: false,
                routing_lexical: true,
                routing_vector: false,
            },
        }
    }

    pub fn has_vector_channels(&self) -> bool {
        self.evidence_vector || self.routing_vector
    }
}

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub channels: QueryChannels,
    pub top_n: usize,
    pub max_tokens: usize,
    pub diagnostics: bool,
    pub exclude_unit_ids: HashSet<i64>,
    pub now: i64,
    pub max_per_source: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            channels: QueryChannels::for_embedder(None),
            top_n: 25,
            max_tokens: 6_000,
            diagnostics: false,
            exclude_unit_ids: HashSet::new(),
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64),
            max_per_source: 3,
        }
    }
}
