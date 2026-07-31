Total indexed state for a codebase, plus the most recent sync job this MCP session queued when one exists.

Omit `codebase` for the launch/current repository, or pass either a codebase id or an indexed local directory path. Path-based access also keeps that checkout watched while this MCP session is active.

Reports whether a local checkout watcher is active. The total section always reports the catalog's file count and source bytes, so a no-op latest run cannot make an already-populated index look empty. When a latest job is known, it also reports the post-sync total chunk count, phase (`queued` → `running` → `done`, or `failed`), and per-run embedded/deleted/failed progress. While the first local scan is still preparing its server job, status says so rather than claiming nothing is happening.

Unlike retrieval/catalog/graph tools, this tool does not wait on a first-index readiness gate, so it can monitor that initial job.
