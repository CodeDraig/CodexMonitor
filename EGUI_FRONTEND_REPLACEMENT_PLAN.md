# Native Rust Frontend Replacement

## Goal and scope

Replace the desktop React/Tauri frontend with a native **egui interface hosted by eframe and rendered through wgpu**, while retaining the shared Rust runtime, daemon protocol, and durable records.

This plan is grounded in commit **`47b0bb952c59a628839e48670a496474060467f0`** and the reviewed working tree. The target is **Linux, macOS, and Windows desktop first**; the existing React/Tauri implementation remains available for iOS.

## Implementation sequence

### 1. Establish the replacement boundary and preserve the current application

The current implementation has these relevant boundaries:

| Current code | Consequence for the replacement |
|---|---|
| `src/features/app/components/MainApp.tsx` and `src/features/layout/hooks/layoutNodes/buildPrimaryNodes.tsx` compose the application and connect conversations, composer, sidebar, and auxiliary panels. | Port this orchestration into Rust controllers and application state. Recreating the visible components alone would leave substantial behavior behind. |
| `src-tauri/src/shared/runtime_rpc.rs` and the shared cores already implement runtime, conversation, run, and workspace behavior. | Reuse these implementations and their existing data contracts. |
| `AppState` in `src-tauri/src/state.rs`, `TauriEventSink` in `src-tauri/src/event_sink.rs`, and `src-tauri/src/remote_backend/transport.rs` still depend on Tauri. | Introduce a host-independent bootstrap and event boundary. |
| `src/features/threads/utils/threadStorage.ts`, layout hooks, and prompt history use webview localStorage. | Provide an explicit migration for persisted presentation state. |
| `src/features/terminal/hooks/useTerminalSession.ts` uses xterm.js; `src/features/git/components/PierreDiffBlock.tsx` uses browser diff components. | Replace terminal emulation/rendering and diff rendering with Rust implementations. |

Before implementation, record a feature inventory from the existing command surface, visible menus, settings, and tests. Each entry identifies its native replacement and acceptance check. Include workspace/worktree/clone operations, Codex and ACP conversations, recorded runs, comparisons, Git/GitHub, files, prompts, configuration, terminal, dictation, usage, notifications, updates, and desktop shortcuts.

Preserve all three existing plans byte-for-byte. Their preservation hashes are:

| Plan | SHA-256 |
|---|---|
| `IMPLEMENTATION_PLAN.md` | `a21b70485abcffb966f5375db31087bafb90c28db1fdde969967442d23775250` |
| `MULTI_AGENT_RUNTIME_PLAN.md` | `415c4dad2d5af05cc4b620495ff8d993a4607957c17959f4a8d7b2f0cbd59b46` |
| `ACP_CONVERSATION_SUPPORT_PLAN.md` | `30d4a19f4f66c87253c9031b2e23f19a6083105d5c5cb5a166accb0e9ba88d8a` |

Acceptance uses isolated repositories and data directories. It does not replace the installed application or change its launcher.

### 2. Make the existing Rust package build independently of Tauri

Keep the current package and shared-core locations. Add a native executable named `codex_monitor_native` and feature boundaries in `src-tauri/Cargo.toml`:

- `tauri-host`: existing Tauri application, plugins, and mobile entry point.
- `native-gui`: egui/eframe frontend and native desktop services.
- `native-acceptance`: optional native inspection and acceptance instrumentation.
- `custom-protocol`: continues to enable the Tauri host and its existing asset protocol.

Gate `src-tauri/build.rs` so a native or daemon build does not execute `tauri_build`. Separate Tauri command registration from host-independent library initialization. Preserve existing Tauri commands and iOS scripts through the transition.

