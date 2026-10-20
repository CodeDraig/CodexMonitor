# CodexMonitor

[![gitcgr](https://gitcgr.com/badge/Dimillian/CodexMonitor.svg)](https://gitcgr.com/Dimillian/CodexMonitor)

![CodexMonitor](screenshot.png)

CodexMonitor orchestrates Codex and configured ACP agents across local workspaces. Desktop has a Rust/egui frontend and a Tauri/React compatibility frontend; the existing Tauri/React path also remains the iOS app. Both desktop hosts use the shared Rust runtime and data stores.

## Features

### Workspaces & Threads

- Add and persist workspaces, group/sort them, and jump into recent agent activity from the home dashboard.
- Share one `codex app-server` process across connected workspaces in each backend, resume threads, and track unread/running state.
- Worktree and clone agents for isolated work; worktrees live under the app data directory (legacy `.codex-worktrees` supported).
- Thread management: pin/rename/archive/copy, per-thread drafts, and stop/interrupt in-flight turns.
- Optional remote backend (daemon) mode for running Codex on another machine.
- [Agent runtimes](docs/agent-runtimes.md): Codex and configured ACP agents, global session discovery, full conversation replay, and recorded runs through local or daemon execution.
- [Durable run records and comparison](docs/run-records.md): initial-turn captures, saved Git and command evidence, and human comparison notes across local and daemon execution.
- Remote setup helpers for self-hosted connectivity (Tailscale detection/host bootstrap for TCP mode).

### Composer & Agent Controls

- Compose with image attachments (picker, drag/drop, paste) and configurable follow-up behavior (`Queue` vs `Steer` while a run is active).
- Use `Shift+Cmd+Enter` (macOS) or `Shift+Ctrl+Enter` (Windows/Linux) to send the opposite follow-up action for a single message.
- Autocomplete for skills (`$`), prompts (`/prompts:`), reviews (`/review`), and file paths (`@`).
- Model picker, collaboration modes, reasoning effort, access mode, and context usage ring.
- Dictation with hold-to-talk shortcuts and live waveform (Whisper).
- Render reasoning/tool/diff items and handle approval prompts.

### Git & GitHub

- Diff stats, staged/unstaged file diffs, revert/stage controls, and commit log.
- Branch list with checkout/create plus upstream ahead/behind counts.
- GitHub Issues and Pull Requests via `gh` (lists, diffs, comments) and open commits/PRs in the browser.
- PR composer: "Ask PR" to send PR context into a new agent thread.

### Files & Prompts

- File tree with search, file-type icons, and Reveal in Finder/Explorer.
- Prompt library for global/workspace prompts: create/edit/delete/move and run in current or new threads.

### UI & Experience

- Resizable sidebar/right/plan/terminal/debug panels with persisted sizes.
- Responsive layouts (desktop/tablet/phone) with tabbed navigation.
- Sidebar usage and credits meter for account rate limits plus a home usage snapshot.
- Terminal dock with multiple tabs for background commands (experimental).
- In-app updates with toast-driven download/install, debug panel copy/clear, sound notifications, plus platform-specific window effects (macOS overlay title bar + vibrancy) and a reduced transparency toggle.

## Requirements

- Node.js + npm (for the Tauri/React compatibility frontend and existing iOS frontend; not required by the native Rust desktop build)
- Rust toolchain (stable)
- CMake (required for native dependencies; dictation/Whisper uses it)
- LLVM/Clang (required on Windows to build dictation dependencies via bindgen)
- Codex CLI installed and available as `codex` in `PATH` (or configure a custom Codex binary in app/workspace settings)
- Git CLI (used for worktree operations)
- GitHub CLI (`gh`) for GitHub Issues/PR integrations (optional)

If you hit native build errors, run:

```bash
npm run doctor
```

## Codex compatibility

The app-server integration targets Codex CLI 0.162.1 with the experimental API enabled.
See [the protocol reference](docs/app-server-events.md) for supported requests,
notifications, input modes, history behavior, and compatibility checks.

Conversation history is loaded through paginated requests. Command/file approvals,
selected permission grants, and standard MCP form/URL input are supported. Steering
and collaboration modes are core capabilities; removed Codex feature flags do not
disable them. App/plugin feature changes apply to connected servers immediately;
other feature changes require restarting CodexMonitor.

## Getting Started

Install dependencies:

```bash
npm install
```

Run in dev mode:

```bash
npm run tauri:dev
```

## Native Rust Desktop Preview

The native desktop implementation uses Rust, egui/eframe, and the wgpu renderer. `codex_monitor_native` is a separate binary; it does not load the React frontend or generated Tauri assets. The existing Tauri/React desktop path and its iOS development/release scripts remain available.

The native host starts from `HostPaths` and `CoreState` in `src-tauri/src/shared/host_core.rs`. Its typed `BackendHandle` boundary (`src-tauri/src/client/`) routes local requests into the shared Rust cores and remote requests to the daemon over the existing JSON-RPC transport. The daemon uses the shared process state and domain cores; managed-daemon lifecycle logic is in `src-tauri/src/shared/managed_daemon_core.rs`, called by both the native platform service and Tauri adapters. Native window/OS services are in `src-tauri/src/desktop/platform.rs`; UI state and drawing are under `src-tauri/src/desktop/`.

Cargo feature boundaries:

- The default `tauri-host` feature builds the existing Tauri host and its plugins. The `codex-monitor` binary requires this feature; `custom-protocol` extends this path for packaged Tauri builds.
- `native-gui` enables egui/eframe, wgpu, native desktop UI/rendering and OS integrations. The `codex_monitor_native` binary requires it. Native builds turn off defaults, so Tauri/webview crates and frontend assets are not part of the native package graph; `scripts/build-native.sh` checks that graph before building.
- `native-acceptance` extends `native-gui` with eframe inspection support for Rust UI/controller tests.

The Rust-only preview build needs Rust 1.95 or newer, Python 3, CMake and the platform development libraries used by the native dependency graph. Linux CI installs CMake, pkg-config, ALSA, libclang, libudev, Wayland, X11 and xkbcommon development packages. Windows CI installs CMake and LLVM/Clang. This path does not need Node.js or npm; the existing Tauri/React and iOS workflows still do.

Build a native package without invoking npm, Vite, Tauri, or webview asset generation:

```bash
./scripts/build-native.sh --output-dir /tmp/codex-monitor-native-preview
```

The command builds `codex_monitor_native`, `codex_monitor_daemon`, and `codex_monitor_daemonctl` with `--no-default-features --features native-gui`, checks the active Cargo dependency graph for Tauri/webview crates, stages the three executables and platform resources, validates staged file hashes and launcher identity, and writes an unsigned package manifest beside the archive. Linux and Windows use an extracted bundle with `bin/` executables and icons; macOS uses a `.app` bundle. Existing staging directories and archives are not overwritten, so choose a new output directory for each build.

For a disposable local data directory, launch the debug binary with:

```bash
test_data_dir="$(mktemp -d)"
cargo run --manifest-path src-tauri/Cargo.toml --locked \
  --no-default-features --features native-gui \
  --bin codex_monitor_native -- --data-dir "$test_data_dir"
```

The native preview does not replace or modify the installed application or desktop launcher. CI is configured to build native packages on Linux, macOS, and Windows; actual native acceptance by platform is recorded separately. The workflow-dispatch release workflow also emits unsigned native preview artifacts; these are separate from the existing signed Tauri release and updater channel. Native package manifests identify target, architecture, format, version, archive hash, and signature status. A null signature means the package is not eligible for a signed updater flow.

The UI embeds egui's default Ubuntu Light and Hack fonts plus its small special-icon font. Font-family settings can load a matching system font through fontique; unavailable fonts fall back to egui's embedded faces. Packages include the applicable font license and attribution texts, plus application icons. They do not contain a web frontend.

Linux bundles include a desktop-entry template whose `@BUNDLE_PREFIX@` paths must be rendered for the extracted bundle location. Generate an absolute-prefix entry with:

```bash
python3 scripts/package-native.py \
  --write-linux-desktop-entry \
  --prefix /path/to/extracted/CodexMonitor-<version>-linux-<architecture> \
  --desktop-entry-output /path/to/task-owned/codex-monitor-native.desktop
```

The command writes only the requested output file and refuses to overwrite it. It does not register or install a desktop entry. Native CI includes a package smoke test; interactive desktop acceptance status and artifacts are recorded separately in `docs/native-egui-acceptance.md`.

For a host-independent check and the native controller/UI contract tests used by CI:

```bash
cd src-tauri
cargo check --locked --no-default-features --lib
cargo test --locked --no-default-features \
  --features native-acceptance --lib --bin codex_monitor_native
```

For the Rust feature boundaries and module ownership, see `docs/codebase-map.md`.

## iOS Support (WIP)

iOS support is currently in progress.

- Current status: mobile layout runs, remote backend flow is wired, and iOS defaults to remote backend mode.
- Current limits: terminal and dictation remain unavailable on mobile builds.
- Desktop behavior is unchanged: macOS/Linux/Windows remain local-first unless remote mode is explicitly selected.

### iOS + Tailscale Setup (TCP)

Use this when connecting the iOS app to a desktop-hosted daemon over your Tailscale tailnet.
Canonical runbook: `docs/mobile-ios-tailscale-blueprint.md`.

1. Install and sign in to Tailscale on both desktop and iPhone (same tailnet).
2. On desktop CodexMonitor, open `Settings > Server`.
3. Set a `Remote backend token`.
4. Start the desktop daemon with `Start daemon` (in `Mobile access daemon`).
5. In `Tailscale helper`, use `Detect Tailscale` and note the suggested host (for example `your-mac.your-tailnet.ts.net:4732`).
6. On iOS CodexMonitor, open `Settings > Server`.
7. Enter the desktop Tailscale host and the same token.
8. Tap `Connect & test` and confirm it succeeds.

Notes:

- The desktop daemon must stay running while iOS is connected.
- If the test fails, confirm both devices are online in Tailscale and that host/token match desktop settings.

### Headless Daemon Management (No Desktop UI)

Use the standalone daemon control CLI when you want iOS remote mode without keeping the desktop app open.

Build binaries:

```bash
cd src-tauri
cargo build --bin codex_monitor_daemon --bin codex_monitor_daemonctl
```

Examples:

```bash
# Show current daemon status
./target/debug/codex_monitor_daemonctl status

# Start daemon using host/token from settings.json
./target/debug/codex_monitor_daemonctl start

# Stop daemon
./target/debug/codex_monitor_daemonctl stop

# Print equivalent daemon start command
./target/debug/codex_monitor_daemonctl command-preview
```

Useful overrides:

- `--data-dir <path>`: app data dir containing `settings.json` / `workspaces.json`
- `--listen <addr>`: bind address override
- `--token <token>`: token override
- `--daemon-path <path>`: explicit `codex-monitor-daemon` binary path
- `--json`: machine-readable output

### iOS Prerequisites

- Xcode + Command Line Tools installed.
- Rust iOS targets installed:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
# Optional (Intel Mac simulator builds):
rustup target add x86_64-apple-ios
```

- Apple signing configured (development team).
  - Set `bundle.iOS.developmentTeam` and `identifier` in `src-tauri/tauri.ios.local.conf.json` (preferred for local machine setup), or
  - set values in `src-tauri/tauri.ios.conf.json`, or
  - pass `--team <TEAM_ID>` to the device script.
  - `build_run_ios*.sh` and `release_testflight_ios.sh` automatically merge `src-tauri/tauri.ios.local.conf.json` when present.

### Run on iOS Simulator

```bash
./scripts/build_run_ios.sh
```

Options:

- `--simulator "<name>"` to target a specific simulator.
- `--target aarch64-sim|x86_64-sim` to override architecture.
- `--skip-build` to reuse the current app bundle.
- `--no-clean` to preserve `src-tauri/gen/apple/build` between builds.

### Run on USB Device

List discoverable devices:

```bash
./scripts/build_run_ios_device.sh --list-devices
```

Build, install, and launch on a specific device:

```bash
./scripts/build_run_ios_device.sh --device "<device name or identifier>" --team <TEAM_ID>
```

Additional options:

- `--target aarch64` to override architecture.
- `--skip-build` to reuse the current app bundle.
- `--bundle-id <id>` to launch a non-default bundle identifier.

First-time device setup usually requires:

1. iPhone unlocked and trusted with this Mac.
2. Developer Mode enabled on iPhone.
3. Pairing/signing approved in Xcode at least once.

If signing is not ready yet, open Xcode from the script flow:

```bash
./scripts/build_run_ios_device.sh --open-xcode
```

### iOS TestFlight Release (Scripted)

Use the end-to-end script to archive, upload, configure compliance, assign beta group, and submit for beta review.

```bash
./scripts/release_testflight_ios.sh
```

The script auto-loads release metadata from `.testflight.local.env` (gitignored).
For new setups, copy `.testflight.local.env.example` to `.testflight.local.env` and fill values.

## Release Build

Build the production Tauri bundle:

```bash
npm run tauri:build
```

Artifacts will be in `src-tauri/target/release/bundle/` (platform-specific subfolders).

### Windows (opt-in)

Windows builds are opt-in and use a separate Tauri config file to avoid macOS-only window effects.

```bash
npm run tauri:build:win
```

Artifacts will be in:

- `src-tauri/target/release/bundle/nsis/` (installer exe)
- `src-tauri/target/release/bundle/msi/` (msi)
 
Note: building from source on Windows requires LLVM/Clang (for `bindgen` / `libclang`) in addition to CMake.

## Type Checking

Run the TypeScript checker (no emit):

```bash
npm run typecheck
```

Note: `npm run build` also runs `tsc` before bundling the frontend.

## Validation

Recommended validation commands:

```bash
npm run lint
npm run test
npm run typecheck
cd src-tauri && cargo check
```

## Codebase Navigation

For task-oriented file lookup ("if you need X, edit Y"), use:

- `docs/codebase-map.md`

## Project Structure

```
src/
  features/         feature-sliced UI + hooks
  features/app/bootstrap/      app bootstrap orchestration
  features/app/orchestration/  app layout/thread/workspace orchestration
  features/threads/hooks/threadReducer/  thread reducer slices
  services/         Tauri IPC wrapper
  styles/           split CSS by area
  types.ts          shared types
src-tauri/
  src/lib.rs        Tauri app backend command registry
  src/bin/codex_monitor_native.rs  egui/wgpu native desktop entrypoint
  src/bin/codex_monitor_daemon.rs  remote daemon JSON-RPC process
  src/bin/codex_monitor_daemon/rpc/  daemon RPC domain handlers
  src/shared/       shared backend core used by app + daemon
  src/client/       typed local/daemon backend request and event boundary
  src/desktop/      native egui app, controller, rendering, and platform services
  src/shared/git_ui_core/      git/github shared core modules
  src/shared/workspaces_core/  workspace/worktree shared core modules
  src/workspaces/   workspace/worktree adapters
  src/codex/        codex app-server adapters
  src/files/        file adapters
  tauri.conf.json   window configuration
```

## Notes

- Workspaces persist to `workspaces.json` under the app data directory. The desktop-managed daemon shares that directory; a directly launched daemon uses its `--data-dir` or the separate `codex-monitor-daemon` data directory.
- App settings persist to `settings.json` under the app data directory (theme, backend mode/provider, remote endpoints/tokens, Codex path, default access mode, UI scale, follow-up message behavior).
- Feature settings are synced to `$CODEX_HOME/config.toml` (or `~/.codex/config.toml`). Background terminal uses `features.unified_exec` and Apps uses `features.apps`. Steering and collaboration modes are core capabilities. Follow-up default behavior is controlled in Settings → Composer; personality selection is not offered.
- On launch and on window focus, the app reconnects and refreshes thread lists for each workspace.
- Threads are restored by filtering `thread/list` results using the workspace `cwd`.
- Selecting a thread resumes it from disk when no local snapshot is available.
- CLI sessions appear if their `cwd` matches the workspace path; they are not live-streamed unless resumed.
- The app uses `codex app-server` over stdio; see `src-tauri/src/lib.rs` and `src-tauri/src/codex/`.
- The remote daemon entrypoint is `src-tauri/src/bin/codex_monitor_daemon.rs`; RPC routing lives in `src-tauri/src/bin/codex_monitor_daemon/rpc.rs` and domain handlers in `src-tauri/src/bin/codex_monitor_daemon/rpc/`.
- Shared domain logic lives in `src-tauri/src/shared/` (notably `src-tauri/src/shared/git_ui_core/` and `src-tauri/src/shared/workspaces_core/`).
- Codex home resolves globally from `$CODEX_HOME`, falling back to `~/.codex`. Workspace-specific homes are not used by the shared process.
- Worktree agents live under the app data directory (`worktrees/<workspace-id>`); legacy `.codex-worktrees/` paths remain supported, and the app no longer edits repo `.gitignore` files.
- UI state (panel sizes, reduced transparency toggle, recent thread activity) is stored in `localStorage`. Queued follow-up messages are held in memory and do not survive an app restart.
- Runtime argument changes replace the shared Codex process only when no turns or requests are active. A busy backend rejects the change; the UI skips automatic argument changes while any workspace is processing.
- Failed clone/add operations retain any destination left on disk and include its path in the error. Inspect or add that checkout before retrying. Exhausted clone-name candidates return an error without selecting an occupied directory.
- Settings saves report Codex config failures before changing app settings. If config succeeds but saving `settings.json` fails, the error reports that partial save.
- Custom prompts load from `$CODEX_HOME/prompts` (or `~/.codex/prompts`) with optional frontmatter description/argument hints.

## Tauri IPC Surface

Frontend calls live in `src/services/tauri.ts` and map to commands in `src-tauri/src/lib.rs`. The current surface includes:

- Settings/config/files: `get_app_settings`, `update_app_settings`, `get_codex_config_path`, `get_config_model`, `file_read`, `file_write`, `codex_doctor`, `menu_set_accelerators`.
- Workspaces/worktrees: `list_workspaces`, `is_workspace_path_dir`, `add_workspace`, `add_clone`, `add_worktree`, `worktree_setup_status`, `worktree_setup_mark_ran`, `rename_worktree`, `rename_worktree_upstream`, `apply_worktree_changes`, `update_workspace_settings`, `remove_workspace`, `remove_worktree`, `connect_workspace`, `list_workspace_files`, `read_workspace_file`, `open_workspace_in`, `get_open_app_icon`.
- Threads/turns/reviews: `start_thread`, `fork_thread`, `compact_thread`, `list_threads`, `resume_thread`, `archive_thread`, `set_thread_name`, `send_user_message`, `turn_interrupt`, `respond_to_server_request`, `start_review`, `remember_approval_rule`, `get_commit_message_prompt`, `generate_commit_message`, `generate_run_metadata`.
- Account/models/collaboration: `model_list`, `account_rate_limits`, `account_read`, `skills_list`, `apps_list`, `collaboration_mode_list`, `codex_login`, `codex_login_cancel`, `list_mcp_server_status`.
- Git/GitHub: `get_git_status`, `list_git_roots`, `get_git_diffs`, `get_git_log`, `get_git_commit_diff`, `get_git_remote`, `stage_git_file`, `stage_git_all`, `unstage_git_file`, `revert_git_file`, `revert_git_all`, `commit_git`, `push_git`, `pull_git`, `fetch_git`, `sync_git`, `list_git_branches`, `checkout_git_branch`, `create_git_branch`, `get_github_issues`, `get_github_pull_requests`, `get_github_pull_request_diff`, `get_github_pull_request_comments`.
- Prompts: `prompts_list`, `prompts_create`, `prompts_update`, `prompts_delete`, `prompts_move`, `prompts_workspace_dir`, `prompts_global_dir`.
- Terminal/dictation/notifications/usage: `terminal_open`, `terminal_write`, `terminal_resize`, `terminal_close`, `dictation_model_status`, `dictation_download_model`, `dictation_cancel_download`, `dictation_remove_model`, `dictation_request_permission`, `dictation_start`, `dictation_stop`, `dictation_cancel`, `send_notification_fallback`, `is_macos_debug_build`, `local_usage_snapshot`.
- Remote backend helpers: `tailscale_status`, `tailscale_daemon_command_preview`, `tailscale_daemon_start`, `tailscale_daemon_stop`, `tailscale_daemon_status`.
