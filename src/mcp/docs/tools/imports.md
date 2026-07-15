File→file import dependency edges across the codebase.

Each edge is one source file importing something satisfied by another file in the same codebase (`from -> to (import_path)`). Use this to understand a file's intra-codebase dependencies or trace how a module is wired into the rest of the project.

For who-references-a-symbol questions use `find_references`; for *cross-codebase* dependencies use `external_links`.
