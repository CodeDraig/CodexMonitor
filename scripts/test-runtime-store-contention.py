#!/usr/bin/env python3
"""Run the 62-second mixed-runtime acceptance with separate desktop/daemon test executables."""
import argparse
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
METHOD = "shared::agent_runtime::contention_tests::sustained_mixed_runtime_writers_preserve_events_captures_and_notes"

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, help="use an already-built host-independent daemon test executable")
    parser.add_argument("--desktop", type=Path, help="use an already-built desktop library test executable; with --daemon this skips Cargo entirely")
    args = parser.parse_args()

    daemon = args.daemon.resolve(strict=True) if args.daemon else None
    desktop = args.desktop.resolve(strict=True) if args.desktop else None
    if desktop is None or daemon is None:
        targets = []
        if desktop is None:
            targets.append("--lib")
        if daemon is None:
            targets.extend(["--bin", "codex_monitor_daemon"])
        command = ["cargo", "test", "--locked", "--no-default-features", "--manifest-path", str(ROOT / "src-tauri/Cargo.toml"),
                   *targets, "--no-run", "--message-format=json"]
        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
        executables = {}
        for line in result.stdout.splitlines():
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("reason") == "compiler-artifact" and message.get("executable") and message.get("profile", {}).get("test"):
                executables[message["target"]["name"]] = message["executable"]
        if desktop is None:
            desktop = Path(executables.pop("codex_monitor_lib")).resolve(strict=True)
        if daemon is None:
            daemon = Path(executables.pop("codex_monitor_daemon")).resolve(strict=True)
        if executables:
            raise RuntimeError(f"Unexpected test executables from host-independent build: {executables}")
    subprocess.run([str(desktop), "--exact", METHOD, "--ignored", "--nocapture"], cwd=ROOT,
                   env={**os.environ, "MONITOR_RUNTIME_STRESS_DAEMON_EXE": str(daemon)}, check=True)

if __name__ == "__main__":
    main()
