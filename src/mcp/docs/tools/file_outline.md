A file's table of contents — every indexed chunk in `path`, in source order, each with its line range, kind (`function` / `container` / `block`), and declared symbol when one was captured.

Use this when you already know the file and want its shape — the methods/types it defines and where they are — without reading the whole thing. Cheaper than opening the file, and the line ranges feed straight into `expand_chunk` or a targeted read.

## Parameters

- `path` (required) — codebase-relative path, e.g. `server/Startup.cs`.

## When to use this vs alternatives

- **file_outline** (this tool): the shape of one known file.
- **search_codebase**: you don't know which file yet.
- **expand_chunk**: you have a line range and want the chunks around it.

## Notes

- Scoped to the selected codebase (the launch/current codebase by default) — a path shared with another codebase resolves to the selected one's chunks.
- Returns "not indexed in this codebase" if the file isn't part of the codebase's index (check `list_files`).
