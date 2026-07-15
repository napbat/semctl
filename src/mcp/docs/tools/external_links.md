Cross-codebase links — this codebase's external imports resolved into the public API of *other* codebases you can see.

Each row is an import that leaves this codebase (`from_file \`import_path\``) resolved to a public definition in another codebase (`-> [target_codebase_id] target_file \`target_name\``). Use this for cross-repo "jump to definition": where does a dependency this codebase pulls in actually get defined, in another indexed codebase.

Only definitions in codebases visible to you (within your tenant) are matched; an import that resolves nowhere visible simply doesn't appear. When several visible codebases export the same coordinate, every candidate is returned ranked by match confidence — expect multiple rows rather than a single collapsed guess.
