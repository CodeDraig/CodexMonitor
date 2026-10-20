# Codebase Map (Task-Oriented)

Canonical navigation guide for CodexMonitor. Use this as: "if you need X, edit Y".

Related docs:

- Setup/build/release: `README.md`
- iOS remote over Tailscale (TCP): `docs/mobile-ios-tailscale-blueprint.md`

## Desktop Host Paths

Desktop currently has two frontend hosts. The native Rust desktop is the native desktop target; Tauri/React remains the compatibility desktop frontend and the existing iOS app path. The iOS app has not been replaced by the egui target.

- Tauri + React desktop and iOS: `src-tauri/src/lib.rs`, `src/`, and `src/services/tauri.ts`.
- Native Rust desktop entrypoint: `src-tauri/src/bin/codex_monitor_native.rs`, with app/controller/content/workbench UI under `src-tauri/src/desktop/`. It uses egui/eframe with wgpu and does not load generated web assets.
- Shared path resolution and process state: `src-tauri/src/shared/host_core.rs` (`HostPaths`, `CoreState`). The native desktop and daemon use this initialization and the existing workspace/settings/run-store locations.
- Typed request/event boundary: `src-tauri/src/client/contracts.rs`, `events.rs`, `local.rs`, and `remote.rs`. `BackendScope` carries endpoint, connection epoch, and store identity; `BackendHandle` dispatches local operations to shared cores and remote operations over daemon JSON-RPC.
- Native desktop orchestration and drawing: `src-tauri/src/desktop/controller.rs`, `app.rs`, `content/`, and `workbench/`.
- Native window, file picker, clipboard, audio, and platform services: `src-tauri/src/desktop/platform.rs` and `src-tauri/src/desktop/platform/*`.
- Shared terminal model and versioned UI state: `src-tauri/src/shared/terminal_core.rs`, `src-tauri/src/shared/ui_state_core.rs`, and `src-tauri/src/ui_state.rs`.
- Shared managed-daemon lifecycle used by native platform code and Tauri adapters: `src-tauri/src/shared/managed_daemon_core.rs`, `src-tauri/src/desktop/platform.rs`, `src-tauri/src/tailscale/`, and `src-tauri/src/state.rs`.
- Native package staging and active dependency-graph check: `scripts/build-native.sh` and `scripts/package-native.py`.

Build a native preview with `scripts/build-native.sh --output-dir <new-path>`. The script builds with `--locked --no-default-features --features native-gui`, checks that the active normal/build dependency graph excludes Tauri and webview crates, builds the native app plus daemon and daemon-control binaries, and stages icons, font-license resources, platform launch metadata, and an unsigned package manifest. It does not run npm or change the installed app. On Linux the bundle has a desktop-entry template with `@BUNDLE_PREFIX@` placeholders; render an absolute-prefix entry for an extracted bundle with `scripts/package-native.py --write-linux-desktop-entry --prefix <bundle-path> --desktop-entry-output <new-entry-path>`. This writes the specified entry but does not install/register it. The package uses egui's embedded default fonts, fontique-backed system font lookup, and fallback to the embedded fonts when a configured system face is unavailable; applicable font licenses and attributions are staged with the app. The separate Tauri build scripts remain the iOS and compatibility path. Native preview artifacts are unsigned and use a channel separate from Tauri updater metadata.

Cargo features are declared in `src-tauri/Cargo.toml`: default `tauri-host` enables Tauri/plugins and the `codex-monitor` binary; `native-gui` enables egui/eframe, wgpu, native controls/rendering, and related desktop integrations; `native-acceptance` adds eframe inspection support for UI/controller tests. The existing Tauri `custom-protocol` feature extends `tauri-host`. Native GUI builds use `--no-default-features` so the Tauri host is absent from the active native package graph. Native build dependencies and reproducible commands are listed in `README.md`.

For desktop backend changes, keep cross-host behavior in `src-tauri/src/shared/*`; route local and daemon operations through `src-tauri/src/client/*`; keep native window/OS integration in `src-tauri/src/desktop/platform.rs`; keep drawing and UI state transitions in `src-tauri/src/desktop/*`. The eframe frame function reads state and issues actions; asynchronous backend work goes through the client/event bridge. The legacy Tauri path continues through `src-tauri/src/lib.rs`, app adapters, `src/services/tauri.ts`, and the React feature tree.

## Start Here: How Changes Flow

For backend behavior, follow this path in order:

