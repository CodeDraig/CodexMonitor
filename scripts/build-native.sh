#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/src-tauri/Cargo.toml"
profile="release"
output_dir="$repo_root/src-tauri/target/native-packages"

usage() {
  cat <<'EOF'
Usage: scripts/build-native.sh [--debug] [--output-dir PATH]

Build and package the native desktop app, daemon, and daemon control utility.
This path does not run npm, Tauri, or frontend asset generation.
EOF
}

while (($#)); do
  case "$1" in
    --debug)
      profile="debug"
      shift
      ;;
    --output-dir)
      if (($# < 2)); then
        usage >&2
        exit 2
      fi
      output_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$output_dir" != /* ]]; then
  output_dir="$repo_root/$output_dir"
fi

cargo_args=(
  --manifest-path "$manifest_path"
  --locked
  --no-default-features
  --features native-gui
)
if [[ "$profile" == release ]]; then
  cargo_args+=(--release)
fi

echo "Checking the active native dependency graph for Tauri/webview crates"
dependency_graph="$(cargo tree \
  --manifest-path "$manifest_path" \
  --locked \
  --no-default-features \
  --features native-gui \
  --edges normal,build \
  --prefix none)"
while IFS= read -r dependency; do
  dependency_name="${dependency%% *}"
  case "$dependency_name" in
    # notify-rust's Windows backend is independent of the retired webview host.
    tauri-winrt-notification) ;;
    tauri|tauri-*|wry|webkit2gtk|javascriptcore-rs)
      echo "The native feature graph contains a webview host dependency: $dependency_name" >&2
      exit 1
      ;;
  esac
done <<<"$dependency_graph"

echo "Building native app and shared daemon binaries ($profile)"
cargo build "${cargo_args[@]}" \
  --bin codex_monitor_native \
  --bin codex_monitor_daemon \
  --bin codex_monitor_daemonctl

target_directory="$(cargo metadata \
  --manifest-path "$manifest_path" \
  --locked \
  --no-deps \
  --format-version 1 \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
if [[ -z "$target_directory" ]]; then
  echo "Cargo metadata did not report a target directory" >&2
  exit 1
fi
binary_dir="$target_directory/$profile"
echo "Packaging binaries from Cargo target directory: $binary_dir"
binary_suffix=""
if [[ -f "$binary_dir/codex_monitor_native.exe" && ! -f "$binary_dir/codex_monitor_native" ]]; then
  binary_suffix=".exe"
fi
for binary in codex_monitor_native codex_monitor_daemon codex_monitor_daemonctl; do
  if [[ ! -f "$binary_dir/$binary$binary_suffix" ]]; then
    echo "Cargo target binary is missing: $binary_dir/$binary$binary_suffix" >&2
    exit 1
  fi
done
python3 "$repo_root/scripts/package-native.py" \
  --native "$binary_dir/codex_monitor_native$binary_suffix" \
  --daemon "$binary_dir/codex_monitor_daemon$binary_suffix" \
  --daemonctl "$binary_dir/codex_monitor_daemonctl$binary_suffix" \
  --output-dir "$output_dir"

python3 "$repo_root/scripts/package-native.py" --self-test
