Find the types that implement a trait/interface — the reverse `implements` edge over the precise type graph. PRIMARY tool for "what implements X" / "what are the concrete types behind this trait".

Returns each implementing type as a chunk at its declaration. Deterministic and exact-name. Empty means the graph records no implementors — cross-check with `grep` for languages outside the graph.

## Use this when

- You have a trait/interface and want every concrete implementor.
- You're reasoning about dynamic dispatch — the candidate types behind a `dyn Trait` / interface call.

## Use instead

- `find_references` — every mention of the trait name, not just implementors.
- `who_calls` — callers of a method, not implementors of a type.

## Coverage

Symbol-graph languages: Rust, C#, Go, TypeScript/JavaScript. Files in other languages aren't on the graph — use `search_codebase` or `grep` there.
