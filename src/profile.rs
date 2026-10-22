//! Reproducible identities for persisted computations and delivery artifacts.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, process::Command, sync::OnceLock};

pub const MODEL_VERSION: u32 = 2;
pub const RENDER_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalysisProfile {
    pub git: String,
    pub dependencies: String,
    pub analyzer: String,
    pub model: u32,
    pub drivers: BTreeMap<String, String>,
}

impl AnalysisProfile {
    pub fn freeze(drivers: &BTreeMap<String, String>) -> Result<Self> {
        static GIT: OnceLock<String> = OnceLock::new();
        let git = if let Some(version) = GIT.get() {
            version.clone()
        } else {
            let result = Command::new("git").arg("--version").output()?;
            ensure!(result.status.success(), "Git unavailable");
            let version = String::from_utf8(result.stdout)?.trim().to_owned();
            let _ = GIT.set(version.clone());
            version
        };
        static BUILD: OnceLock<(String, String)> = OnceLock::new();
        let build = BUILD.get_or_init(|| {
            (
                crate::hash(include_str!("../Cargo.lock")),
                crate::hash(concat!(
                    include_str!("diff.rs"),
                    include_str!("git.rs"),
                    include_str!("model.rs"),
                    include_str!("document.rs"),
                    include_str!("config.rs"),
                    include_str!("monitor.rs")
                )),
            )
        });
        Ok(Self {
            git,
            dependencies: build.0.clone(),
            analyzer: build.1.clone(),
            model: MODEL_VERSION,
            drivers: drivers.clone(),
        })
    }
    pub fn key(&self) -> String {
        crate::hash(serde_json::to_vec(self).expect("profile serialization"))
    }
}

pub fn render_key() -> String {
    crate::hash(concat!(
        include_str!("render.rs"),
        include_str!("report.css"),
        include_str!("report.js"),
        include_str!("mail.rs"),
        include_str!("prepare.rs"),
        include_str!("split.rs")
    ))
}
