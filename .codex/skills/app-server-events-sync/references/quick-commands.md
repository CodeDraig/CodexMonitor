# Protocol comparison commands

Run from the repository root. Use the executable configured in CodexMonitor if it differs from `codex`.

```bash
codex --version
protocol_schema_dir=$(mktemp -d /tmp/codex-monitor-schema-XXXXXX)
codex app-server generate-json-schema --experimental --out "$protocol_schema_dir"
python3 scripts/check-codex-protocol.py --schema-dir "$protocol_schema_dir"
rg -n 'SUPPORTED_APP_SERVER_METHODS' src/utils/appServerEvents.ts
rg -n 'send_request' src-tauri/src/shared/codex_core.rs src-tauri/src/backend/app_server.rs
```

The schema check uses Python jsonschema. Inspect `ClientRequest.json`, `ServerRequest.json`, and `ServerNotification.json` in the generated directory for complete method sets; inspect individual response/parameter schemas for payload drift. Compare stable and experimental inventories separately.

When an upstream checkout exists, use `git -C ../Codex rev-parse HEAD` to record it. Protocol declarations are in `codex-rs/app-server-protocol/src/protocol/common.rs` and `protocol/v2/`. No fetch, checkout, or prune is required.

Update `docs/app-server-events.md` after implementation. Run targeted tests, typecheck, the frontend suite, and Rust checks for changed backend behavior. Desktop acceptance is separate from schema and unit checks.
