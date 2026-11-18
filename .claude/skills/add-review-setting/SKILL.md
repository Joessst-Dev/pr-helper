---
name: add-review-setting
description: How to add or change a review behaviour setting that is configurable globally, per server and per repository (like comment language, skip nits, auto-review, auto-approve, forbid AI attribution) in PR Helper, end to end from Rust resolution to the settings UI. Use whenever the user wants a new per-server or per-repo option.
---

# Adding a review setting

Settings resolve **defaults → global → server → repo**. The most specific level wins, and the UI shows where each value came from.

1. **`src-tauri/src/settings.rs`**
   - Add an `Option<T>` field to `SettingsOverrides` (camelCase over serde, so old JSON still deserializes thanks to `#[serde(default)]`).
   - Add a `Sourced<T>` field to `EffectiveSettings`.
   - In `resolve()`, pick it with `pick(default, levels!(field))`, or `pick_opt` when "unset" is a valid final value. Clamp unsafe values there (see `poll_interval_secs`).
   - Add or extend a test in `mod tests`.
2. **Use it** where the behaviour lives:
   - prompt text: `review/prompt.rs` `system_prompt` / `user_prompt`
   - post-processing: `review/postprocess.rs` `process` / `finalize_body`
   - after-review actions: `review/pipeline.rs` `after_review`
   - polling: `watcher.rs` (keep `decide()` pure and tested)
   Read settings through `Service::effective_settings(server_id, Some(repo))`, never from one level directly.
3. **Bindings**: run `cargo test` to regenerate `src/lib/bindings/SettingsOverrides.ts` and `EffectiveSettings.ts`.
4. **Frontend**
   - `src/lib/settings.ts`: add the key to `EMPTY_OVERRIDES` (tsc fails until you do).
   - `src/components/SettingsForm.tsx`: add a `<Row>`. Use `BoolSelect` for booleans and `inheritLabel(inherited?.field, fmt)` so "Inherit: X (source)" shows. The same form serves the global, server and repo levels.
   - `src/test/fixtures.ts`: add the field to `effective`.
5. Run the `verify-changes` skill.

Existing settings and their defaults: `commentLanguage` English, `forbidAiAttribution` true, `attributionFooter` "", `skipNits` false, `autoReviewOnNewCommits` false, `autoApproveOnApproveVerdict` false, `pollIntervalSecs` 300 (min 60), `model`/`effort`/`maxBudgetUsd` unset, `defaultSelectedMinSeverity` medium. App-wide (non-hierarchical) options belong in `AppConfig` instead.
