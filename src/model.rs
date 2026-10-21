use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub recipient: String,
    pub generated_at: u64,
    pub events: Vec<EventView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventView {
    pub id: i64,
    pub repository: String,
    pub remote: String,
    pub branch: String,
    pub kind: String,
    pub old: Option<String>,
    pub new: Option<String>,
    pub observed_at: u64,
    pub watches: Vec<String>,
    pub commits: Vec<CommitView>,
    pub outgoing: Vec<String>,
    pub files: Vec<FileDiff>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitView {
    pub oid: String,
    pub subject: String,
    pub author: String,
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub kind: String,
    pub engine: String,
    pub note: String,
    pub rows: Vec<Row>,
    pub old_bytes: String,
    pub new_bytes: String,
    pub old_start: usize,
    pub new_start: usize,
    pub language: String,
    pub syntax: Option<SyntaxSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyntaxSummary {
    pub unchanged_subtrees: usize,
    pub deleted_tokens: usize,
    pub inserted_tokens: usize,
    pub text_only_change: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub kind: String,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub left: Vec<Span>,
    pub right: Vec<Span>,
    pub moved: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub changed: bool,
    #[serde(default)]
    pub syntax: String,
}

impl FileDiff {
    pub fn diagnostic(path: &str, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: "oversized_item".into(),
            engine: "metadata".into(),
            note: reason.into(),
            rows: vec![],
            old_bytes: String::new(),
            new_bytes: String::new(),
            old_start: 1,
            new_start: 1,
            language: String::new(),
            syntax: None,
        }
    }
}
