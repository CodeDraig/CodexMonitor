# Native run-record acceptance

The frontend is the Rust egui/eframe/wgpu desktop. Run recording and comparison
use the shared SQLite store and the same domain operations locally and through
the authenticated daemon. Automated tests do not establish an interactive GUI
result for an untested executable.

## Automated gates

```sh
python3 scripts/check-native-only.py
cargo test --locked --no-default-features --features native-acceptance \
  --all-targets --manifest-path src-tauri/Cargo.toml
python3 scripts/test-run-store-contention.py
python3 scripts/test-runtime-store-contention.py
```

The suites cover initial capture immutability, durable execution evidence,
comparison revision conflicts, schema migration, writer contention, recovery,
and restart persistence. The sustained runners use separate desktop/daemon test
processes against one isolated store, including a lock beyond the busy timeout.

## Interactive acceptance

Use an isolated repository and data directory, an explicit executable, and a
separate authenticated daemon endpoint. Keep the installed application and live
data outside the fixture.

1. Start a local recorded run from the native workbench. Inspect its initial
   capture, completed command evidence, and Git patch.
2. Open the same run in the main conversation, continue it, and verify the initial
   capture is unchanged while a later capture is separate.
3. Repeat through the daemon. Compare local and daemon captures, save notes, and
   force a revision conflict in a second client.
4. During live execution, write comparison notes from another process sharing
   the store. Verify every acknowledged revision and committed evidence item.
5. Restart desktop and daemon, reopen both conversations and captures, and verify
   notes, hashes, metadata, and ordered evidence.
6. Check SQLite integrity and foreign keys. Close task-owned processes and remove
   disposable test data after inspection.

Interactive evidence must identify source, executable hash, backend/store,
process, display, action, and observed result. A help command, unit test, or
screenshot alone does not prove this workflow.
