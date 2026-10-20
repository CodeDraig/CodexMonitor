# App-server protocol reference

Compatibility target: Codex CLI **0.162.1**, experimental API enabled. Schemas are generated from the selected executable. The 0.163.0 stable method inventory adds `thread/readState/changed`, which is not routed. Method inventory alone does not establish payload compatibility.

Protocol documentation: https://learn.chatgpt.com/docs/app-server

## Integration behavior

- Initialization must return a successful JSON-RPC result before the backend emits `codex/connected`. Configuration warnings, runtime warnings, deprecations, failed MCP startup, and model rerouting reach visible diagnostics and Debug.
- Command and file approvals return decision objects. Permission approvals return the selected filesystem/network grant and explicit turn/session scope. Command allowlists never auto-accept permission grants or network/additional-permission requests.
- MCP elicitation supports standard `form` and HTTP(S) `url` modes. Device verification and OpenAI-specific forms require another client; the prompt remains visible with decline/cancel controls. `serverRequest/resolved` removes pending approvals and user-input requests.
- Resume requests use `excludeTurns: true`. The shared backend reads every `thread/turns/list` page with ascending order and `itemsView: full`, deduplicates anchor turns, and returns the existing IPC thread snapshot only after all pages succeed. It does not treat summary or failed pages as complete history. Paginated history support is required; very long histories still require a complete load before display, and the existing frontend scrollback limit applies.
- History hydration retains messages changed during loading and does not overwrite newer live turn state. Fork uses `excludeTurns: true` and hydrates through resume.
- Foreground subscription verifies loaded threads or resumes the stored thread. Foreground detach deliberately retains the shared server subscription so background work and approvals continue. It does not call `thread/unsubscribe`, which could remove monitoring needed by another view.
- Current item variants render through existing message/tool/image components. Unknown variants retain an inspectable tool record. Generated images use the existing image viewer; subagent activity contributes parent links. File patch updates and MCP tool progress update conversation items.
- Steering and collaboration modes do not depend on removed Codex feature flags. Personality selection is not offered. Existing obsolete config entries are preserved, but settings saves do not write them.
- A `collaborationMode/list` entry is preset metadata (`name`, optional `mode`, optional `model` and `reasoning_effort`), not a valid `turn/start.collaborationMode` object. Before sending a turn, the shared Rust core maps it to `{mode, settings:{model, reasoning_effort, developer_instructions}}`; explicit native model and effort selections override the flat preset defaults, while already-nested protocol settings remain authoritative. An entry without a protocol `mode` or any available model is omitted. This satisfies the nested `settings` requirement in Codex CLI 0.162.1 while preserving the existing desktop/daemon `send_user_message` payload contract. `turn/steer` and `review/start` schemas do not have that collaboration settings field.
- Feature settings persist through the shared config writer. `apps` and `plugins` also update loaded app-server processes through `experimentalFeature/enablement/set`; runtime errors identify the persisted-but-not-applied state. Other feature changes require an app-server restart.
- Model discovery follows pagination, excludes hidden entries, and retains advertised input modalities and service-tier metadata. The composer rejects image submission to a model explicitly advertising no image support. `skills/changed` refreshes skills.

## Supported notifications

- `account/login/completed`
- `account/rateLimits/updated`
- `account/updated`
- `app/list/updated`
- `configWarning`
- `deprecationNotice`
- `error`
- `hook/completed`
- `hook/started`
- `item/agentMessage/delta`
- `item/commandExecution/outputDelta`
- `item/commandExecution/terminalInteraction`
- `item/completed`
- `item/fileChange/outputDelta`
- `item/fileChange/patchUpdated`
- `item/mcpToolCall/progress`
- `item/plan/delta`
- `item/reasoning/summaryPartAdded`
- `item/reasoning/summaryTextDelta`
- `item/reasoning/textDelta`
- `item/started`
- `mcpServer/startupStatus/updated`
- `model/rerouted`
- `serverRequest/resolved`
- `skills/changed`
- `thread/archived`
- `thread/closed`
- `thread/name/updated`
- `thread/started`
- `thread/status/changed`
- `thread/tokenUsage/updated`
- `thread/unarchived`
- `turn/completed`
- `turn/diff/updated`
- `turn/plan/updated`
- `turn/started`
- `warning`

## Unrouted notifications

These are not exposed as dedicated UI behavior. Their raw envelopes remain available in Debug. Optional voice, goals, remote control, projects, and plugin management are separate feature scope. Deprecated `thread/compacted` is intentionally ignored in favor of context-compaction items.

