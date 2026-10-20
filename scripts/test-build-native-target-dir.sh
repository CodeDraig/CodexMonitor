#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/codex-monitor-cargo-target-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/fake-bin" "$tmp/nondefault-target"
cat >"$tmp/fake-bin/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  tree)
    printf '\n'
    ;;
  build)
    profile=debug
    for arg in "$@"; do
      if [[ "$arg" == --release ]]; then
        profile=release
      fi
    done
    bin_dir="${CARGO_TARGET_DIR:?}/$profile"
    mkdir -p "$bin_dir"
    suffix="${FAKE_WINDOWS_EXES:+.exe}"
    cat >"$bin_dir/codex_monitor_native$suffix" <<'NATIVE'
#!/bin/sh
echo 'codex-monitor-native test binary'
NATIVE
    for binary in codex_monitor_daemon codex_monitor_daemonctl; do
      cat >"$bin_dir/$binary$suffix" <<'BIN'
#!/bin/sh
exit 0
BIN
    done
    chmod +x "$bin_dir"/*
    ;;
  metadata)
    printf '{"target_directory":"%s"}\n' "${CARGO_TARGET_DIR:?}"
    ;;
  *)
    echo "unexpected fake cargo command: $*" >&2
    exit 2
    ;;
esac
CARGO
chmod +x "$tmp/fake-bin/cargo"

PATH="$tmp/fake-bin:$PATH" \
CARGO_TARGET_DIR="$tmp/nondefault-target" \
  "$repo_root/scripts/build-native.sh" \
  --output-dir "$tmp/packages"

version="$(python3 -c 'import sys,tomllib; from pathlib import Path; print(tomllib.loads(Path(sys.argv[1]).read_text())["package"]["version"])' "$repo_root/src-tauri/Cargo.toml")"
arch="$(python3 -c 'import platform; print({"amd64":"x86_64","arm64":"aarch64"}.get(platform.machine().lower(), platform.machine().lower()))')"
test -f "$tmp/packages/CodexMonitor-$version-linux-$arch/bin/codex_monitor_native"
test -f "$tmp/packages/CodexMonitor-$version-linux-$arch/bin/codex_monitor_daemon"
test -f "$tmp/packages/CodexMonitor-$version-linux-$arch/bin/codex_monitor_daemonctl"
test -f "$tmp/packages/CodexMonitor-$version-linux-$arch.tar.gz"
echo "Native build target-directory smoke test passed with CARGO_TARGET_DIR=$tmp/nondefault-target"

mkdir -p "$tmp/windows-target"
PATH="$tmp/fake-bin:$PATH" \
CARGO_TARGET_DIR="$tmp/windows-target" \
FAKE_WINDOWS_EXES=1 \
  "$repo_root/scripts/build-native.sh" \
  --output-dir "$tmp/packages-exe"
test -f "$tmp/packages-exe/CodexMonitor-$version-linux-$arch/bin/codex_monitor_native"
test -f "$tmp/packages-exe/CodexMonitor-$version-linux-$arch/bin/codex_monitor_daemon"
test -f "$tmp/packages-exe/CodexMonitor-$version-linux-$arch/bin/codex_monitor_daemonctl"
test -f "$tmp/packages-exe/CodexMonitor-$version-linux-$arch.tar.gz"
echo "Native .exe source binary smoke test passed with CARGO_TARGET_DIR=$tmp/windows-target"
