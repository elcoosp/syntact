//! Deterministic JSON serialization for the parse service (issue 038):
//! fixed field order, hand-rolled escaping, no map iteration — the same
//! request always serializes to the same bytes.

use std::sync::Arc;

use runtime_core::tree::{GreenChild, GreenNode};

/// Quote a string as a JSON string literal with minimal escaping.
#[must_use]
pub fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if u32::from(ch) < 0x20 => {
                let _ =
                    std::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", u32::from(ch)));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Render a `/parse` response: pack, ok, the flat token list, and the
/// green tree. Fixed field order; `ok` is false when the tree carries
/// ERROR nodes (recovery kept it lossless — the request still succeeds).
#[must_use]
pub fn render_parse(pack: &str, tree: Result<Arc<GreenNode>, String>) -> String {
    let mut out = String::new();
    let _ = std::fmt::Write::write_fmt(
        &mut out,
        format_args!("{{\n  \"pack\": {},\n", json_str(pack)),
    );
    match tree {
        Ok(root) => {
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("  \"ok\": {},\n", !root.has_error),
            );
            let tokens = root.token_ranges();
            out.push_str("  \"tokens\": [\n");
            for (index, (kind, range)) in tokens.iter().enumerate() {
                let comma = if index + 1 < tokens.len() { "," } else { "" };
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!(
                        "    {{\"kind\": {}, \"start\": {}, \"end\": {}}}{comma}\n",
                        kind, range.start, range.end
                    ),
                );
            }
            out.push_str("  ],\n  \"tree\": ");
            render_tree(&root, &mut out, 0);
            out.push_str("\n}");
        }
        Err(message) => {
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("  \"ok\": false,\n  \"error\": {}\n", json_str(&message)),
            );
            out.push('}');
        }
    }
    out
}

/// Render one green subtree at `level` of pretty-printing: children get
/// a 4-space column plus two per level; closers get two per level plus
/// two (aligned under the inline `{"node": ...}` wrapper).
fn render_tree(node: &GreenNode, out: &mut String, level: usize) {
    let _ = std::fmt::Write::write_fmt(
        out,
        format_args!(
            "{{\"node\": {{\"kind\": {}, \"error\": {}, \"children\": ",
            node.kind, node.has_error
        ),
    );
    if node.children.is_empty() {
        out.push_str("[]");
    } else {
        let pad = format!("{:1$}", "", 4 + 2 * level);
        let close = format!("{:1$}", "", 2 + 2 * level);
        out.push_str("[\n");
        for (index, child) in node.children.iter().enumerate() {
            let comma = if index + 1 < node.children.len() {
                ","
            } else {
                ""
            };
            match child {
                GreenChild::Node(inner) => {
                    out.push_str(&pad);
                    render_tree(inner, out, level + 1);
                    out.push_str(comma);
                    out.push('\n');
                }
                GreenChild::Token { kind, range } => {
                    let _ = std::fmt::Write::write_fmt(
                        out,
                        format_args!(
                            "{pad}{{\"token\": {{\"kind\": {}, \"range\": [{}, {}]}}}}{comma}\n",
                            kind, range.start, range.end
                        ),
                    );
                }
            }
        }
        out.push_str(&close);
        out.push(']');
    }
    out.push_str("}}");
}

/// The request body is not a JSON object carrying `pack` and `source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractError;

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request body is not a JSON object with `pack` and `source` strings")
    }
}

impl std::error::Error for ExtractError {}

/// Extract `(pack, source)` from a `/parse` request body: a tiny
/// JSON-object scanner that accepts `{"pack": "...", "source": "..."}`
/// (any key order, unknown keys ignored, minimal string unescaping).
///
/// # Errors
/// When the body is not a JSON object with string values for both keys.
pub fn extract_pack_source(body: &str) -> Result<(String, String), ExtractError> {
    let bytes = body.as_bytes();
    let mut i = 0usize;
    skip_ws(bytes, &mut i);
    if bytes.get(i) != Some(&b'{') {
        return Err(ExtractError);
    }
    i += 1;
    let mut pack = None;
    let mut source = None;
    loop {
        skip_ws(bytes, &mut i);
        match bytes.get(i) {
            Some(b'}') => break,
            Some(b',') => {
                i += 1;
                continue;
            }
            Some(b'"') => {}
            _ => return Err(ExtractError),
        }
        let key = scan_string(body, &mut i)?;
        skip_ws(bytes, &mut i);
        if bytes.get(i) != Some(&b':') {
            return Err(ExtractError);
        }
        i += 1;
        skip_ws(bytes, &mut i);
        match key.as_str() {
            "pack" if pack.is_none() => pack = Some(scan_string(body, &mut i)?),
            "source" if source.is_none() => source = Some(scan_string(body, &mut i)?),
            _ => skip_value(body, &mut i)?,
        }
    }
    Ok((pack.ok_or(ExtractError)?, source.ok_or(ExtractError)?))
}

fn skip_ws(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && matches!(bytes[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

/// Scan a JSON string starting at `bytes[*i] == '"'`, returning the
/// unescaped contents and advancing past the closing quote.
fn scan_string(body: &str, i: &mut usize) -> Result<String, ExtractError> {
    let bytes = body.as_bytes();
    if bytes.get(*i) != Some(&b'"') {
        return Err(ExtractError);
    }
    *i += 1;
    let mut out = String::new();
    while *i < bytes.len() {
        match bytes[*i] {
            b'"' => {
                *i += 1;
                return Ok(out);
            }
            b'\\' => {
                let next = bytes.get(*i + 1).ok_or(ExtractError)?;
                *i += 2;
                match next {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'u' => {
                        let hex = body.get(*i..*i + 4).ok_or(ExtractError)?;
                        let code = u32::from_str_radix(hex, 16).map_err(|_| ExtractError)?;
                        let ch = char::from_u32(code).ok_or(ExtractError)?;
                        *i += 4;
                        out.push(ch);
                    }
                    _ => return Err(ExtractError),
                }
            }
            _ => {
                let ch = body[*i..].chars().next().ok_or(ExtractError)?;
                *i += ch.len_utf8();
                out.push(ch);
            }
        }
    }
    Err(ExtractError)
}

/// Skip a non-string JSON value (number, bool, null, or nested
/// structure) — the playground only needs the two string fields.
fn skip_value(body: &str, i: &mut usize) -> Result<(), ExtractError> {
    let bytes = body.as_bytes();
    match bytes.get(*i) {
        Some(b'"') => {
            scan_string(body, i)?;
            Ok(())
        }
        Some(b'{' | b'[') => {
            let open = bytes[*i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            let mut in_str = false;
            while *i < bytes.len() {
                let b = bytes[*i];
                if in_str {
                    if b == b'\\' {
                        *i += 2;
                        continue;
                    }
                    if b == b'"' {
                        in_str = false;
                    }
                } else {
                    match b {
                        b'"' => in_str = true,
                        b if b == open => depth += 1,
                        b if b == close => {
                            depth -= 1;
                            if depth == 0 {
                                *i += 1;
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                }
                *i += 1;
            }
            Err(ExtractError)
        }
        Some(_) => {
            while *i < bytes.len()
                && !matches!(bytes[*i], b',' | b'}' | b']' | b' ' | b'\n' | b'\t' | b'\r')
            {
                *i += 1;
            }
            Ok(())
        }
        None => Err(ExtractError),
    }
}