1. Frontend callsite: `src/features/**` hooks/components
2. Frontend IPC API: `src/services/tauri.ts`
3. Tauri command registration: `src-tauri/src/lib.rs` (`invoke_handler`)
4. App adapter: `src-tauri/src/{codex,workspaces,git,files,settings,prompts}/*`
5. Shared core source of truth: `src-tauri/src/shared/*`
6. Daemon RPC method parity: `src-tauri/src/bin/codex_monitor_daemon/rpc.rs`
7. Daemon command bindings: `src-tauri/src/bin/codex_monitor_daemon/rpc/*`
8. Standalone daemon lifecycle CLI: `src-tauri/src/bin/codex_monitor_daemonctl.rs`

If a behavior must work in both app and daemon, implement it in `src-tauri/src/shared/*` first.

## If You Need X, Edit Y

| Need | Primary files to edit |
| --- | --- |
| Change native desktop UI composition, sidebar, or dialogs | `src-tauri/src/desktop/app.rs`, `src-tauri/src/desktop/app/*` |
| Change native message rendering, markdown, or diffs | `src-tauri/src/desktop/content.rs`, `src-tauri/src/desktop/content/*` |
| Change native backend orchestration, requests, or event application | `src-tauri/src/desktop/controller.rs`, `src-tauri/src/desktop/controller/*`, `src-tauri/src/client/contracts.rs`, `src-tauri/src/client/events.rs` |
| Change native local or daemon backend transport | `src-tauri/src/client/local.rs`, `src-tauri/src/client/remote.rs`, `src-tauri/src/client/mod.rs` |
| Change native window, OS, tray, or host services | `src-tauri/src/desktop/platform.rs`, `src-tauri/src/desktop/platform/*` |
| Change native package layout, launcher metadata, or package smoke checks | `scripts/build-native.sh`, `scripts/package-native.py`, `.github/workflows/ci.yml`, `.github/workflows/release.yml` |
| App-level UI composition/layout wiring | `src/App.tsx`, `src/features/app/components/AppLayout.tsx`, `src/features/app/bootstrap/*`, `src/features/app/orchestration/*`, `src/features/app/hooks/*` |
| Add/change Tauri IPC methods used by frontend | `src/services/tauri.ts`, `src-tauri/src/lib.rs`, matching backend adapter module |
| Add/change app-server event handling in UI | `src/services/events.ts`, `src/features/app/hooks/useAppServerEvents.ts`, `src/utils/appServerEvents.ts`, `src/features/threads/utils/threadNormalize.ts` |
| Change thread state transitions | `src/features/threads/hooks/useThreadsReducer.ts`, `src/features/threads/hooks/threadReducer/*`, `src/features/threads/hooks/useThreads.ts`, focused thread hooks under `src/features/threads/hooks/*` |
| Change durable runs, recording, or comparison | `src-tauri/src/shared/runs_core/*`, `src-tauri/src/shared/runs_rpc.rs`, `src-tauri/src/runs.rs`, `src/features/runs/*`, `src/features/workspaces/hooks/useWorkspaceHome.ts`; see `docs/run-records.md` |
| Change workspace lifecycle/worktree behavior | `src/features/workspaces/hooks/useWorkspaces.ts`, `src-tauri/src/workspaces/commands.rs`, `src-tauri/src/shared/workspaces_core.rs`, `src-tauri/src/shared/workspaces_core/*`, `src-tauri/src/shared/worktree_core.rs` |
| Change settings model/load/update | `src/features/settings/components/SettingsView.tsx`, `src/features/settings/hooks/useAppSettings.ts`, `src/services/tauri.ts`, `src-tauri/src/settings/mod.rs`, `src-tauri/src/shared/settings_core.rs`, `src-tauri/src/types.rs`, `src/types.ts` |
| Change Git/GitHub backend behavior | `src/features/git/hooks/*`, `src/services/tauri.ts`, `src-tauri/src/git/mod.rs`, `src-tauri/src/shared/git_ui_core.rs`, `src-tauri/src/shared/git_ui_core/*`, `src-tauri/src/shared/git_core.rs`, `src-tauri/src/bin/codex_monitor_daemon/rpc.rs`, `src-tauri/src/bin/codex_monitor_daemon/rpc/git.rs` |
| Change prompts CRUD/listing behavior | `src/features/prompts/hooks/useCustomPrompts.ts`, `src/features/prompts/components/PromptPanel.tsx`, `src/services/tauri.ts`, `src-tauri/src/prompts.rs`, `src-tauri/src/shared/prompts_core.rs`, `src-tauri/src/bin/codex_monitor_daemon/rpc.rs` |
| Change file read/write for Agents/config | `src/services/tauri.ts`, `src-tauri/src/files/mod.rs`, `src-tauri/src/shared/files_core.rs`, `src-tauri/src/bin/codex_monitor_daemon/rpc.rs` |
| Add/change daemon JSON-RPC surface | `src-tauri/src/bin/codex_monitor_daemon/rpc.rs`, `src-tauri/src/bin/codex_monitor_daemon/rpc/*`, `src-tauri/src/bin/codex_monitor_daemon.rs`, matching shared core |

