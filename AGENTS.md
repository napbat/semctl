# Repository Instructions

These instructions apply to the complete repository.

## Communication

- Use clear, STE-like English in documentation, comments, help text, logs, and
  change summaries.
- Use short sentences and the active voice.
- Put one instruction or fact in each sentence when practical.
- Use one term for one concept. Do not change terms without a reason.
- Define an acronym on first use unless it is a standard Rust, Cargo, HTTP, or
  MCP term.
- Avoid idioms, slang, filler, rhetorical questions, and vague pronouns.
- State the actor and the action when ambiguity is possible.
- Preserve exact protocol names, command names, code identifiers, and quoted
  external text. Controlled language must not reduce technical accuracy.

## Scope and Change Discipline

- Make the smallest cohesive change that solves the stated problem.
- Preserve unrelated work in the working tree.
- Read the current implementation before editing it.
- Follow an existing local pattern when that pattern is correct.
- Fix a root cause. Do not add a symptom-only workaround when a shared fix is
  available.
- Do not perform an unrelated cleanup in the same change.
- Keep backward compatibility at protocol, configuration, and command-line
  boundaries unless the task explicitly removes it.
- Do not log credentials, tokens, private source, or other secrets.

## Cargo and Rust Layout

- Keep `Cargo.toml` and `Cargo.lock` at the package root.
- Keep product source in `src/`.
- Keep the primary binary entry point at `src/main.rs`.
- Put additional binaries in `src/bin/` only when the repository needs a
  separate executable.
- Put integration tests in the top-level `tests/` directory.
- Put private unit tests next to their implementation in one
  `#[cfg(test)] mod tests` block.
- Name an external unit-test module `tests.rs` and declare it as
  `#[cfg(test)] mod tests;`.
- Do not use descriptive test-module names such as `page_envelope_tests` or
  `gateway_error_tests`. Put descriptive names on the test functions.
- Use `snake_case` for Rust module and source-file names.

## Rust File Size and Module Boundaries

- Keep each hand-written Rust file near or below 1,000 physical lines.
- Start a split before a file grows materially beyond 1,000 lines.
- When a touched file is already over the limit, do not add another
  responsibility to it. Extract a cohesive responsibility when the extraction
  is safe and relevant to the task.
- Split by ownership and behavior. Do not split at an arbitrary line number.
- Give each module one clear purpose and a narrow interface.
- Keep orchestration and public re-exports in the parent module.
- Move implementation details into named child modules.
- Do not create dumping-ground modules named `utils`, `common`, or `helpers`.
- Generated code and immutable external fixtures are exempt from the line
  target. Do not edit generated code by hand.
- If cohesion requires a temporary size exception, explain the exception in the
  change summary.

## Design and DRY Rules

- Keep one source of truth for each protocol shape, validation rule, state
  transition, and algorithm.
- Reuse shared domain types. Do not create a second wire type for the same
  response shape.
- Route all triggers for one behavior through one engine. Transport, scheduling,
  progress, and readiness wrappers must delegate to that engine.
- Extract repeated logic after the shared responsibility is clear.
- Do not add an abstraction for a single speculative future use.
- Prefer small functions with explicit names and one level of responsibility.
- Separate pure decisions from file, process, network, and clock I/O.
- Keep platform-specific behavior behind a narrow `cfg` boundary.
- Use the narrowest practical visibility. Prefer private or `pub(crate)` items
  over public items.
- Prefer typed state and enums over loosely related booleans or strings.
- Make ownership and cancellation behavior explicit in asynchronous code.
- Do not block a Tokio worker with filesystem walks, process waits, or other
  blocking work.

## Errors and Defensive Code

- Return useful errors at recoverable boundaries.
- Add context to file, process, parse, and network failures.
- Do not discard an error unless the operation is explicitly best effort.
- Document why a best-effort failure is safe.
- Avoid `unwrap` and `expect` in production paths.
- Use `expect` only for a proven invariant. State the invariant in its message.
- Validate external data at the boundary. Keep internal code strongly typed.
- Fail closed when an operation can corrupt, delete, or misidentify indexed
  state.

## Formatting and Lints

- Format Rust with `cargo fmt`.
- Treat every `clippy::pedantic` warning as a defect. This workspace enables the
  lint group in `Cargo.toml`.
- Do not add a broad `allow` attribute to silence Clippy.
- If a lint is a false positive, use the narrowest item-level `allow` and explain
  the invariant on the same line.
- Keep imports minimal and let rustfmt format them.
- End text files with one newline.
- Do not leave trailing whitespace, dead code, commented-out code, or temporary
  diagnostics.
- Write comments that explain a constraint or a reason. Do not restate the code.
- Document non-obvious invariants and public behavior.

## Tests

- Add a regression test for each fixed defect when a deterministic test is
  practical.
- Test public behavior and important boundary cases.
- Keep unit tests deterministic and offline.
- Name tests after the behavior and expected result.
- Reuse test builders and fixtures when they improve clarity.
- Do not share mutable global test state.
- Use integration tests only for behavior that must cross the package boundary.
- Keep compatibility tests for current and legacy wire formats when both remain
  supported.

## Required Verification

Run the checks that cover every changed area. Run the complete Rust gate before
handoff unless the environment prevents it.

```text
cargo fmt --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --locked
git diff --check
```

The Bun version in `mise.toml` is the single source of truth. When the OMP
TypeScript adapter changes, obtain Bun through mise and run:

```text
mise exec -c "bun test plugins/semctx/adapters/omp/index.test.ts"
```

- Report each skipped check and its reason.
- Do not claim completion while a relevant check is failing.
