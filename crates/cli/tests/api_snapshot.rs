//! API surface snapshot (issue 040): the public-item inventory of every
//! workspace crate is frozen in `docs/api-snapshot.md`. Any signature or
//! item change alters the generated inventory and fails this test —
//! the freeze makes API drift a reviewable event.
//!
//! Regen: `SYNTACT_REGEN_SNAPSHOT=1 cargo test -p cli --test api_snapshot`

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root (two levels up from this crate).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Crate source roots: `crates/*` and `packs/*`.
fn crate_src_dirs() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut out = Vec::new();
    for group in ["crates", "packs"] {
        let mut dirs: Vec<PathBuf> = fs::read_dir(root.join(group))
            .expect("workspace group")
            .filter_map(|entry| {
                let path = entry.expect("entry").path();
                if path.join("src").is_dir() {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        dirs.sort();
        out.extend(dirs);
    }
    out
}

/// All `src` files of a crate except the `#[cfg(test)]` module files
/// (`tests.rs`) — the shipped surface, not the suite.
fn src_files(crate_dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("src dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&crate_dir.join("src"), &mut files);
    files.sort();
    files
}

/// Normalize a `pub` declaration line: collapse whitespace so signature
/// reflows do not churn the snapshot.
fn normalize(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The inventory for one crate: every top-level `pub` item per file.
fn crate_inventory(crate_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for file in src_files(crate_dir) {
        let rel = file
            .strip_prefix(crate_dir)
            .expect("under crate")
            .to_string_lossy()
            .into_owned();
        for line in fs::read_to_string(&file).expect("source").lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("pub ") || trimmed.starts_with("pub(") {
                out.push(format!("- {rel}: {}", normalize(trimmed)));
            }
        }
    }
    out
}

/// The whole generated snapshot document.
fn generate() -> String {
    let mut doc = String::from(
        "# API surface snapshot (issue 040)\n\n\
         Frozen public-item inventory of every workspace crate (top-level\n\
         `pub` declarations per source file, whitespace-normalized).\n\
         Regenerate with `SYNTACT_REGEN_SNAPSHOT=1 cargo test -p cli \\\n\
         --test api_snapshot` and commit the diff.\n",
    );
    for crate_dir in crate_src_dirs() {
        let name = crate_dir
            .strip_prefix(workspace_root())
            .expect("under root")
            .to_string_lossy()
            .into_owned();
        let _ = write!(doc, "\n## {name}\n\n");
        let items = crate_inventory(&crate_dir);
        if items.is_empty() {
            doc.push_str("(no public items)\n");
        } else {
            for item in items {
                let _ = writeln!(doc, "{item}");
            }
        }
    }
    doc
}

#[test]
fn api_surface_matches_the_freeze() {
    let generated = generate();
    let path = workspace_root().join("docs/api-snapshot.md");
    if std::env::var_os("SYNTACT_REGEN_SNAPSHOT").is_some() {
        fs::write(&path, &generated).expect("write snapshot");
        println!("regenerated {}", path.display());
        return;
    }
    let frozen = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing {} — regen with SYNTACT_REGEN_SNAPSHOT=1 cargo test \
             -p cli --test api_snapshot",
            path.display()
        )
    });
    assert_eq!(
        frozen, generated,
        "the public API drifted from docs/api-snapshot.md — review the \
         diff, then regen with SYNTACT_REGEN_SNAPSHOT=1 cargo test -p cli \
         --test api_snapshot and commit"
    );
}
