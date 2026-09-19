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

Coverage follows the server's registered language packs: Rust, C#, Go, TypeScript/JavaScript, and C++. C++ needs build context (a CMakeLists.txt or compile database) and can miss member declarations and calls — confirm an empty C++ result with `grep`. Other languages aren't on the graph — use `search_codebase` or `grep`.
