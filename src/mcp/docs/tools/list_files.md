List the files the codebase's catalog currently holds, each with its size in bytes.

Use to orient before drilling in — confirm a file is actually catalogued, or scope to a subtree.

Three ways to use it:
- **Default**: omit every argument to get the first page — up to 1000 files. If the catalog is larger, the result footer reads `rows 1–1000 of N; pass page=1 for the next 1000`, so you know there's more and how to fetch it. It does **not** silently return only part of a small repo — under 1000 files you get everything in one call.
- **Filter**: pass `path` to keep only files whose codebase-relative path contains that substring, case-insensitive (e.g. `path: "src/auth"` or `path: ".rs"`). The filter searches the **entire** catalog across all pages, so it never misses matches past the first 1000.
- **Scroll**: omit `path` and pass a zero-based `page` (and optionally a smaller `page_size`, 1–1000) to walk a large catalog page by page. The footer reports `rows X–Y of N` and the next page to request; request pages until the footer shows no "next".

Note: an empty result means the catalog has no rows yet — either indexing is still running (check `sync_status`) or nothing has been synced. The catalog and the searchable index are populated by the same sync, so once a sync is `done` they agree; a catalog that stays empty while `search_codebase` returns hits is an inconsistency to investigate, not an expected state.
