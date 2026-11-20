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

## Ticket context (`tickets/`)

- Jira tokens follow the token rules above. They are stored under the keychain account `ticket-source:<id>` (`Service::ticket_secret_id`), written only by `Service::save_ticket_source` (verified with `/rest/api/2/myself` first), and removed by `delete_ticket_source` or when a source switches to MCP. Guard test: `ticket_source_tokens_live_in_the_secret_store_never_in_the_database`.
- Ticket keys come from author-controlled text. Only keys from the configured `ticketProjectKeys` are fetched, and every key must match `KEY_RE` before it goes into a URL or prompt.
- Ticket text is untrusted. It is fenced in `tickets.md`, the prompt says it is data, and `steering_attempt` checks it before auto-approval.
- The MCP fetcher (`ToolPolicy::TicketFetcher`) must keep `--tools ""`, `--permission-mode dontAsk` and an `--allowed-tools` list of validated `mcp__<server>__<tool>` names (no wildcards). It runs in an empty dir and never sees PR content. The user's other MCP servers are loaded and visible to it, and `dontAsk` is what denies them. `fetcher_args_allow_only_the_named_mcp_tools` and `fetch_runs_a_locked_down_session` assert this.
- The reviewer never gets MCP or network access for tickets.

Custom skills are copied into a per-review plugin dir (`review/skills.rs`). Symlinks inside skill folders are skipped on purpose.
