#!/usr/bin/env python3
"""Run the 62-second mixed-runtime acceptance with separate desktop/daemon test executables."""
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
METHOD = "shared::agent_runtime::contention_tests::sustained_mixed_runtime_writers_preserve_events_captures_and_notes"

def main():
    command = ["cargo", "test", "--locked", "--manifest-path", str(ROOT / "src-tauri/Cargo.toml"),
               "--lib", "--bin", "codex_monitor_daemon", "--no-run", "--message-format=json"]
    result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
    executables = {}
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") == "compiler-artifact" and message.get("executable") and message.get("profile", {}).get("test"):
            executables[message["target"]["name"]] = message["executable"]
    desktop = executables["codex_monitor_lib"]
    daemon = executables["codex_monitor_daemon"]
    subprocess.run([desktop, "--exact", METHOD, "--ignored", "--nocapture"], cwd=ROOT,
                   env={**os.environ, "MONITOR_RUNTIME_STRESS_DAEMON_EXE": daemon}, check=True)

if __name__ == "__main__":
    main()
