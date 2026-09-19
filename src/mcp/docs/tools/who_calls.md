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

Symbol-graph coverage follows the language packs registered on the server, not a fixed list. Rust, C#, Go, TypeScript/JavaScript, and C++ resolve on the current service. C++ needs build context in the checkout (a CMakeLists.txt or a compile database): without it, calls appear as plain names and cross-file bindings are missing, and even with it the graph can miss member declarations and member calls, so cross-check an empty C++ result with `grep`. A file in a language without a registered pack has no graph entries — use `search_codebase` or `grep` there.
