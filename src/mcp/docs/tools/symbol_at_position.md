Resolve the symbol at a position. Two modes:

- **Without `column`** — the innermost enclosing definition (function / type / member) whose body covers the line, returned as a hover-style hit. The "what am I inside" lookup.
- **With `column`** — the identifier directly under the cursor at `line:column`, resolved to *its* definition (go-to-definition). On `foo(bar())` a column on `bar` returns `bar`'s definition, not `foo`'s.

## Parameters

- `path` (required) — codebase-relative path, e.g. `server/Startup.cs`.
- `line` (required) — 1-based line number.
- `column` (optional) — 1-based column. Omit for the enclosing-definition hover; set it to resolve the identifier under the cursor.

## When to use this vs alternatives

- **symbol_at_position** (this tool): you have a `file:line` (+ optional column) and want the symbol there.
- **file_outline**: you want the whole file's shape, not one position.
- **find_definition**: you already know the symbol *name* and want where it's defined.

## Notes

- With a column, a cursor on whitespace/punctuation — or on a word that isn't a known symbol — falls back to the enclosing definition (never worse than the line-only answer).
- Returns "(no symbol at …)" when nothing is indexed over the position — a normal empty result, not an error.
- Files outside the symbol-graph languages (Rust, C#, Go, TypeScript/JavaScript) have no symbols to resolve.
- Scoped to the launched codebase.
