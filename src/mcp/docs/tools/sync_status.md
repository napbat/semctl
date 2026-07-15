Status of the most recent index job the server's background sync queued for this codebase.

Use it to tell "still indexing" from "nothing indexed": if `list_files` / `search` come back empty right after attaching, check here — a `queued` or `running` job means the index is still being built, so retry shortly.

Reports the phase (`queued` → `running` → `done`, or `failed`), per-file progress (embedded / deleted / failed over the planned total), the post-sync chunk count once done, and any failure reason. Returns a note instead if nothing has been synced this session yet.
