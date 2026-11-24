//! One bare clone per repo, one detached worktree per review.
//!
//! Credentials are passed per command through `GIT_CONFIG_*` environment variables
//! (`http.extraHeader`), so tokens never land in the remote URL, the repo config or `ps` output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};

use tokio::process::Command;
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};

pub struct FetchSpec<'a> {
    pub server_id: &'a str,
    pub repo: &'a str,
    pub clone_url: &'a str,
    /// e.g. `refs/pull/12/head` or `refs/merge-requests/12/head`
    pub pr_ref: &'a str,
    pub number: u64,
    pub base_ref: &'a str,
    pub head_sha: &'a str,
    pub base_sha: Option<&'a str>,
    /// Full header value, e.g. `Authorization: Basic ...`
    pub auth_header: Option<&'a str>,
}

pub struct RepoCache {
    root: PathBuf,
    git: std::sync::RwLock<PathBuf>,
    path_env: String,
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl RepoCache {
    pub fn new(root: PathBuf, git: PathBuf, path_env: String) -> Self {
        Self {
            root,
            git: std::sync::RwLock::new(git),
            path_env,
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn bare_path(&self, server_id: &str, repo: &str) -> PathBuf {
        // The hash keeps `group/sub_proj` and `group_sub/proj` apart after sanitizing.
        self.root
            .join("repos")
            .join(sanitize(server_id))
            .join(format!("{}-{:016x}.git", sanitize(repo), fnv1a(repo)))
    }

    /// Deletes every bare clone of a server (used when the server is removed).
    pub fn remove_server(&self, server_id: &str) {
        let dir = self.root.join("repos").join(sanitize(server_id));
        if dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&dir)
        {
            tracing::warn!("could not remove {}: {e}", dir.display());
        }
    }

    pub fn review_dir(&self, review_id: &str) -> PathBuf {
        self.root.join("reviews").join(sanitize(review_id))
    }

    async fn lock_for(&self, path: &Path) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks.entry(path.to_path_buf()).or_default().clone()
    }

    pub fn set_git(&self, git: PathBuf) {
        *self.git.write().unwrap_or_else(PoisonError::into_inner) = git;
    }

    fn cmd(&self, auth_header: Option<&str>) -> Command {
        let git = self
            .git
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut c = Command::new(git);
        c.env("PATH", &self.path_env)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            .env("LC_ALL", "C")
            // PR checkouts are untrusted: never run LFS smudge filters (they fetch from a URL
            // the PR's own .lfsconfig controls) or the user's global hooks on them.
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(["-c", "core.quotePath=false"])
            .args(["-c", "diff.noprefix=false"])
            .args(["-c", "diff.mnemonicPrefix=false"])
            .args(["-c", "diff.relative=false"])
            .kill_on_drop(true);
        if let Some(h) = auth_header {
            c.env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "http.extraHeader")
                .env("GIT_CONFIG_VALUE_0", h);
        }
        c
    }

