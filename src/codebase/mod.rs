//! Resolve which server codebase a local checkout maps to.
//!
//! Local path identity is explicit: the canonical working-copy cache written by
//! `semctl index` binds a checkout (plus an explicitly configured umbrella
//! ancestor for read-time resolution). If that cache is missing or stale, a
//! version-aware server may recover the same checkout by its opaque source id;
//! Git remotes and folder names are never used as identity.
//!
//! A Git remote identifies a project only when the server supports separate
//! checkout copies. Each checkout still owns an independent manifest. A folder
//! without a remote keeps a checkout-specific slug because its name does not
//! establish project identity.

mod git;
mod identity;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::client::{
    Client, api, api::CAPABILITY_CODEBASE_VERSIONS, api::CAPABILITY_PROJECT_KEYS, is_local_source,
};
use git::{git_capture, git_is_dirty, git_remote, git_working_copy_root};

pub(crate) use identity::{path_from_key, path_key, source_id as checkout_source_id};

/// Where the checkout at `dir` is standing: its branch, its revision, the
/// remote it tracks, and whether the tree is clean. `None` when the folder is
/// not a Git checkout.
///
/// The remote is reported on every sync, not just at registration. It moves —
/// a repository is renamed or transferred, a remote is re-pointed, and a plain
/// folder becomes a checkout — and the server derives the project identity from
/// it, so saying it once would leave that identity describing a repository this
/// copy no longer tracks.
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
        remote_url: git_remote(dir).await,
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
    let Some(root) = git_working_copy_root(&canonical).await else {
        return canonical;
    };
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// Resolve the codebase for `dir`. Git paths are first lifted to their working
/// copy root, matching the directory [`sync`](crate::sync) records and walks.
/// The on-disk path cache is preferred; an explicitly configured umbrella
/// ancestor may satisfy a child lookup. When the cache is absent or points to a
/// codebase the server no longer has, a version-aware server is asked for the
/// codebase holding this exact checkout's opaque source id and the repaired
/// mapping is cached locally.
///
/// `Ok(None)` means neither local nor server-side evidence says this checkout
/// was indexed before; callers must not guess by Git remote or folder name.
pub async fn resolve(client: &Client, dir: &Path) -> Result<Option<Resolved>> {
    let dir = working_copy_root(dir).await;
    resolve_root(client, &dir, false).await
}

/// Registration's stricter lookup: an explicit `semctl index PATH` reuses only
/// PATH's own cache entry, never an umbrella ancestor.
pub(crate) async fn resolve_exact(client: &Client, dir: &Path) -> Result<Option<Resolved>> {
    let dir = working_copy_root(dir).await;
    resolve_root(client, &dir, true).await
}

/// Resolve an already-normalized working-copy root. `exact` disables umbrella
/// inheritance for registration while keeping exact source-id recovery: a
/// server copy with this installation+path identity is prior indexing, not a
/// guess or a new registration.
async fn resolve_root(client: &Client, dir: &Path, exact: bool) -> Result<Option<Resolved>> {
    let cached = crate::config::load().ok().and_then(|config| {
        if exact {
            config.cached_codebase_exact(dir).map(|id| (id, "cache"))
        } else {
            config.cached_codebase_for(dir)
        }
    });
    if let Some(resolved) = validate_cached(client, cached).await? {
        return Ok(Some(resolved));
    }

    recover_by_source(client, dir).await
}

