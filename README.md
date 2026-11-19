# PR Helper

A desktop app for reviewing pull requests with Claude across GitHub, GitHub Enterprise, GitLab.com and self-managed GitLab.

- Connect several servers and see every open PR/MR your account can see (filters: review requested, involved, authored, all).
- Start a review: the PR is checked out locally and a read-only, headless Claude Code session reviews it.
- Findings are sorted by severity, each with an explanation, diff context and a proposed comment you can edit. You pick which comments to post, and choose Comment, Request changes or Approve.
- Settings you can set per server and per repository (both override the global defaults):
  - comment language
  - strict ban on AI attribution
  - skip nits
  - auto re-review on new commits when the last verdict wasn't approve
  - auto-approve on an approve verdict
  - model, effort and budget
- Proposed comments never contain double dashes or em/en dashes in prose. This is enforced in code before anything is posted.
- Custom review skills per server or repository: point at a folder with a `SKILL.md`, or write one in the app.
- Runs in the menu bar. Reviewed PRs are polled for new commits and you get a notification. It can also launch at login.

## Install

```sh
brew install --cask joessst-dev/tap/pr-helper
```

The app is ad-hoc signed, not notarized (no Apple Developer ID). The cask clears the quarantine flag so Gatekeeper lets it open. Because the ad-hoc signature changes with every build, macOS may ask once after an upgrade whether PR Helper may access its Keychain items; choose "Always Allow". `brew uninstall --zap` removes app data but not the tokens in the Keychain.

## Requirements

- [Claude Code](https://claude.com/claude-code), logged in (`claude` on your PATH, or set its path in Settings)
- git
- Rust (stable) and Node 20+ to build from source

## Development

```sh
npm install
npm run tauri dev     # run the app
npm run check         # typecheck, frontend tests, rustfmt, clippy, Rust tests
npm run tauri build   # build the .app / .dmg
```

## Tokens

- **GitHub:** a classic PAT with `repo` and `read:org`, or a fine-grained token with Pull requests (read/write) and Contents (read).
- **GitLab:** a PAT with the `api` scope.

Tokens are stored in the system keychain.

## Releasing

1. Bump the version in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json` (they must match), commit and push.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

`.github/workflows/release.yml` builds a universal `.dmg`, publishes a GitHub Release and updates `Casks/pr-helper.rb` in [Joessst-Dev/homebrew-tap](https://github.com/Joessst-Dev/homebrew-tap). Tags with a `-` (e.g. `v0.2.0-rc1`) become prereleases and skip the tap. Running the workflow by hand only builds (dry run).

The tap push needs the repo secret `HOMEBREW_TAP_TOKEN`: a fine-grained PAT with Contents read/write on `homebrew-tap` only.
