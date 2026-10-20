# CodexMonitor

CodexMonitor is a native Rust desktop workbench for Codex and configured ACP agents.
The frontend uses egui, eframe, and wgpu. Local execution and authenticated remote
daemon execution share the same Rust runtime, workspace state, and durable stores.

## Features

- Workspace sidebar, Codex and ACP agent chooser, global agent-session discovery,
  complete agent-supplied replay, and cached conversation browsing.
- Composer with negotiated model, effort, mode, permissions, attachments, queue,
  steer, cancellation, and recoverable drafts.
- Durable run records, immutable initial captures, Git and command evidence,
  revision-checked comparison notes, and links into the main conversation view.
- Git, Files, Prompts, Settings, usage history, and recorded-run workbenches.
- Local PTY terminal, native notifications, clipboard, file pickers, dictation,
  window preferences, and native package/update configuration.
- Managed daemon lifecycle and Tailscale helpers for remote workspaces.

See [agent runtimes](docs/agent-runtimes.md), [run records](docs/run-records.md),
and the [native feature inventory](docs/native-egui-feature-inventory.md) for
contracts and acceptance boundaries. Remote conversation execution is supported;
the general terminal dock is local-only.

## Requirements

- Rust 1.95 or newer, Python 3.11 or newer, CMake, pkg-config, and LLVM/libclang.
- Linux development libraries: ALSA, libudev, Wayland, X11, xkbcommon, and
  xkbcommon-x11. A Vulkan-capable renderer or compatible software implementation
  is needed for the native window.
- Codex CLI on PATH, or a configured executable, when using Codex sessions.
- Configured ACP executable/arguments and authentication for each ACP profile.
- Git; GitHub CLI (`gh`) for GitHub workflows.

The application build does not require Node.js, npm, a webview, or generated web
assets. A configured external agent may independently require its own runtime.

## Run from source

```sh
cargo run --locked --manifest-path src-tauri/Cargo.toml --bin codex_monitor_native
```

The default Cargo feature is `native-gui`, and the default executable is
`codex_monitor_native`. To isolate application state, append
`-- --data-dir /path/to/isolated-data`. Without this option the app uses its normal
per-user data directory. Building from source does not replace an installation.

For headless daemon and control binaries without GUI dependencies:

```sh
cargo build --locked --no-default-features --features acp-host-services \
  --manifest-path src-tauri/Cargo.toml \
  --bin codex_monitor_daemon --bin codex_monitor_daemonctl
```

`codex_monitor_daemon --help` and `codex_monitor_daemonctl --help` describe the
listen, authentication, data-directory, and lifecycle options. Remote endpoints
use the shared JSON-RPC contracts; tokens are excluded from diagnostic identities.

## Package

```sh
./scripts/build-native.sh --output-dir /path/to/new-package-directory
```

The script checks the native dependency graph, builds the desktop, daemon, and
control executables, stages icons and font licenses, validates file hashes and
launcher identity, and writes an unsigned package and manifest. Use a fresh output
directory; existing packages are not overwritten. Linux and Windows use extracted
bundles; macOS uses an application bundle. No package is installed automatically.

Linux bundles include a desktop-entry template. Render paths for the actual
extracted bundle with:

```sh
python3 scripts/package-native.py --write-linux-desktop-entry \
  --prefix /path/to/extracted/CodexMonitor-VERSION-linux-ARCH \
  --desktop-entry-output /path/to/new/codex-monitor.desktop
```

This writes the requested file without registering it. The manual release
workflow builds unsigned native artifacts for Linux, macOS, and Windows. Signed
native updates require a configured trusted feed and signature; an unsigned
artifact is not eligible for automatic installation.

## Validation

```sh
python3 scripts/check-native-only.py
cargo check --locked --no-default-features --lib --manifest-path src-tauri/Cargo.toml
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --no-default-features --features acp-host-services \
  --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --no-default-features --features native-acceptance \
  --all-targets --manifest-path src-tauri/Cargo.toml
python3 scripts/package-native.py --self-test
bash scripts/test-build-native-target-dir.sh
python3 scripts/test-runtime-store-contention.py
```

Unit, protocol, packaging, and native interactive acceptance are separate checks.
Use isolated repositories and data directories for local/daemon acceptance. See
[the native acceptance record](docs/native-egui-acceptance.md) and
[ACP integration acceptance](docs/acp-agent-integration-acceptance.md).

## Source map

The Rust crate remains under `src-tauri/`:

- `src/bin/codex_monitor_native.rs`: native desktop entry point.
- `src/desktop/`: egui shell, panels, content rendering, controller, and OS services.
- `src/client/`: typed local/daemon requests, responses, events, and connection scope.
- `src/shared/`: runtime, conversations, run store, workspaces, Git, settings,
  dictation, terminal, and other backend cores.
- `src/backend/`: Codex/ACP subprocess transport and event routing.
- `src/bin/codex_monitor_daemon/`: authenticated JSON-RPC transport and handlers.
- `src/bin/codex_monitor_daemonctl.rs`: headless daemon lifecycle CLI.
- `assets/`: notification audio embedded by the Rust desktop.
- `icons/`: native package artwork.

See [the codebase map](docs/codebase-map.md) for task-level ownership.

## Data and compatibility

Settings and workspaces persist in `settings.json` and `workspaces.json` under
the application data directory. `runs.sqlite3` holds durable run evidence,
conversation history, metadata, and comparison revisions. Native UI state is
versioned separately. The managed daemon shares the selected data directory;
standalone daemons can use an explicit `--data-dir`.

Codex home resolves from `$CODEX_HOME`, then `~/.codex`. Worktree agents live under
the app data directory, with existing legacy worktree paths still recognized.
Runtime argument changes replace the shared Codex process only while idle.

The implementation plans at the repository root are preserved design artifacts;
current source, commands, and contracts are described by this README and the
canonical runtime documentation.
