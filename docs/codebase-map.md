# Codebase map

CodexMonitor has one frontend: the Rust egui/eframe/wgpu desktop. The crate is
located in `src-tauri/`; native and daemon execution use its shared Rust cores.

## Change flow

Implement cross-runtime behavior in `src-tauri/src/shared/*` first. Extend typed
requests/replies in `client/contracts.rs`, local dispatch in `client/local.rs`,
remote mapping in `client/remote.rs`, and daemon RPC handlers together. Native
controller actions enqueue work; frame drawing never waits for backend calls.

## Task ownership

| Task | Files under `src-tauri/src/` |
| --- | --- |
| Start the desktop | `bin/codex_monitor_native.rs`, `lib.rs`, `desktop/mod.rs` |
| Shell, menus, sidebar, composer, dialogs | `desktop/app.rs`, `desktop/app/*` |
| Request/event orchestration and conversation state | `desktop/controller.rs`, `desktop/controller/*` |
| Markdown, tools, media, reasoning, and diffs | `desktop/content.rs`, `desktop/content/*`, `desktop/app/media.rs` |
| Git, Files, Runs, Prompts, Settings | `desktop/workbench/*` |
| Window, picker, clipboard, audio, tray, updates | `desktop/platform.rs`, `desktop/platform/*` |
| Local/daemon backend boundary | `client/contracts.rs`, `client/mod.rs`, `client/events.rs`, `client/local.rs`, `client/remote.rs` |
| Codex subprocess and transport | `backend/app_server.rs`, `backend/app_server/*`, `backend/events.rs` |
| ACP subprocess transport | `backend/acp.rs` |
| ACP profiles, connections, callbacks, replay, projection | `shared/agent_runtime.rs`, `shared/agent_runtime/*`, `shared/runtime_rpc.rs` |
| Workspaces and worktrees | `shared/workspaces_core.rs`, `shared/workspaces_core/*`, `shared/worktree_core.rs` |
| Shared paths, initialization, settings, and persistence | `shared/host_core.rs`, `shared/settings_core.rs`, `shared/remote_settings.rs`, `types.rs`, `storage.rs` |
| Codex account, turns, permissions, history, agents, config | `shared/codex_core/*`, `shared/codex_aux_core.rs`, `shared/agents_config_core.rs`, `codex/{args,config,home}.rs` |
| Git and GitHub | `shared/git_core.rs`, `shared/git_ui_core.rs`, `shared/git_ui_core/*` |
| File policies and workspace reads | `files/{io,ops,policy}.rs`, `shared/files_core.rs`, `shared/workspace_files_core.rs` |
| Prompts and usage | `shared/prompts_core.rs`, `shared/local_usage_core.rs` |
| Runs, capture, evidence, comparisons | `shared/runs_core.rs`, `shared/runs_core/*`, `shared/runs_rpc.rs` |
| Native presentation state | `shared/ui_state_core.rs`, `desktop/app/presentation.rs` |
| PTY and terminal emulator | `shared/terminal_core.rs`, `desktop/terminal.rs` |
| Microphone, models, transcription | `shared/dictation_core.rs` |
| Managed daemon and Tailscale | `shared/managed_daemon_core.rs`, `tailscale/{core,rpc_client}.rs`, `daemon_binary.rs` |
| Daemon entry and transport | `bin/codex_monitor_daemon.rs`, `bin/codex_monitor_daemon/{config,events,transport}.rs` |
| Daemon RPC | `bin/codex_monitor_daemon/rpc.rs`, `bin/codex_monitor_daemon/rpc/*` |
| Standalone daemon lifecycle | `bin/codex_monitor_daemonctl.rs` |

Keep JSON-RPC names and payload shapes stable. Local and remote drivers must agree
on typed requests, durable events, connection epoch, store, and conversation scope.
Backend switches must reject stale replies and preserve backend-specific drafts.

## Build and resources

`native-gui` is the default feature and enables egui/eframe/wgpu plus desktop OS
services. `native-acceptance` adds eframe inspection support for UI/controller
tests. `--no-default-features` builds the host-independent core; enable
`acp-host-services` for headless ACP authentication PTYs. Native notification
sounds live under `src-tauri/assets/`, and package icons under `src-tauri/icons/`.

`scripts/build-native.sh` checks the active dependency graph, builds the desktop,
daemon, and control binaries, then calls `scripts/package-native.py` to stage
resources and an unsigned archive. `scripts/check-native-only.py` verifies the
frontend/build boundary. CI and manual release workflows use the same native
package path. Setup, commands, and platform requirements are in `README.md`.

## Tests and acceptance

Rust tests live beside the shared cores, backend drivers, native controller,
rendering, platform, and daemon handlers. `tests/fixtures/app-server-contracts.json`
is shared protocol data. ACP fixtures are under
`src-tauri/src/shared/agent_runtime/fixtures/`. Python and shell runners under
`scripts/` cover protocol conformance, contention, daemon contracts, and packages.

Preserve the five implementation plans. Use isolated data and explicit executable
paths for native local/daemon acceptance; builds do not change the installation.
