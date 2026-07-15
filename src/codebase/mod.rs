//! Resolve which server codebase the current working directory maps to, so
//! `semctl mcp` can be launched in a project folder without being told the
//! codebase id.
//!
//! Only the caller's *local* codebases (`SourceKind == Local`) are considered:
//! a folder on disk is a working copy, so it must resolve to the user's own
//! local index, never a shared canonical one (Personal/Org/Global) that merely
//! shares the same git remote. Among those, the match is the folder's git
//! `origin` remote against a codebase's `remote_url`, then the folder name
//! against its `slug`. Best-effort — a miss leaves the MCP server running with
//! search/domains but the code tools reporting "no codebase".

mod git;

use std::path::Path;

use anyhow::Result;

use crate::client::{Client, api, is_local_source};
use git::{git_capture, git_is_dirty, git_remote, normalize_remote};

/// A resolved codebase + how it matched, for a one-line stderr note.
pub struct Resolved {
    pub id: String,
    pub how: &'static str,
}

/// Resolve the codebase for `dir`: the on-disk cache first (a folder
/// `semctl index` has seen), then a server lookup among the caller's *local*
/// codebases. `Ok(None)` when nothing matches — the server has no local
/// codebase for this folder yet. A server hit is written back to the cache.
pub async fn resolve(client: &Client, dir: &Path) -> Result<Option<Resolved>> {
    if let Some((id, how)) = crate::config::load()
        .ok()
        .and_then(|c| c.cached_codebase_for(dir))
    {
        // The cache is keyed by directory, not by server, and a codebase can be
        // deleted/expired — so verify the id still exists on THIS server before
        // trusting it. A definitive miss purges the stale mapping and falls
        // through to a server lookup / (for `index`) a fresh registration; a
        // transient error keeps the cache rather than punishing a flaky network.
        match client
            .get_opt::<api::CodebaseSummary>(&format!("/v1/codebases/{id}"))
            .await
        {
            Ok(None) => {
                let _ = crate::config::uncache_codebase_id(&id);
            }
            Ok(Some(_)) | Err(_) => return Ok(Some(Resolved { id, how })),
        }
    }

    let codebases: Vec<api::CodebaseSummary> = client
        .get_page::<api::CodebaseSummary>("/v1/codebases?page=0&pageSize=500")
        .await?
        .items
        .into_iter()
        // A working copy maps to a Local index, never a shared canonical one.
        .filter(|c| is_local_source(&c.source_kind))
        .collect();

    let by_remote = match git_remote(dir).await {
        Some(remote) => {
            let want = normalize_remote(&remote);
            codebases
                .iter()
                .find(|c| c.remote_url.as_deref().map(normalize_remote).as_deref() == Some(&want))
        }
        None => None,
    };
    let by_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| codebases.iter().find(|c| c.slug.eq_ignore_ascii_case(name)));

    if let Some((c, how)) = by_remote
        .map(|c| (c, "git remote"))
        .or_else(|| by_name.map(|c| (c, "folder name")))
    {
        let _ = crate::config::cache_codebase(dir, &c.id);
        return Ok(Some(Resolved {
            id: c.id.clone(),
            how,
        }));
    }
    Ok(None)
}

/// The codebase id for `dir`, creating a **Local** codebase (the working-copy
/// kind) if none exists yet. Used by `semctl index` so a fresh folder gets
/// registered before its first sync. The id is cached for later resolution.
pub async fn ensure(client: &Client, dir: &Path) -> Result<String> {
    if let Some(resolved) = resolve(client, dir).await? {
        return Ok(resolved.id);
    }
    let id = create_local(client, dir).await?;
    let _ = crate::config::cache_codebase(dir, &id);
    Ok(id)
}

/// Register a new Local codebase for `dir`. Slug/name come from the folder;
/// VCS metadata (remote, revision, branch, dirty) is attached for a git
/// checkout. `sourceKind`/`visibility` are left to the server defaults
/// (`Local` / `Personal`).
async fn create_local(client: &Client, dir: &Path) -> Result<String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("codebase")
        .to_string();
    let vcs = match git_remote(dir).await {
        Some(remote) => Some(api::CodebaseVcsInfo {
            remote_url: Some(remote),
            revision: git_capture(dir, &["rev-parse", "HEAD"]).await,
            ref_name: git_capture(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await,
            dirty: git_is_dirty(dir).await,
        }),
        None => None,
    };
    let body = api::CreateCodebaseRequest {
        slug: &slugify(&name),
        display_name: &name,
        vcs,
    };
    let created: api::CodebaseSummary = client.post("/v1/codebases", &body).await?;
    Ok(created.id)
}

/// A URL/slug-safe form of the folder name: lowercase, non-alphanumerics → `-`,
/// collapsed and trimmed.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "codebase".to_string()
    } else {
        slug
    }
}
