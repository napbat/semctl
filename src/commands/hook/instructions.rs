//! Deliver the server manual once per context segment through lifecycle hooks.

use super::{HookInput, debug, resets_segment, state};

const SERVER_INSTRUCTIONS: &str = include_str!("../../mcp/docs/instructions/server.md");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Ensure,
    ResetAndEnsure,
    Reset,
}

/// Run after context retrieval so a network timeout cannot consume the delivery flag.
pub(super) async fn context(input: &HookInput) -> Option<String> {
    let action = match input.hook_event_name.as_str() {
        "PostCompact" => Action::Reset,
        "SessionStart" if resets_segment(&input.hook_event_name, &input.source) => {
            Action::ResetAndEnsure
        }
        "SessionStart" | "UserPromptSubmit" => Action::Ensure,
        _ => return None,
    };
    if input.session_id.is_empty() {
        return None;
    }
    let session_id = input.session_id.clone();
    let cleanup = input.hook_event_name != "UserPromptSubmit";
    match tokio::task::spawn_blocking(move || {
        let store = state::Store::default_store();
        let instructions = take(&store, &session_id, action);
        if cleanup {
            store.cleanup();
        }
        instructions.map(str::to_string)
    })
    .await
    {
        Ok(context) => context,
        Err(error) => {
            // Guidance is advisory. A failed task must not break the session.
            debug(format_args!(
                "server instruction state task failed: {error}"
            ));
            None
        }
    }
}

fn take(store: &state::Store, session_id: &str, action: Action) -> Option<&'static str> {
    let _lock = store.try_lock(session_id)?;
    let mut state = store.load(session_id);
    if action != Action::Ensure {
        state.reset_segment();
    }
    let emit = action != Action::Reset && !state.server_instructions_emitted;
    if emit {
        state.server_instructions_emitted = true;
    }
    if (emit || action != Action::Ensure)
        && let Err(error) = store.try_save(session_id, &state)
    {
        // Without durable deduplication, every prompt could repeat the manual.
        debug(format_args!(
            "server instruction state save failed: {error}"
        ));
        return None;
    }
    emit.then_some(SERVER_INSTRUCTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_claims_emit_the_manual_once() {
        let directory = tempfile::tempdir().unwrap();
        let store = state::Store::with_dir(directory.path().to_path_buf());
        let barrier = std::sync::Barrier::new(4);
        let emitted = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        take(&store, "session", Action::Ensure)
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(emitted, [SERVER_INSTRUCTIONS]);
        assert!(store.load("session").server_instructions_emitted);
    }

    #[test]
    fn contended_claim_retries_without_consuming_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let store = state::Store::with_dir(directory.path().to_path_buf());
        let lock = store.try_lock("session").unwrap();
        assert!(take(&store, "session", Action::Ensure).is_none());
        assert!(!store.load("session").server_instructions_emitted);
        drop(lock);
        assert_eq!(
            take(&store, "session", Action::Ensure),
            Some(SERVER_INSTRUCTIONS)
        );
    }

    #[test]
    fn failed_state_save_does_not_emit_the_manual() {
        let directory = tempfile::tempdir().unwrap();
        let store = state::Store::with_dir(directory.path().to_path_buf());
        store
            .try_save("session", &state::NudgeState::default())
            .unwrap();
        let state_path = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::remove_file(&state_path).unwrap();
        std::fs::create_dir(&state_path).unwrap();
        assert!(take(&store, "session", Action::Ensure).is_none());
        assert!(!store.load("session").server_instructions_emitted);
        std::fs::remove_dir(state_path).unwrap();
        assert_eq!(
            take(&store, "session", Action::Ensure),
            Some(SERVER_INSTRUCTIONS)
        );
    }
}
