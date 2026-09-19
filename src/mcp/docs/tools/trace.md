A symbol's neighbourhood in one call: its **definition** plus its **direct
callers and callees**. Use this when you're trying to understand how a piece of
code is wired up and would otherwise fan out by hand (find_definition →
find_references → chase each callee).

Backed by the symbol graph + call graph the chunker builds during indexing —
deterministic, no embedding round-trip. Composes the `definitions` and
`call-graph` endpoints so you don't have to.

## Parameters

- `symbol` (required) — the exact symbol name to centre on (e.g.
  `ConfigureAuthentication`, `CodeDomain::search`). Leaf and composite forms both
  work.
- `depth` — how many call-graph hops to include, 1–10. Defaults to 1 (immediate
  callers/callees); raise it to widen the blast radius.

## Use this when

- "How does X work / what calls X / what does X call?" — one call instead of three.
- Scoping the blast radius of a change to a function or type.

## Use something else when

- You only want the definition site → `find_definition` (lighter).
- You want every reference, not just the call neighbourhood → `find_references`.
- You don't know the exact name → `search_codebase` first, then trace the symbol
  it surfaces.

## Output

The definition chunk(s), then a `callers` group and a `callees` group, each a
compact `path:line-range symbol` list. An empty group prints `(none)`. If the
symbol has no definition, returns the same "did you mean" suggestions as
`find_definition`.

Symbol-graph coverage follows the language packs registered on the server —
Rust, C#, Go, TypeScript/JavaScript, and C++ resolve on the current service.
C++ callers and callees need build context in the checkout (a CMakeLists.txt
or a compile database) and can still miss member calls, so cross-check an
empty C++ group with `grep`. Files in a language without a registered pack
aren't on the graph; use `search_codebase` or `grep` there.
