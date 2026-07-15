Inter-procedural value flow, forward: given a value entering the codebase from an external boundary `from`, list the external boundaries that value flows out to. Answers "where does data coming from X end up".

This is a *neutral data-flow* surface — it tracks how values move across function calls, returns, and field writes, composed from per-function summaries. It makes no judgment about whether a flow is good or bad; that labelling (what counts as a sensitive source/sink) is a separate layer.

A **boundary** is an un-indexed callee the corpus crosses — a library/stdlib call (e.g. `env/var`, `fs/write`, a crate path). `from` matches a boundary moniker exactly or as a substring.

Returns the list of destination boundary monikers, or empty when the entering value doesn't reach any other boundary. Flow coverage follows the symbol graph: Rust, C#, Go, TypeScript/JavaScript.

## Use this when

- You have a value produced by a library call and want to know which other library calls it can reach.
- You're reasoning about how data propagates across function boundaries.

## Use instead

- `flows_into` — the backward dual: what reaches a given boundary.
- `flows_between` — the *functions* a value passes through between two boundaries (the witness).
- `call_path` — control flow (who-calls-whom), not data flow.
