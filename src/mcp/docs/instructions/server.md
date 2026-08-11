Code retrieval and graph navigation over semctx-indexed codebases. Tools default to the repository this MCP server was launched in, and can be scoped per call to another indexed codebase.

## Tool selection

- **find_definition / find_references** — use these FIRST when you know an exact symbol name. Deterministic, fast, precise (no embedding round-trip).
- **search_codebase** — use for fuzzy / conceptual queries when you don't know the symbol or file.
- **search_symbols** — declaration-name discovery by exact/prefix/substring/glob/fuzzy matching; use between exact `find_definition` and conceptual `search_codebase`.
- **grep** — exact literal / regex search over file content ("find all occurrences of X"); the exhaustive counterpart to semantic search.
- **who_calls** — every definition that calls a function (the inverse call edge); the "who calls X" tool.
- **implementations_of** — the types implementing a trait/interface (the reverse implements edge).
- **call_path** — a shortest call chain from one function to another.
- **trace / call_graph** — compact symbol neighbourhood versus the complete bounded caller/callee graph.
- **cycles / unused / duplicates** — exact graph analyses: strongly connected calls, unreferenced definitions with caveats, and content-hash duplicate groups.
- **type_hierarchy** — bounded supertype/subtype traversal with declared-versus-structural relations and external identities.
- **reaches / flows_into / flows_between** — inter-procedural value flow between external boundaries: forward from a source, backward from a destination, and the witness path between two.
- **symbol_at_position** — what's at `path:line[:column]`: hover (no column) or go-to-definition (with column).
- **batch_lookup** — resolve up to 256 symbols (definitions or references) in one round-trip instead of N calls.
- **file_outline** — a known file's nested table of contents; filters, qualified identities, and optional declaration bodies avoid inventing a second overview tool.
- **read_source** — revision-pinned line/byte ranges from the content plane, including codebases with no local checkout.
- **expand_chunk** — grow context around a `path:line-range` (e.g. a search hit) by pulling the neighbouring chunks.
- **list_files / file_tree** — orient: the indexed files, as a filterable paged list or a directory tree.
- **list_projects** — the codebase's detected project graph (leaf projects + workspace/solution groups).
- **imports** — file→file import dependencies within the codebase.
- **symbol_edges** — finer than imports: which definition each imported name binds to (across files/crates in this codebase).
- **external_links** — cross-repo "jump to definition": imports that leave this codebase, resolved into the public API of other codebases you can see.
- **list_codebases / current_context** — discover visible codebases and diagnose the effective server/tenant/codebase/checkout/index/graph binding.
- **list_domains** — discover what's searchable and which tags are available for filtering.
- **sync_status** — check the background index job; if `list_files` / `search` are empty right after attaching, this tells you whether indexing is still in progress.
- **index_codebase** — explicit opt-in indexing. If startup or a tool says the current repository is not indexed, tell the user and ask permission; call this only after they agree.
- **rename_symbol / safe_delete_symbol / replace_symbol_body / insert_before_symbol / insert_after_symbol** — immediate local edit actions. Each internally consumes the server's grammar-validated plan, verifies source identity, graph generation, paths, and hashes, and goes through the client's normal approval flow.
- **undo_edit** — reverse a completed symbolic edit by its returned edit id while retained preimages and current postimage hashes still match.

Every codebase-scoped tool accepts an optional codebase selector. Omit it for the launch/current repository. To work across repositories, pass either a codebase id or an indexed local directory path. A previously indexed local repository already has user consent: activate its initial sync and filesystem watcher immediately without asking again. Ask only when the directory has never been indexed and would need `index_codebase`.

The first-ever `index_codebase` call is a readiness boundary: retrieval, catalog, and graph tools for that codebase wait until server embedding completes successfully. `sync_status` remains callable while they wait. Later re-syncs do not block retrieval and continue to expose the last complete snapshot with freshness warnings.

## Workflow

1. Don't know where something is → `search_codebase`.
2. Know the symbol name → `find_definition` / `find_references`; callers → `who_calls`; the whole neighbourhood → `trace`; many symbols at once → `batch_lookup`.
3. Need every literal occurrence (including strings and comments) → `grep`.
4. Understand how files depend on each other → `imports`, then `symbol_edges` for the exact bindings.
5. A dependency points outside this codebase → `external_links`.
6. Have a known file or a hit's line range → `file_outline` for its shape, `expand_chunk` to grow context around the hit.
7. Need exact remote bytes → `read_source`; need a refactor → call the matching edit action, which obtains the server plan internally and applies it after the host's normal approval.

Prefer the symbol-graph tools over search whenever you have an exact name — they're cheaper and more precise. The symbol graph covers Rust, C#, Go, and TypeScript/JavaScript; for files in other languages the graph tools return nothing, so use `search_codebase` / `grep` there.
