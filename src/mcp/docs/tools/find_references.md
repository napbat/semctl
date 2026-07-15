Find references to a symbol — the resolved usages (calls, imports, type mentions) that bind to an in-codebase definition. PRIMARY tool for "who uses X" when you know the exact name; for "who *calls* X" prefer `who_calls`.

Backed by the symbol graph, same precision tradeoff as `find_definition`: deterministic, exact-name, case-sensitive. Each hit is pinned to the use site's line.

Precise, not exhaustive: only references that resolve to a definition in this codebase count — uses of std/dependency symbols return nothing, repeat uses inside one chunk collapse to the first, and strings/comments/markdown never match. When you need EVERY occurrence (rename impact, log strings, docs), use `grep`.

On a miss you get "Did you mean" suggestions — retry with one before falling back.

## Use this when

- You want the code sites that actually bind to a known symbol's definition.
- You're scoping the blast radius of a change to a function or type (pair with `who_calls` / `trace`).

## Use instead

- `who_calls` — just the calling definitions ("who calls X").
- `grep` — every literal occurrence, including strings, comments, and unsupported languages.
- `search_codebase` — you only have a description, not the exact name.

## Coverage

Symbol-graph languages: Rust, C#, Go, TypeScript/JavaScript. Files in other languages aren't on the graph — use `search_codebase` or `grep` there.
