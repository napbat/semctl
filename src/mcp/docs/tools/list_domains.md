List the engine's registered search domains and the tag schema each one exposes.

A domain is an ingestion source (code, and — in future — history, chat, tickets, etc.). Domain *ids* go in `search_codebase`'s `domains` parameter to scope the fan-out. Each domain also declares the tags it stamps on chunks; of those, `kind` is the one filterable through `search_codebase` (via its `kinds` parameter).

Use this to discover what's searchable before scoping a query.
