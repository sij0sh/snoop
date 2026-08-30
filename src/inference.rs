use std::error::Error;
use std::time::Duration;

pub type EmbedResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Classified embed failure. Transport faults, timeouts, and 5xx/429/408
/// responses are transient; other HTTP client errors and payload mismatches
/// are permanent and are never retried.
#[derive(Debug)]
pub enum EmbedError {
    Transient(String),
    Permanent(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(message) | Self::Permanent(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for EmbedError {}

impl EmbedError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

pub trait Embedder: Send + Sync {
    fn model_version(&self) -> &str;
    fn embed_query(&self, text: &str) -> EmbedResult<Vec<f32>>;
    fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>>;
    /// Same as `embed_documents`, but every HTTP batch must finish inside
    /// `timeout`. Implementations that cannot bound a batch may ignore the
    /// timeout; the server-side embedder (llama.cpp) honors it.
    fn embed_documents_bounded(
        &self,
        texts: &[String],
        timeout: Duration,
    ) -> EmbedResult<Vec<Vec<f32>>> {
        let _ = timeout;
        self.embed_documents(texts)
    }
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

    fn request(&self, texts: &[String], timeout: Duration) -> EmbedResult<Vec<Vec<f32>>> {
        let response = ureq::post(&format!("{}/v1/embeddings", self.base_url))
            .timeout(timeout)
            .send_json(serde_json::json!({"input": texts, "model": "embed"}))
            .map_err(|error| match error {
                // 4xx client errors other than 408/429 will not improve on
                // retry; transport faults and server-side pressure will.
                ureq::Error::Status(code, _)
                    if (400..500).contains(&code) && code != 408 && code != 429 =>
                {
                    EmbedError::Permanent(format!("embeddings request failed: {error}"))
                }
                _ => EmbedError::Transient(format!("embeddings request failed: {error}")),
            })?;
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
            .map_err(|error| EmbedError::Permanent(format!("embeddings decode failed: {error}")))?;
        if reply.data.len() != texts.len() {
            return Err(EmbedError::Permanent(format!(
                "embeddings count mismatch: sent {} got {}",
                texts.len(),
                reply.data.len()
            ))
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
        self.request(&[text.to_string()], Duration::from_secs(300))?
            .pop()
            .ok_or_else(|| "embedding server returned no vector".into())
    }

    fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        self.embed_documents_bounded(texts, Duration::from_secs(300))
    }

    fn embed_documents_bounded(
        &self,
        texts: &[String],
        timeout: Duration,
    ) -> EmbedResult<Vec<Vec<f32>>> {
        let mut output = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.batch.max(1)) {
            output.extend(self.request(chunk, timeout)?);
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
