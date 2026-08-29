use std::error::Error;
use std::time::Duration;

pub type EmbedResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub trait Embedder: Send + Sync {
    fn model_version(&self) -> &str;
    fn embed_query(&self, text: &str) -> EmbedResult<Vec<f32>>;
    fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>>;
}

pub struct LlamaServerEmbedder {
    base_url: String,
    version: String,
    batch: usize,
}

impl LlamaServerEmbedder {
    pub fn new(base_url: &str, model_version: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            version: model_version.to_string(),
            batch: 32,
        }
    }

    fn request(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        let response = ureq::post(&format!("{}/v1/embeddings", self.base_url))
            .timeout(Duration::from_secs(300))
            .send_json(serde_json::json!({"input": texts, "model": "embed"}))
            .map_err(|error| format!("embeddings request failed: {error}"))?;
        #[derive(serde::Deserialize)]
        struct Reply {
            data: Vec<Item>,
        }
        #[derive(serde::Deserialize)]
        struct Item {
            embedding: Vec<f32>,
        }
        let reply: Reply = response
            .into_json()
            .map_err(|error| format!("embeddings decode failed: {error}"))?;
        if reply.data.len() != texts.len() {
            return Err(format!(
                "embeddings count mismatch: sent {} got {}",
                texts.len(),
                reply.data.len()
            )
            .into());
        }
        Ok(reply.data.into_iter().map(|item| item.embedding).collect())
    }
}

impl Embedder for LlamaServerEmbedder {
    fn model_version(&self) -> &str {
        &self.version
    }

    fn embed_query(&self, text: &str) -> EmbedResult<Vec<f32>> {
        self.request(&[text.to_string()])?
            .pop()
            .ok_or_else(|| "embedding server returned no vector".into())
    }

    fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        let mut output = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.batch.max(1)) {
            output.extend(self.request(chunk)?);
        }
        Ok(output)
    }
}

pub const MOCK_MODEL_VERSION: &str = "mock-v1";

pub struct MockEmbedder {
    version: String,
}

const MOCK_DIMS: usize = 10;
const MOCK_KEYWORDS: &[(&str, usize)] = &[
    ("retry", 0),
    ("backoff", 0),
    ("cache", 1),
    ("database", 2),
    ("sqlite", 2),
    ("history", 3),
    ("commit", 3),
    ("auth", 4),
    ("authentication", 4),
    ("token", 5),
    ("session", 6),
    ("markdown", 7),
    ("parser", 8),
    ("route", 9),
];

impl MockEmbedder {
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_string(),
        }
    }

    fn vector_for(text: &str) -> Vec<f32> {
        let lower = text.to_lowercase();
        let mut vector = vec![0.0; MOCK_DIMS];
        let mut hit = false;
        for (keyword, dimension) in MOCK_KEYWORDS {
            if lower.contains(keyword) {
                vector[*dimension] += 1.0;
                hit = true;
            }
        }
        if !hit {
            let hash = blake3::hash(text.as_bytes());
            for (index, value) in vector.iter_mut().enumerate() {
                *value = hash.as_bytes()[index] as f32 / 255.0;
            }
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }
}

impl Embedder for MockEmbedder {
    fn model_version(&self) -> &str {
        &self.version
    }

    fn embed_query(&self, text: &str) -> EmbedResult<Vec<f32>> {
        Ok(Self::vector_for(text))
    }

    fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| Self::vector_for(text)).collect())
    }
}
