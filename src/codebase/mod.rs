//! Resolve which server codebase a local checkout maps to.
//!
//! Local path identity is explicit: only the canonical directory cache written
//! by `semctl index` may bind a checkout (plus an explicitly configured umbrella
//! ancestor for read-time resolution), and the server is asked which project
//! holds THIS checkout's copy before anything is registered.
//!
//! A git remote does now decide which project a checkout belongs under. It did
//! not use to: two clones carry different complete manifests, and one codebase
//! could only hold one of them, so sharing a remote meant deleting each other's
//! files. The server keeps a copy per checkout now, so clones of one repository
//! belong in one catalog entry — the thing a tenant holding ten of them for one
//! repository was missing. A folder with NO remote still gets a slug of its
//! own: a bare folder name is not a project identity, and two unrelated `src`
//! directories must not be fused on that guess.

mod git;
mod identity;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::client::{Client, api, api::CAPABILITY_CODEBASE_VERSIONS, is_local_source};
use git::{git_capture, git_is_dirty, git_remote};

pub(crate) use identity::source_id as checkout_source_id;

/// Where the checkout at `dir` is standing: its branch, its revision, and
/// whether the tree is clean. `None` when the folder is not a git checkout,
/// which is a thing to say nothing about rather than a thing to guess at.
pub(crate) async fn checkout_state(dir: &Path) -> Option<api::CheckoutVcsInfo> {
    let ref_name = git_capture(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let revision = git_capture(dir, &["rev-parse", "HEAD"]).await;
    if ref_name.is_none() && revision.is_none() {
        return None;
    }

    Some(api::CheckoutVcsInfo {
        ref_name,
        revision,
        dirty: git_is_dirty(dir).await,
    })
}

/// A resolved codebase + how it matched, for a one-line stderr note.
pub struct Resolved {
    pub id: String,
    pub how: &'static str,
}

/// Canonical root of the working copy containing `dir`.
///
/// A sync manifest is complete desired state, so walking an arbitrary launch
/// subdirectory would mean "delete everything outside this subtree". Git
/// checkouts therefore always sync from `--show-toplevel`; non-git directories
/// remain independently indexable at the path the caller selected.
pub async fn working_copy_root(dir: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let Some(root) = git_capture(&canonical, &["rev-parse", "--show-toplevel"]).await else {
        return canonical;
    };
    let root = PathBuf::from(root);
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// Resolve the codebase for `dir` from the on-disk path cache. An explicitly
/// configured umbrella ancestor may satisfy a child lookup. `Ok(None)` means
/// this path has never been indexed locally; callers must not guess by Git
/// remote or folder name.
pub async fn resolve(client: &Client, dir: &Path) -> Result<Option<Resolved>> {
    let cached = crate::config::load()
        .ok()
        .and_then(|config| config.cached_codebase_for(dir));
    validate_cached(client, cached).await
}

/// Registration's stricter lookup: an explicit `semctl index PATH` reuses only
/// PATH's own cache entry, never an umbrella ancestor.
pub(crate) async fn resolve_exact(client: &Client, dir: &Path) -> Result<Option<Resolved>> {
    let cached = crate::config::load()
        .ok()
        .and_then(|config| config.cached_codebase_exact(dir))
        .map(|id| (id, "cache"));
    validate_cached(client, cached).await
}

async fn validate_cached(
    client: &Client,
    cached: Option<(String, &'static str)>,
) -> Result<Option<Resolved>> {
    let Some((id, how)) = cached else {
        return Ok(None);
    };
    // The cache is not server-scoped and a codebase can be deleted. A
    // definitive miss purges the stale mapping; a transient error keeps it
    // rather than turning a network wobble into duplicate registration.
    match client
        .get_opt::<api::CodebaseSummary>(&format!("/v1/codebases/{id}"))
        .await
    {
        Ok(None) => {
            let _ = crate::config::uncache_codebase_id(&id);
            Ok(None)
        }
        Ok(Some(_)) | Err(_) => Ok(Some(Resolved { id, how })),
    }
}

/// The codebase id for `dir`, creating a **Local** codebase (the working-copy
/// kind) if none exists yet. Used by `semctl index` so a fresh folder gets
/// registered before its first sync. The id is cached for later resolution.
pub async fn ensure(client: &Client, dir: &Path) -> Result<String> {
    if let Some(resolved) = resolve_exact(client, dir).await? {
        return Ok(resolved.id);
    }
    let id = create_local(client, dir).await?;
    let _ = crate::config::cache_codebase(dir, &id);
    Ok(id)
}

/// The codebase already holding this checkout's copy, asked of the server
/// rather than remembered.
///
/// The source id is recomputed on every run and never stored, so this survives
/// what a cached id does not: a project merged into another keeps the copy, and
/// the answer here is simply the codebase that now holds it.
async fn find_by_source(client: &Client, source_id: &str) -> Result<Option<api::CodebaseSummary>> {
    let page = client
        .get_page::<api::CodebaseSummary>(&format!(
            "/v1/codebases?sourceId={source_id}&page=0&pageSize=2"
        ))
        .await?;
    Ok(page.items.into_iter().next())
}

/// Register or recover the deterministic Local codebase for `dir`. The friendly
/// name comes from the folder; its slug includes the opaque checkout identity,
/// so another same-named clone is a separate catalog row. VCS metadata is still
/// attached for display and source navigation, never matching.
async fn create_local(client: &Client, dir: &Path) -> Result<String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("codebase")
        .to_string();
    let remote = git_remote(dir).await;
    let vcs = match &remote {
        Some(remote) => Some(api::CodebaseVcsInfo {
            remote_url: Some(remote.clone()),
            revision: git_capture(dir, &["rev-parse", "HEAD"]).await,
            ref_name: git_capture(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await,
            dirty: git_is_dirty(dir).await,
        }),
        None => None,
    };
    let source_id = identity::source_id(dir)?;

    // A server that keeps one manifest per codebase cannot hold two checkouts,
    // so against one of those this stays what it always was: a codebase per
    // checkout, slugged with its own digest. Putting them together there would
    // have them delete each other's files on every sync.
    let versioned = client.supports(CAPABILITY_CODEBASE_VERSIONS).await;

    // Already known? Then this checkout has synced before, under a project it
    // may since have been merged into.
    if versioned && let Some(existing) = find_by_source(client, &source_id).await? {
        return Ok(existing.id);
    }

    // The project this checkout belongs under, or — with no remote to say what
    // that project is — a codebase of its own.
    let project = versioned
        .then(|| identity::project_slug(remote.as_deref()))
        .flatten();
    let slug = match &project {
        Some(slug) => slug.clone(),
        None => identity::slug(&name, &source_id),
    };
    if let Some(existing) = find_by_slug(client, &slug, project.is_none()).await? {
        return Ok(existing.id);
    }
    let body = api::CreateCodebaseRequest {
        slug: &slug,
        display_name: &name,
        source_id: &source_id,
        vcs,
    };
    match client
        .post::<_, api::CodebaseSummary>("/v1/codebases", &body)
        .await
    {
        Ok(created) => Ok(created.id),
        Err(create_error) => {
            // Two processes may index the same new path concurrently, and two
            // clones of one repository race for its project slug. Either way the
            // slug is deterministic, so the loser adopts the winner instead of
            // inventing a second codebase.
            if let Ok(Some(existing)) = find_by_slug(client, &slug, project.is_none()).await {
                return Ok(existing.id);
            }
            Err(create_error)
        }
    }
}

/// The codebase with this slug, if any.
///
/// `local_only` is the no-remote case: that slug carries a checkout digest and
/// means one specific working copy, so adopting a server-pulled codebase that
/// happened to take the name would point a full manifest at a corpus the server
/// maintains. A project slug has no such worry — attaching a working copy to the
/// project's pulled codebase is the intent, and the server keeps the two apart
/// as separate copies.
async fn find_by_slug(
    client: &Client,
    slug: &str,
    local_only: bool,
) -> Result<Option<api::CodebaseSummary>> {
    let mut page_number = 0_u32;
    loop {
        let page = client
            .get_page::<api::CodebaseSummary>(&format!(
                "/v1/codebases?page={page_number}&pageSize=500"
            ))
            .await?;
        if let Some(found) = page.items.into_iter().find(|codebase| {
            codebase.slug == slug && (!local_only || is_local_source(&codebase.source_kind))
        }) {
            return Ok(Some(found));
        }
        let consumed = page.number.saturating_add(1).saturating_mul(page.size);
        if page.size == 0 || consumed >= page.total {
            return Ok(None);
        }
        page_number = page_number.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::working_copy_root;

    #[tokio::test]
    async fn nested_git_directory_resolves_to_the_worktree_root() {
        let temp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let nested = temp.path().join("src").join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            working_copy_root(&nested).await,
            std::fs::canonicalize(temp.path()).unwrap()
        );
    }

    #[tokio::test]
    async fn non_git_directory_remains_its_own_root() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            working_copy_root(temp.path()).await,
            std::fs::canonicalize(temp.path()).unwrap()
        );
    }
}