Use matching **egui/eframe/egui_kittest 0.37.0** dependencies and the compatible wgpu version selected by that integration. The tagged release requires Rust 1.95; the inspected local toolchain is 1.96.1. Select `Renderer::Wgpu` explicitly and enable the required graphics backends, clipboard, system-font fallback, AccessKit, X11, and Wayland features. Ordinary interface rendering uses egui-wgpu without a custom renderer. See the [tagged dependency manifest](https://github.com/emilk/egui/blob/0.37.0/Cargo.toml) and [eframe feature definitions](https://github.com/emilk/egui/blob/0.37.0/crates/eframe/Cargo.toml).

**Acceptance:** native and daemon targets build without Node, npm, Vite assets, Tauri, Wry, WebKit, or JavaScriptCore in their active dependency graph. The separately selected Tauri/mobile build remains valid. Adding the native target must not force egui dependencies into the iOS build.

### 3. Extract host-independent startup and define the native backend contract

Add `shared::host_core` for paths and shared process state. Refactor the existing application and daemon states to use it without moving domain logic out of the shared cores.

The initial contracts are:

```rust
pub struct HostPaths {
    pub data_dir: PathBuf,
    pub settings_path: PathBuf,
    pub workspaces_path: PathBuf,
    pub run_store_path: PathBuf,
}

pub struct CoreState {
    // Existing workspace/session maps, runtime registry,
    // settings state, and Codex login cancellation state.
}

pub struct BackendScope {
    pub endpoint_id: String,
    pub connection_epoch: u64,
    pub store_id: Option<String>,
}

pub enum ConversationRef {
    Codex {
        workspace_id: String,
        thread_id: String,
    },
    Acp {
        workspace_id: String,
        store_id: String,
        session_id: String,
    },
}

pub struct ConversationKey {
    pub endpoint_id: String,
    pub conversation: ConversationRef,
}
```

`HostPaths` must resolve the existing application data locations, including `com.dimillian.codexmonitor`, rather than adopting eframe's default persistence directory. An explicit `--data-dir` takes precedence. A path error produces a visible startup failure instead of opening an apparently empty store elsewhere.

Add a typed `client` facade with `local`, `remote`, `contracts`, and `events` modules. Reuse existing Rust DTOs where they exist; preserve raw protocol content where extensibility requires it.

```rust
pub enum BackendRequest {
    Workspaces(WorkspaceRequest),
    Codex(CodexRequest),
    Runtime(RuntimeRequest),
    Runs(RunsRequest),
    Git(GitRequest),
    Files(FilesRequest),
    Prompts(PromptsRequest),
}

pub struct RequestEnvelope {
    pub correlation_id: Uuid,
    pub scope: BackendScope,
    pub request: BackendRequest,
}

pub enum BackendMessage {
    Reply {
        correlation_id: Uuid,
        scope: BackendScope,
        result: Result<BackendReply, BackendError>,
    },
    Event {
        scope: BackendScope,
        event: BackendEvent,
    },
    ResyncRequired {
        scope: BackendScope,
        stream: EventStream,
    },
}

impl BackendHandle {
    pub fn enqueue(
        &self,
        request: RequestEnvelope,
    ) -> Result<(), EnqueueError>;
}
```

Correlation IDs identify UI requests. Existing durable submission request IDs remain inside runtime/run requests and retain their current idempotency meaning.

Local dispatch calls the shared cores. Remote dispatch maps the same requests onto existing daemon method names and payloads. Local application preferences, clipboard access, dialogs, and dictation remain host operations; they are not accidentally routed to a remote daemon.

**Acceptance:** run the same contract fixtures through local dispatch and authenticated daemon dispatch. Verify payload serialization, errors, selected-backend settings behavior, and that delayed replies from an earlier connection epoch cannot update the current interface.

### 4. Replace Tauri event delivery with a controlled Rust event bridge

Implement a channel-backed version of the existing `EventSink` in `src-tauri/src/backend/events.rs`. Refactor remote transport to deliver notifications to this abstraction instead of calling `AppHandle.emit`.

Run backend I/O on Tokio outside the UI thread. The eframe application drains messages into state and requests repaint when state changes. No network request, SQLite access, subprocess wait, image decoding, or full Markdown parsing runs inside repeated drawing passes. Current eframe supports separate application logic and repaint requests from background threads. See the [eframe application interface](https://docs.rs/eframe/0.37.0/eframe/trait.App.html).

Preserve event order and distinguish repaint coalescing from data processing. Replies, permission requests, and terminal data must not disappear because repaint notifications were merged.

The daemon currently skips broadcast lag notifications. Add an additive resynchronization notification when lag occurs, with corresponding handling in both clients. Runtime streams recover using their durable sequences and history APIs; Codex conversations reload authoritative history and refresh their live subscription. A transport disconnect never automatically repeats an uncertain mutation.

**Acceptance:** introduce delayed replies, disconnect immediately after submission, and deliberately overflow a test event queue. Verify one submitted execution, receipt-based recovery, ordered content, and visible resynchronization rather than silently incomplete conversations.

### 5. Build the native shell, presentation state, and migration path

Add Rust UI modules under the desktop module: `app`, `state`, `controllers`, `panels`, `widgets`, `content`, and `platform`.

Translate the current visual system from `src/styles/base.css` and `src/styles/ds-tokens.css` into shared Rust theme tokens. Preserve the refreshed appearance's spacing, hierarchy, colors, typography, selection, and compact controls. Centralize modal, popover, tooltip, toast, and panel behavior.

Implement the sidebar, main conversation region, composer, resizable auxiliary panels, Home, Workspace Home, settings, and About view. Use stable egui widget IDs derived from scoped application identities.

Add a versioned `UiStateV1` file for presentation state that is currently persisted outside backend records: panel dimensions, sort/group preferences, collapsed sections, Codex title overrides and pins, prompt history, and per-thread parameter preferences. Keep ACP metadata in its existing backend store.

For migration, add a small exporter to the existing frontend that writes an allowlisted, versioned snapshot through a local Rust command. The native importer preserves the source snapshot, imports idempotently, and reports conflicts. It does not scrape WebKit storage internals. Any transient text captured during handoff becomes a recoverable draft; queued prompts are never automatically submitted on startup.

**Acceptance:** import a populated snapshot twice; verify equivalent state, no duplicate entries, unchanged source files, and recovery after an interrupted write. Test keyboard navigation, resizing, themes, scaling, clipboard, Unicode input, and IME composition early.

### 6. Deliver a working conversation slice through both backends

Port the behavior represented by `src/features/threads/hooks/useThreadActions.ts`, the thread reducer slices, `src/features/runtime/hooks/useAcpConversations.ts`, and the sidebar agent chooser.

Use a Rust controller contract:

```rust
impl ConversationController {
    pub fn handle(
        &mut self,
        action: ConversationAction,
    ) -> Vec<ConversationEffect>;

    pub fn apply_backend(
        &mut self,
        message: BackendMessage,
    ) -> Vec<ConversationEffect>;
}
```

Effects request backend work or host services. Rendering does not execute effects repeatedly.

Preserve sidebar reconciliation: incoming ordering, required active/processing/ancestor anchors, hidden-row precedence, and promotion of children whose parents are absent. Coalesce Monitor-owned Codex bindings with their native threads into one visible conversation and one authoritative event path.

ACP discovery, global browsing, pagination, workspace import, history loading, and metadata editing continue through the existing shared APIs. Preserve connection-scoped cursors, read-only ownership, history coverage, stale-cache labels, and backend/store identity.

Implement configuration controls from advertised ACP options, including grouped options and legacy model/mode fallbacks. No OpenCode-name checks control behavior.

**Acceptance:** create Codex, OpenCode, and independent generic ACP conversations from the sidebar locally and through the daemon. Discover an external ACP session, add its workspace without starting Codex, select it, read history, send, stop, and reopen it. Assert that ACP selection invokes no Codex account, model, collaboration, or metadata APIs.

This slice must pass in a real native window before the remaining interface is ported.

### 7. Complete conversation rendering and composer behavior

Introduce a shared Rust presentation model for messages, reasoning, tools, diffs, plans, media, resources, compaction, annotations, and inspectable unknown content. Reuse the backend ACP projection rather than creating another protocol normalizer.

Replace `react-markdown` with a Rust Markdown document model using `pulldown-cmark`, including the extensions currently supplied by `remark-gfm`. Use cached syntax highlighting for code. Preserve workspace-aware file links, external links, copy, quote, tables, code blocks, and review content. The parser supplies source offsets suitable for mapping selection back to text. See the [pulldown-cmark documentation](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/).

Implement viewport rendering with cached heights for variable-height conversation items. Preserve a scroll anchor identified by message and offset. History refresh must not move a reader to the bottom, destroy selection, or erase a draft. Media loads on demand through existing attachment APIs; supported audio needs native playback controls.

Port queue-versus-steer behavior, opposite-action shortcuts, suggestions, slash commands, attachments, draft switching, server forms, permissions, and submission uncertainty. ACP attachments remain optional and capability-driven.

Defer background replay while executing, submitting, draining a queue, answering permissions, or interacting with a configuration popup. Preserve the existing 30-second idle freshness behavior.

**Acceptance:** replay repeated identical messages, partial tool patches, media, resources, and compaction markers. Reload twice and verify every supplied item appears once in order. Test selection across formatted blocks, exact Unicode copying, queued sends, failed steering, historical permissions, and live events immediately following replay.

### 8. Port durable runs and the comparison workbench

Replace the React run controllers and `src/features/runs/components/RunWorkbench.tsx` using the existing `runs_core`, `runs_rpc`, and runtime execution contracts.

Preserve run creation, instance preparation, local/worktree launches, submission receipts, reconciliation, immutable initial-turn captures, separate later captures, Git/command evidence, capture selection, comparison choices, notes, and revision conflicts.

Internal navigation uses `ConversationKey`; a recorded-run link opens the normal conversation surface. Comparison edits retain the user's unsaved text when a revision conflict requires reloading.

This frontend replacement requires **no run-store schema change**. Retain schema 3, WAL/FULL durability, writer-free reads, ownership checks, and contention recovery.

**Acceptance:** open existing records created by the current application, compare captures, edit notes from two clients, and restart. Verify capture payload hashes, acknowledged comparison revisions, session identities, event sequences, and SQLite integrity. Repeat sustained desktop/daemon writes, including a lock longer than the busy timeout.

### 9. Replace the remaining workbench surfaces and native services

| Area | Implementation and acceptance |
|---|---|
| **Git and diffs** | Reuse shared Git/GitHub operations. Replace Pierre/Prism rendering with cached Rust patch models and viewport rendering. Test unified/split views, renamed files, binary/image diffs, line selection, staging, branches, worktrees, PR context, and large patches. |
| **Files, prompts, and configuration** | Port tree/search, previews, file links, prompt CRUD, AGENTS/config editors, runtime-profile editing, account/MCP/app settings, and effective-backend routing. Test remote paths on the executing backend and preserve existing write policies. |
| **Terminal** | Extract the existing PTY lifecycle into a host-independent core. Use `alacritty_terminal` for terminal state/parser behavior with an egui renderer; retain portable-pty for process handling. Test alternate screen, colors, wide/combining characters, bracketed paste, keyboard modes, resize, selection, scrollback, terminal responses, and exit cleanup. See the [terminal-core documentation](https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/). |
| **Dictation and media** | Retain CPAL/Whisper behavior, separate it from `AppHandle`, and route status/transcripts through the native bridge. Test model download/cancellation, recording, permissions, transcription, and insertion into the intended draft. |
| **Desktop services** | Provide host adapters for dialogs, clipboard images/text, notifications/sound, URL/file opening, menus, tray behavior, window restoration, shortcuts, and application shutdown. Test close-versus-quit and managed-daemon lifetime. |

Remote terminal support is not currently implemented by the daemon RPC surface. Preserve the local terminal and make remote availability explicit; do not open a local shell against an assumed remote path. Dictation continues to run on the desktop host.

### 10. Package the native application and complete replacement acceptance

Add Cargo-driven desktop build/package entry points and adapt the existing CI/release workflows. Package the native executable with the matching daemon and daemon-control utility, icons, fonts, and required resources. Desktop builds must not invoke npm or regenerate frontend assets.

Replace the Tauri updater integration with a Rust native-package update service behind a host interface:

```rust
impl UpdateService {
    pub async fn check(&self) -> Result<Option<UpdateCandidate>, UpdateError>;
    pub async fn download_verified(
        &self,
        candidate: &UpdateCandidate,
    ) -> Result<VerifiedPackage, UpdateError>;
    pub async fn install(
        &self,
        package: VerifiedPackage,
    ) -> Result<RestartRequired, UpdateError>;
}
```

Native update metadata must identify target platform, architecture, package format, version, hash, and signature. It must not install a legacy Tauri artifact into the native distribution. Exercise verification, failure recovery, and rollback against temporary installations and test signing keys.

Preserve the React/Tauri sources and Node tooling needed by iOS. Update `README.md`, `docs/codebase-map.md`, runtime documentation, and native acceptance documentation to describe the resulting live architecture.

Publishing, replacing the current installation, and switching the real desktop launcher remain separate deployment actions.

## Acceptance gates

| Gate | Required evidence |
|---|---|
| **Controller and contract parity** | Port the meaningful assertions from the current thread, queue, runtime, and run tests. Exercise local and daemon adapters with the same fixtures, including backend switches and overlapping native IDs. |
| **Native chooser and dropdowns** | Open the sidebar chooser and OpenCode model, effort, and mode controls with mouse and keyboard. Keep a popup open across a freshness interval, select a value, verify backend acknowledgement, send, and reopen. Repeat locally and through the daemon. |
| **Real conversations** | Use Codex, a conversation created externally in OpenCode, and the independent ACP fixture. Verify discovery, complete supplied history, continuation, external idle updates, ownership, permissions, run links, and explicit capability limitations. Record actual agent versions. |
| **Restart and contention** | Run native local and daemon-backed conversations against one isolated store while comparison notes change. Restart both processes; verify committed evidence, metadata, notes, captures, and integrity. |
| **Rendering and interaction load** | Exercise 10,000 mixed conversation items, a 100,000-line diff, and sustained terminal output. Verify viewport-bounded drawing, stable scroll, bounded caches, and no lost content. Proposed reference-host targets are under 100 ms input response and p95 UI work below 16.7 ms during normal interaction. |
| **Accessibility and platform behavior** | Use egui_kittest for named controls and interaction assertions, then native keyboard, IME, clipboard, screen-reader, scaling, and window tests. Custom-painted controls must expose semantics. See the [egui_kittest documentation](https://docs.rs/egui_kittest/0.37.0/egui_kittest/). |
| **Graphics** | Test the actual Linux desktop, available X11/Wayland sessions, and the user's remote-display setup. Verify adapter selection, minimize/restore, resize, and startup diagnostics. Record native Windows/macOS results separately. |
| **Packaging** | Launch an isolated packaged build through a test desktop entry or platform equivalent; confirm executable identity, daemon identity, resources, data paths, and absence of a webview process. |

If the computer-use tool cannot bind the window, use the acceptance-only eframe inspection facility together with OS-level input and screenshots. Accessibility-tree and rendered-harness tests supplement the real native run; they do not replace it. The current eframe inspection feature exposes native input, accessibility, and screenshot inspection when explicitly enabled. See the [inspection feature](https://github.com/emilk/egui/blob/0.37.0/crates/eframe/Cargo.toml).

## Validation commands

After focused tests, run the planned Rust validations:

```sh
cd /srv/data/code/CodexMonitor/src-tauri

cargo check --locked --no-default-features --all-targets
cargo test --locked --no-default-features --lib --bins

cargo check --locked --no-default-features --features native-gui --all-targets
cargo test --locked --no-default-features --features native-gui

cargo build --locked --release --no-default-features \
  --features native-gui --bin codex_monitor_native

cargo build --locked --release --no-default-features \
  --bin codex_monitor_daemon --bin codex_monitor_daemonctl

cargo test --locked --features custom-protocol
```

Retain frontend validation for the preserved iOS path and transitional exporter:

```sh
cd /srv/data/code/CodexMonitor

npm run typecheck
npm run test
npm run build

python3 /srv/data/code/CodexMonitor/scripts/check-codex-protocol.py
python3 /srv/data/code/CodexMonitor/scripts/test-run-store-contention.py
python3 /srv/data/code/CodexMonitor/scripts/test-runtime-store-contention.py
```

Update the contention runners to exercise the host-independent/native build while retaining their existing preservation assertions. Store new acceptance artifacts separately from existing evidence.

## Completion boundary

The first technical gate is a native conversation screen that proves text selection, IME, scrolling, configuration popups, and local/daemon operation. Failure there must be resolved before expanding the port. Completion requires the full desktop feature inventory; an attractive chat window alone does not complete this replacement.
