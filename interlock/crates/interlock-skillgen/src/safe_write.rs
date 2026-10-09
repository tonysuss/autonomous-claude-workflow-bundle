//! Writes generated files into someone's repository without clobbering it:
//! never through a symlink, never outside the root, and never over a file
//! interlock did not write (or that changed since), unless forced. A manifest
//! of what interlock wrote, with hashes, is how it tells.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Paths relative to the root, with the SHA-256 of what interlock wrote.
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WriteReport {
    pub written: Vec<PathBuf>,
    /// Already held exactly what interlock would write.
    pub unchanged: Vec<PathBuf>,
    /// Not written, with the reason.
    pub refused: Vec<(PathBuf, String)>,
}

impl WriteReport {
    pub fn extend(&mut self, other: WriteReport) {
        self.written.extend(other.written);
        self.unchanged.extend(other.unchanged);
        self.refused.extend(other.refused);
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Writes under one root, keeping a manifest of what it wrote.
pub struct SafeWriter {
    root: PathBuf,
    manifest_path: Option<PathBuf>,
    manifest: Manifest,
    force: bool,
    pub report: WriteReport,
}

impl SafeWriter {
    /// `root` must exist and is resolved once; nothing is written outside it.
    pub fn new(root: &Path, manifest_path: Option<PathBuf>, force: bool) -> Result<SafeWriter> {
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let manifest = match &manifest_path {
            Some(p) if p.exists() => serde_json::from_str(&std::fs::read_to_string(p)?)
                .map_err(|e| Error::Invalid(format!("{}: {e}", p.display())))?,
            _ => Manifest::default(),
        };
        Ok(SafeWriter { root, manifest_path, manifest, force, report: WriteReport::default() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Why `rel` cannot be written, if it cannot: it leaves the root, or a
    /// component on the way (or the file itself) is a symlink or not a directory.
    fn path_problem(&self, rel: &Path) -> Option<String> {
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Some("the path leaves the target directory".into());
        }
        let mut at = self.root.clone();
        let parts: Vec<_> = rel.components().collect();
        for (i, c) in parts.iter().enumerate() {
            at.push(c.as_os_str());
            match std::fs::symlink_metadata(&at) {
                Err(_) => return None,
                Ok(m) if m.file_type().is_symlink() => {
                    return Some(format!("{} is a symlink; interlock does not write through symlinks", at.display()));
                }
                Ok(m) if i + 1 < parts.len() && !m.is_dir() => {
                    return Some(format!("{} is not a directory", at.display()));
                }
                Ok(_) => {}
            }
        }
        None
    }

    /// Writes `text` at `rel` under the root, or records why not.
    pub fn write(&mut self, rel: &Path, text: &str) -> Result<()> {
        let path = self.root.join(rel);
        if let Some(problem) = self.path_problem(rel) {
            self.report.refused.push((path, problem));
            return Ok(());
        }
        let key = rel.to_string_lossy().replace('\\', "/");
        let new_hash = sha256(text.as_bytes());
        if path.exists() {
            let current = std::fs::read(&path)?;
            let current_hash = sha256(&current);
            if current_hash == new_hash {
                self.manifest.files.insert(key, new_hash);
                self.report.unchanged.push(path);
                return Ok(());
            }
            let ours = self.manifest.files.get(&key) == Some(&current_hash);
            if !ours && !self.force {
                let why = if self.manifest.files.contains_key(&key) {
                    "it changed since interlock wrote it"
                } else {
                    "interlock did not write it"
                };
                self.report.refused.push((path, format!("{why}; pass --force to overwrite")));
                return Ok(());
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text)?;
        self.manifest.files.insert(key, new_hash);
        self.report.written.push(path);
        Ok(())
    }

    /// Saves the manifest and returns what happened.
    pub fn finish(self) -> Result<WriteReport> {
        if let Some(p) = &self.manifest_path {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(p, serde_json::to_string_pretty(&self.manifest).unwrap_or_default())?;
        }
        Ok(self.report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_new_files_and_its_own_but_not_the_users() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let manifest = root.join(".interlock/setup-manifest.json");
        std::fs::create_dir_all(root.join(".github/skills/review")).unwrap();
        std::fs::write(root.join(".github/skills/review/SKILL.md"), "the user's own review skill\n").unwrap();

        let mut w = SafeWriter::new(root, Some(manifest.clone()), false).unwrap();
        w.write(Path::new(".github/skills/interlock-route/SKILL.md"), "route v1\n").unwrap();
        w.write(Path::new(".github/skills/review/SKILL.md"), "interlock's review\n").unwrap();
        let report = w.finish().unwrap();
        assert_eq!(report.written.len(), 1);
        assert_eq!(report.refused.len(), 1);
        assert!(report.refused[0].1.contains("interlock did not write it"), "{:?}", report.refused);
        assert_eq!(
            std::fs::read_to_string(root.join(".github/skills/review/SKILL.md")).unwrap(),
            "the user's own review skill\n"
        );

        // Its own unmodified file is updated; one the user edited is not, unless forced.
        let mut w = SafeWriter::new(root, Some(manifest.clone()), false).unwrap();
        w.write(Path::new(".github/skills/interlock-route/SKILL.md"), "route v2\n").unwrap();
        assert_eq!(w.finish().unwrap().written.len(), 1);
        std::fs::write(root.join(".github/skills/interlock-route/SKILL.md"), "edited by hand\n").unwrap();
        let mut w = SafeWriter::new(root, Some(manifest.clone()), false).unwrap();
        w.write(Path::new(".github/skills/interlock-route/SKILL.md"), "route v3\n").unwrap();
        let report = w.finish().unwrap();
        assert!(report.refused[0].1.contains("changed since interlock wrote it"));
        let mut w = SafeWriter::new(root, Some(manifest), true).unwrap();
        w.write(Path::new(".github/skills/interlock-route/SKILL.md"), "route v3\n").unwrap();
        assert_eq!(w.finish().unwrap().written.len(), 1, "--force overwrites");
    }

    #[test]
    fn never_writes_through_symlinks_or_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(outside.path(), root.join(".github")).unwrap();
        std::fs::write(outside.path().join("victim.txt"), "keep\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("victim.txt"), root.join("link.md")).unwrap();
        let mut w = SafeWriter::new(root, None, true).unwrap();
        w.write(Path::new(".github/skills/x/SKILL.md"), "x").unwrap();
        w.write(Path::new("link.md"), "x").unwrap();
        w.write(Path::new("../escape.md"), "x").unwrap();
        let report = w.finish().unwrap();
        assert_eq!(report.written, Vec::<PathBuf>::new(), "even with --force");
        assert_eq!(report.refused.len(), 3, "{:?}", report.refused);
        assert!(!outside.path().join("skills").exists());
        assert_eq!(std::fs::read_to_string(outside.path().join("victim.txt")).unwrap(), "keep\n");
    }
}
