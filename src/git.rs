use crate::{
    config::{duration, Execution, Function, Limits},
    process::{self, Shutdown},
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone)]
pub struct Git {
    pub path: PathBuf,
    pub execution: Execution,
    pub limits: Limits,
    pub shutdown: Shutdown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Changed {
    pub path: String,
    pub old_blob: Option<String>,
    pub new_blob: Option<String>,
    pub old_mode: String,
    pub new_mode: String,
}
#[derive(Debug)]
pub struct Selection {
    pub bytes: Vec<u8>,
    pub start: usize,
}

impl Git {
    fn command(&self) -> Command {
        self.command_for(false)
    }
    fn command_for(&self, network: bool) -> Command {
        let mut c = Command::new("git");
        c.arg("--no-pager")
            .arg("--literal-pathspecs")
            .arg("-C")
            .arg(&self.path)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "diff.external=",
                "-c",
                "diff.renames=false",
                "-c",
                "color.ui=false",
            ])
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        if !network {
            c.env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .env_remove("GIT_CONFIG_COUNT")
                .env_remove("GIT_DIFF_OPTS")
                .env_remove("GIT_EXTERNAL_DIFF")
                .env_remove("GIT_ATTR_SOURCE");
        }
        for (driver, expression) in &self.execution.diff_drivers {
            c.args(["-c", &format!("diff.{driver}.xfuncname={expression}")]);
        }
        c
    }
    pub fn raw(&self, args: &[&str]) -> Result<Vec<u8>> {
        let out = process::run(
            self.command().args(args),
            duration(&self.execution.git_timeout)?,
            self.limits.max_patch_bytes,
            &self.shutdown,
            duration(&self.execution.shutdown_grace)?,
            None,
        )?;
        ensure!(
            out.code == 0,
            "Git operation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(out.stdout)
    }
    pub fn text(&self, args: &[&str]) -> Result<String> {
        String::from_utf8(self.raw(args)?).context("non-UTF-8 Git metadata is unsupported in v0.1")
    }
    pub fn raw_input(&self, args: &[&str], input: Vec<u8>) -> Result<Vec<u8>> {
        let out = process::run_with_input(
            self.command().args(args),
            input,
            duration(&self.execution.git_timeout)?,
            self.limits.max_patch_bytes,
            &self.shutdown,
            duration(&self.execution.shutdown_grace)?,
        )?;
        ensure!(
            out.code == 0,
            "Git operation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(out.stdout)
    }
    pub fn init(&self) -> Result<()> {
        self.init_format("sha1")
    }
    pub fn init_format(&self, format: &str) -> Result<()> {
        if !self.path.exists() {
            std::fs::create_dir_all(self.path.parent().context("repository parent")?)?;
            let mut c = Command::new("git");
            c.args([
                "init",
                "--bare",
                "--quiet",
                &format!("--object-format={format}"),
            ])
            .arg(&self.path);
            let out = process::run(
                &mut c,
                duration(&self.execution.git_timeout)?,
                65536,
                &self.shutdown,
                duration(&self.execution.shutdown_grace)?,
                None,
            )?;
            ensure!(out.code == 0, "git init failed");
        }
        ensure!(
            self.text(&["rev-parse", "--show-object-format"])?.trim() == format,
            "managed repository object format differs from configuration"
        );
        self.raw(&["config", "gc.auto", "0"])?;
        self.raw(&["config", "maintenance.auto", "false"])?;
        Ok(())
    }
    pub fn configure_remote(&self, remote: &str, url: &str) -> Result<()> {
        self.raw(&["config", &format!("remote.{remote}.url"), url])?;
        Ok(())
    }
    pub fn fetch(&self, remote: &str, bootstrap: bool) -> Result<()> {
        let spec = format!("+refs/heads/*:refs/remotes/{remote}/*");
        self.network(
            &[
                "fetch",
                "--atomic",
                "--prune",
                "--no-prune-tags",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-auto-maintenance",
                "--no-write-fetch-head",
                "--porcelain",
                "--refmap=",
                remote,
                &spec,
            ],
            None,
            remote,
            bootstrap,
        )?;
        Ok(())
    }
    fn network(
        &self,
        args: &[&str],
        input: Option<Vec<u8>>,
        remote: &str,
        bootstrap: bool,
    ) -> Result<Vec<u8>> {
        let mut c = self.command_for(true);
        c.args(args);
        let timeout = if bootstrap {
            &self.execution.bootstrap_timeout
        } else {
            &self.execution.git_timeout
        };
        let out = if let Some(input) = input {
            process::run_with_input(
                &mut c,
                input,
                duration(timeout)?,
                self.limits.max_patch_bytes,
                &self.shutdown,
                duration(&self.execution.shutdown_grace)?,
            )?
        } else {
            process::run(
                &mut c,
                duration(timeout)?,
                self.limits.max_patch_bytes,
                &self.shutdown,
                duration(&self.execution.shutdown_grace)?,
                None,
            )?
        };
        // Remote stderr can contain credentials; never copy it into status or logs.
        ensure!(
            out.code == 0,
            "remote operation failed for {remote} (exit {}); check access and Git capabilities",
            out.code
        );
        Ok(out.stdout)
    }
    pub fn discover(&self, remote: &str, bootstrap: bool) -> Result<BTreeMap<String, String>> {
        let bytes = self.network(
            &["ls-remote", "--heads", "--refs", remote],
            None,
            remote,
            bootstrap,
        )?;
        let text = String::from_utf8(bytes).context("non-UTF-8 branch names are unsupported")?;
        let mut refs = BTreeMap::new();
        for line in text.lines() {
            let (oid, name) = line.split_once('\t').context("invalid discovery record")?;
            ensure!(
                [40, 64].contains(&oid.len()) && oid.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid discovery OID"
            );
            let name = name
                .strip_prefix("refs/heads/")
                .context("unexpected discovery namespace")?;
            ensure!(
                !name.is_empty()
                    && !name
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()),
                "invalid discovery ref"
            );
            refs.insert(name.into(), oid.into());
        }
        Ok(refs)
    }
    fn missing_commits(&self, refs: &BTreeMap<String, String>) -> Result<BTreeSet<String>> {
        let oids: BTreeSet<_> = refs.values().cloned().collect();
        if oids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let input = oids
            .iter()
            .map(|oid| format!("{oid}\n"))
            .collect::<String>();
        let output = String::from_utf8(self.raw_input(
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            input.into_bytes(),
        )?)?;
        ensure!(
            output.lines().count() == oids.len(),
            "incomplete object availability response"
        );
        let mut missing = BTreeSet::new();
        for (oid, line) in oids.iter().zip(output.lines()) {
            let (actual, kind) = line
                .split_once(' ')
                .context("invalid object availability response")?;
            ensure!(actual == oid, "unexpected object availability response");
            match kind {
                "commit" => {}
                "missing" => {
                    missing.insert(oid.clone());
                }
                _ => bail!("branch tip is not a commit"),
            }
        }
        Ok(missing)
    }
    pub fn download_selected(
        &self,
        remote: &str,
        staging: &str,
        desired: &BTreeMap<String, String>,
        bootstrap: bool,
    ) -> Result<bool> {
        let missing = self.missing_commits(desired)?;
        let mut needed = BTreeMap::new();
        for (name, oid) in desired {
            if missing.contains(oid) {
                needed.entry(oid).or_insert(name);
            }
        }
        if !needed.is_empty() {
            let input = needed
                .into_values()
                .map(|name| format!("+refs/heads/{name}:{staging}{}\n", crate::hash(name)))
                .collect::<String>();
            self.network(
                &[
                    "fetch",
                    "--atomic",
                    "--no-prune",
                    "--no-prune-tags",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-auto-maintenance",
                    "--no-write-fetch-head",
                    "--porcelain",
                    "--refmap=",
                    "--stdin",
                    remote,
                ],
                Some(input.into_bytes()),
                remote,
                bootstrap,
            )?;
        }
        Ok(self.missing_commits(desired)?.is_empty())
    }
    pub fn ref_oid(&self, name: &str) -> Result<Option<String>> {
        let refs = self.text(&["for-each-ref", "--format=%(refname) %(objectname)", name])?;
        Ok(refs
            .lines()
            .filter_map(|line| line.split_once(' '))
            .find_map(|(r, oid)| (r == name).then(|| oid.to_owned())))
    }
    pub fn write_manifest(&self, bytes: Vec<u8>) -> Result<String> {
        Ok(
            String::from_utf8(self.raw_input(&["hash-object", "-w", "--stdin"], bytes)?)?
                .trim()
                .into(),
        )
    }
    pub fn publish_selected(
        &self,
        remote: &str,
        old: &BTreeMap<String, String>,
        desired: &BTreeMap<String, String>,
        marker_ref: &str,
        marker: &str,
        previous_marker: Option<&str>,
    ) -> Result<()> {
        let mut input = String::from("start\n");
        for (name, oid) in old {
            if !desired.contains_key(name) {
                input.push_str(&format!("delete refs/remotes/{remote}/{name} {oid}\n"));
            }
        }
        for (branch, oid) in desired {
            let name = format!("refs/remotes/{remote}/{branch}");
            if let Some(previous) = old.get(branch) {
                input.push_str(&format!("update {name} {oid} {previous}\n"));
            } else {
                input.push_str(&format!("create {name} {oid}\n"));
            }
        }
        if let Some(previous) = previous_marker {
            input.push_str(&format!("update {marker_ref} {marker} {previous}\n"));
        } else {
            input.push_str(&format!("create {marker_ref} {marker}\n"));
        }
        input.push_str("prepare\ncommit\n");
        self.raw_input(&["update-ref", "--no-deref", "--stdin"], input.into_bytes())?;
        Ok(())
    }
    pub fn cleanup_staging(&self, prefix: &str) -> Result<()> {
        let refs = self.text(&["for-each-ref", "--format=%(refname) %(objectname)", prefix])?;
        if refs.is_empty() {
            return Ok(());
        }
        let mut input = String::from("start\n");
        for line in refs.lines() {
            let (name, oid) = line.split_once(' ').context("invalid staging ref")?;
            ensure!(name.starts_with(prefix), "unexpected staging namespace");
            input.push_str(&format!("delete {name} {oid}\n"));
        }
        input.push_str("prepare\ncommit\n");
        self.raw_input(&["update-ref", "--no-deref", "--stdin"], input.into_bytes())?;
        Ok(())
    }
    pub fn refs(&self, remote: &str) -> Result<BTreeMap<String, String>> {
        let prefix = format!("refs/remotes/{remote}/");
        let text = self.text(&["for-each-ref", "--format=%(refname) %(objectname)", &prefix])?;
        let mut refs = BTreeMap::new();
        for l in text.lines() {
            let (name, oid) = l.split_once(' ').context("invalid ref record")?;
            refs.insert(
                name.strip_prefix(&prefix).context("ref namespace")?.into(),
                oid.into(),
            );
        }
        Ok(refs)
    }
    pub fn pin(&self, oid: &str) -> Result<()> {
        self.raw(&["update-ref", &format!("refs/git-monitor/pins/{oid}"), oid])?;
        Ok(())
    }
    pub fn ancestor(&self, a: &str, b: &str) -> Result<bool> {
        let out = process::run(
            self.command().args(["merge-base", "--is-ancestor", a, b]),
            duration(&self.execution.git_timeout)?,
            65536,
            &self.shutdown,
            duration(&self.execution.shutdown_grace)?,
            None,
        )?;
        ensure!(
            out.code == 0 || out.code == 1,
            "cannot inspect commit ancestry"
        );
        Ok(out.code == 0)
    }
    pub fn commits(&self, a: &str, b: &str) -> Result<Vec<String>> {
        Ok(self
            .text(&[
                "rev-list",
                "--reverse",
                "--topo-order",
                &format!("{a}..{b}"),
            ])?
            .lines()
            .map(str::to_owned)
            .collect())
    }
    pub fn parent(&self, commit: &str) -> Result<Option<String>> {
        Ok(self
            .text(&["show", "-s", "--format=%P", commit])?
            .split_whitespace()
            .next()
            .map(str::to_owned))
    }
    pub fn metadata(&self, oid: &str) -> Result<(String, String)> {
        let b = self.raw(&["show", "-s", "--format=%s%x00%an", oid])?;
        let separator = b
            .iter()
            .position(|x| *x == 0)
            .context("invalid commit metadata")?;
        let (subject, author) = (&b[..separator], &b[separator + 1..]);
        Ok((
            String::from_utf8_lossy(subject).into(),
            String::from_utf8_lossy(author).trim().into(),
        ))
    }
    pub fn changed(&self, old: Option<&str>, new: &str) -> Result<Vec<Changed>> {
        let bytes = if let Some(a) = old {
            self.raw(&[
                "diff",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                a,
                new,
            ])?
        } else {
            self.raw(&[
                "show",
                "--format=",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-renames",
                "--diff-merges=first-parent",
                "--no-ext-diff",
                "--no-textconv",
                new,
            ])?
        };
        let mut fields = bytes.split(|b| *b == 0).filter(|x| !x.is_empty());
        let mut result = Vec::new();
        while let Some(meta) = fields.next() {
            let m = std::str::from_utf8(meta)?.trim_start_matches('\n');
            ensure!(m.starts_with(':'), "invalid raw diff metadata");
            let f: Vec<_> = m[1..].split_whitespace().collect();
            ensure!(f.len() == 5, "invalid raw diff fields");
            let path = String::from_utf8(fields.next().context("missing raw path")?.to_vec())
                .context("non-UTF-8 paths are unsupported in v0.1")?;
            let oid = |s: &str| {
                if s.bytes().all(|x| x == b'0') {
                    None
                } else {
                    Some(s.to_owned())
                }
            };
            result.push(Changed {
                path,
                old_blob: oid(f[2]),
                new_blob: oid(f[3]),
                old_mode: f[0].into(),
                new_mode: f[1].into(),
            });
        }
        Ok(result)
    }
    pub fn show_patch(&self, commit: &str, path: &str, context: usize) -> Result<Vec<u8>> {
        self.raw(&[
            "show",
            "--format=",
            "--patch",
            "--no-color",
            "--no-renames",
            "--diff-merges=first-parent",
            "--no-ext-diff",
            "--no-textconv",
            "--diff-algorithm=histogram",
            &format!("--unified={context}"),
            commit,
            "--",
            path,
        ])
    }
    pub fn blob(&self, oid: Option<&str>) -> Result<Vec<u8>> {
        let Some(oid) = oid else {
            return Ok(vec![]);
        };
        let size: usize = self.text(&["cat-file", "-s", oid])?.trim().parse()?;
        ensure!(
            size <= self.limits.max_blob_bytes,
            "blob budget exceeded ({size} bytes)"
        );
        self.raw(&["cat-file", "blob", oid])
    }
    pub fn tree(&self, rev: &str) -> Result<String> {
        Ok(self
            .text(&["rev-parse", &format!("{rev}^{{tree}}")])?
            .trim()
            .into())
    }
    pub fn tree_blob(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let spec = format!("{rev}:{path}");
        let out = process::run(
            self.command().args(["rev-parse", "--verify", &spec]),
            duration(&self.execution.git_timeout)?,
            65536,
            &self.shutdown,
            duration(&self.execution.shutdown_grace)?,
            None,
        )?;
        Ok((out.code == 0).then(|| String::from_utf8_lossy(&out.stdout).trim().into()))
    }
    pub fn select(&self, rev: Option<&str>, f: &Function) -> Result<Option<Selection>> {
        let Some(rev) = rev else {
            return Ok(None);
        };
        let Some(oid) = self.tree_blob(rev, &f.path)? else {
            return Ok(None);
        };
        let bytes = self.blob(Some(&oid))?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let tree = self.tree(rev)?;
        let mut c = self.command();
        c.env("GIT_AUTHOR_NAME", "Git Monitor")
            .env("GIT_AUTHOR_EMAIL", "git-monitor@localhost")
            .env("GIT_COMMITTER_NAME", "Git Monitor")
            .env("GIT_COMMITTER_EMAIL", "git-monitor@localhost")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z");
        c.args(["commit-tree", &tree, "-m", "git-monitor snapshot"]);
        let out = process::run(
            &mut c,
            duration(&self.execution.git_timeout)?,
            65536,
            &self.shutdown,
            duration(&self.execution.shutdown_grace)?,
            None,
        )?;
        ensure!(out.code == 0, "cannot create range snapshot");
        let root = String::from_utf8(out.stdout)?.trim().to_owned();
        self.pin(&root)?;
        let selector = format!(":{}:{}", f.expression, f.path);
        let mut c = self.command();
        c.env("GIT_ATTR_SOURCE", rev).args([
            "log",
            "--format=",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-L",
            &selector,
            &root,
        ]);
        let out = process::run(
            &mut c,
            duration(&self.execution.git_timeout)?,
            self.limits.max_patch_bytes,
            &self.shutdown,
            duration(&self.execution.shutdown_grace)?,
            None,
        )?;
        if out.code != 0 {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("no match") && err.contains("-L parameter") {
                return Ok(None);
            }
            bail!("function selector failed: {}", err.trim());
        }
        let patch = String::from_utf8_lossy(&out.stdout);
        let header = patch
            .lines()
            .find(|l| l.starts_with("@@ "))
            .context("range has no hunk")?;
        let token = header
            .split_whitespace()
            .nth(2)
            .context("range coordinates")?;
        let (start, count) = token
            .trim_start_matches('+')
            .split_once(',')
            .unwrap_or((token.trim_start_matches('+'), "1"));
        let start: usize = start.parse()?;
        let count: usize = count.parse()?;
        let mut offset = 0;
        let mut lines = Vec::new();
        for l in bytes.split_inclusive(|x| *x == b'\n') {
            lines.push((offset, offset + l.len()));
            offset += l.len();
        }
        ensure!(
            start > 0 && start - 1 + count <= lines.len(),
            "range outside blob"
        );
        let begin = lines[start - 1].0;
        let end = lines[start - 1 + count - 1].1;
        Ok(Some(Selection {
            bytes: bytes[begin..end].to_vec(),
            start,
        }))
    }
}

pub fn repository_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.git"))
}
