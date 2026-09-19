Inter-procedural value flow, backward: given an external boundary `to`, list the external boundaries whose entering value reaches it. Answers "where does the data arriving at Y come from". The backward dual of `reaches`.

This is a *neutral data-flow* surface — it tracks how values move across calls, returns, and field writes, composed from per-function summaries, with no judgment about whether a flow is sensitive.

A **boundary** is an un-indexed callee the corpus crosses — a library/stdlib call (e.g. `env/var`, `fs/write`, a crate path). `to` matches a boundary moniker exactly or as a substring.

Returns the list of source boundary monikers, or empty when nothing flows in. Flow coverage follows the resolved symbol graph and its boundary monikers. It is confirmed on Rust, C#, Go, and TypeScript/JavaScript. On C++ it needs build context in the checkout for resolved calls, and external declarations are often incomplete, so an empty C++ result is not evidence that no flow exists.

## Use this when

- You have a library call that consumes a value and want to know which library-produced values can reach it.
- You're tracing data provenance backward across function boundaries.

## Use instead

- `reaches` — the forward dual: where a value from a boundary goes.
- `flows_between` — the *functions* a value passes through between two boundaries.
- `find_references` — textual usages, not data flow.
