use anyhow::{bail, ensure, Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub storage: Storage,
    #[serde(default)]
    pub execution: Execution,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub diff: DiffConfig,
    #[serde(default)]
    pub limits: Limits,
    pub recipients: BTreeMap<String, Recipient>,
    pub repositories: BTreeMap<String, Repository>,
    #[serde(default)]
    pub watches: Vec<Watch>,
    #[serde(default)]
    pub maintenance: Maintenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Maintenance {
    pub cache_items: usize,
    pub history_days: u64,
    pub artifact_days: u64,
    pub temporary_hours: u64,
}
impl Default for Maintenance {
    fn default() -> Self {
        Self {
            cache_items: 10000,
            history_days: 90,
            artifact_days: 30,
            temporary_hours: 24,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Storage {
    pub repositories_dir: PathBuf,
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Execution {
    pub max_fetches: usize,
    pub max_fetches_per_host: usize,
    pub max_analyses: usize,
    pub git_timeout: String,
    pub bootstrap_timeout: String,
    pub shutdown_grace: String,
    /// Explicitly permitted textual Git function-boundary rules.
    #[serde(default)]
    pub diff_drivers: BTreeMap<String, String>,
    #[serde(default)]
    pub analysis_profile: crate::profile::AnalysisProfile,
}
impl Default for Execution {
    fn default() -> Self {
        Self {
            max_fetches: 4,
            max_fetches_per_host: 2,
            max_analyses: 4,
            git_timeout: "3m".into(),
            bootstrap_timeout: "30m".into(),
            shutdown_grace: "60s".into(),
            diff_drivers: BTreeMap::new(),
            analysis_profile: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub poll_every: String,
    pub digest_every: String,
    pub new_branch_mode: String,
    pub max_message_bytes: usize,
}
impl Default for Defaults {
    fn default() -> Self {
        Self {
            poll_every: "5m".into(),
            digest_every: "1h".into(),
            new_branch_mode: "summary".into(),
            max_message_bytes: 15_000_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffConfig {
    pub mode: String,
    pub layouts: Vec<String>,
    pub theme: String,
    pub line_numbers: bool,
    pub moved_lines: bool,
    pub line_algorithm: String,
    pub context_lines: usize,
}
impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
            layouts: vec!["unified".into(), "side-by-side".into()],
            theme: "light".into(),
            line_numbers: true,
            moved_lines: true,
            line_algorithm: "histogram".into(),
            context_lines: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub render_timeout: String,
    pub max_render_output_bytes: usize,
    pub max_blob_bytes: usize,
    pub max_patch_bytes: usize,
    pub max_job_output_bytes: usize,
    pub max_worker_address_space_bytes: u64,
    pub analysis_timeout: String,
    pub syntax_timeout: String,
    pub max_syntax_nodes: usize,
    pub max_syntax_candidates: usize,
    pub max_alignment_cells: usize,
    pub max_part_count: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            render_timeout: "30s".into(),
            max_render_output_bytes: 256_000_000,
            max_blob_bytes: 32_000_000,
            max_patch_bytes: 64_000_000,
            max_job_output_bytes: 128_000_000,
            max_worker_address_space_bytes: 1_073_741_824,
            analysis_timeout: "30s".into(),
            syntax_timeout: "5s".into(),
            max_syntax_nodes: 500_000,
            max_syntax_candidates: 1_000_000,
            max_alignment_cells: 1_000_000,
            max_part_count: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipient {
    pub to: Vec<String>,
    pub digest_every: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    #[serde(default = "default_object_format")]
    pub object_format: String,
    pub remotes: BTreeMap<String, Remote>,
}
fn default_object_format() -> String {
    "sha1".into()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub url: String,
    pub poll_every: Option<String>,
    /// Nonsecret identity of the credentials/visibility context. Rotate when access changes.
    #[serde(default)]
    pub access_context: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Watch {
    pub id: String,
    pub repository: String,
    pub remotes: Vec<String>,
    pub recipients: Vec<String>,
    pub branches: Branches,
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    pub function: Option<Function>,
    #[serde(default = "default_events")]
    pub events: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Branches {
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Function {
    pub path: String,
    #[serde(rename = "match")]
    pub expression: String,
}
fn default_events() -> Vec<String> {
    [
        "commits",
        "branch_created",
        "branch_deleted",
        "history_rewritten",
    ]
    .map(String::from)
    .to_vec()
}

pub fn duration(s: &str) -> Result<Duration> {
    humantime::parse_duration(s).with_context(|| format!("invalid duration {s:?}"))
}
pub fn globs(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(GlobBuilder::new(p).literal_separator(true).build()?);
    }
    Ok(b.build()?)
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}
impl Watch {
    pub fn branch_matches(&self, name: &str) -> bool {
        (self.branches.all || globs(&self.branches.include).is_ok_and(|g| g.is_match(name)))
            && !globs(&self.branches.exclude).is_ok_and(|g| g.is_match(name))
    }
    pub fn path_matches(&self, path: &str) -> bool {
        if let Some(f) = &self.function {
            return f.path == path;
        }
        self.paths
            .as_ref()
            .is_none_or(|ps| globs(ps).is_ok_and(|g| g.is_match(path)))
            && !globs(&self.exclude_paths).is_ok_and(|g| g.is_match(path))
    }
    pub fn selector_key(&self) -> String {
        let mut w = self.clone();
        w.id.clear();
        w.recipients.clear();
        w.remotes.clear();
        w.events.clear();
        w.branches = Branches {
            all: true,
            include: vec![],
            exclude: vec![],
        };
        if let Some(p) = &mut w.paths {
            p.sort();
            p.dedup();
        }
        w.exclude_paths.sort();
        crate::hash(serde_json::to_vec(&w).expect("watch serialization"))
    }
}
impl Config {
    pub fn freeze(&mut self) -> Result<()> {
        self.execution.analysis_profile =
            crate::profile::AnalysisProfile::freeze(&self.execution.diff_drivers)?;
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let mut c: Self = toml::from_str(&text)?;
        let base = std::fs::canonicalize(path)?
            .parent()
            .context("config directory")?
            .to_path_buf();
        for p in [&mut c.storage.repositories_dir, &mut c.storage.state_dir] {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.execution.max_fetches > 0
                && self.execution.max_fetches_per_host > 0
                && self.execution.max_analyses > 0,
            "concurrency limits must be positive"
        );
        for (name, expression) in &self.execution.diff_drivers {
            ensure!(
                valid_id(name) && !expression.is_empty() && !expression.contains('\0'),
                "invalid allowed diff driver"
            );
        }
        ensure!(self.version == 1, "unsupported configuration version");
        ensure!(
            self.defaults.new_branch_mode == "summary",
            "v0.1 supports new_branch_mode=summary"
        );
        ensure!(
            self.defaults.max_message_bytes >= 32768,
            "max_message_bytes must be at least 32768"
        );
        ensure!(
            ["text", "syntax", "auto"].contains(&self.diff.mode.as_str()),
            "invalid diff mode"
        );
        ensure!(
            self.diff.line_algorithm == "histogram",
            "v0.1 requires line_algorithm=histogram"
        );
        ensure!(
            !self.diff.layouts.is_empty()
                && self
                    .diff
                    .layouts
                    .iter()
                    .all(|x| ["unified", "side-by-side"].contains(&x.as_str())),
            "invalid layouts"
        );
        ensure!(
            ["light", "dark"].contains(&self.diff.theme.as_str()),
            "invalid theme"
        );
        for s in [
            &self.execution.git_timeout,
            &self.execution.bootstrap_timeout,
            &self.execution.shutdown_grace,
            &self.defaults.poll_every,
            &self.defaults.digest_every,
            &self.limits.analysis_timeout,
            &self.limits.syntax_timeout,
            &self.limits.render_timeout,
        ] {
            duration(s)?;
        }
        ensure!(
            duration(&self.defaults.poll_every)?.as_secs() > 0,
            "poll interval must be positive"
        );
        ensure!(
            self.limits.max_blob_bytes > 0
                && self.limits.max_render_output_bytes > 0
                && self.limits.max_patch_bytes > 0
                && self.limits.max_job_output_bytes > 0
                && self.limits.max_part_count > 0,
            "limits must be positive"
        );
        for (id, r) in &self.repositories {
            ensure!(valid_id(id), "invalid repository ID");
            ensure!(
                ["sha1", "sha256"].contains(&r.object_format.as_str()),
                "unsupported repository object format"
            );
            for (id, r) in &r.remotes {
                ensure!(valid_id(id), "invalid remote ID");
                ensure!(
                    !r.url.is_empty() && !r.url.starts_with('-'),
                    "invalid remote URL"
                );
                if let Some(s) = &r.poll_every {
                    ensure!(!duration(s)?.is_zero(), "poll interval must be positive");
                }
            }
        }
        for (id, r) in &self.recipients {
            ensure!(valid_id(id) && !r.to.is_empty(), "invalid recipient");
            for address in &r.to {
                address
                    .parse::<lettre::message::Mailbox>()
                    .context("invalid recipient address")?;
            }
            if let Some(s) = &r.digest_every {
                duration(s)?;
            }
        }
        let mut ids = std::collections::HashSet::new();
        for w in &self.watches {
            ensure!(
                valid_id(&w.id) && ids.insert(&w.id),
                "invalid or duplicate watch ID"
            );
            let repo = self
                .repositories
                .get(&w.repository)
                .context("unknown watch repository")?;
            ensure!(
                !w.remotes.is_empty() && w.remotes.iter().all(|r| repo.remotes.contains_key(r)),
                "unknown or empty watch remotes"
            );
            ensure!(
                !w.recipients.is_empty()
                    && w.recipients.iter().all(|r| self.recipients.contains_key(r)),
                "unknown or empty watch recipients"
            );
            ensure!(
                w.branches.all != !w.branches.include.is_empty(),
                "use all=true or include branches"
            );
            ensure!(
                w.function.is_none() || (w.paths.is_none() && w.exclude_paths.is_empty()),
                "function and paths are mutually exclusive"
            );
            for ps in [&w.branches.include, &w.branches.exclude, &w.exclude_paths] {
                globs(ps)?;
            }
            if let Some(ps) = &w.paths {
                ensure!(!ps.is_empty(), "empty path selector");
                globs(ps)?;
            }
            if let Some(f) = &w.function {
                ensure!(
                    !f.path.is_empty()
                        && !f.path.starts_with('/')
                        && !f.path.split('/').any(|x| x == "..")
                        && !f.expression.is_empty(),
                    "invalid function selector"
                );
            }
            if w.events.iter().any(|e| !default_events().contains(e)) {
                bail!("unknown watch event");
            }
        }
        Ok(())
    }
}