async fn validate_cached(
    client: &Client,
    cached: Option<(String, &'static str)>,
) -> Result<Option<Resolved>> {
    let Some((id, how)) = cached else {
        return Ok(None);
    };
    // The cache is not server-scoped and a codebase can be deleted. Cleanup after
    // a definitive miss is best effort because the next lookup validates a
    // retained entry. A transient server failure keeps the entry to prevent
    // duplicate registration.
    match client
        .get_opt::<api::CodebaseSummary>(&format!("/v1/codebases/{id}"))
        .await
    {
        Ok(None) => {
            let _ = crate::config::uncache_codebase_id(&id).await;
            Ok(None)
        }
        Ok(Some(_)) | Err(_) => Ok(Some(Resolved { id, how })),
    }
}

/// Recover a checkout whose local path cache was lost or whose cached codebase
/// id became stale after a server-side project merge. The source id hashes this
/// installation plus the canonical checkout path, so a match is proof that this
/// exact working copy was indexed before. Older single-manifest servers cannot
/// safely filter by source id and are deliberately left on cache-only lookup.
async fn recover_by_source(client: &Client, dir: &Path) -> Result<Option<Resolved>> {
    if !client.supports(CAPABILITY_CODEBASE_VERSIONS).await {
        return Ok(None);
    }
    let source_id = identity::source_id(dir)?;
    let Some(existing) = find_by_source(client, &source_id).await? else {
        return Ok(None);
    };
    // Cache failure does not invalidate the authoritative server match.
    let _ = crate::config::cache_codebase(dir, &existing.id).await;
    Ok(Some(Resolved {
        id: existing.id,
        how: "server source id",
    }))
}

/// The codebase id for `dir`, creating a **Local** codebase (the working-copy
/// kind) if none exists yet. Used by `semctl index` so a fresh folder gets
/// registered before its first sync. The id is cached for later resolution.
pub async fn ensure(client: &Client, dir: &Path) -> Result<String> {
    let dir = working_copy_root(dir).await;
    if let Some(resolved) = resolve_root(client, &dir, true).await? {
        return Ok(resolved.id);
    }
    let id = create_local(client, &dir).await?;
    // Source-id recovery can rebuild this best-effort cache entry.
    let _ = crate::config::cache_codebase(&dir, &id).await;
    Ok(id)
}

async fn find_by_source(client: &Client, source_id: &str) -> Result<Option<api::CodebaseSummary>> {
    let page = client
        .get_page::<api::CodebaseSummary>(&format!(
            "/v1/codebases?sourceId={source_id}&page=0&pageSize=2"
        ))
        .await?;
    Ok(page.items.into_iter().next())
}

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

    // Legacy servers need one codebase per checkout because each codebase holds
    // one manifest.
    let versioned = client.supports(CAPABILITY_CODEBASE_VERSIONS).await;

    if versioned && let Some(existing) = find_by_source(client, &source_id).await? {
        return Ok(existing.id);
    }

    let derives_projects = client.supports(CAPABILITY_PROJECT_KEYS).await;

    // Only a project-key server can establish that two remotes identify the
    // same project. Older servers use the checkout identity, even when they
    // support separate copies. A basename never proves project ownership.
    let slug = if derives_projects {
        identity::label(&name)
    } else {
        identity::slug(&name, &source_id)
    };
    if !derives_projects && let Some(existing) = find_by_slug(client, &slug).await? {
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
            // Only legacy creation can lose the digest-slug race. A project-key
            // error has another cause and must be returned.
            if !derives_projects && let Ok(Some(existing)) = find_by_slug(client, &slug).await {
                return Ok(existing.id);
            }
            Err(create_error)
        }
    }
}

/// The codebase with this slug, if any.
///
/// This slug contains the checkout digest. Only a Local codebase can satisfy
/// the lookup; a server-pulled corpus must not receive this desired manifest.
async fn find_by_slug(client: &Client, slug: &str) -> Result<Option<api::CodebaseSummary>> {
    let mut page_number = 0_u32;
    loop {
        let page = client
            .get_page::<api::CodebaseSummary>(&format!(
                "/v1/codebases?page={page_number}&pageSize=500"
            ))
            .await?;
        if let Some(found) = page
            .items
            .into_iter()
            .find(|codebase| codebase.slug == slug && is_local_source(&codebase.source_kind))
        {
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

    #[cfg(unix)]
    #[tokio::test]
    async fn git_root_preserves_native_bytes_and_trailing_whitespace() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        for name in [b"repo \n".to_vec(), b"repo-\xff".to_vec()] {
            let root = temp.path().join(std::ffi::OsString::from_vec(name));
            std::fs::create_dir(&root).unwrap();
            assert!(
                std::process::Command::new("git")
                    .args(["init", "--quiet"])
                    .current_dir(&root)
                    .status()
                    .unwrap()
                    .success()
            );
            let child = root.join("src");
            std::fs::create_dir(&child).unwrap();
            assert_eq!(
                working_copy_root(&child).await,
                std::fs::canonicalize(&root).unwrap()
            );
        }
    }
}
