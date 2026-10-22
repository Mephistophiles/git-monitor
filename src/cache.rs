//! Disposable, checksummed cache entries. Invalid or oversized data is a miss.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Serialize, Deserialize)]
struct Entry {
    version: u32,
    sha256: String,
    bytes: String,
}
pub fn get(directory: &Path, key: &str, limit: usize) -> Option<Vec<u8>> {
    let path = directory.join(key);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > (limit as u64).saturating_mul(2).saturating_add(4096)
    {
        return None;
    }
    let entry: Entry = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let bytes = STANDARD.decode(entry.bytes).ok()?;
    (entry.version == 1 && bytes.len() <= limit && crate::hash(&bytes) == entry.sha256)
        .then_some(bytes)
}
pub fn put(directory: &Path, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
    crate::atomic_write(
        &directory.join(key),
        &serde_json::to_vec(&Entry {
            version: 1,
            sha256: crate::hash(bytes),
            bytes: STANDARD.encode(bytes),
        })?,
    )
}
pub fn prune(directory: &Path, count: usize, limit: u64) -> anyhow::Result<usize> {
    if !directory.exists() {
        return Ok(0);
    }
    let mut files = vec![];
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file()
            && name.len() == 64
            && name.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let m = entry.metadata()?;
            files.push((m.modified()?, m.len(), entry.path()));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
    let (mut bytes, mut removed) = (0u64, 0);
    for (i, (_, size, path)) in files.into_iter().enumerate() {
        bytes = bytes.saturating_add(size);
        if i >= count || bytes > limit {
            std::fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}
