//! Embeds the canonical skills (`interlock/skills`) and agents (`interlock/agents`)
//! in the binary, so `interlock skills generate` works without the source tree.

use std::path::{Path, PathBuf};

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push((rel.to_string_lossy().replace('\\', "/"), path.canonicalize().unwrap_or(path)));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..").canonicalize().unwrap();
    let mut files = Vec::new();
    for dir in ["skills", "agents"] {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
        walk(&root, &root.join(dir), &mut files);
    }
    files.sort();
    let mut code = String::from("/// Canonical skill and agent files, by path relative to `interlock/`.\n");
    code.push_str("pub static FILES: &[(&str, &str)] = &[\n");
    for (rel, path) in &files {
        code.push_str(&format!("    ({rel:?}, include_str!({:?})),\n", path.display().to_string()));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded.rs");
    std::fs::write(out, code).unwrap();
}
