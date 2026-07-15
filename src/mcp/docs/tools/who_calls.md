Find the callers of a function — every definition that calls `symbol`, resolved over the precise call graph (the inverse `calls` edge). PRIMARY tool for "who calls X" when you know the exact name.

Returns the calling definitions as chunks at their declaration sites. Deterministic and exact-name. Empty means the graph records no callers — cross-check with `grep` before concluding a public symbol is unused (dynamic/reflective calls and files outside the graph languages produce no call edges).

## Use this when

- You want the immediate callers of a function/method.
- You're scoping the blast radius of changing a function's signature or behaviour.

## Use instead

- `find_references` — every *usage* (imports, type mentions, value uses), not just calls.
- `call_path` — *how* one function reaches another, not just the direct callers.
- `trace` — a symbol's definition plus callers AND callees in one shot.

## Coverage

Symbol-graph languages: Rust, C#, Go, TypeScript/JavaScript. Files in other languages aren't on the graph — use `search_codebase` or `grep` there.
