Total indexed state for a codebase, plus the most recent sync job this MCP session queued when one exists.

Omit `codebase` for the launch/current repository, or pass either a codebase id or an indexed local directory path. Path-based access also keeps that checkout watched while this MCP session is active.

The answer leads with the current failure or phase. Expanded status evidence separates persisted catalog totals, current local work, and the last recorded server job. Catalog file and source-byte totals are independent of the latest job's plan. An unavailable catalog request leaves totals unknown, not zero. A completed or zero-file job does not establish checkout freshness.

When available, server counters retain their names: embedded, deleted, failed, planned embed, planned delete, and reported chunks. Missing counters are unknown. Do not infer a processed/total ratio or a completion percentage from these counts. The last server job is historical evidence, not necessarily current work. Job labels are Pending, Running, Done, or Failed; server errors and failed files take precedence over a success label.

The first-index phase is one of:

- registering: `index_codebase` has not yet registered the codebase.
- syncing: the codebase exists, and the scan and upload run.
- embedding: the upload is done, and the server embeds the files. A server job is named when known.
- ready: the first index is complete, and retrieval tools serve it.
- failed: the first index ended without a usable index. Inspect and correct the reported failure before calling `index_codebase` for the same path to retry.

The first-index phase is absent when this MCP server has no first index for the checkout, such as a checkout indexed in an earlier session. Call this tool every 60 seconds while a first index runs. Unlike retrieval/catalog/graph tools, this tool does not wait on the first-index readiness gate.

Local work can report preparation, scanning, scanned-file counts, uploading, or finalization. A local upload done/total pair counts only the files requested for that upload, not every checkout file or the server's embedding work. Local progress is absent when no scan/upload runs, including while the server embeds after upload. Read the phase and named evidence rather than relying on line order, a progress ratio, or a fixed `first index:` / `current sync:` layout. This is a snapshot, not live push progress or a measured elapsed time.

The watcher evidence reports the actual state and the reason when watching is unavailable. Periodic reconciliation may still run without an active watcher. Local scan/upload failures stay visible even when the previous server job completed. Complete local, server, watcher, and status-request diagnostics remain in the evidence; a compact preview does not replace them.

Correct the reported local access, authentication, or connectivity failure. Retained local work retries on the reported schedule when one exists; no reported delay is not a promise of an immediate retry. For a failed first index, correct the cause before calling `index_codebase` again. For unavailable status, resolve access/connectivity and call `sync_status` again. A failed background job is status evidence, not itself a failed `sync_status` RPC. Do not request a reindex merely because a snapshot is incomplete or an indexed location is unverified.
