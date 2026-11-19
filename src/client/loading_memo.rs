//! Recent loading failures shared by the request clients of one session.

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use tokio::time::Instant;

use super::LoadingKind;

const MAX_MEMO_ENTRIES: usize = 64;

/// The codebase and the loading projection that a success can establish.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CodebaseRequest<'a> {
    pub(super) id: &'a str,
    pub(super) kind: Option<LoadingKind>,
}

impl<'a> CodebaseRequest<'a> {
    pub(super) fn from_path(path: &'a str) -> Option<Self> {
        let path = path.strip_prefix('/').unwrap_or(path);
        let path = path
            .strip_prefix("v1/codebases/")?
            .split(['?', '#'])
            .next()?;
        let (id, route) = path.split_once('/').unwrap_or((path, ""));
        let kind = if route.starts_with("graph/") || route.starts_with("edits/") {
            Some(LoadingKind::Graph)
        } else if route == "files/content" {
            Some(LoadingKind::File)
        } else {
            // A metadata or catalog success does not establish readiness.
            None
        };
        (!id.is_empty()).then_some(Self { id, kind })
    }
}

struct Entry {
    codebase: String,
    kind: LoadingKind,
    until: Instant,
}

impl Entry {
    fn matches(&self, codebase: &str, kind: LoadingKind) -> bool {
        self.codebase == codebase && self.kind == kind
    }
}

#[derive(Clone, Default)]
pub(super) struct LoadingMemo(Arc<Mutex<Vec<Entry>>>);

impl LoadingMemo {
    pub(super) fn contains(&self, codebase: &str, kind: LoadingKind, now: Instant) -> bool {
        self.lock()
            .iter()
            .any(|entry| entry.matches(codebase, kind) && now < entry.until)
    }

    pub(super) fn record(&self, codebase: &str, kind: LoadingKind, now: Instant, window: Duration) {
        let mut entries = self.lock();
        let until = now + window;
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.matches(codebase, kind))
        {
            entry.until = until;
            return;
        }
        // Prefer to evict the entry that becomes inactive first.
        if entries.len() == MAX_MEMO_ENTRIES
            && let Some((index, _)) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.until)
        {
            entries.swap_remove(index);
        }
        entries.push(Entry {
            codebase: codebase.to_owned(),
            kind,
            until,
        });
    }

    pub(super) fn clear(&self, codebase: &str, kind: LoadingKind) {
        self.lock().retain(|entry| !entry.matches(codebase, kind));
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        // The lock only protects this bounded map; no request runs under it.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
