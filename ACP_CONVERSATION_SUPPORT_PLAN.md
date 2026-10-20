# Full ACP Conversations in the Sidebar and Main View

## Summary and agreed behavior

Integrate ACP conversations into the existing sidebar, message view, and composer. OpenCode is the first real acceptance target; discovery, history, rendering, and interaction behavior must use shared ACP code for every configured ACP profile.

This plan is grounded in `d4dbad7` **plus the current uncommitted runtime implementation**. Today, session listing reads only Monitor-owned bindings, replayed ACP history is excluded from rendering, and adding a workspace starts Codex automatically.

Agreed behavior:

- A global browser discovers agent sessions, including directories outside existing workspaces.
- Opening a discovered session reuses its matching workspace or offers **Add workspace and open**.
- Agents lacking discovery or history loading remain usable, with explicit capability and history-availability labels.
- Conversations refresh automatically while idle, including changes made by other clients.
- “Complete history” means the conversation supplied by a successful agent replay. Compacted or unavailable original content is identified honestly.

ACP v1 supports capability-gated [session discovery](https://agentclientprotocol.com/protocol/v1/session-list) and [full conversation replay through `session/load`](https://agentclientprotocol.com/protocol/v1/session-setup). Keep v1 as this milestone’s compatibility target; v2 remains a separate draft migration.

## Shared contracts and persistence

Extend the shared runtime domain in `src-tauri/src/shared/agent_runtime/` first, then expose identical operations through desktop, daemon, and frontend IPC.

**Identity and profiles**

- Introduce a generic `acp` profile kind with an exact executable/argument vector. OpenCode becomes a launch preset; generic ACP launchers must not receive an automatically appended `acp` argument.
- Continue reading existing `open_code_acp` profiles and session snapshots without changing their launch behavior.
- Give each effective profile launch configuration a stable connection scope. Display-name changes preserve it; executable, arguments, or configured launch-directory changes create a new scope.
- Identify native sessions by store, profile connection scope, and native session ID. Preserve existing Monitor session UUIDs.
- Add a tagged `ConversationRef` to sidebar summaries and selection state. ACP UI keys include backend/store identity and Monitor session UUID; native IDs remain separate. Never pass an ACP UI key to a Codex command.

**Additive runtime operations**

| Operation | Required contract |
|---|---|
| `discover_sessions` | `{profileId, cwd?, cursor?}` → session summaries, negotiated capabilities, source status, and optional next cursor. Omitted `cwd` requests global discovery. |
| `attach_session` | `{catalogId, workspaceAssociation}` → existing or newly registered Monitor binding. Association explicitly reuses a workspace or adds the reported directory. |
| `load_conversation_history` | `{sessionId, requestId}` → prompt return of a durable load ticket; replay runs asynchronously. Repeated request IDs return the same ticket. |
| `read_conversation` | `{sessionId, generation?, cursor?, limit?}` → ordered items, snapshot generation, load state, coverage, freshness, and pagination. |
| `update_conversation_metadata` | `{sessionId, expectedRevision, titleOverride?, pinned?, hidden?}` → revision-checked Monitor presentation metadata. |

Keep existing execution, submission, permission, cancellation, and capture contracts. Keep the existing `history` operation’s Monitor-execution meaning; complete conversation history gets its own contract.

**Schema 3**

Add a session catalog, history-load records, materialized conversation items, and conversation attachments. Retain raw protocol evidence in the existing journal, adding an optional history-load identity.

Backfill current bindings into the catalog while preserving UUIDs and references. Discovery uses an identity upsert rather than creating another binding on every refresh. Preserve ambiguous legacy bindings through aliases instead of deleting or silently rewriting referenced records.

Create a SQLite-consistent backup before upgrading schema 2. Preserve existing run, execution, capture, attachment, comparison, and event payloads. Retain WAL/FULL durability, writer-free reads, ownership checks, and contention handling.

## Implementation slices

### 1. Generic discovery and global browsing

- Generalize ACP capability checks and profile configuration. Recognize object-valued capability markers by presence, including `{}`.
- Use dedicated per-profile discovery connections so listing cannot reload or interrupt an active conversation.
- Add a global **Browse agent sessions** surface with profile filters, title/path search, source errors, and independent pagination for each profile. Treat native cursors as opaque and connection-local.
- Merge discovered sessions for known directories into workspace sidebar lists. Discovery and refresh must work without a Codex connection.
- Persist catalog metadata for cached browsing. A failed or partial scan must not erase cached sessions or resurrect locally hidden rows.
- Add workspace registration without Codex startup. Use the agent-reported directory on the executing backend; validate and normalize it there, including remote paths. Missing directories remain visible with an actionable error.
- For agents without listing, expose known sessions and **Open session by ID** when loading is supported.

### 2. Durable, complete replay

- Replace startup queue draining with explicit replay boundaries. Tag updates for the target session with a load ID before sending `session/load`; enqueue an end barrier in the protocol reader before resolving its response.
- Persist replay and materialize a new history generation. Publish it as authoritative only after the successful response barrier and preceding writes commit.
- Render that snapshot plus subsequent live events. Reload replaces the earlier snapshot; it never appends a second copy of the conversation.
- Preserve message IDs when supplied. Otherwise use generation-scoped ordering identities and preserve chunk order. Never deduplicate repeated messages by matching text.
- Merge tool patches by tool-call identity, preserving omitted fields. Route child-session updates separately.
- Retain the previous successful snapshot after a failed refresh. Label incomplete first loads and stale cached history explicitly.
- Treat replayed permissions as historical content, never actionable requests. Replay cannot create execution receipts, assign historical turns to a new execution, or change immutable initial captures.
- Keep loading asynchronous and history reads paginated, so large conversations do not depend on one long desktop-to-daemon RPC.

### 3. Normal sidebar, message view, and composer

- Extend the existing thread orchestration around `src/features/threads/hooks/useThreadActions.ts` with conversation adapters that resolve the selected reference before issuing commands.
- Merge ACP summaries with Codex summaries while preserving ordering, active/processing anchors, hidden rows, and parent/child reconciliation rules.
- Show agent/profile identity on rows and headers. Support selection, unread/processing indicators, local title overrides, pinning, and hiding. Hiding must not call native deletion.
- Use the existing main `Messages` and `Composer` surfaces through `src/features/layout/hooks/layoutNodes/buildPrimaryNodes.tsx`.
- Route new conversations, sends, queueing, stop, permissions, configuration, attachments, and slash commands according to the selected adapter and negotiated capabilities. ACP selection must not trigger Codex account, model, collaboration, or metadata calls.
- Make run-history and comparison links open the corresponding main conversation. Remove the separate conversation viewer from Workspace Home; retain runtime/profile setup and recorded-run selection there.
- Coalesce existing Codex runtime bindings and their native threads into one visible conversation, with one authoritative event path.

### 4. Content fidelity and automatic refresh

- Use one shared ACP projection for replay and live updates. Preserve text, reasoning, images, audio, embedded resources, resource links, tool content/diffs, plans, configuration, and compaction information when supplied.
- Store binary content as conversation attachments and fetch it on demand. Render supported media and resource types in the main view; retain other content with a visible descriptive fallback and inspectable original data.
- Preserve native message IDs, content annotations, tool identities, and metadata updates currently discarded by normalization.
- Advertise optional client capabilities only after their handlers and renderers exist.
- Refresh on conversation open, focus, explicit refresh, and relevant agent notifications. While the view is visible, perform a 30-second idle freshness check; reload when metadata changes or freshness cannot otherwise be established.
- Defer replay while submitting, executing, draining queued sends, or handling permissions. Preserve drafts and scroll position during background refresh.
- Read cached history without taking execution ownership. If another backend owns the live session, show its available history with a clear read-only ownership state; do not steal it or resubmit prompts.
- Scope requests, cursors, permissions, drafts, and caches by backend, store, conversation, and connection generation.

## Acceptance tests

1. **Discovery:** Exercise multiple pages, profiles sharing native IDs, global sessions outside workspaces, metadata updates, missing directories, stale cursors, and one failed source beside healthy sources. Repeated discovery creates no duplicate conversation; adding an ACP workspace starts no Codex process.

2. **Replay correctness:** Load externally created multi-turn conversations containing repeated identical messages, optional message IDs, partial tool patches, media/resources, and compaction markers. Repeated loads preserve order and show every supplied item once. Test replay response barriers, failed loads, and live events immediately after replay.

3. **Shared ACP behavior:** Run the same conformance suite against an OpenCode-labelled fixture and an independent generic ACP fixture with different launch arguments and capability combinations. Verify explicit limitations for missing discovery/loading and ensure no OpenCode-name checks control history or rendering.

4. **Main-view integration:** Test sidebar discovery, global import, selection, reopening, main composer routing, queue/cancel/permissions, advertised configuration, attachments, run links, hidden-row reconciliation, and backend switching. Assert that ACP actions never invoke Codex-only APIs. Preserve existing Codex behavior.

5. **Migration and contention:** Upgrade a populated schema-2 store and compare existing payloads and capture hashes byte-for-byte. Test desktop and daemon sharing one store during replay, live execution, and comparison-note writes, including a lock beyond the busy timeout. Verify committed generations, event ordering, ownership, acknowledged revisions, and SQLite integrity.

6. **Native acceptance:** In isolated repositories and data directories, create an OpenCode conversation outside Monitor, discover it globally, add its directory, inspect its full conversation in the normal main view, continue it, and observe an external update through idle refresh. Repeat locally and through the daemon; restart both and verify history, captures, metadata, and notes. Exercise a second generic ACP fixture through the same native UI and record its narrower interoperability claim accurately.

Run focused tests during implementation, then:

```sh
npm run typecheck
npm run test
npm run build
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml
python3 scripts/test-runtime-store-contention.py
```

## Preservation and delivery

- Preserve `IMPLEMENTATION_PLAN.md` and `MULTI_AGENT_RUNTIME_PLAN.md` byte-for-byte; their current hashes have been checked.
- Preserve unrelated edits, existing acceptance evidence, and the current local installation.
- After approval, save this plan separately as `ACP_CONVERSATION_SUPPORT_PLAN.md`.
- Update canonical runtime and native acceptance documentation during implementation. Record actual tested agents, versions, capabilities, and artifacts.
- Use shared ACP protocol behavior without provider-specific database scraping or REST fallbacks. Unsupported features remain explicit.
- Do not install or replace the running application as part of implementation acceptance unless separately requested.
