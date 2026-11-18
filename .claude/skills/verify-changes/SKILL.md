---
name: verify-changes
description: How to verify changes to PR Helper, from the full check suite and focused test runs to the opt-in real-Claude review test, a headless app smoke test on Linux, and the manual macOS checklist. Use before reporting any change to PR Helper as done.
---

# Verifying changes

## Always

```sh
npm run check
```
This runs `tsc --noEmit`, vitest, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. All must pass. Report failures with their output, never as "mostly working".

Focused runs:
- `npx vitest run src/components/FindingCard.test.tsx`
- `cd src-tauri && cargo test postprocess` (comment rules)
- `cargo test e2e` (queue → git → fake Claude → store → auto-approve → post)
- `cargo test providers` (wiremock GitHub/GitLab)

If you change the Rust types, `cargo test` regenerates `src/lib/bindings/`. Commit those files too.

## When reviewer behaviour changes

Real Claude, cheap model, local fake forge:
```sh
cd src-tauri && cargo test real_claude -- --ignored --nocapture
```
Check the printed findings: in the configured language, sorted by severity, no dashes in prose, `in_diff=true` for line findings. To check the CLI contract directly, run `claude -p --output-format stream-json --verbose --json-schema '<schema>' ...` and inspect the `init.tools` and `result.structured_output` events.

## App smoke test (Linux sandbox, no display)

```sh
npx vite --port 1420 --strictPort &          # dev build loads the dev server
cd src-tauri && cargo build
xvfb-run -a -s "-screen 0 1400x900x24" dbus-run-session -- ./target/debug/pr-helper
```
Desktop-portal and Wayland warnings are sandbox noise; look for panics. To screenshot, run `import -window root shot.png` inside the xvfb session. Don't `pkill -f` with a pattern that appears in your own command line.

## On macOS (the user's real target)

`npm run tauri dev`, then:
1. Add a GitHub and a GitLab server; tokens land in Keychain Access under `dev.jostweyers.prhelper`.
2. Review a PR, edit a comment adding an em dash, check the warning, post, and verify the posted text on the forge.
3. Push a commit and see the notification (and the auto-review if enabled).
4. Get an approve verdict with auto-approve on, and confirm the approval was posted and you were notified.
5. Close the window: the app stays in the menu bar.

Dev builds are unsigned, so macOS may ask for Keychain access after each rebuild ("Always Allow").
