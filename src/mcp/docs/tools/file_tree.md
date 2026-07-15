The codebase's indexed files as a directory tree — built from the catalog by splitting paths on `/`, directories first then files, each alphabetical. The "show me the layout" view: get the shape of the repo before drilling in.

## Parameters

None — returns the whole tree for the launched codebase.

## When to use this vs alternatives

- **file_tree** (this tool): the directory structure / layout of the codebase.
- **list_files**: a flat, filterable list (and per-file index state).
- **file_outline**: the shape *inside* one file.

## Notes

- Reflects the indexed catalog, not the working tree — a file shows up once it's synced.
- Directories carry their children; files carry a byte size.
