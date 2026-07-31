Explicitly opt a local directory into semctl indexing and keep it watched for the rest of this MCP session.

Call this only after the user has agreed to index a genuinely unindexed directory. Never infer first-time consent from a search request, an unindexed-repository notice, or a failed code tool. A repository that resolves to an existing index already has prior consent and should be synced/watched automatically through normal codebase-scoped tools without asking again. Omit `path` to index the MCP launch directory, or pass an absolute directory path.

For a first-ever index, the tool registers the codebase and starts the scan/upload in a detached task, then waits for the server to finish embedding every file. All codebase-scoped retrieval/catalog/graph tools targeting that repository wait on the same readiness gate, so they cannot observe a partial first index. `sync_status` deliberately remains available for progress checks. If the initiating tool call times out or is cancelled, indexing and the readiness gate continue in the MCP server.

For a previously indexed repository, this starts the normal background re-sync/watcher immediately without a first-index gate.
