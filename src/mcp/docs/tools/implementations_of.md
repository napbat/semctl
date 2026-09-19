Find the types that implement a trait/interface — the reverse `implements` edge over the precise type graph. PRIMARY tool for "what implements X" / "what are the concrete types behind this trait".

Returns each implementing type as a chunk at its declaration. Deterministic and exact-name. Empty means the graph records no implementors — cross-check with `grep` for languages outside the graph.

## Use this when

- You have a trait/interface and want every concrete implementor.
- You're reasoning about dynamic dispatch — the candidate types behind a `dyn Trait` / interface call.

## Use instead

- `find_references` — every mention of the trait name, not just implementors.
- `who_calls` — callers of a method, not implementors of a type.

## Coverage

Symbol-graph coverage follows the language packs registered on the server, not a fixed list. Rust, C#, Go, TypeScript/JavaScript, and C++ resolve on the current service. C++ needs build context in the checkout (a CMakeLists.txt or a compile database): without it, calls appear as plain names and cross-file bindings are missing, and even with it the graph can miss member declarations and member calls, so cross-check an empty C++ result with `grep`. A file in a language without a registered pack has no graph entries — use `search_codebase` or `grep` there.

C++ records inheritance as an extends relation, not as implements, so this tool returns nothing for a C++ base class even when derived classes exist. Use `type_hierarchy` for C++ base and derived classes; an empty result here is not evidence that the hierarchy is missing.
