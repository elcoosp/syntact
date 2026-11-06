# The grammar language server

`syntact lsp` is the editor-side of grammar authoring (roadmap §9.4,
issue 041): a std-only LSP 3.17 server over stdio that understands
`.gir` files. It is served by the same binary that checks and builds
grammars, so the editor and the CLI always agree on what a grammar
means — both read it through the canonical loader.

## Starting it

Point your editor at the binary over stdio (every LSP client speaks
this; here is the raw protocol for a manual smoke test):

```text
syntact lsp
```

Optionally bound the inbound message size (default 8 MiB):

```text
syntact lsp --max-message 1048576
```

A session is plain JSON-RPC frames — `Content-Length` header, blank
line, JSON body:

```text
Content-Length: 55

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

The server answers with its capabilities, then consumes
`initialized`, `textDocument/didOpen`, `textDocument/didChange`
(incremental ranges), and the query methods below, pushing
`textDocument/publishDiagnostics` after every change. `shutdown` then
`exit` ends the session; the exit code is 0 only after a clean
shutdown.

## What the server knows

The analysis layer (`lsp::analysis`) loads each open file into the
span-preserving surface document (`GirDoc`) and collects:

- **declarations** — every `tokens` and `grammar` entry with its name
  span and node span;
- **reference sites** — every occurrence of a declared name inside an
  `alt`, `seq`, or `sep` body;
- **diagnostics** — surface errors at their spans, left recursion
  without a pratt hint, LL(1) conflict sites (downgraded to
  informational on pratt rules, where conflicts are the expected shape
  of an expression region), LR(1) shadow conflicts mapped through
  `lr-shadow` production origins, and unreachable rules.

Symbol identity is per file: a `.gir` package is a closed grammar (a
reference must resolve in-file), so two packs that both declare `value`
never merge. The workspace indexes every open file, and every query
answers per document — deterministic under name collisions.

## Features

- **completion** — rule and token names plus the five `(seq)` markers
  (`opt`, `rep`, `rep1`, `and`, `not`), filtered by the word under the
  cursor; rules are `CompletionItemKind` 3, tokens 14, markers 24.
- **hover** — signatures on declarations and references (the raw KDL
  node), and documentation for every annotation word (`kind`, `node`,
  `reuse`, `wrap-error`, `lbp`/`rbp`/`assoc`, `trivia`,
  `index-bound`, `allow-trailing`, `unicode`) and marker.
- **definition** — from a reference to the declaration's name span.
- **references** — every site of the symbol under the cursor,
  declaration included on request.
- **rename** — one `TextDocumentEdit` per touched file, every site
  rewritten in place. Declaration order never moves, so the dense
  `RuleId`/`TokenId` numbering (and therefore the generated tables'
  layout) is stable across a rename — the "stable-ID preservation"
  clause of the spec. Renames are validated: surface identifiers only,
  reserved marker words rejected, existing names rejected.
- **document symbols** — the token and rule outline.
- **diagnostics** — pull (`textDocument/diagnostic`) and push
  (`publishDiagnostics`) carry the same items; severities are 1 error,
  2 warning, 3 information, 4 hint.

## Performance model

Measured with `cargo bench -p bench --bench lsp` on a 300-rule
grammar (~20 KiB): the query features answer in 0.1–0.7 µs, document
symbols in ~46 µs, and a full open (parse → symbols → determinism
report → shadow automaton → diagnostics) in ~10 ms — dominated by the
canonical KDL loader it shares with `syntact check`/`build`. Allocation
budgets (`crates/bench/tests/lsp_budgets.rs`) pin the LSP-owned
allocations: the definition lookup allocates nothing, hover allocates
only its markdown payload, and completion costs one allocation per
item.

## Cookbook: renaming a rule with stable IDs

Renaming a rule rewrites every site in place — the declaration keeps
its position, so ids stay stable:

```text
grammar {
    value kind=alt number list
    list kind=seq value rep
}
```

Rename `value` → `atom` and the file becomes:

```text
grammar {
    atom  kind=alt number list
    list  kind=seq atom rep
}
```

Same positions, same order, same dense ids — a diff an editor can
apply blindly, and a grammar diff that reads like a source diff
(ADR-013).
