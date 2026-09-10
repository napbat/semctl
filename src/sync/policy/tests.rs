use super::*;

#[test]
fn changed_or_new_rules_invalidate_a_loaded_policy() {
    for existed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let rule = directory.path().join(".semctlignore");
        if existed {
            fs::write(&rule, "old.txt\n").unwrap();
        }
        let cancellation = Cancellation::default();
        let policy = SourcePolicy::load(directory.path(), &cancellation).unwrap();
        fs::write(&rule, "private.txt\n").unwrap();
        assert!(policy.verify(&cancellation).is_err());
    }
}

#[test]
fn custom_rule_precedence_and_nested_whitelists_match_the_walker() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("src");
    fs::create_dir(&child).unwrap();
    fs::write(directory.path().join(".gitignore"), "*.txt\n").unwrap();
    fs::write(directory.path().join(".semctxignore"), "!allowed.txt\n").unwrap();
    fs::write(directory.path().join(".semctlignore"), "allowed.txt\n").unwrap();
    fs::write(child.join(".semctlignore"), "!allowed.txt\n").unwrap();
    let cancellation = Cancellation::default();
    let mut policy = SourcePolicy::load(directory.path(), &cancellation).unwrap();
    policy.load_directory(&child, &cancellation).unwrap();
    assert!(!policy.includes(&directory.path().join("allowed.txt"), false));
    assert!(policy.includes(&child.join("allowed.txt"), false));
    assert!(!policy.includes(&child.join("other.txt"), false));
    assert!(
        policy
            .event_is_relevant(&child.join("allowed.txt"), false, &cancellation)
            .unwrap()
    );
    assert!(
        !policy
            .event_is_relevant(&child.join("other.txt"), false, &cancellation)
            .unwrap()
    );
}

#[test]
fn policy_preserves_ignore_precedence_across_nested_repositories() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let nested = root.join("nested");
    fs::create_dir_all(nested.join(".git/info")).unwrap();
    fs::write(
        nested.join(".git/config"),
        "[core]\nrepositoryformatversion = 0\n",
    )
    .unwrap();
    fs::create_dir_all(nested.join(".git/objects")).unwrap();
    fs::create_dir_all(nested.join(".git/refs")).unwrap();
    fs::write(nested.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(root.join(".gitignore"), "*.tmp\nblocked/\n*.data\n").unwrap();
    fs::write(root.join(".ignore"), "*.txt\n").unwrap();
    fs::write(root.join(".semctxignore"), "!custom.txt\n").unwrap();
    fs::write(root.join(".semctlignore"), "custom.txt\n").unwrap();
    fs::write(nested.join(".gitignore"), "!allowed.data\n").unwrap();
    fs::write(nested.join(".semctlignore"), "!custom.txt\n").unwrap();
    fs::write(nested.join(".git/info/exclude"), "private.rs\n").unwrap();
    for name in [
        "main.rs",
        "hidden.tmp",
        "custom.txt",
        "nested/custom.txt",
        "nested/allowed.data",
        "nested/hidden.data",
        "nested/private.rs",
        "blocked/private.rs",
    ] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture\n").unwrap();
    }
    let mut builder = ignore::WalkBuilder::new(root);
    builder.require_git(false);
    for name in IGNORE_FILES {
        builder.add_custom_ignore_filename(name);
    }
    let mut expected: Vec<_> = builder
        .build()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.path().strip_prefix(root).unwrap().to_path_buf())
        .collect();
    expected.sort();
    let actual = crate::sync::walker::walk(
        root,
        &crate::sync::walker::WalkOptions::default(),
        &Cancellation::default(),
    )
    .unwrap();
    let paths: Vec<_> = actual
        .candidates
        .iter()
        .map(|entry| PathBuf::from(&entry.rel))
        .collect();
    assert_eq!(paths, expected);
}

#[test]
fn ignored_directory_rules_are_not_loaded() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(directory.path().join("ignored/.semctlignore")).unwrap();
    fs::write(directory.path().join("main.rs"), "fn main() {}\n").unwrap();
    let walk = crate::sync::walker::walk(
        directory.path(),
        &crate::sync::walker::WalkOptions::default(),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(walk.candidates.len(), 1);
    assert_eq!(walk.candidates[0].rel, "main.rs");
}

#[test]
fn explicit_whitelists_cannot_expose_private_recovery_files() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(".semctlignore"), "!*\n").unwrap();
    let recovery = directory.path().join(".semctl-123456abcdef-0.edit");
    fs::create_dir(&recovery).unwrap();
    fs::write(recovery.join("preimage"), "private fixture\n").unwrap();
    fs::write(
        directory.path().join(".semctl-123456abcdef-1.tmp"),
        "private fixture\n",
    )
    .unwrap();
    fs::write(
        directory.path().join(".semctl-123456abcdef-1.bak"),
        "private fixture\n",
    )
    .unwrap();
    fs::write(directory.path().join("main.rs"), "fn main() {}\n").unwrap();
    let cancellation = Cancellation::default();
    let mut policy = SourcePolicy::load(directory.path(), &cancellation).unwrap();
    assert!(
        !policy
            .event_is_relevant(&recovery.join("preimage"), false, &cancellation)
            .unwrap()
    );
    let walk = crate::sync::walker::walk(
        directory.path(),
        &crate::sync::walker::WalkOptions::default(),
        &cancellation,
    )
    .unwrap();
    assert!(
        walk.candidates
            .iter()
            .all(|entry| !is_private_path(Path::new(&entry.rel)))
    );
    assert!(walk.candidates.iter().any(|entry| entry.rel == "main.rs"));
}

#[cfg(unix)]
#[test]
fn retargeting_a_git_directory_symlink_invalidates_the_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let marker = directory.path().join(".git");
    std::os::unix::fs::symlink(&first, &marker).unwrap();
    let mut sources = Sources::default();
    sources.observe(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    std::os::unix::fs::symlink(&second, &marker).unwrap();
    assert!(sources.verify(&Cancellation::default()).is_err());
}

#[test]
fn private_policy_probe_events_cannot_create_a_watch_feedback_loop() {
    let root = Path::new("/checkout");
    let observed = std::collections::HashSet::new();
    assert!(!event_may_affect_policy(
        root,
        Path::new("/tmp/.semctl-123456abcdef-1.tmp"),
        &observed
    ));
    assert!(!event_may_affect_policy(
        root,
        Path::new("/checkout/.semctl-123456abcdef-1.tmp"),
        &observed
    ));
    assert!(!event_may_affect_policy(
        root,
        Path::new("/tmp/unrelated"),
        &observed
    ));
    assert!(event_may_affect_policy(
        root,
        Path::new("/checkout/main.rs"),
        &observed
    ));
}
