//! Reproducible identities for persisted computations and delivery artifacts.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, process::Command, sync::OnceLock};

pub const MODEL_VERSION: u32 = 1;
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
        Ok(Self {
            git,
            dependencies: crate::hash(include_str!("../Cargo.lock")),
            analyzer: crate::hash(concat!(
                include_str!("diff.rs"),
                include_str!("git.rs"),
                include_str!("model.rs"),
                include_str!("monitor.rs")
            )),
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
        include_str!("mail.rs")
    ))
}
