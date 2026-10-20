# Claude, Gemini CLI, and Antigravity ACP support

Implement all three through the existing shared ACP runtime, with authentication and client callbacks running on the selected backend. The native desktop should provide the same conversation, permission, configuration, and recorded-run experience already available for OpenCode.

This plan is grounded in **commit `52fe9aa53a519ce197f0498766cfb797debcb234`**. The tracked working tree is unchanged. The four existing plans and current installation remain preserved.

## 1. Current code and implementation decisions

The existing foundation is sufficient for this extension:

| Current implementation | Required extension |
|---|---|
| [ACP transport](/srv/data/code/CodexMonitor/src-tauri/src/backend/acp.rs) negotiates v1, handles responses, updates, and permissions. Other agent requests receive `-32601`. | Dispatch filesystem, terminal, authentication-related, and supported user-interaction requests without blocking the protocol reader. |
| [Runtime registry](/srv/data/code/CodexMonitor/src-tauri/src/shared/agent_runtime/registry.rs) owns agent processes, execution bindings, discovery connections, and history loads. | Retain connections before session creation so users can authenticate; own callback services and their lifecycle. |
| [Runtime types](/srv/data/code/CodexMonitor/src-tauri/src/shared/agent_runtime/types.rs) contain generic executable/argument profiles. | Add launch presets, backend environment references, callback policy, and authentication descriptors. |
| [Shared runtime dispatcher](/srv/data/code/CodexMonitor/src-tauri/src/shared/runtime_rpc.rs) serves native, Tauri, and daemon callers. | Add identical connection, authentication, callback-decision, and terminal-reading operations. |
| [Shared terminal core](/srv/data/code/CodexMonitor/src-tauri/src/shared/terminal_core.rs) provides a PTY; the native terminal is currently local-only. | Add backend-owned interactive login terminals and separate ACP command processes. |
| [Workspace file core](/srv/data/code/CodexMonitor/src-tauri/src/shared/workspace_files_core.rs) provides bounded, read-only previews. | Add a distinct ACP text-file service supporting protocol reads and writes. Its reads must not silently inherit preview truncation. |

Use these decisions:

- **ACP v1 remains the compatibility target.** Add only negotiated extensions required by the tested agents.
- Keep agent-specific launch recipes in presets. Authentication, callbacks, rendering, and persistence use shared code.
- Filesystem and command callbacks execute beside the agent: locally in local mode, on the daemon host in remote mode.
- Authentication credentials remain in the agent’s credential store or backend environment. Monitor stores connection and operation metadata, not credentials.
- Enable callbacks through explicit profile policy. Existing profiles retain their current disabled callback defaults.
- Preserve the React/Tauri and iOS contracts. Update their shared types and provide explicit unsupported states where an interactive host capability is unavailable.

## 2. Establish reproducible agent launch profiles

Extend the native [profile editor](/srv/data/code/CodexMonitor/src-tauri/src/desktop/app/browser.rs) and shared profile contracts with three presets, while keeping `kind: "acp"`:

