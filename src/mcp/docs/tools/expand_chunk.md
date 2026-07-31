Return the indexed chunks that overlap an inclusive line range in a file — the way to "grow context" around a hit. A search or `find_definition` result gives you a `path:line-range`; pass it here (widened to taste) to pull the neighbouring chunks in source order, so you see the surrounding declarations without reading the whole file.

## Parameters

- `path` (required) — codebase-relative path, e.g. `server/Startup.cs`.
- `line_start` (required) — first line of the range (1-based, inclusive).
- `line_end` (required) — last line of the range (1-based, inclusive).

## When to use this vs alternatives

- **expand_chunk** (this tool): you have a location and want the chunks around it.
- **file_outline**: you want the whole file's shape, not a neighbourhood.
- **search_codebase**: you don't have a location yet.

## Notes

- Half-overlap counts: a chunk is returned if it touches the range at all.
- Scoped to the selected codebase (the launch/current codebase by default). Empty result = nothing indexed overlaps that range (or the file isn't in this codebase).
