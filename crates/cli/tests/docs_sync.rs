//! Docs gate (issue 039 TDD): the book and the cookbook cannot rot.
//!
//! 1. Every cookbook recipe is compiled and executed here (`include!`)
//!    — a snippet that stops running fails the suite.
//! 2. Every fenced rust block in a book chapter must equal a recipe
//!    file byte-for-byte — chapters are generated, never hand-copied.
//! 3. Every recipe file is embedded in at least one chapter.
//! 4. `SUMMARY.md` and chapter links resolve (local links only).

use std::fs;
use std::path::{Path, PathBuf};

fn docs_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

fn book_dir() -> PathBuf {
    docs_root().join("book")
}

fn recipes_dir() -> PathBuf {
    docs_root().join("cookbook/recipes")
}

fn chapter_files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(book_dir())
        .expect("book dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter(|path| path.file_name().is_some_and(|name| name != "SUMMARY.md"))
        .collect();
    out.sort();
    out
}

fn extract_fenced_rust(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```rust") {
            let mut block = Vec::new();
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    break;
                }
                block.push(inner);
            }
            blocks.push(format!("{}\n", block.join("\n")));
        }
    }
    blocks
}

fn extract_local_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Only `](...)` is a markdown link — parenthesized prose in the
        // body text is not a link.
        if bytes[i] == b'(' && i > 0 && bytes[i - 1] == b']' {
            let end = text[i + 1..].find(')').map(|off| i + 1 + off);
            if let Some(end) = end {
                let target = &text[i + 1..end];
                if !target.starts_with("http") && !target.is_empty() && !target.contains(' ') {
                    links.push(target.to_owned());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    links
}

// ---------------------------------------------------------------------------
// 1. The recipes run (each module below is a docs/cookbook/recipes file).
// ---------------------------------------------------------------------------

mod recipe_parse_json {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/cookbook/recipes/parse_json.rs"
    ));
}
mod recipe_event_stream {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/cookbook/recipes/event_stream.rs"
    ));
}
mod recipe_incremental_reparse {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/cookbook/recipes/incremental_reparse.rs"
    ));
}
mod recipe_query_pairs {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/cookbook/recipes/query_pairs.rs"
    ));
}
mod recipe_recovery_plan {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/cookbook/recipes/recovery_plan.rs"
    ));
}

// ---------------------------------------------------------------------------
// 2-4. Book ↔ cookbook sync and link checks.
// ---------------------------------------------------------------------------

#[test]
fn every_book_code_block_is_a_cookbook_recipe() {
    let recipes: Vec<(String, String)> = fs::read_dir(recipes_dir())
        .expect("recipes dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .map(|path| {
            (
                path.file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned(),
                fs::read_to_string(&path).expect("recipe"),
            )
        })
        .collect();
    assert!(!recipes.is_empty(), "cookbook is not empty");
    for chapter in chapter_files() {
        let text = fs::read_to_string(&chapter).expect("chapter");
        for (index, block) in extract_fenced_rust(&text).iter().enumerate() {
            let Some((name, _)) = recipes.iter().find(|(_, content)| content == block) else {
                panic!(
                    "{} block #{index} matches no recipe in docs/cookbook/recipes",
                    chapter.display()
                );
            };
            let name = name.clone();
            println!(
                "{} block #{index} == {name}",
                chapter.file_name().expect("name").to_string_lossy()
            );
        }
    }
}

#[test]
fn every_recipe_is_embedded_in_a_chapter() {
    let chapter_blocks: Vec<String> = chapter_files()
        .iter()
        .map(|path| fs::read_to_string(path).expect("chapter"))
        .flat_map(|text| extract_fenced_rust(&text))
        .collect();
    for entry in fs::read_dir(recipes_dir()).expect("recipes dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let content = fs::read_to_string(&path).expect("recipe");
            assert!(
                chapter_blocks.contains(&content),
                "recipe {} is never embedded in a chapter",
                path.display()
            );
        }
    }
}

#[test]
fn summary_links_resolve_to_chapters() {
    let summary = fs::read_to_string(book_dir().join("SUMMARY.md")).expect("SUMMARY.md");
    let links = extract_local_links(&summary);
    assert!(links.len() >= 6, "SUMMARY lists the chapters: {links:?}");
    for link in links {
        let target = book_dir().join(&link);
        assert!(target.is_file(), "SUMMARY link {link} does not resolve");
    }
}

#[test]
fn chapter_links_resolve() {
    for chapter in chapter_files() {
        let text = fs::read_to_string(&chapter).expect("chapter");
        for link in extract_local_links(&text) {
            let target = book_dir().join(&link);
            assert!(
                target.is_file(),
                "{} links {} which does not resolve",
                chapter.display(),
                link
            );
        }
    }
}

#[test]
fn summary_lists_every_chapter() {
    let summary = fs::read_to_string(book_dir().join("SUMMARY.md")).expect("SUMMARY.md");
    for chapter in chapter_files() {
        let name = chapter.file_name().expect("name").to_string_lossy();
        assert!(
            summary.contains(&format!("]({name})")),
            "SUMMARY.md is missing chapter {name}"
        );
    }
}
