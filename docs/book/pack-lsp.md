# The pack language server

`syntact pack-lsp` is the editor-side of the pack languages (roadmap
§12.2 p3d "LSP for packs", issue 042): a std-only LSP 3.17 server over
stdio that understands the ten languages the shipped packs parse —
JSON, TOML, Python, TypeScript, Rust, C, SQL, INI, CSV, Markdown. It
rides the same spine as the grammar server (`lsp::rpc` framing, the
UTF-16 position index, the transport loop), so both servers always
speak the same protocol.

## Starting it

Point your editor at the binary over stdio:

```text
syntact pack-lsp
```

Optionally bound the inbound message size (default 8 MiB):

```text
syntact pack-lsp --max-message 1048576
```

A session is plain JSON-RPC frames — `Content-Length` header, blank
line, JSON body:

```text
Content-Length: 55

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

## How documents are analyzed

Each open document resolves to a pack: the `languageId` first, the uri
extension second (`file:///cfg.toml` finds the TOML pack even when the
editor sends `plaintext`). One pack `parse` per document version
produces the green tree; a single DFS materializes the **outline** —
every node as a byte span with parent links, subtree bounds, and the
first-significant-token span — plus the flat token list and the error
regions. Every view below derives from that outline and is rebuilt
once per version, so queries answer from prebuilt data:

- **diagnostics** — every `Event::Error` materializes an ERROR node in
  the tree; the outline collects the node spans (zero-width when the
  parser consumed nothing) and the view turns each into a severity-1
  diagnostic. Pushed after every open and change, served on
  `textDocument/diagnostic` pull.
- **document symbols** — a hierarchical outline of the interior nodes;
  names are the first significant token of each subtree in document
  order (a TOML table is named by its header's name token, a JSON pair
  by its key). The root is the document itself and is not listed;
  nodes without a nameable token are spliced out.
- **folding ranges** — every node spanning more than one line, from
  the node's first line to the line of its last byte.
- **semantic tokens** — a fixed, pack-agnostic four-entry legend
  (`string`, `number`, `operator`, `identifier`) driven purely by token
  shape: quoted → string, optional sign + digit → number, punctuation
  runs → operator, everything else significant → identifier. Whitespace
  and prose runs stay unhighlighted. Delta-encoded LSP data, precomputed
  as flat `u32` quintuples.
- **document highlight** — every token matching the token under the
  cursor by kind and text; whitespace positions match nothing.
- **selection ranges** — the ancestor chain of node spans at a
  position, served straight from the outline's parent links; a position
  at the very end of the document clamps to the root chain.

Unknown languages (no languageId match, no extension match) are ignored
cleanly: no document is stored and no diagnostics are pushed.

## The position layer, in one sweep

Green nodes are position-independent; the outline is the per-document
positioning layer. Because the DFS emits every node's span boundaries
(entry, name token, exit) in document order, one monotonic byte→UTF-16
sweep — no sort — converts the whole event stream to LSP positions, and
a second sequential sweep covers the token stream (the delta encoder
for semantic tokens rides the same machinery). Queries never re-walk
the text: the folding, symbol, and highlight paths read the prebuilt
views, and a no-match highlight allocates nothing (pinned by dhat gates
in `crates/bench/tests/pack_lsp_budgets.rs`).

## Performance model

Measured with `cargo bench -p bench --bench pack_lsp` on a 400-pair
JSON document (~25 KiB): document open (parse → outline → every view)
in ~1.1 ms, a didChange round trip (edit → reparse → view rebuild) in
~1.1 ms, semantic tokens in ~85 µs, highlight in ~8 µs, selection
ranges in ~0.3 µs, and the prebuilt folding/diagnostic reads in
nanoseconds. The pack's own parse is roughly a third of the open cost
(~380 µs) — the packs carry their own allocation budgets; the LSP-side
open budget is pinned at ≤ 450 blocks per KiB of source.

## Why the reuse engine is not on this path

The packs parse tree *and* token stream in one pass, so the
incremental engine's token-level reuse (`incremental::Engine`) cannot
skip the reparse every tree view needs after an edit. The engine stays
the standalone incremental layer for pure-lexer consumers; didChange
here costs exactly one pack parse plus one outline DFS, both
benchmarked above.
