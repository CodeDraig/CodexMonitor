---
name: app-server-events-sync
description: Maintain CodexMonitor and Codex app-server protocol parity. Use when asked to audit supported or missing app-server notifications/requests, trace event routing, diagnose schema drift, or update docs/app-server-events.md after upstream Codex changes.
---

# App-server events sync

Use `docs/app-server-events.md` as the canonical protocol runbook.

1. Record the selected Codex executable's version and generate experimental schemas
   into a fresh temporary directory. If `../Codex` exists, record its hash without
   fetching, checking out, or pruning. The sibling checkout is optional.
2. Compare notification and request method sets against the shared Rust transport,
   native controller, typed client, and daemon RPC mappings. Keep stable and
   experimental inventories distinct.
3. Inspect payload schemas and Rust normalization when method lists are unchanged.
4. Implement shared behavior in shared cores first, then client/daemon bindings,
   native controller state, and rendering. Preserve app/daemon parity.
5. Update supported/missing methods in the canonical runbook. State deliberate
   non-support and concrete remaining acceptance gaps.
6. Run the schema validator, focused behavioral tests, native-only boundary check,
   Rust checks, and native controller tests. Native desktop acceptance is separate.

## Key files

- Transport and event routing: `src-tauri/src/backend/app_server.rs`, `backend/app_server/*`.
- Shared requests/history: `src-tauri/src/shared/codex_core.rs`, `shared/codex_core/*`.
- Typed client and event decoding: `src-tauri/src/client/*`.
- Controller and hydration: `src-tauri/src/desktop/controller.rs`, `desktop/controller/*`.
- Composer and permissions: `src-tauri/src/desktop/app/composer.rs`.
- Content projection/rendering: `src-tauri/src/desktop/content.rs`, `desktop/content/*`.
- Daemon RPC: `src-tauri/src/bin/codex_monitor_daemon/rpc.rs` and `rpc/*`.
- Validation commands: `references/quick-commands.md`.
