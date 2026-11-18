---
name: security-invariants
description: Security invariants of PR Helper. API tokens live only in the OS keychain (never in SQLite, logs, files or git config), and the Claude reviewer runs locked down against untrusted PR checkouts. Use when touching secrets.rs, server saving, providers' auth, git/repo_cache.rs, review/runner.rs, review/pipeline.rs, or anything that spawns processes or stores credentials.
---

# Security invariants

## Tokens: keychain only

- Tokens are stored via the `SecretStore` trait (`src-tauri/src/secrets.rs`). In the app that is `KeyringStore`: `keyring` v3 with `apple-native` (macOS Keychain). The service is `dev.jostweyers.prhelper` and the account is the server id.
- The database has **no** token column. `ServerInput.token` is transient and only flows through `Service::save_server` (`service.rs`), which:
  - verifies the token with the provider (`current_user`) **before** saving;
  - writes the token to the keychain;
  - rolls back a new server row if the keychain write fails;
  - keeps the stored token when the user edits a server without entering a new one.
- Hashing is not an option: the app must send the real token to the APIs. Encrypt-at-rest = keychain.
- Never log tokens, put them in error messages, persist them in app config, or return them to the frontend. There is intentionally no "get token" command.
- `delete_server` also deletes the keychain entry.
- Git over HTTPS gets the token per process through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0=http.extraHeader`/`GIT_CONFIG_VALUE_0` env vars (`RepoCache::cmd`). It is never in the remote URL, `.git/config` or argv (visible in `ps`).
- Guard tests (keep them passing):
  - `tokens_live_in_the_secret_store_never_in_the_database` scans the raw SQLite files, including WAL.
  - `a_failed_keychain_write_does_not_leave_a_server_behind`.
  - `keychain_roundtrip` (ignored; run on macOS with `cargo test keychain_roundtrip -- --ignored`).
- If the `keyring` features ever change, make sure macOS still uses `apple-native`. Without a platform feature, keyring 3 silently falls back to an in-memory mock store.

## Reviewer isolation

The worktree contains PR author-controlled files. `RunRequest::args()` in `review/runner.rs` **must** keep:

- `--setting-sources user`. Without it, a PR's `.claude/settings.json` hooks execute on the user's machine. This was verified experimentally.
- `--strict-mcp-config`, so the PR can't add MCP servers.
- `--tools Read Grep Glob Skill` (and the same `--allowed-tools`). There is no Bash, no Edit/Write and no web access. The reviewer gets the diff, commits and description as files in a context dir passed via `--add-dir` instead of running git.
- The prompt via stdin, and `env_remove("CLAUDECODE")`.

`args_lock_down_the_session` and the e2e test `full_review_and_post_flow` assert these flags. Extend them if you add flags. Never add `--dangerously-skip-permissions` or Bash.

Custom skills are copied into a per-review plugin dir (`review/skills.rs`). Symlinks inside skill folders are skipped on purpose.
