# CodexMonitor Agent Guide

All docs must be canonical, with no past commentary, only live state.

## Scope

This file is the repository agent contract. Navigation and runbooks live in
`docs/codebase-map.md`, `docs/multi-agent-sync-runbook.md`, and `README.md`.

## Project Snapshot

CodexMonitor is a Rust desktop workbench for Codex and ACP agents.

- Desktop: egui + eframe + wgpu (`src-tauri/src/desktop/`).
- Entry point: `src-tauri/src/bin/codex_monitor_native.rs`.
- Daemon: JSON-RPC process (`src-tauri/src/bin/codex_monitor_daemon.rs`).
- Shared backend source of truth: `src-tauri/src/shared/*`.
- Typed local/daemon boundary: `src-tauri/src/client/*`.

## Non-Negotiable Architecture Rules

1. Put shared/domain backend logic in `src-tauri/src/shared/*` first.
2. Keep desktop and daemon as thin adapters around shared cores.
3. Do not duplicate logic between desktop and daemon.
4. Keep JSON-RPC names and payloads stable unless intentionally changing contracts.
5. Keep typed client contracts, local dispatch, and daemon surfaces aligned.

## Backend Routing Rules

For cross-runtime behavior, update in order:

1. Shared core (`src-tauri/src/shared/*`).
2. Typed request/reply boundary (`src-tauri/src/client/contracts.rs`).
3. Local and remote drivers (`client/local.rs`, `client/remote.rs`).
4. Daemon router (`bin/codex_monitor_daemon/rpc.rs` and `rpc/*`).
5. Native controller and the affected UI (`desktop/controller*`, `desktop/app/*`).

When adding a command, update all relevant layers and contract tests.
Keep correlated replies, events, drafts, and permission state scoped by backend,
store, conversation, and connection epoch. Drawing must not block on backend work.

## Frontend Routing Rules

- Keep frame composition in `desktop/app.rs` and feature drawing in `desktop/app/*`.
- Keep request/event orchestration in `desktop/controller.rs` and `controller/*`.
- Keep conversation rendering in `desktop/content.rs` and `content/*`.
- Keep Git, Files, Runs, Prompts, and Settings panels in `desktop/workbench/*`.
- Keep window, clipboard, picker, audio, and OS integrations in `desktop/platform*`.
- Keep frontend resources under `src-tauri/assets/` and package icons under `src-tauri/icons/`.
- Use existing native theme, typography, spacing, and widget conventions.

## Sidebar and Conversation Invariants

- Preserve incoming order and required active, processing, and ancestor anchors
  when reconciling partial conversation lists.
- Never resurrect locally hidden conversations during reconciliation.
- Render children under a parent only when the parent is visible; otherwise
  promote the child to a root.
- Resolve Codex versus ACP conversation identity before issuing commands.
- Preserve Queue versus Steer behavior, cancellation, and unsent drafts.

## App/Daemon Parity Checklist

1. Shared behavior updated, or explicitly desktop-only/daemon-only.
2. Typed client contract and local driver updated.
3. Remote mapping and daemon RPC updated.
4. Contract and regression coverage updated.

## Safety and Git Behavior

- Prefer safe Git operations (`status`, `diff`, `log`).
- Do not reset or revert unrelated changes.
- Continue around unrelated edits unless they block correctness.
- If conflicts affect correctness, identify them and take the safest path.
- Fix the root cause.
- Preserve all existing implementation plans and the current installation unless
  the user explicitly requests changing them.

## Validation Matrix

- Always: `python3 scripts/check-native-only.py`.
- Shared backend: `cargo check --locked --no-default-features --lib --manifest-path src-tauri/Cargo.toml`.
- Shared/daemon tests: `cargo test --locked --no-default-features --features acp-host-services --all-targets --manifest-path src-tauri/Cargo.toml`.
- Native UI/controller: `cargo test --locked --no-default-features --features native-acceptance --all-targets --manifest-path src-tauri/Cargo.toml`.
- Package changes: `python3 scripts/package-native.py --self-test` and `bash scripts/test-build-native-target-dir.sh`.
- Use focused tests while iterating, then required full checks. Do not claim GUI
  or platform acceptance from compilation or unit tests alone.

## Quick Runbook

```bash
cargo run --locked --manifest-path src-tauri/Cargo.toml --bin codex_monitor_native
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --no-default-features --features native-acceptance --manifest-path src-tauri/Cargo.toml
./scripts/build-native.sh --output-dir /path/to/new-package-directory
```

Use isolated data directories for runtime acceptance. Do not install test builds
or alter live data without authorization. Builds and packages do not imply an
installation change.

## Hotspots

Use extra care in `desktop/controller.rs`, `desktop/app.rs`,
`shared/agent_runtime/*`, `shared/git_ui_core.rs`, `shared/workspaces_core.rs`,
and `bin/codex_monitor_daemon/rpc.rs`.