- `account/gatewayOAuth/changed`
- `autoApprovalReview/strictReviewRequired`
- `command/exec/outputDelta`
- `externalAgentConfig/import/completed`
- `externalAgentConfig/import/progress`
- `fs/changed`
- `fuzzyFileSearch/sessionCompleted`
- `fuzzyFileSearch/sessionUpdated`
- `guardianWarning`
- `item/autoApprovalReview/completed`
- `item/autoApprovalReview/started`
- `mcpServer/event/stream/notification`
- `mcpServer/oauthLogin/completed`
- `model/safetyBuffering/updated`
- `model/verification`
- `modelProvider/authRecoveryCompleted`
- `modelProvider/authRecoveryStarted`
- `process/exited`
- `process/outputDelta`
- `project/changed`
- `remoteControl/status/changed`
- `thread/attachment/updated`
- `thread/compacted`
- `thread/deleted`
- `thread/environment/connected`
- `thread/environment/disconnected`
- `thread/goal/cleared`
- `thread/goal/updated`
- `thread/prediction/updated`
- `thread/project/updated`
- `thread/queue/changed`
- `thread/realtime/closed`
- `thread/realtime/error`
- `thread/realtime/item/completed`
- `thread/realtime/item/started`
- `thread/realtime/item/transcript/delta`
- `thread/realtime/itemAdded`
- `thread/realtime/outputAudio/delta`
- `thread/realtime/sdp`
- `thread/realtime/started`
- `thread/realtime/transcript/delta`
- `thread/realtime/transcript/done`
- `thread/reverted`
- `thread/settings/updated`
- `turn/moderationMetadata`
- `windows/worldWritableWarning`
- `windowsSandbox/setupCompleted`

## Outgoing requests

- `account/login/cancel`
- `account/login/start`
- `account/rateLimits/read`
- `account/read`
- `app/list`
- `collaborationMode/list`
- `experimentalFeature/enablement/set`
- `experimentalFeature/list`
- `initialize`
- `mcpServerStatus/list`
- `model/list`
- `review/start`
- `skills/list`
- `thread/archive`
- `thread/compact/start`
- `thread/fork`
- `thread/list`
- `thread/loaded/list`
- `thread/name/set`
- `thread/read`
- `thread/resume`
- `thread/start`
- `thread/turns/list`
- `turn/interrupt`
- `turn/start`
- `turn/steer`

## Unused client requests

The following installed-schema methods are not sent by this client. Absence is not automatically a defect; many belong to optional surfaces.

- `account/bedrock/checkGovCloudRequirements`
- `account/bedrock/discover`
- `account/bedrock/setup`
- `account/gatewayOAuth/cancel`
- `account/gatewayOAuth/login`
- `account/gatewayOAuth/read`
- `account/logout`
- `account/rateLimitResetCredit/consume`
- `account/sendAddCreditsNudgeEmail`
- `account/usage/read`
- `account/workspaceMessages/read`
- `app/installed`
- `app/read`
- `command/exec`
- `command/exec/resize`
- `command/exec/terminate`
- `command/exec/write`
- `config/batchWrite`
- `config/mcpServer/reload`
- `config/read`
- `config/value/write`
- `configRequirements/read`
- `environment/add`
- `environment/info`
- `environment/status`
- `externalAgentConfig/detect`
- `externalAgentConfig/import`
- `externalAgentConfig/import/readHistories`
- `externalAgentConfig/import/recordHistory`
- `feedback/upload`
- `fs/copy`
- `fs/createDirectory`
- `fs/getMetadata`
- `fs/readDirectory`
- `fs/readFile`
- `fs/remove`
- `fs/unwatch`
- `fs/watch`
- `fs/writeFile`
- `fuzzyFileSearch`
- `fuzzyFileSearch/sessionStart`
- `fuzzyFileSearch/sessionStop`
- `fuzzyFileSearch/sessionUpdate`
- `hooks/list`
- `marketplace/add`
- `marketplace/remove`
- `marketplace/upgrade`
- `mcpServer/event/stream/start`
- `mcpServer/event/stream/stop`
- `mcpServer/oauth/login`
- `mcpServer/resource/read`
- `mcpServer/tool/call`
- `memory/reset`
- `memory/status`
- `mock/experimentalMethod`
- `modelProvider/capabilities/read`
- `permissionProfile/list`
- `plugin/install`
- `plugin/installed`
- `plugin/list`
- `plugin/read`
- `plugin/reconcile`
- `plugin/search`
- `plugin/share/checkout`
- `plugin/share/delete`
- `plugin/share/list`
- `plugin/share/save`
- `plugin/share/updateTargets`
- `plugin/skill/read`
- `plugin/uninstall`
- `process/kill`
- `process/resizePty`
- `process/spawn`
- `process/writeStdin`
- `project/create`
- `project/delete`
- `project/import`
- `project/list`
- `project/move`
- `project/read`
- `project/update`
- `remoteControl/client/list`
- `remoteControl/client/revoke`
- `remoteControl/disable`
- `remoteControl/enable`
- `remoteControl/pairing/start`
- `remoteControl/pairing/status`
- `remoteControl/status/read`
- `rollout/compress`
- `server/diagnostics`
- `skills/config/write`
- `skills/extraRoots/set`
- `thread/approveGuardianDeniedAction`
- `thread/attachment/add`
- `thread/attachment/list`
- `thread/attachment/remove`
- `thread/attachmentOwner/list`
- `thread/backgroundTerminals/clean`
- `thread/backgroundTerminals/list`
- `thread/backgroundTerminals/terminate`
- `thread/decrement_elicitation`
- `thread/delete`
- `thread/goal/clear`
- `thread/goal/get`
- `thread/goal/set`
- `thread/increment_elicitation`
- `thread/inject_items`
- `thread/items/list`
- `thread/memoryMode/set`
- `thread/metadata/update`
- `thread/prediction/request`
- `thread/queue/add`
- `thread/queue/delete`
- `thread/queue/list`
- `thread/queue/reorder`
- `thread/queue/start`
- `thread/queue/update`
- `thread/realtime/appendAudio`
- `thread/realtime/appendSpeech`
- `thread/realtime/appendText`
- `thread/realtime/listVoices`
- `thread/realtime/start`
- `thread/realtime/stop`
- `thread/revert`
- `thread/search`
- `thread/searchOccurrences`
- `thread/section/move`
- `thread/settings/update`
- `thread/shellCommand`
- `thread/timeline/list`
- `thread/unarchive`
- `thread/unsubscribe`
- `threadSection/create`
- `threadSection/delete`
- `threadSection/list`
- `threadSection/update`
- `turn/settings/update`
- `userVerification/cancel`
- `userVerification/delete`
- `userVerification/enroll`
- `userVerification/status`
- `userVerification/verify`
- `windowsSandbox/readiness`
- `windowsSandbox/setupStart`

