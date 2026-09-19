Find the types that implement a trait/interface — the reverse `implements` edge over the precise type graph. PRIMARY tool for "what implements X" / "what are the concrete types behind this trait".

Returns each implementing type as a chunk at its declaration. Deterministic and exact-name. Empty means the graph records no implementors — cross-check with `grep` for languages outside the graph.

## Use this when

- You have a trait/interface and want every concrete implementor.
- You're reasoning about dynamic dispatch — the candidate types behind a `dyn Trait` / interface call.

## Use instead

- `find_references` — every mention of the trait name, not just implementors.
- `who_calls` — callers of a method, not implementors of a type.

## Coverage

Coverage follows the server's registered language packs: Rust, C#, Go, TypeScript/JavaScript, and C++. C++ needs build context (a CMakeLists.txt or compile database) and can miss member declarations and calls — confirm an empty C++ result with `grep`. Other languages aren't on the graph — use `search_codebase` or `grep`.

C++ inheritance is an extends relation, not implements: use `type_hierarchy` for C++ base and derived classes.
