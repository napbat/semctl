Reference→definition symbol bindings across the codebase.

Finer-grained than `imports`: each row binds an imported name in one file to the specific definition it resolves to (`from_file \`name\` -> to_file (moniker)`). The `moniker` is the codebase-internal key for that definition. Use this to see exactly which definition a file's imports land on, across files and crates within the codebase.

For cross-codebase resolution (imports that leave this codebase) use `external_links`.
