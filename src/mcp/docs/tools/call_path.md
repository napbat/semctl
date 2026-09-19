Find a shortest call chain from one function to another — the sequence of definitions along one path of `calls` edges from `from` to `to`. Answers "how does X reach Y" / "is Y reachable from X through calls".

Returns the chunks along the path in call order, or empty when no `calls`-edge path exists. Deterministic, exact-name on both ends. Symbol-graph coverage follows the language packs registered on the server — Rust, C#, Go, TypeScript/JavaScript, and C++ on the current service. C++ call edges need build context in the checkout (a CMakeLists.txt or a compile database) and can miss member calls, so an empty C++ path is not proof that none exists. Files in a language without a registered pack produce no call edges.

## Use this when

- You want to see *how* control reaches a function from another (the intermediate hops).
- You're confirming whether one entry point can transitively invoke a given function.

## Use instead

- `who_calls` — the direct callers of a single function.
- `trace` — a symbol's immediate neighbourhood (callers + callees), not a path between two.
- `flows_between` — how a *value* (data), not control, travels from one boundary to another.
