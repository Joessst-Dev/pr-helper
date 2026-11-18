---
name: add-tauri-command
description: Checklist for exposing new backend functionality to the PR Helper React frontend, covering the Tauri command, the handler registration, ts-rs type generation, the typed api.ts wrapper, capability permissions and backend events. Use when adding or changing anything the UI calls or listens to.
---

# Adding a Tauri command

1. Put the logic in a module or on `Service` (`src-tauri/src/service.rs`) so it is testable without Tauri. Commands in `commands.rs` stay thin: `svc.something(...)`.
2. Add `#[tauri::command] pub fn|async fn name(svc: Svc<'_>, ...) -> AppResult<T>` to `src-tauri/src/commands.rs`. `AppError` serializes to a string for the UI.
3. Register it in `tauri::generate_handler![...]` in `src-tauri/src/lib.rs`. Forgetting this only fails at runtime.
4. Types that cross the boundary: `#[derive(Serialize, TS)] #[serde(rename_all = "camelCase")] #[ts(export)]`. Annotate `u64` with `#[ts(type = "number")]`, otherwise TS gets `bigint`. Run `cargo test` to write `src/lib/bindings/*.ts`. Never hand-edit those files.
5. Add a typed wrapper to `src/lib/api.ts`. Argument keys are camelCase versions of the Rust parameter names (`server_id` → `serverId`).
6. Using a Tauri plugin from JS (dialog, opener, …)? Add its permission to `src-tauri/capabilities/default.json`.
7. Push updates from the backend through `UiSink` (`service.rs`), implemented by `TauriSink` in `lib.rs`. On the frontend, invalidate React Query keys in `src/lib/events.ts`.
8. Run the `verify-changes` skill.
