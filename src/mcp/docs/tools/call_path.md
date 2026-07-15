Find a shortest call chain from one function to another — the sequence of definitions along one path of `calls` edges from `from` to `to`. Answers "how does X reach Y" / "is Y reachable from X through calls".

Returns the chunks along the path in call order, or empty when no `calls`-edge path exists. Deterministic, exact-name on both ends. Symbol-graph coverage: Rust, C#, Go, TypeScript/JavaScript — files in other languages produce no call edges.

## Use this when

- You want to see *how* control reaches a function from another (the intermediate hops).
- You're confirming whether one entry point can transitively invoke a given function.

## Use instead

- `who_calls` — the direct callers of a single function.
- `trace` — a symbol's immediate neighbourhood (callers + callees), not a path between two.
- `flows_between` — how a *value* (data), not control, travels from one boundary to another.
