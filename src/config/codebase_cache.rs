//! Directory → codebase cache and umbrella-root resolution.
//!
//! The cache (populated by `semctl index`) lets `semctl mcp` resolve a folder it
//! has indexed before without a server round-trip. Umbrella roots are the opt-in
//! mechanism that lets a cached *parent* stand in for an un-indexed sub-repo.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::Config;

impl Config {
    /// Resolve `dir` to a cached codebase. An **exact** match (the folder
    /// `semctl index` recorded) always wins. Otherwise a cached *ancestor* is
    /// used ONLY when it's opted in as an umbrella root ([`super::Config::umbrella_roots`] /
    /// [`super::Config::umbrella_globs`]); the nearest (longest) eligible ancestor wins.
    ///
    /// With no umbrella config this returns `None` for any non-exact folder — the
    /// deliberate default: a folder that is its own git repo (e.g.
    /// `.../napbat/platform`, under a bare dir-of-repos) won't silently resolve to
    /// the parent's index; the caller reports "not indexed" and the user runs
    /// `semctl index`. A genuine multi-repo umbrella opts back in via config.
    ///
    /// Returns the id and a short note on how it matched (for the stderr line).
    pub fn cached_codebase_for(&self, dir: &Path) -> Option<(String, &'static str)> {
        let dir = canonical(dir);

        if let Some(id) = self.codebase_cache.get(&dir.to_string_lossy().into_owned()) {
            return Some((id.clone(), "cache"));
        }
        // Ancestor resolution is opt-in: the cached parent must be declared an
        // umbrella root. `dir.starts_with(k)` is component-wise, so only true
        // ancestors match (never a mere string-prefix sibling).
        if let Some((_, id)) = self
            .codebase_cache
            .iter()
            .filter(|(k, _)| dir.starts_with(k) && self.is_umbrella_root(Path::new(k)))
            .max_by_key(|(k, _)| k.len())
        {
            return Some((id.clone(), "cache (umbrella ancestor)"));
        }
        None
    }

    /// Whether `ancestor` is an opted-in umbrella root — a cached parent that a
    /// different sub-repo is allowed to resolve to. True when it equals a
    /// [`super::Config::umbrella_roots`] entry (trailing slash ignored) or matches a
    /// [`super::Config::umbrella_globs`] pattern. Empty config ⇒ always false, which is what
    /// makes plain per-folder scoping the default.
    fn is_umbrella_root(&self, ancestor: &Path) -> bool {
        let s = ancestor.to_string_lossy().replace('\\', "/");
        let s_trim = s.trim_end_matches('/');
        if self
            .umbrella_roots
            .iter()
            .any(|r| r.replace('\\', "/").trim_end_matches('/') == s_trim)
        {
            return true;
        }
        // Separator-literal so `*` stays within one path segment (see the
        // `umbrella_globs` field doc). `glob`'s default would let `*` cross `/`.
        let opts = glob::MatchOptions {
            require_literal_separator: true,
            ..glob::MatchOptions::new()
        };
        self.umbrella_globs
            .iter()
            .filter_map(|g| glob::Pattern::new(g).ok())
            .any(|p| p.matches_with(&s, opts))
    }

    /// Reverse of the codebase cache: the local checkout root
    /// `semctl index` recorded for `codebase_id`, if any. The MCP server
    /// uses this to absolutize the server's codebase-relative hit paths.
    /// If more than one directory maps to the same id (indexed from two
    /// checkouts), prefer one that contains `prefer` (the launch cwd) so
    /// the paths resolve under the checkout the host is actually in.
    pub fn codebase_root(
        &self,
        codebase_id: &str,
        prefer: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        let mut matches = self
            .codebase_cache
            .iter()
            .filter(|(_, id)| id.as_str() == codebase_id)
            .map(|(dir, _)| PathBuf::from(dir));
        if let Some(p) = prefer {
            let p = canonical(p);
            // Re-collect so the preference scan doesn't consume the fallback.
            let all: Vec<PathBuf> = matches.collect();
            if let Some(hit) = all.iter().find(|root| p.starts_with(root)) {
                return Some(hit.clone());
            }
            return all.into_iter().next();
        }
        matches.next()
    }
}

