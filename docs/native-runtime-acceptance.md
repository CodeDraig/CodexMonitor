# Native runtime acceptance

CodexMonitor's frontend is the Rust egui/eframe/wgpu application. Its local and
remote backend drivers share runtime contracts with the daemon. See
`agent-runtimes.md`, `native-egui-acceptance.md`, and
`acp-agent-integration-acceptance.md` for behavior and evidence boundaries.

## Build and automated checks

```sh
python3 scripts/check-native-only.py
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --no-default-features --features acp-host-services \
  --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --no-default-features --features native-acceptance \
  --all-targets --manifest-path src-tauri/Cargo.toml
python3 scripts/test-acp-fixture-session-identity.py
python3 scripts/test-runtime-store-contention.py
python3 scripts/package-native.py --self-test
```

Package the desktop, daemon, and daemon-control utility with
`scripts/build-native.sh --output-dir <new-directory>`. Native assets are embedded
or staged from the Rust crate. This command does not install a build.

## Local and daemon matrix

Exercise the following with an isolated local data directory, then with a
separate authenticated daemon. Repeat common ACP behavior with independent
profiles and launch arguments; keep protocol fixtures distinct from real-agent
acceptance.

- Discover paginated external sessions globally, including a directory absent
  from the workspace list. Import it without starting Codex.
- Open a complete agent-supplied replay in the normal sidebar/main view. Preserve
  repeated messages, tool patches, resource/media content, and compaction markers.
- Continue the conversation; change advertised model, effort, and mode options.
  Exercise permissions, queue, cancel, configuration, and attachments.
- Observe an external change through idle refresh while retaining drafts and
  scroll position. Keep cached history readable after a failed refresh.
- Switch backends and verify connection epochs, store identities, drafts,
  permissions, and late replies stay scoped correctly.
- Create recorded runs, inspect immutable initial captures, save comparison
  notes, restart both processes, and verify history, metadata, and notes.
- Exercise concurrent replay, execution, and comparison writes against a shared
  store. Verify committed generations, ordering, ownership, revisions, integrity,
  and foreign keys.

For authentication and client callbacks, also exercise the ACP services runner
with explicit daemon/executable paths. Provider-native tools and Monitor client
callbacks are different coverage claims. Record actual agent versions and
negotiated capabilities. Local/same-host daemon evidence does not imply a
physical remote host, another platform, or another provider has passed.

All interactive checks use task-owned processes and disposable state. They do
not replace the current installation or modify its live data.
