use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct SourceId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Code,
    Markdown,
    Text,
    GitCommit,
    AgentSession,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::GitCommit => "git_commit",
            Self::AgentSession => "agent_session",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "code" => Some(Self::Code),
            "markdown" => Some(Self::Markdown),
            "text" => Some(Self::Text),
            "git_commit" => Some(Self::GitCommit),
            "agent_session" => Some(Self::AgentSession),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub root_path: String,
    pub content_version: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    pub locator: String,
    pub content_hash: String,
    pub modified_at: Option<i64>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtomKind {
    Document,
    Heading,
    Paragraph,
    List,
    ListItem,
    BlockQuote,
    CodeBlock,
    File,
    Module,
    Class,
    Function,
    Declaration,
    Comment,
    Commit,
    FileChange,
    DiffHunk,
    Episode,
}

impl AtomKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Heading => "heading",
            Self::Paragraph => "paragraph",
            Self::List => "list",
            Self::ListItem => "list_item",
            Self::BlockQuote => "block_quote",
            Self::CodeBlock => "code_block",
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Function => "function",
            Self::Declaration => "declaration",
            Self::Comment => "comment",
            Self::Commit => "commit",
            Self::FileChange => "file_change",
            Self::DiffHunk => "diff_hunk",
            Self::Episode => "episode",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "document" => Some(Self::Document),
            "heading" => Some(Self::Heading),
            "paragraph" => Some(Self::Paragraph),
            "list" => Some(Self::List),
            "list_item" => Some(Self::ListItem),
            "block_quote" => Some(Self::BlockQuote),
            "code_block" => Some(Self::CodeBlock),
            "file" => Some(Self::File),
            "module" => Some(Self::Module),
            "class" => Some(Self::Class),
            "function" => Some(Self::Function),
            "declaration" => Some(Self::Declaration),
            "comment" => Some(Self::Comment),
            "commit" => Some(Self::Commit),
            "file_change" => Some(Self::FileChange),
            "diff_hunk" => Some(Self::DiffHunk),
            "episode" => Some(Self::Episode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedAtom {
    pub kind: AtomKind,
    pub parent_index: Option<usize>,
    pub ordinal: u32,
    pub start_offset: usize,
    pub end_offset: usize,
    pub text: String,
    pub breadcrumb: String,
    pub content_hash: String,
    pub metadata: Value,
}

impl ParsedAtom {
    pub fn content_hash_of(kind: AtomKind, breadcrumb: &str, text: &str) -> String {
        hash_segments(&[breadcrumb, kind.as_str(), text])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitKind {
    Prose,
    Code,
    Git,
    Episode,
}

impl UnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Code => "code",
            Self::Git => "git",
            Self::Episode => "episode",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "prose" => Some(Self::Prose),
            "code" => Some(Self::Code),
            "git" => Some(Self::Git),
            "episode" => Some(Self::Episode),
            // Legacy rows written by the retired segmentation policy.
            "episode_segment" => Some(Self::Episode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AnchorKind {
    File,
    Symbol,
    Commit,
    Session,
}

impl AnchorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Symbol => "symbol",
            Self::Commit => "commit",
            Self::Session => "session",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "symbol" => Some(Self::Symbol),
            "commit" => Some(Self::Commit),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAnchor {
    pub kind: AnchorKind,
    pub value: String,
    pub relationship: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltAnchor {
    pub kind: AnchorKind,
    pub value: String,
    pub relationship: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltUnit {
    pub kind: UnitKind,
    pub evidence_text: String,
    pub routing_text: String,
    pub token_count: usize,
    pub content_hash: String,
    pub metadata: Value,
    pub anchors: Vec<BuiltAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalUnit {
    pub id: UnitId,
    pub source_id: SourceId,
    pub source_kind: SourceKind,
    pub locator: String,
    pub kind: UnitKind,
    pub evidence_text: String,
    pub routing_text: String,
    pub token_count: usize,
    pub content_hash: String,
    pub timestamp: Option<i64>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SelectionReason {
    EvidenceLexicalRank(u32),
    EvidenceVectorRank(u32),
    RoutingLexicalRank(u32),
    RoutingVectorRank(u32),
    RrfRank(u32),
    AnchorExpansion(String, String, i64),
    RoleAware(String, bool),
}

/// Lean item rendered into every packet: no provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub source_kind: SourceKind,
    pub evidence_text: String,
    pub source_locator: String,
    pub timestamp: Option<i64>,
}

/// Per-item provenance, built only for explain requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDiagnostics {
    pub unit_id: UnitId,
    pub source_slices: Vec<Value>,
    pub anchors: Vec<ResolvedAnchor>,
    pub selected_because: Vec<SelectionReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPacket {
    pub query: String,
    pub items: Vec<ContextItem>,
    pub token_count: usize,
    pub budget: usize,
}

pub fn hash_segments(segments: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for segment in segments {
        hasher.update(segment.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}
