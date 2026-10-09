Explicitly opt a local directory into semctl indexing and keep it watched for the rest of this MCP session.

Call this only after the user has agreed to index a genuinely unindexed directory. Never infer first-time consent from a search request, an unindexed-repository notice, or a failed code tool. A repository that resolves to an existing index already has prior consent and should be synced/watched automatically through normal codebase-scoped tools without asking again. Omit `path` to index the MCP launch directory, or pass an absolute directory path.

For a first-ever index, the tool registers the codebase, starts the scan and upload in a background task, and returns at once. It does not wait for the server to embed the files. The answer names the codebase id and the path. Call `sync_status` every 10 to 15 seconds to follow the first index, and use local Read/Grep until it completes. If the call times out or is cancelled, the first index continues in the MCP server.

All codebase-scoped retrieval/catalog/graph tools targeting that repository check the same readiness gate, so they cannot observe a partial first index: each waits at most 5 seconds, then fails with a "still running" error. `sync_status` deliberately remains available and reports the first-index phase: registering, syncing, embedding, ready, or failed.

A second call for the same path while the first index runs does not start another one. It answers "already in progress" with the same instruction. After the first index completes, a call answers that it is complete. If the first index failed, call this tool again with the same path to retry it.

For a previously indexed repository, this starts the normal background re-sync/watcher immediately without a first-index gate.
