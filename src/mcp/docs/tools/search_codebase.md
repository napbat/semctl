PRIMARY tool for semantic codebase search. Use this FIRST when you don't know which files contain what you need, or when gathering high-level context for a task.

Hybrid (dense + lexical) retrieval over the indexed codebase. Returns ranked hits with path, line range, symbol, kind, and a snippet; open the cited path:line range to read more, or pass it to `expand_chunk` to grow context.

## When to use this vs alternatives

- **search_codebase** (this tool): natural-language queries, unknown file locations, understanding how a feature is wired up.
- **find_definition** (`Foo::bar`, `verify_token`): you know the exact name and want the def site. Faster + more precise — no embedding round-trip.
- **find_references**: who calls / uses a symbol. Same precision tradeoff vs search.
- **file_outline**: you already know the file and want its table of contents (symbols + line ranges).
- **expand_chunk**: you have a `path:line-range` (e.g. a search hit) and want the surrounding chunks.
- **list_files**: you already know (or can guess) the path and just want to confirm a file is indexed or scope a subtree.
- **grep** (the semctx tool): exact string / regex matching — every occurrence of a known token, error messages, config values. Don't search semantically for exact strings, and don't grep for concepts.

## Parameters

- `query` (required) — the natural-language search.
- `top_k` — max hits to return (default 20; values above 1000 are clamped server-side). For initial discovery, a focused 5–8 hits is usually enough.
- `domains` — restrict to specific registered domain ids. Omit to fan out across every registered domain (run `list_domains` to see what's available).
- `prefer` — ranking bias: `"code"` demotes documentation (markdown) so the implementation leads; `"docs"` does the reverse. Omit for the unbiased hybrid ranking. Use `"code"` for "how is X implemented", leave off for "explain X conceptually".
- `kinds` — restrict to chunk kinds: `function`, `container` (type/class/module), or `block` (free text, incl. prose). Omit for every kind.
- `expand` — return the **full enclosing-symbol body** per hit instead of a 4-line snippet. Block hits snap to their enclosing function/type. Default false; expand only the most relevant hit or small set rather than combining expansion with a broad `top_k`.

## Server result budget

The server enforces a total result-content budget, especially for expanded searches.
`top_k` is an upper bound, not a promise that every expanded body will fit in
one response. Start with snippets, then refine the query or use `expand_chunk`
for the relevant path/range. When using `expand: true`, reason from those bodies
instead of immediately fetching the same files again.

## Result freshness

When the codebase has a local checkout, a hit whose file has been edited since it
was indexed is flagged `⚠ stale (edited since indexed)` — read those files
directly rather than trusting the snippet. A footer flags when a background sync
is still running or failed (check `sync_status`).

## Good queries

- "Where is the function that handles user authentication?"
- "How does the engine decide which files to re-embed?"
- "What checks run before a file is sent to embedding?"

## Bad queries (use a different tool)

- "Find definition of `verify_token`" → use find_definition
- "All callers of `delete_files`" → use who_calls
- "All occurrences of `TODO`" → use grep
