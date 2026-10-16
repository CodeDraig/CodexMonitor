#!/usr/bin/env python3
"""Exercise one SQLite store from the desktop and daemon Rust test executables."""
import argparse
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--iterations", type=int, default=500, help="evidence items and comparison edits per process")
parser.add_argument("--seconds", type=int, default=30, help="minimum paced write duration, excluding forced contention")
args = parser.parse_args()
if args.iterations < 1 or args.seconds < 1:
    parser.error("iterations and seconds must be positive")
root = Path(__file__).resolve().parents[1]
command = ["cargo", "test", "--locked", "--no-run", "--message-format=json", "--lib", "--bin", "codex_monitor_daemon"]
result = subprocess.run(command, cwd=root / "src-tauri", stdout=subprocess.PIPE, text=True, check=True)
executables = {}
for line in result.stdout.splitlines():
    artifact = json.loads(line)
    if artifact.get("reason") == "compiler-artifact" and artifact.get("profile", {}).get("test") and artifact.get("executable"):
        executables[artifact["target"]["name"]] = artifact["executable"]
daemon = executables.pop("codex_monitor_daemon")
if len(executables) != 1:
    raise RuntimeError(f"Expected exactly one desktop library test executable: {executables}")
desktop = next(iter(executables.values()))
print(json.dumps({"desktop": desktop, "daemon": daemon, "iterations": args.iterations, "seconds": args.seconds}), flush=True)
env = dict(os.environ, CODEX_RUN_CONTENTION_DAEMON_EXE=daemon,
           CODEX_RUN_CONTENTION_ITERATIONS=str(args.iterations), CODEX_RUN_CONTENTION_DURATION_MS=str(args.seconds * 1000))
subprocess.run([desktop, "--exact", "shared::runs_core::contention_tests::sustained_desktop_daemon_writers_share_store", "--nocapture"], env=env, cwd=root, check=True)