## Server requests

Handled:

- `item/commandExecution/requestApproval`
- `item/fileChange/requestApproval`
- `item/permissions/requestApproval`
- `item/tool/requestUserInput`
- `mcpServer/elicitation/request` (mode limits above)

Not implemented:

- `item/tool/call`: client-defined dynamic tools are not registered.
- `account/chatgptAuthTokens/refresh`: external token-managed login is not implemented.
- `attestation/generate` and `currentTime/read`: corresponding client capabilities are not enabled.
- `applyPatchApproval` and `execCommandApproval`: legacy v1 approvals are not supported.

## Source map

- Shared transport and initialization: `src-tauri/src/backend/app_server.rs`.
- Shared requests and history: `src-tauri/src/shared/codex_core.rs`, `src-tauri/src/shared/codex_core/history.rs`.
- App and daemon adapters: `src-tauri/src/codex/mod.rs`, `src-tauri/src/bin/codex_monitor_daemon.rs`, `src-tauri/src/bin/codex_monitor_daemon/rpc/codex.rs`.
- IPC: `src/services/tauri.ts`; event fanout: `src/services/events.ts`.
- Method inventory: `src/utils/appServerEvents.ts`; routing: `src/features/app/hooks/useAppServerEvents.ts`.
- Requests: `useThreadApprovals.ts`, `useThreadApprovalEvents.ts`, `ApprovalToasts.tsx`, `ServerRequestForm.tsx`.
- State and hydration: `useThreadActions.ts`, `useThreadsReducer.ts`, `threadReducer/threadItemsSlice.ts`.
- Rendering conversion: `src/utils/threadItems.conversion.ts`; messages: `src/features/messages/components/Messages.tsx` and the item-specific row components alongside it.

## Refresh and verification

Use a fresh output directory; schema generation must not overwrite the only copy of previous evidence.

```bash
codex --version
protocol_schema_dir=$(mktemp -d /tmp/codex-monitor-schema-XXXXXX)
codex app-server generate-json-schema --experimental --out "$protocol_schema_dir"
python3 scripts/check-codex-protocol.py --schema-dir "$protocol_schema_dir"
npm run typecheck
npm run test
(cd src-tauri && cargo check --locked)
npm run doctor:strict
npm run tauri:build
```

The optional schema check requires Python `jsonschema`. It validates outgoing method names and the request/response fixtures in `tests/fixtures/app-server-contracts.json`; it does not replace behavioral tests or desktop acceptance. Stable and experimental schema inventories must be compared separately.

If an upstream checkout is available, record `git -C ../Codex rev-parse HEAD` without changing it. Protocol declarations live in `codex-rs/app-server-protocol/src/protocol/common.rs` and `protocol/v2/`; generated release schemas live under `codex-rs/app-server-protocol/schema/json/`. A sibling checkout is not required.

Desktop acceptance covers connection/errors, existing history, streaming a turn, approval and denial, MCP form/URL input, interruption, queue/steer, settings save, and reconnection. Run local and daemon checks for shared behavior. A passing build or isolated protocol fixture does not establish interactive desktop acceptance.
