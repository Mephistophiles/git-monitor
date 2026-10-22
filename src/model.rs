use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub recipient: String,
    pub generated_at: u64,
    pub events: Vec<EventView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonBase {
    pub remote: String,
    pub branch: String,
    pub oid: String,
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
    #[serde(default)]
    pub comparison_base: Option<ComparisonBase>,
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
    #[serde(default)]
    pub continuation: Option<Continuation>,
    #[serde(default)]
    pub document: Option<crate::document::DiffDocument>,
    #[serde(default)]
    pub canonical_patch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Continuation {
    pub part: usize,
    pub total: usize,
    pub old_range: (usize, usize),
    pub new_range: (usize, usize),
    pub old_sha256: String,
    pub new_sha256: String,
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
            continuation: None,
            document: None,
            canonical_patch: None,
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        anyhow::ensure!(
            self.document.is_some() || self.engine == "metadata" || self.continuation.is_some(),
            "publishable text diff is missing its document"
        );
        if let Some(doc) = &self.document {
            anyhow::ensure!(
                self.path == doc.old.path
                    && self.path == doc.new.path
                    && self.old_start == doc.old.start_line
                    && self.new_start == doc.new.start_line,
                "presentation/source identity mismatch"
            );
            anyhow::ensure!(
                self.rows.is_empty() || !doc.hunks.is_empty(),
                "missing byte projection"
            );
            doc.reconstruct(
                &STANDARD.decode(&self.old_bytes)?,
                &STANDARD.decode(&self.new_bytes)?,
            )?;
        }
        Ok(())
    }
}
