---
name: add-git-provider
description: Step-by-step guide for adding a new git hosting provider (e.g. Bitbucket, Gitea/Forgejo, Azure DevOps) to PR Helper behind the GitProvider trait, including listing PRs, inline comment anchoring, approvals, git auth and frontend wiring. Use when the user asks to support another forge or change provider behaviour.
---

# Adding a git provider

Reference implementations: `src-tauri/src/providers/github.rs` and `gitlab.rs`. Copy their structure, including the wiremock tests.

## Backend

1. **Kind**: add a variant to `ProviderKind` in `src-tauri/src/db/models.rs` and update `as_str`/`parse`. Kinds are stored as strings, so no migration is needed.
2. **Module**: create `providers/<name>.rs` with a struct that holds `server_id`, the API base, the token, a `tokio::sync::OnceCell<String>` username and a `reqwest::Client`, and implement `GitProvider` (`providers/mod.rs`):
   - `current_user`: used to verify tokens on save.
   - `list_open_prs(PrFilter)`: support `All` / `ReviewRequested` / `Authored` / `Involved`. Respect `server.pr_scope` to limit `All`. Paginate with a page cap. Sort by `updated_at` desc and dedupe.
   - `get_pr`: must fill `clone_url`, `head_sha`, `base_sha` (and `start_sha` if the forge needs it for positions) and `state`. The watcher relies on `head_sha` + `state`.
   - `post_review(ReviewSubmission)`: inline comments use `InlineComment` (`path`/`old_path`, `line`, optional `start_line`, `side`, and both `new_line`/`old_line` for context lines). If the forge rejects a position, **fold** the comment into the review body with `fold_comment` and report it in `PostResult.folded`. Never drop a comment silently.
   - `approve`: pin it to the reviewed head SHA if the API allows, so a newer push doesn't get approved.
   - `git_auth_header`: `basic_auth_header(user, token)` with the forge's expected username.
   - `pr_fetch_ref`: the ref that holds the PR head on the base repo (forks!).
   - Map non-2xx responses through `check(NAME, resp)` so errors reach the UI with the provider's message.
3. **Factory**: add the variant to `make_provider` in `providers/mod.rs`.
4. **Posting edge case**: `review/post.rs` matches on `server.kind` when the PR head moved since the review. Decide whether inline comments can target the old commit, or must be folded.

## Frontend

- `src/pages/ServerPages.tsx`: add entries to `URL_HINT`, `TOKEN_HINT` and `SCOPE_HINT` (typed `Record<ProviderKind, …>`, so tsc errors until you do), plus an option in the provider `Segmented`.
- `src/App.tsx`: the sidebar shows a two-letter kind label.
- Regenerate bindings with `cargo test` (`ProviderKind.ts`).

## Tests (required)

- wiremock tests for: list mapping, including pagination and review-requested/authored flags; `get_pr` mapping, including merged/closed state; inline comment payload, including a multi-line range and a suggestion; folding on a rejected position; approve payload.
- Then run the `verify-changes` skill. Mention in the summary that the provider has only been tested against mocks until it's tried against a real server.
