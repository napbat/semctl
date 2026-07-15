Resolve many symbols in one call — for each name, its definitions (or references), returned in the order requested. One round-trip instead of N `find_definition` calls; use it when you have a set of identifiers (e.g. every symbol on a screen, or a list you collected) and want them all resolved at once.

## Parameters

- `symbols` (required) — exact symbol names to resolve (cap 256 per call).
- `references` (optional) — return references instead of definitions for each. Defaults to false.

## When to use this vs alternatives

- **batch_lookup** (this tool): several known names to resolve together.
- **find_definition** / **find_references**: a single name.
- **search_codebase**: you don't have exact names yet.

## Notes

- Each result echoes its `symbol`, so you can match results back without re-deriving keys; a name with no match comes back with an empty hit list (not an error).
- Same symbol graph as `find_definition` — exact, case-sensitive name match, scoped to the launched codebase. Graph coverage: Rust, C#, Go, TypeScript/JavaScript.
