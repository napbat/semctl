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
- `top_k` — max hits to return, 1–1000 (default 8). Another value fails before the search. The content budget of the server can return fewer hits.
- `domains` — restrict to specific registered domain ids. Omit to fan out across every registered domain (run `list_domains` to see what's available).
- `prefer` — ranking bias: `"code"` demotes documentation (markdown) so the implementation leads; `"docs"` does the reverse. Omit for the unbiased hybrid ranking. Use `"code"` for "how is X implemented", leave off for "explain X conceptually".
- `kinds` — restrict to chunk kinds: `function`, `container` (type/class/module), or `block` (free text, incl. prose). Omit for every kind.
- `expand` — return the **full enclosing-symbol body** per hit instead of a 4-line snippet. Block hits snap to their enclosing function/type. Default false. Expand a small `top_k`, and do not reread files whose bodies were returned.

## Result budget and expanded hits

The server limits the snippet text of one search to 32 KiB by default (`Search:ContentBudgetBytes`). It cuts the snippet that reaches the limit and drops the hits after it. Ask for a small `top_k`, mainly with `expand`.

When the server shortens a result, the result starts with `note: the server cut this result to its content budget …` and a `next:` step. The result is then not complete: narrow the query, lower `top_k`, or search without `expand`.

With `expand`, each hit tells whether its snippet is the whole symbol:

- `[expanded]` — the snippet is the body of the enclosing symbol.
- `[not expanded: <reason> — …]` — the snippet is the chunk only. Do not treat it as the whole symbol. The reasons:
  - `SymbolChunk` — the hit is a function or type chunk, and the snippet is that chunk.
  - `NoEnclosingSymbol` — no named declaration contains the hit.
  - `GraphLoading` — the graph is not ready. Search again later for the body.
  - `Deadline` — the expansion deadline of the server passed. Use `expand_chunk`.
  - `ContentBudget` — the body does not fit in the content budget. Use `expand_chunk`.
  - `MultipleProjects` — the search spans more than one codebase. Only a search of one codebase expands hits.

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
