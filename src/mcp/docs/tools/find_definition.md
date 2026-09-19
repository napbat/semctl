Locate where a symbol is defined. PRIMARY tool when you already know the exact name — faster + more precise than `search_codebase` (no embedding round-trip, deterministic).

Backed by the symbol graph the chunker builds during indexing. Accepts both leaf names (`verify_token`) and composite forms (`Foo::bar`). Matching is exact and case-sensitive, and every same-named definition in the codebase comes back — expect multiple hits for common names.

On a miss you get "Did you mean" suggestions (from a backing search) — retry with one before falling back to `search_codebase`.

## Use this when

- You can spell the symbol exactly (e.g. `verify_token`, `CodeDomain::search`).
- A previous search hit named a symbol and you want its definition site.

## Use `search_codebase` instead when

- You only have a description ("the function that validates JWTs") not the name.
- You're not sure if the symbol exists at all.

## Coverage

Symbol-graph coverage follows the language packs registered on the server, not a fixed list. Rust, C#, Go, TypeScript/JavaScript, and C++ resolve on the current service. C++ needs build context in the checkout (a CMakeLists.txt or a compile database): without it, calls appear as plain names and cross-file bindings are missing, and even with it the graph can miss member declarations and member calls, so cross-check an empty C++ result with `grep`. A file in a language without a registered pack has no graph entries — use `search_codebase` or `grep` there.