/// Record `dir → codebase_id` in the on-disk cache (load, update, save).
pub fn cache_codebase(dir: &std::path::Path, codebase_id: &str) -> Result<()> {
    let mut cfg = super::load()?;
    cfg.codebase_cache
        .insert(cache_key(dir), codebase_id.to_string());
    super::save(&cfg)
}

/// Drop every `dir → codebase_id` mapping pointing at `codebase_id`. Called when
/// the server reports the id gone (deleted/expired, or the cache predates a
/// switch to a different server) — the cache isn't server-scoped, so a dead id
/// is purged everywhere rather than clung to. A re-`index` then registers fresh.
pub fn uncache_codebase_id(codebase_id: &str) -> Result<()> {
    let mut cfg = super::load()?;
    let before = cfg.codebase_cache.len();
    cfg.codebase_cache.retain(|_, v| v != codebase_id);
    if cfg.codebase_cache.len() != before {
        super::save(&cfg)?;
    }
    Ok(())
}

/// Absolute, canonical form of `p`, falling back to `p` unchanged when it can't
/// be canonicalized (e.g. it doesn't exist yet).
fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Canonical, stable cache key for a directory (absolute path, lossy string).
fn cache_key(dir: &Path) -> String {
    canonical(dir).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Config with the given `dir -> id` cache entries. Paths are absolute
    /// and non-existent, so `cached_codebase_for`'s canonicalize falls back to the
    /// path as-is — matching is purely lexical/component-wise, no disk needed.
    fn cfg(cache: &[(&str, &str)]) -> Config {
        Config {
            codebase_cache: cache
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Config::default()
        }
    }

    fn resolved(c: &Config, dir: &str) -> Option<String> {
        c.cached_codebase_for(Path::new(dir)).map(|(id, _)| id)
    }

    #[test]
    fn exact_match_wins_over_umbrella_parent() {
        let mut c = cfg(&[("/x", "id-umbrella"), ("/x/repo", "id-repo")]);
        c.umbrella_roots = vec!["/x".into()];
        assert_eq!(resolved(&c, "/x/repo"), Some("id-repo".into()));
    }

    #[test]
    fn no_ancestor_match_without_umbrella_config() {
        // The whole point: a sub-repo under a bare dir-of-repos does NOT grab it.
        let c = cfg(&[("/home/git/napbat", "id-napbat")]);
        assert_eq!(resolved(&c, "/home/git/napbat/platform"), None);
    }

    #[test]
    fn umbrella_root_opts_the_parent_back_in() {
        let mut c = cfg(&[("/home/git/napbat", "id-napbat")]);
        c.umbrella_roots = vec!["/home/git/napbat/".into()]; // trailing slash tolerated
        assert_eq!(
            resolved(&c, "/home/git/napbat/platform"),
            Some("id-napbat".into())
        );
    }

    #[test]
    fn umbrella_glob_opts_the_parent_back_in() {
        let mut c = cfg(&[("/home/git/napbat", "id-napbat")]);
        c.umbrella_globs = vec!["/home/git/*".into()];
        assert_eq!(
            resolved(&c, "/home/git/napbat/platform"),
            Some("id-napbat".into())
        );
    }

    #[test]
    fn umbrella_glob_star_does_not_cross_slash() {
        // `/home/git/*` blesses napbat but not a deeper cached dir-of-repos, so a
        // grandchild leaf under an undeclared inner dir stays unresolved.
        let mut c = cfg(&[("/home/git/napbat/inner", "id-inner")]);
        c.umbrella_globs = vec!["/home/git/*".into()];
        assert_eq!(resolved(&c, "/home/git/napbat/inner/leaf"), None);
    }

    #[test]
    fn string_prefix_sibling_is_not_an_ancestor() {
        // `/x/repo-two` must not resolve to `/x/repo` just because the string
        // starts with it — component-wise `starts_with` guards this.
        let mut c = cfg(&[("/x/repo", "id-repo")]);
        c.umbrella_roots = vec!["/x/repo".into()];
        assert_eq!(resolved(&c, "/x/repo-two"), None);
    }
}
