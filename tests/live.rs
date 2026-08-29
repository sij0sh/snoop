use snoop::inference::{Embedder, LlamaServerEmbedder};

#[test]
#[ignore = "requires a local llama-server embedding endpoint"]
fn llama_server_embedding_adapter_returns_a_vector() {
    let url =
        std::env::var("SNOOP_EMBED_URL").unwrap_or_else(|_| "http://127.0.0.1:8097".to_string());
    let version = std::env::var("SNOOP_EMBED_VERSION")
        .unwrap_or_else(|_| "Qwen3-Embedding-0.6B-Q8_0".to_string());
    let embedder = LlamaServerEmbedder::new(&url, &version);
    let vector = embedder.embed_query("repository context").unwrap();
    assert!(!vector.is_empty());
}
