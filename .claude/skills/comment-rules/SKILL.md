---
name: comment-rules
description: The hard rules for review comments PR Helper proposes or posts (no double dashes or em/en dashes in prose, no AI attribution when forbidden, severity ordering, skipping nits). Use whenever touching review output, prompts, posting, finding display, or anything in src-tauri/src/review/postprocess.rs, prompt.rs, post.rs or the FindingCard/ReviewPage UI.
---

# Comment rules

These are product requirements, not style preferences. Every rule is enforced **deterministically in Rust**. The prompt also states them, but the model's output is never trusted.

| Rule | Where it is enforced |
|---|---|
| No `--`, `—` (em dash) or `–` (en dash) in prose. Inline code, fenced code blocks and URLs are exempt. | `sanitize_dashes` / `dash_violations` in `src-tauri/src/review/postprocess.rs` |
| When `forbid_ai_attribution` is on (default `true`): no Claude/Anthropic/AI attribution, signatures, 🤖, `Co-Authored-By`, "generated with…", vendor links, and no footer | `strip_attribution` / `attribution_violations` |
| Findings sorted by severity `critical > high > medium > low > nit`, then file and line | `sort_findings`, also on the frontend (rows arrive by `position`) |
| `skip_nits` drops nit findings entirely | `process()` filter, plus the prompt line in `prompt.rs` |
| An `approve` verdict with any medium-or-higher finding is downgraded | `process()` |
| Comment language is per server/repo (`comment_language`) | `system_prompt()` in `prompt.rs` |

## The two gates

1. `process()` sanitizes model output before it is stored.
2. `finalize_body()` runs on the **final text right before posting**. That covers user edits, the summary, the approval body, and the joined body including fold headings. It sanitizes first, then refuses to post if any violation remains. Never bypass it. Any new code path that sends text to a provider must call `finalize_body` (see `review/post.rs` and `auto_approve` in `review/pipeline.rs`).

The UI shows live warnings through the `check_comment_text` command (`check_comment`), so users see what will be rewritten before they post.

## Changing a rule

1. Add a failing test to `mod tests` in `postprocess.rs` first. Include the code-exemption case, e.g. `` `git push --force` `` must survive.
2. Implement it inside `map_prose_lines`. That helper masks code spans and URLs as placeholders, so regexes only ever see prose.
3. If the rule matters to the model, update `system_prompt()` and its test in `prompt.rs`.
4. Run the `verify-changes` skill.

Pitfalls:
- Dash replacements depend on context: ranges `3–5` become `3-5`, list markers `-- x` become `- x`, arrows `-->` become `→`, bare flags `--foo` become `` `--foo` ``, horizontal rules are dropped, and markdown table delimiter rows shrink to single `-`.
- Attribution rules strip sentences, not words, and leave legitimate mentions in code (`` `ClaudeClient` ``) alone. Don't add a blanket "remove the word Claude" rule. This app reviews code that may legitimately mention it.
