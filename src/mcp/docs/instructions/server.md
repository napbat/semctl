Code retrieval and graph navigation over a semctx-indexed codebase, served by the semctx server. This MCP server is scoped to one codebase (set when it was launched); every code tool below operates on that codebase.

## Tool selection

- **find_definition / find_references** — use these FIRST when you know an exact symbol name. Deterministic, fast, precise (no embedding round-trip).
- **search_codebase** — use for fuzzy / conceptual queries when you don't know the symbol or file.
- **grep** — exact literal / regex search over file content ("find all occurrences of X"); the exhaustive counterpart to semantic search.
- **who_calls** — every definition that calls a function (the inverse call edge); the "who calls X" tool.
- **implementations_of** — the types implementing a trait/interface (the reverse implements edge).
- **call_path** — a shortest call chain from one function to another.
- **trace** — a symbol's neighbourhood in one call: its definition plus direct callers and callees (`depth` widens it).
- **reaches / flows_into / flows_between** — inter-procedural value flow between external boundaries: forward from a source, backward from a destination, and the witness path between two.
- **symbol_at_position** — what's at `path:line[:column]`: hover (no column) or go-to-definition (with column).
- **batch_lookup** — resolve up to 256 symbols (definitions or references) in one round-trip instead of N calls.
- **file_outline** — a known file's table of contents (symbols + line ranges + kinds), cheaper than reading it.
- **expand_chunk** — grow context around a `path:line-range` (e.g. a search hit) by pulling the neighbouring chunks.
- **list_files / file_tree** — orient: the indexed files, as a filterable paged list or a directory tree.
- **list_projects** — the codebase's detected project graph (leaf projects + workspace/solution groups).
- **imports** — file→file import dependencies within the codebase.
- **symbol_edges** — finer than imports: which definition each imported name binds to (across files/crates in this codebase).
- **external_links** — cross-repo "jump to definition": imports that leave this codebase, resolved into the public API of other codebases you can see.
- **list_domains** — discover what's searchable and which tags are available for filtering.
- **sync_status** — check the background index job; if `list_files` / `search` are empty right after attaching, this tells you whether indexing is still in progress.

## Workflow

1. Don't know where something is → `search_codebase`.
2. Know the symbol name → `find_definition` / `find_references`; callers → `who_calls`; the whole neighbourhood → `trace`; many symbols at once → `batch_lookup`.
3. Need every literal occurrence (including strings and comments) → `grep`.
4. Understand how files depend on each other → `imports`, then `symbol_edges` for the exact bindings.
5. A dependency points outside this codebase → `external_links`.
6. Have a known file or a hit's line range → `file_outline` for its shape, `expand_chunk` to grow context around the hit.

Prefer the symbol-graph tools over search whenever you have an exact name — they're cheaper and more precise. The symbol graph covers Rust, C#, Go, and TypeScript/JavaScript; for files in other languages the graph tools return nothing, so use `search_codebase` / `grep` there.
