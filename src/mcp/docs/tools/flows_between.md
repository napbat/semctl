Inter-procedural value flow witness: the functions a value passes through as it travels from external boundary `from` to external boundary `to`. Answers "show me the path a value takes from X to Y across the program".

This is a *neutral data-flow* surface — composed from per-function summaries along resolved call edges (no inlining), it returns the function chunks that carry the value: the function that calls the `from` boundary, the functions the value composes through, and the function that calls the `to` boundary. Empty when no such flow exists — never a fabricated path. Flow coverage follows the symbol graph: Rust, C#, Go, TypeScript/JavaScript.

A **boundary** is an un-indexed callee the corpus crosses — a library/stdlib call (e.g. `env/var`, `fs/write`, a crate path). Both ends match a boundary moniker exactly or as a substring.

## Use this when

- You want to see the concrete functions data passes through between two library calls.
- You're confirming whether a value from one boundary can reach another, and through what.

## Use instead

- `reaches` / `flows_into` — just the *set* of connected boundaries, not the function witness.
- `call_path` — control flow between two functions, not data flow between two boundaries.
