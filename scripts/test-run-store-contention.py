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
parser.add_argument("--daemon", type=Path, help="use an already-built host-independent daemon test executable")
parser.add_argument("--desktop", type=Path, help="use an already-built desktop library test executable; with --daemon this skips Cargo entirely")
args = parser.parse_args()
if args.iterations < 1 or args.seconds < 1:
    parser.error("iterations and seconds must be positive")
root = Path(__file__).resolve().parents[1]
daemon = args.daemon.resolve(strict=True) if args.daemon else None
desktop = args.desktop.resolve(strict=True) if args.desktop else None
if desktop is None or daemon is None:
    targets = []
    if desktop is None:
        targets.append("--lib")
    if daemon is None:
        targets.extend(["--bin", "codex_monitor_daemon"])
    command = ["cargo", "test", "--locked", "--no-default-features", "--no-run", "--message-format=json", *targets]
    result = subprocess.run(command, cwd=root / "src-tauri", stdout=subprocess.PIPE, text=True, check=True)
    executables = {}
    for line in result.stdout.splitlines():
        try:
            artifact = json.loads(line)
        except json.JSONDecodeError:
            continue
        if artifact.get("reason") == "compiler-artifact" and artifact.get("profile", {}).get("test") and artifact.get("executable"):
            executables[artifact["target"]["name"]] = artifact["executable"]
    if daemon is None:
        daemon = Path(executables.pop("codex_monitor_daemon")).resolve(strict=True)
    if desktop is None and len(executables) != 1:
        raise RuntimeError(f"Expected exactly one desktop library test executable: {executables}")
    if desktop is None:
        desktop = Path(next(iter(executables.values()))).resolve(strict=True)
if daemon is None:
    raise RuntimeError("daemon test executable was not selected")
print(json.dumps({"desktop": str(desktop), "daemon": str(daemon), "iterations": args.iterations, "seconds": args.seconds}), flush=True)
env = dict(os.environ, CODEX_RUN_CONTENTION_DAEMON_EXE=str(daemon),
           CODEX_RUN_CONTENTION_ITERATIONS=str(args.iterations), CODEX_RUN_CONTENTION_DURATION_MS=str(args.seconds * 1000))
subprocess.run([desktop, "--exact", "shared::runs_core::contention_tests::sustained_desktop_daemon_writers_share_store", "--nocapture"], env=env, cwd=root, check=True)
