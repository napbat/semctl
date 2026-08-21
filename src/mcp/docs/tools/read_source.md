Read a revision-pinned source line or byte range from the content plane. A
supplied revision is a strong content hash and stale hashes are rejected. When
the bound local file has the same verified hash, the returned window is read
locally; otherwise the server copy is authoritative. Use this for indexed
codebases that are not checked out locally or for revision-pinned evidence.
Use host Read for current working-tree bytes at a known local path.
