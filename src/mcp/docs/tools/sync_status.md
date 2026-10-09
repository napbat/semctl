Total indexed state for a codebase, plus the most recent sync job this MCP session queued when one exists.

Omit `codebase` for the launch/current repository, or pass either a codebase id or an indexed local directory path. Path-based access also keeps that checkout watched while this MCP session is active.

Reports whether a local checkout watcher is active. The total section always reports the catalog's file count and source bytes, so a no-op latest run cannot make an already-populated index look empty. When a latest job is known, it also reports the post-sync total chunk count, phase (`queued` → `running` → `done`, or `failed`), and per-run embedded/deleted/failed progress. While the first local scan is still preparing its server job, status says so rather than claiming nothing is happening.

While a first index is running, or after it ended, the answer has a `first index:` line after the codebase line. Its value is one of these phases:

- registering: `index_codebase` has not yet registered the codebase.
- syncing: the codebase exists, and the scan and upload run.
- embedding: the upload is done, and the server embeds the files. The line names the server job when it is known.
- ready: the first index is complete, and retrieval tools serve it.
- failed: the first index ended without a usable index. The line gives the reason. Call `index_codebase` for the same path to retry.

The line is absent when the MCP server has no first index for the checkout, such as a checkout that was indexed in an earlier session. Call this tool every 10 to 15 seconds while a first index runs.

While a sync runs for the checkout, the answer has a `current sync:` line after the `first index:` line. This holds for a first index and for a later re-sync. When there is no `first index:` line, the `current sync:` line follows the codebase line. The line shows the latest step of the sync:

- `current sync: preparing index`: the sync started.
- `current sync: scanning files in <path>`: the sync reads the checkout.
- `current sync: scanned <n> files (<m> filter decisions reused) — checking for changes`: the scan is done, and the sync compares the files with the server.
- `current sync: uploading <done>/<total> files`: the sync uploaded `<done>` of `<total>` files. `<total>` counts only the files that the server needs, not all files in the checkout.
- `current sync: finalizing upload`: all files are uploaded, and the sync completes the upload.

During a first index, the `first index: syncing` line and the `current sync:` line together show how far the first index has come. The `current sync:` line is absent while no sync runs, for example while the server embeds the files after the upload. Read the `first index:` line for the phase.

Unlike retrieval/catalog/graph tools, this tool does not wait on a first-index readiness gate, so it can monitor that initial job.