## Frontend Navigation

- Composition root: `src/App.tsx`
- App bootstrap orchestration: `src/features/app/bootstrap/*`
- App layout/thread/workspace orchestration: `src/features/app/orchestration/*`
- Tauri IPC wrapper: `src/services/tauri.ts`
- Tauri event hub (single-listener fanout): `src/services/events.ts`
- Event subscription hook: `src/features/app/hooks/useTauriEvent.ts`
- App-server event router: `src/features/app/hooks/useAppServerEvents.ts`
- Shared frontend types: `src/types.ts`

### Import Aliases

Use TS/Vite aliases for refactor-safe imports:

- `@/*` -> `src/*`
- `@app/*` -> `src/features/app/*`
- `@settings/*` -> `src/features/settings/*`
- `@threads/*` -> `src/features/threads/*`
- `@services/*` -> `src/services/*`
- `@utils/*` -> `src/utils/*`

### Threads

- Orchestrator: `src/features/threads/hooks/useThreads.ts`
- Reducer composition entrypoint: `src/features/threads/hooks/useThreadsReducer.ts`
- Reducer slices: `src/features/threads/hooks/threadReducer/*`
- Event-focused handlers: `src/features/threads/hooks/useThreadEventHandlers.ts`, `src/features/threads/hooks/useThreadTurnEvents.ts`, `src/features/threads/hooks/useThreadItemEvents.ts`, `src/features/threads/hooks/useThreadApprovalEvents.ts`, `src/features/threads/hooks/useThreadUserInputEvents.ts`
- Message send/steer/interrupt: `src/features/threads/hooks/useThreadMessaging.ts`
- Persistence/local thread metadata: `src/features/threads/hooks/useThreadStorage.ts`, `src/features/threads/utils/threadStorage.ts`

### Workspaces

- Workspace state and lifecycle: `src/features/workspaces/hooks/useWorkspaces.ts`
- Workspace home behavior: `src/features/workspaces/hooks/useWorkspaceHome.ts`
- Workspace file list and reads in app layer: `src/features/app/hooks/useWorkspaceFileListing.ts`, `src/features/workspaces/hooks/useWorkspaceFiles.ts`

### Settings

- Main settings surface: `src/features/settings/components/SettingsView.tsx`
- Settings state + persistence flow: `src/features/settings/hooks/useAppSettings.ts`, `src/features/app/hooks/useAppSettingsController.ts`
- Typed settings contracts: `src/types.ts`

### Git

- Git UI hooks: `src/features/git/hooks/*`
- Git panel components: `src/features/git/components/*`
- Branch workflows: `src/features/git/hooks/useGitBranches.ts`, `src/features/git/hooks/useBranchSwitcher.ts`

### Prompts

- Prompt UI and workflow: `src/features/prompts/components/PromptPanel.tsx`, `src/features/prompts/hooks/useCustomPrompts.ts`

## Backend App (Tauri) Navigation

- Command registry (what frontend can invoke): `src-tauri/src/lib.rs`
- Codex command adapters: `src-tauri/src/codex/{threads,subscriptions,turns,models,features,agents,account,login,skills,apps,approvals,mcp,metadata,installation}.rs`
- App-specific Codex session startup: `src-tauri/src/codex/mod.rs`
- Workspace/worktree adapters: `src-tauri/src/workspaces/commands.rs`
- Git adapters: `src-tauri/src/git/mod.rs`
- Settings adapters: `src-tauri/src/settings/mod.rs`
- Prompts adapters: `src-tauri/src/prompts.rs`
- File adapters: `src-tauri/src/files/mod.rs`
- Event emission implementation: `src-tauri/src/event_sink.rs`
- Event payload definitions: `src-tauri/src/backend/events.rs`

## Daemon Navigation

