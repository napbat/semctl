use std::sync::{Arc, Barrier};

use super::*;

#[test]
fn blank_server_overrides_fall_through_to_the_next_candidate() {
    for blank in ["", " ", "\t\n"] {
        assert_eq!(
            server_url_from(Some(blank), Some(blank), Some("http://configured")),
            "http://configured"
        );
        assert_eq!(
            server_url_from(
                Some(blank),
                Some("http://environment"),
                Some("http://configured")
            ),
            "http://environment"
        );
    }
    assert_eq!(
        server_url_from(
            Some("http://cli"),
            Some("http://environment"),
            Some("http://configured")
        ),
        "http://cli"
    );
}

#[test]
fn concurrent_config_updates_preserve_each_change() {
    let directory = tempfile::tempdir().unwrap();
    let current = directory.path().join("config.toml");
    let legacy = directory.path().join("legacy.toml");
    update_at(&current, &legacy, |cfg| {
        cfg.active_tenant = Some("retained".into());
    })
    .unwrap();
    let ready = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for index in 0..8 {
            let ready = ready.clone();
            let current = &current;
            let legacy = &legacy;
            scope.spawn(move || {
                ready.wait();
                update_at(current, legacy, |cfg| {
                    cfg.codebase_cache
                        .insert(format!("repo-{index}"), format!("id-{index}"));
                })
                .unwrap();
            });
        }
    });
    let stored = load_from(&current, &legacy).unwrap();
    assert_eq!(stored.active_tenant.as_deref(), Some("retained"));
    assert_eq!(stored.codebase_cache.len(), 8);
}

#[test]
fn failed_migration_preserves_all_legacy_files() {
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory.path().join("semctx");
    let current = directory.path().join("semctl");
    fs::create_dir_all(legacy.join("credentials.json")).unwrap();
    fs::write(legacy.join("config.toml"), "active_tenant = 'retained'\n").unwrap();

    assert!(migrate_from_paths(&legacy, &current).is_err());

    assert!(legacy.join("credentials.json").is_dir());
    assert_eq!(
        fs::read(current.join("config.toml")).unwrap(),
        fs::read(legacy.join("config.toml")).unwrap()
    );
}

#[test]
fn failed_destination_creation_preserves_legacy_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory.path().join("semctx");
    let current = directory.path().join("semctl");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("credentials.json"), b"legacy credentials").unwrap();
    fs::write(&current, b"blocks destination directory").unwrap();

    assert!(migrate_from_paths(&legacy, &current).is_err());
    assert_eq!(
        fs::read(legacy.join("credentials.json")).unwrap(),
        b"legacy credentials"
    );
}

#[test]
fn destination_directory_preserves_legacy_files() {
    for name in ["config.toml", "credentials.json"] {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("semctx");
        let current = directory.path().join("semctl");
        fs::create_dir(&legacy).unwrap();
        fs::create_dir_all(current.join(name)).unwrap();
        fs::write(legacy.join(name), b"retained legacy file").unwrap();

        assert!(migrate_from_paths(&legacy, &current).is_err());
        assert_eq!(
            fs::read(legacy.join(name)).unwrap(),
            b"retained legacy file"
        );
        assert!(current.join(name).is_dir());
    }
}

#[cfg(unix)]
#[test]
fn ambiguous_migration_paths_preserve_legacy_files() {
    for source_is_link in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("semctx");
        let current = directory.path().join("semctl");
        fs::create_dir(&legacy).unwrap();
        fs::create_dir(&current).unwrap();
        fs::write(legacy.join("config.toml"), b"retained legacy config").unwrap();
        let source = legacy.join("credentials.json");
        if source_is_link {
            // A loop causes Path::exists to suppress its metadata error.
            std::os::unix::fs::symlink(&source, &source).unwrap();
        } else {
            fs::write(&source, b"retained credentials").unwrap();
            std::os::unix::fs::symlink(&source, current.join("credentials.json")).unwrap();
        }

        assert!(migrate_from_paths(&legacy, &current).is_err());
        assert_eq!(
            fs::read(legacy.join("config.toml")).unwrap(),
            b"retained legacy config"
        );
        assert!(fs::symlink_metadata(&source).is_ok());
    }
}

#[test]
fn successful_migration_keeps_current_config_and_retires_legacy() {
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory.path().join("semctx");
    let current = directory.path().join("semctl");
    fs::create_dir(&legacy).unwrap();
    fs::create_dir(&current).unwrap();
    fs::write(legacy.join("config.toml"), b"old config").unwrap();
    fs::write(current.join("config.toml"), b"current config").unwrap();
    fs::write(legacy.join("credentials.json"), b"retained credentials").unwrap();

    assert!(migrate_from_paths(&legacy, &current).unwrap());

    assert!(!legacy.exists());
    assert_eq!(
        fs::read(current.join("config.toml")).unwrap(),
        b"current config"
    );
    assert_eq!(
        fs::read(current.join("credentials.json")).unwrap(),
        b"retained credentials"
    );
    assert!(!migrate_from_paths(&legacy, &current).unwrap());
}