    async fn run(
        &self,
        dir: Option<&Path>,
        args: &[&str],
        auth: Option<&str>,
    ) -> AppResult<String> {
        let mut c = self.cmd(auth);
        if let Some(d) = dir {
            c.arg("-C").arg(d);
        }
        c.args(args);
        let out = c.output().await?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(AppError::Git(format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    async fn has_commit(&self, bare: &Path, sha: &str) -> bool {
        self.run(
            Some(bare),
            &["cat-file", "-e", &format!("{sha}^{{commit}}")],
            None,
        )
        .await
        .is_ok()
    }

    /// Make sure the bare clone has the PR head and base commits. Returns the bare repo path.
    pub async fn ensure_pr(&self, spec: &FetchSpec<'_>) -> AppResult<PathBuf> {
        let bare = self.bare_path(spec.server_id, spec.repo);
        let lock = self.lock_for(&bare).await;
        let _guard = lock.lock().await;

        if !bare.join("HEAD").exists() {
            tokio::fs::create_dir_all(&bare).await?;
            self.run(Some(&bare), &["init", "--bare", "--quiet"], None)
                .await?;
        }
        // `set-url` fails when an earlier run died between `init` and `remote add`.
        if self
            .run(
                Some(&bare),
                &["remote", "set-url", "origin", spec.clone_url],
                None,
            )
            .await
            .is_err()
        {
            self.run(
                Some(&bare),
                &["remote", "add", "origin", spec.clone_url],
                None,
            )
            .await?;
        }

        let head_target = format!("+{}:refs/pr/{}", spec.pr_ref, spec.number);
        let base_target = format!("+refs/heads/{0}:refs/remotes/origin/{0}", spec.base_ref);
        self.run(
            Some(&bare),
            &[
                "fetch",
                "--no-tags",
                "--prune",
                "--quiet",
                "origin",
                &head_target,
                &base_target,
            ],
            spec.auth_header,
        )
        .await?;

        for sha in std::iter::once(spec.head_sha).chain(spec.base_sha) {
            if !self.has_commit(&bare, sha).await {
                // The base branch may have moved; GitHub and GitLab both allow fetching by SHA.
                self.run(
                    Some(&bare),
                    &["fetch", "--no-tags", "--quiet", "origin", sha],
                    spec.auth_header,
                )
                .await
                .map_err(|e| AppError::Git(format!("commit {sha} is not available: {e}")))?;
            }
        }
        Ok(bare)
    }

    pub async fn merge_base(&self, bare: &Path, base: &str, head: &str) -> AppResult<String> {
        Ok(self
            .run(Some(bare), &["merge-base", base, head], None)
            .await?
            .trim()
            .to_string())
    }

    /// Resolve the base to diff against: an explicit SHA, or the fetched base branch tip.
    pub async fn base_commit(
        &self,
        bare: &Path,
        base_sha: Option<&str>,
        base_ref: &str,
    ) -> AppResult<String> {
        match base_sha {
            Some(sha) => Ok(sha.to_string()),
            None => Ok(self
                .run(
                    Some(bare),
                    &["rev-parse", &format!("refs/remotes/origin/{base_ref}")],
                    None,
                )
                .await?
                .trim()
                .to_string()),
        }
    }

    /// Three-dot diff (from the merge base), which is what GitHub and GitLab show.
    pub async fn diff(&self, bare: &Path, base: &str, head: &str) -> AppResult<String> {
        self.run(
            Some(bare),
            &[
                "diff",
                "--no-color",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--no-ext-diff",
                "--no-textconv",
                "-U3",
                "-M",
                &format!("{base}...{head}"),
            ],
            None,
        )
        .await
    }

    /// Two-dot diff between two reviewed heads, for incremental re-reviews.
    pub async fn diff_between(&self, bare: &Path, from: &str, to: &str) -> AppResult<String> {
        self.run(
            Some(bare),
            &[
                "diff",
                "--no-color",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--no-ext-diff",
                "--no-textconv",
                "-U3",
                "-M",
                from,
                to,
            ],
            None,
        )
        .await
    }

    pub async fn commit_log(&self, bare: &Path, base: &str, head: &str) -> AppResult<String> {
        self.run(
            Some(bare),
            &[
                "log",
                "--no-color",
                "--format=%h %an: %s%n%n%b%n---",
                &format!("{base}..{head}"),
            ],
            None,
        )
        .await
    }

    /// What `path` is at commit `sha`. Reads the object database, never a checkout, so symlinks
    /// in the PR can't point it at files outside the repository. Git failures are errors; only
    /// a path that really isn't in the commit is `Missing`.
    pub async fn tree_entry(&self, bare: &Path, sha: &str, path: &str) -> AppResult<TreeEntry> {
        object_spec(sha, path)?;
        // `--literal-pathspecs`: the path is matched as is, not as a glob.
        let out = self
            .run(
                Some(bare),
                &[
                    "--literal-pathspecs",
                    "ls-tree",
                    "-z",
                    "-l",
                    sha,
                    "--",
                    path,
                ],
                None,
            )
            .await?;
        Ok(parse_tree_entry(&out, path))
    }

    /// The blob at `sha:path` as lossy UTF-8. Check the size with `tree_entry` first.
    pub async fn show_blob(&self, bare: &Path, sha: &str, path: &str) -> AppResult<String> {
        let spec = object_spec(sha, path)?;
        self.run(Some(bare), &["cat-file", "blob", &spec], None)
            .await
    }

    pub async fn add_worktree(&self, bare: &Path, dest: &Path, sha: &str) -> AppResult<()> {
        let lock = self.lock_for(bare).await;
        let _guard = lock.lock().await;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let dest_s = dest.to_string_lossy();
        self.run(
            Some(bare),
            &["worktree", "add", "--detach", "--force", &dest_s, sha],
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn remove_worktree(&self, bare: &Path, dest: &Path) -> AppResult<()> {
        let lock = self.lock_for(bare).await;
        let _guard = lock.lock().await;
        let dest_s = dest.to_string_lossy();
        let removed = self
            .run(
                Some(bare),
                &["worktree", "remove", "--force", &dest_s],
                None,
            )
            .await;
        if removed.is_err() && dest.exists() {
            tokio::fs::remove_dir_all(dest).await?;
        }
        let _ = self.run(Some(bare), &["worktree", "prune"], None).await;
        Ok(())
    }
}

/// Stable 64-bit FNV-1a hash (std's hasher is not guaranteed stable across releases).
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Filesystem-safe name that keeps nested group paths readable.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeEntry {
    Missing,
    Blob {
        size: u64,
    },
    /// A tree or a submodule commit.
    NotAFile,
}

/// Parses `ls-tree -z -l` output (`<mode> <type> <oid> <size>\t<path>\0`) for exactly `path`.
fn parse_tree_entry(out: &str, path: &str) -> TreeEntry {
    for record in out.split('\0') {
        let Some((meta, entry_path)) = record.split_once('\t') else {
            continue;
        };
        if entry_path != path {
            continue;
        }
        let mut fields = meta.split_whitespace();
        let kind = fields.nth(1);
        let size = fields.nth(1).and_then(|s| s.parse().ok());
        return match (kind, size) {
            (Some("blob"), Some(size)) => TreeEntry::Blob { size },
            _ => TreeEntry::NotAFile,
        };
    }
    TreeEntry::Missing
}

/// `sha:path`. The commit must be a hex id, so the spec can never be read as an option.
fn object_spec(sha: &str, path: &str) -> AppResult<String> {
    if sha.is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::invalid(format!("not a commit id: {sha}")));
    }
    if path.is_empty() || path.contains('\0') {
        return Err(AppError::invalid("empty or invalid path"));
    }
    Ok(format!("{sha}:{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn parses_tree_entries() {
        let out = "100644 blob 1111 42\tsrc/a.rs\x00160000 commit 2222       -\tvendor/lib\0";
        assert_eq!(
            parse_tree_entry(out, "src/a.rs"),
            TreeEntry::Blob { size: 42 }
        );
        assert_eq!(parse_tree_entry(out, "vendor/lib"), TreeEntry::NotAFile);
        assert_eq!(parse_tree_entry(out, "src"), TreeEntry::Missing);
        assert_eq!(parse_tree_entry("", "src/a.rs"), TreeEntry::Missing);
    }

    #[test]
    fn sanitize_keeps_paths_flat_and_safe() {
        assert_eq!(sanitize("group/sub/project"), "group_sub_project");
        assert_eq!(sanitize("../evil"), "_evil");
    }

    /// Simulates a forge with a local "remote" that has a PR ref, then exercises the full flow.
    #[tokio::test]
    async fn fetches_pr_and_manages_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]).await;
        std::fs::write(origin.join("a.txt"), "one\n").unwrap();
        git(&origin, &["add", "."]).await;
        git(&origin, &["commit", "-qm", "base"]).await;
        let base = git(&origin, &["rev-parse", "HEAD"]).await;
        git(&origin, &["checkout", "-qb", "feature"]).await;
        std::fs::write(origin.join("a.txt"), "one\ntwo\n").unwrap();
        git(&origin, &["commit", "-qam", "feature work"]).await;
        let head = git(&origin, &["rev-parse", "HEAD"]).await;
        git(&origin, &["update-ref", "refs/pull/5/head", &head]).await;
        git(&origin, &["checkout", "-q", "main"]).await;

        let cache = RepoCache::new(
            tmp.path().join("cache"),
            PathBuf::from("git"),
            std::env::var("PATH").unwrap(),
        );
        let url = origin.to_string_lossy().to_string();
        let spec = FetchSpec {
            server_id: "srv",
            repo: "team/app",
            clone_url: &url,
            pr_ref: "refs/pull/5/head",
            number: 5,
            base_ref: "main",
            head_sha: &head,
            base_sha: None,
            auth_header: None,
        };
        let bare = cache.ensure_pr(&spec).await.unwrap();
        // Second call reuses the clone.
        assert_eq!(cache.ensure_pr(&spec).await.unwrap(), bare);

        let base_commit = cache.base_commit(&bare, None, "main").await.unwrap();
        assert_eq!(base_commit, base);
        let diff = cache.diff(&bare, &base_commit, &head).await.unwrap();
        assert!(diff.contains("+two"));
        let log = cache.commit_log(&bare, &base_commit, &head).await.unwrap();
        assert!(log.contains("feature work"));

        assert_eq!(
            cache.tree_entry(&bare, &head, "a.txt").await.unwrap(),
            TreeEntry::Blob { size: 8 }
        );
        assert_eq!(
            cache.show_blob(&bare, &head, "a.txt").await.unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(
            cache.tree_entry(&bare, &head, "missing.txt").await.unwrap(),
            TreeEntry::Missing
        );
        // Matched literally, not as a glob.
        assert_eq!(
            cache.tree_entry(&bare, &head, "*.txt").await.unwrap(),
            TreeEntry::Missing
        );
        assert!(cache.tree_entry(&bare, "--help", "a.txt").await.is_err());
        // An unknown commit is a git error, not a missing file.
        assert!(
            cache
                .tree_entry(&bare, "0123456789abcdef0123456789abcdef01234567", "a.txt")
                .await
                .is_err()
        );

        let wt = cache.review_dir("r1").join("worktree");
        cache.add_worktree(&bare, &wt, &head).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        cache.remove_worktree(&bare, &wt).await.unwrap();
        assert!(!wt.exists());
    }
}