- Daemon startup and shared state: `src-tauri/src/bin/codex_monitor_daemon.rs`
- Daemon CLI configuration: `src-tauri/src/bin/codex_monitor_daemon/config.rs`
- Daemon event broadcasting: `src-tauri/src/bin/codex_monitor_daemon/events.rs`
- Daemon lifecycle CLI (headless start/stop/status): `src-tauri/src/bin/codex_monitor_daemonctl.rs`
- Daemon JSON-RPC dispatcher/router: `src-tauri/src/bin/codex_monitor_daemon/rpc.rs`
- Daemon RPC parsing and shared-core bindings: `src-tauri/src/bin/codex_monitor_daemon/rpc/*`
- Daemon transport: `src-tauri/src/bin/codex_monitor_daemon/transport.rs`

When adding a new method, keep method names and payload shape aligned with `src/services/tauri.ts` and app commands in `src-tauri/src/lib.rs`.

## Shared Cores (Source of Truth)

All cross-runtime domain behavior belongs in `src-tauri/src/shared/*`:

- Codex threads/approvals/account/skills/config: `src-tauri/src/shared/codex_core/*`
- Codex helper commands: `src-tauri/src/shared/codex_aux_core.rs`
- Codex update/version helpers: `src-tauri/src/shared/codex_update_core.rs`
- Workspaces/worktrees: `src-tauri/src/shared/workspaces_core.rs`, `src-tauri/src/shared/workspaces_core/*`, `src-tauri/src/shared/worktree_core.rs`
- Settings model/update: `src-tauri/src/shared/settings_core.rs`
- Config/Agents file reads and writes: `src-tauri/src/shared/files_core.rs`
- Workspace file listing and bounded reads: `src-tauri/src/shared/workspace_files_core.rs`
- Git and GitHub logic: `src-tauri/src/shared/git_core.rs`, `src-tauri/src/shared/git_ui_core.rs`, `src-tauri/src/shared/git_ui_core/*`
- Prompts CRUD/listing: `src-tauri/src/shared/prompts_core.rs`
- Usage snapshot and aggregation: `src-tauri/src/shared/local_usage_core.rs`
- Process helpers: `src-tauri/src/shared/process_core.rs`
- Process paths and shared host state (`HostPaths`, `CoreState`): `src-tauri/src/shared/host_core.rs`
- Managed daemon/Tailscale lifecycle shared with Tauri host adapters: `src-tauri/src/shared/managed_daemon_core.rs`
- Native terminal emulator model: `src-tauri/src/shared/terminal_core.rs`
- Versioned native presentation state: `src-tauri/src/shared/ui_state_core.rs`, `src-tauri/src/ui_state.rs`

## Events Map (Backend -> Frontend)

- Backend emits through sink: `src-tauri/src/event_sink.rs`
- App-server event name: `app-server-event`
- Terminal event names: `terminal-output`, `terminal-exit`
- Frontend fanout hubs: `src/services/events.ts`
- Frontend routing into thread state: `src/features/app/hooks/useAppServerEvents.ts` -> thread hooks/reducer under `src/features/threads/hooks/*`

If event payload format changes, update parser/guards first in `src/utils/appServerEvents.ts`.

## Type Contract Files

Keep Rust and TypeScript contracts in sync:

- Rust backend types: `src-tauri/src/types.rs`
- Frontend types: `src/types.ts`

This is required for settings, workspace metadata, app-server payload handling, and RPC response decoding.

## Message Rendering and Dictation

- Message list composition: `src/features/messages/components/Messages.tsx`
- Individual message, tool, diff, review, reasoning, and user-input rows: their named components in `src/features/messages/components/`
- Message quoting and plan export: `src/features/messages/hooks/useMessageQuote.ts`, `src/features/messages/hooks/usePlanExport.ts`
- Markdown composition: `src/features/messages/components/Markdown.tsx`; code-block copying: `MarkdownPreBlock.tsx`; link policy: `markdownLinks.tsx`
- Markdown transformations: `src/features/messages/utils/normalizeListIndentation.ts`, `structuredReviewTables.ts`, `standaloneMarkdownTable.ts`
- Desktop dictation session lifecycle: `src-tauri/src/dictation/real.rs`
- Dictation model lifecycle, device capture, permissions, and transcription: `src-tauri/src/dictation/desktop/*`
- Remote backend selection: `src/features/settings/utils/remoteBackends.ts`, `src-tauri/src/shared/remote_settings.rs`. The selected `remoteBackends` entry owns its connection details.
- App-server startup: `src-tauri/src/backend/app_server.rs`; command resolution, sessions, event routing, and thread/workspace classification: `src-tauri/src/backend/app_server/*`