| Preset | Launch baseline to validate |
|---|---|
| Claude | The `claude-agent-acp` executable from a pinned `@agentclientprotocol/claude-agent-acp` installation. The current registry lists `0.94.0`. [Registry](https://raw.githubusercontent.com/agentclientprotocol/registry/main/claude-acp/agent.json) |
| Gemini CLI | A pinned Gemini CLI executable with `--acp`. The current registry lists `0.64.0`. [Registry](https://raw.githubusercontent.com/agentclientprotocol/registry/main/gemini/agent.json) |
| Antigravity | The Google-distributed ACP server. The current registry lists `1.4.1`; its Linux recipe includes the exact `--uid=` argument. Preserve that recipe until a probe establishes any required adjustment. [Registry](https://raw.githubusercontent.com/agentclientprotocol/registry/main/antigravity-acp/agent.json) |

Before implementing provider-specific accommodations, run an isolated initialization probe for each pinned version. Record its binary/package identity, negotiated version, authentication methods, session capabilities, callback requirements, and relevant extensions.

This probe must answer concrete unresolved questions: how Antigravity authenticates headlessly, which authentication flow each agent advertises, and whether any required extension falls outside the current client.

Add optional profile fields:

```rust
preset_id: Option<String>,
environment: BTreeMap<String, EnvironmentSource>,
client_policy: AcpClientPolicy,
```

`EnvironmentSource` supports non-secret literals and references to variables resolved on the executing backend. Do not return resolved secret values through profile APIs.

Preserve existing connection scopes when new launch fields are absent. Configured executable, arguments, launch directory, or environment configuration changes create a new scope; display-name and callback-policy changes do not. Policy changes require a new connection generation before capability advertisement changes.

Use isolated, pinned agent installations for acceptance. Do not install dependencies automatically when a user selects a profile.

## 3. Shared connection and callback contracts

Add connection, authentication, filesystem, and command-service modules under [the shared runtime directory](/srv/data/code/CodexMonitor/src-tauri/src/shared/agent_runtime).

Introduce a connection context containing:

```rust
struct AcpConnectionContext {
    connection_id: Uuid,
    generation: u64,
    profile: RuntimeRef,
    store_id: String,
    role: ConnectionRole, // conversation or discovery
    workspace: Option<WorkspaceContext>,
}
```

Keep this separate from `RuntimeSession`: authentication may happen before a native session exists.

Add these operations through `RuntimeMethod`, `runtime_rpc::dispatch`, native client routing, the existing `runtime_request` daemon envelope, and compatibility IPC:

| Operation | Proposed contract |
|---|---|
| `prepare_connection` | `{profileId, expectedProfileRevision, cwd?, sessionId?, role, requestId}` → asynchronous operation ticket. |
| `connection_status` | `{connectionId}` → generation, agent information, negotiated capabilities, advertised auth methods, connection/auth state. |
| `authenticate` | `{connectionId, generation, methodId, requestId}` → authentication ticket. |
| `connection_operation` | `{operationId}` → pending, awaiting-user, succeeded, failed, cancelled, or interrupted state. |
| `cancel_connection_operation` | `{operationId, generation, requestId}` → acknowledged cancellation. |
| `respond_client_request` | `{connectionId, generation, clientRequestId, decision}` → response to a specific pending callback approval or elicitation. |
| Login-terminal operations | Read output by cursor; send sequenced input; resize; close. All require authentication-operation identity and generation. |
| `read_acp_terminal` | `{sessionId, terminalId, cursor?, limit?}` → bounded output, truncation state, exit status, and live/released state. |

Existing session creation, discovery, and loading accept an optional prepared connection reference. Existing callers retain their automatic connection path and existing success payloads.

Transport changes:

- Preserve incoming JSON-RPC IDs exactly, including string versus numeric identity.
- Install the callback dispatcher before setup requests begin.
- Process callbacks concurrently with outgoing requests. A permission wait or `terminal/wait_for_exit` must not block cancellation or response delivery.
- Establish provisional session/workspace context during creation, then validate the returned native session identity before publishing the binding.
- Reject requests from stale generations, unrelated sessions, discovery connections attempting mutations, or another backend’s owned session.
- Return protocol errors for malformed or unsupported requests; preserve structured agent errors instead of reducing every failure to text.
- Bound pending requests and message sizes, with explicit overload errors.

## 4. Authentication, including daemon-hosted login

Implement both authentication paths defined by ACP v1:

- **Agent/default method:** send `authenticate` with the advertised method ID on the retained connection.
- **Terminal method:** launch the configured agent invocation interactively, append the advertised login arguments and environment, wait for successful exit, then reconnect and initialize. Do not send `authenticate` for this method type. [ACP authentication](https://agentclientprotocol.com/protocol/v1/authentication)

This is directly relevant to Claude: its current adapter conditionally advertises terminal login methods when the client declares terminal-auth support. [Claude adapter source](https://raw.githubusercontent.com/agentclientprotocol/claude-agent-acp/main/src/acp-agent.ts)

Implementation requirements:

1. Add shared PTY support usable by daemon-only builds without requiring egui or Tauri.
2. Generalize PTY spawning to accept an exact executable, argument vector, environment, and directory. Do not substitute an interactive shell.
3. Add a native login panel showing the agent and executing backend, terminal output, input, resize, cancellation, and completion state.
4. Carry remote login input/output through authenticated daemon RPC. Input sequence acknowledgements prevent accidental duplicate submission; uncertain input is not automatically resent.
5. Keep login output and input out of conversation journals, captures, comparisons, and ordinary diagnostics.
6. Advertise `clientCapabilities.auth.terminal` only for hosts whose interactive transport is implemented.
7. Support capability-gated logout, with clear scope and user initiation.
8. Treat initialization as a protocol connection, not proof of authentication. Authentication failures during later operations return the UI to the appropriate login state.

Support URL-mode `elicitation/create` for authentication flows that use it: display the full destination, open only after user action, and return consent separately from authentication completion. Scope completion notifications to their originating connection and elicitation ID. [ACP elicitation](https://agentclientprotocol.com/protocol/v1/elicitation)

For daemon authentication, verify the agent’s actual headless/device-code flow. Do not open a daemon-local callback URL on the desktop and report success. A flow requiring an unavailable callback route remains an explicit blocker until an appropriate vendor-supported flow or narrowly scoped relay is implemented.

Authentication never automatically resubmits an interrupted prompt.

## 5. Filesystem callbacks

Implement these wire methods in a shared ACP file service:

```text
fs/read_text_file  {sessionId, path, line?, limit?} → {content}
fs/write_text_file {sessionId, path, content}      → {}
```

Reads use absolute backend paths and one-based line selection. Writes support creating a missing file. [ACP filesystem contract](https://agentclientprotocol.com/protocol/v1/file-system)

The service must:

- Resolve access against the session’s workspace/worktree and explicitly approved additional roots.
- Validate existing paths and creation parents, including symlinks; prevent path replacement from bypassing the approved root.
- Return explicit errors for unsupported encodings, excessive sizes, missing resources, and denied access. Never present truncated preview content as a complete file.
- Use disk contents as authoritative; Monitor currently has no general unsaved source editor.
- Serialize conflicting writes to the same file, preserve a recoverable preimage before replacement, and acknowledge only after the write and its result record commit.
- Refresh file previews and Git status after successful writes.
- Keep filesystem callback permissions separate from agent tool approvals unless the protocol provides a reliable association.

Callback policy should expose disabled/read-only/read-write access and command-execution permission. Terminal commands run with the backend account’s permissions; callback path checks do not sandbox those commands.

During replay, permit necessary reads but reject writes and command creation. Historical permission records remain historical.

## 6. ACP command terminals and lifecycle

Implement all five command-terminal methods:

```text
terminal/create
terminal/output
terminal/wait_for_exit
terminal/kill
terminal/release
```

Use a shared backend process manager with exact executable/arguments, environment, directory, bounded UTF-8 output, and exit-code/signal reporting. Creation returns a terminal ID promptly; waiting remains asynchronous. Killing preserves output; release invalidates the handle while retained tool output remains displayable. [ACP terminals](https://agentclientprotocol.com/protocol/v1/terminals)

These command processes are distinct from interactive login PTYs.

Bind handles to backend/store/session/connection generation. Implement whole-process-tree termination and reaping; the current Unix helper’s direct-child kill is insufficient for commands that spawn children.

Cancellation, disconnection, session closure, and backend shutdown must settle pending waits and terminate owned command trees according to the operation lifecycle. A desktop disconnect must not kill unrelated daemon work.

Extend shared projection and native content rendering so terminal references in tool calls show live output, truncation, status, and retained final output after release or restart.

## 7. Persistence and UI integration

Use an additive schema-4 migration for:

- Callback operation identities, intent/result state, and recovery metadata.
- Terminal metadata and retained output references.

Keep authentication terminal contents transient. Keep credentials out of these tables.

Before upgrading schema 3, create a SQLite-consistent backup. Retain WAL/FULL durability, contention handling, and writer-free reads. Update the currently hard-coded schema reporting and version checks.

Filesystem effects and process creation cannot be made atomic with SQLite. Record intent before dispatch and completion afterward. Recovery must distinguish completed, unapplied, and uncertain effects; it must never rerun commands automatically.

Integrate native UI changes into the existing profile browser, controller, composer permissions, and tool rendering:

- Preset selection and backend-specific readiness.
- Sign-in, login progress, cancellation, retry, and advertised logout.
- Callback policy and scoped approval prompts.
- Agent-provided configuration controls.
- Explicit capability/history limitations.

Update compatibility types and controls through [the existing IPC wrapper](/srv/data/code/CodexMonitor/src/services/tauri.ts) and [runtime feature](/srv/data/code/CodexMonitor/src/features/runtime). Avoid adding a separate provider-specific conversation viewer.

## 8. Acceptance tests and delivery gates

### Shared protocol and service tests

Extend the existing ACP fixtures with authentication, filesystem, terminal, and URL-elicitation scenarios. Run the same suite under different profile names to prove behavior is not selected by provider name.

Required cases:

- Callbacks arriving while a prompt, setup request, permission, or terminal wait is pending.
- Numeric/string request IDs, stale generations, duplicate requests, and cross-session handle rejection.
- Existing credentials, authentication required, agent authentication, terminal login success/failure/cancellation, logout, and unknown auth-method types.
- Login output absent from durable evidence; reconnect does not resend a prompt.
- Unicode and line-range reads, new-file creation, recoverable replacement, denied paths, symlink changes, invalid UTF-8, and oversized input.
- Command output beyond the limit, split multibyte characters, exit codes/signals, concurrent output/wait/kill, release, child-tree cleanup, and daemon termination.
- Replay attempting mutations: no file changes, command launch, actionable permission, or new execution/capture.
- Transport loss after a mutation: explicit uncertainty and no automatic repeat.

### Local and daemon contract tests

Extend [the backend contract runner](/srv/data/code/CodexMonitor/scripts/test-backend-client-contracts.py).

Use distinct local and daemon directories containing different sentinel files at corresponding paths. Assert that remote callbacks read/write/execute only on the daemon host. Exercise remote interactive login input/output and connection switching.

Upgrade a populated schema-3 store, compare existing payloads and capture hashes byte-for-byte, and rerun sustained contention with callback recording, replay, live execution, and comparison-note writes.

### Native acceptance: three agents × two backends

For **each pinned agent**, complete these checks locally and through the daemon:

1. Configure its preset and record the actual initialized capabilities.
2. Exercise a fresh authentication flow and a subsequent authenticated restart.
3. Create a conversation and render a bounded response.
4. Read a unique fixture file, create/edit another, and execute a bounded command. Protocol receipts must distinguish client callbacks from operations the agent performs itself.
5. Exercise permission denial/approval, cancellation, queued follow-ups, and advertised model/mode/effort controls.
6. Reopen after app and daemon restart; verify supplied history, captures, file changes, terminal evidence, and comparison notes.
7. Exercise discovery and external history where supported; otherwise verify explicit limitations.
8. Test headless daemon login without transferring desktop credentials silently.

Use [the native acceptance driver](/srv/data/code/CodexMonitor/scripts/test-native-egui.py), isolated repositories/data directories, and immutable binary identities. Record actual versions, account/authentication method, capabilities, receipts, and redacted screenshots.

Missing credentials or a failed provider flow must remain a named acceptance blocker. Fixture success does not establish real-agent support.

Run focused checks during implementation, then:

```sh
npm run typecheck
npm run test
npm run build

cargo check --locked --no-default-features --lib \
  --manifest-path src-tauri/Cargo.toml
cargo check --locked --no-default-features --features native-gui --all-targets \
  --manifest-path src-tauri/Cargo.toml
cargo test --locked --no-default-features --features native-gui --all-targets \
  --manifest-path src-tauri/Cargo.toml
cargo test --locked --features custom-protocol \
  --manifest-path src-tauri/Cargo.toml

python3 scripts/test-run-store-contention.py
python3 scripts/test-runtime-store-contention.py
```

Add daemon-only shared-service feature checks and the extended authenticated contract runner using their implementation’s explicit evidence-directory arguments.

Preserve the current installation throughout implementation acceptance. Installation, launcher replacement, and publication require a separate request. Update canonical runtime and native acceptance documentation with actual tested behavior.
