File→file import dependency edges across the codebase.

Each edge is one source file importing something satisfied by another file in the same codebase (`from -> to (import_path)`). Use this to understand a file's intra-codebase dependencies or trace how a module is wired into the rest of the project.

Coverage follows the language pack. C# has no file-level imports, and C++ include edges were empty in the September 2026 audit even with build context, so use `symbol_edges` for C++ cross-file bindings.

For who-references-a-symbol questions use `find_references`; for *cross-codebase* dependencies use `external_links`.
