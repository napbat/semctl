Report the effective server, tenant, codebase, checkout root, opaque source
identity, watcher state, indexed revision, and graph freshness. Use this when a
tool appears scoped to the wrong repository or stale generation. Tokens and
credentials are never included.

Indexed copies are listed by copy ID and exact source identity, with canonical
status, the current-checkout match, revision and file count. Other copies remain
intact. The codebase's graph state does not establish per-copy freshness.
