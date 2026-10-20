#!/usr/bin/env python3
"""Exercise the real authenticated daemon contract using isolated, retained evidence.

Usage: scripts/test-backend-client-contracts.py --daemon-bin PATH [--evidence-dir PATH]

The script never reuses an evidence directory. It launches only the supplied daemon binary,
uses a protocol-only ACP fixture and a Codex PATH sentinel, and leaves the resulting stores,
catalog, transcript, and JSON receipt in place for inspection.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import time
import uuid

REPO = Path(__file__).resolve().parents[1]
FIXTURE = REPO / "src-tauri/src/shared/agent_runtime/fixtures/conversations_acp.py"
TOKEN = "backend-client-contract-fixture-token"


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class Rpc:
    def __init__(self, host: str, port: int):
        self.sock = socket.create_connection((host, port), timeout=8)
        self.sock.settimeout(15)
        self.reader = self.sock.makefile("r", encoding="utf-8")
        self.writer = self.sock.makefile("w", encoding="utf-8")
        self.next_id = 1
        self.call("auth", {"token": TOKEN})

    def call(self, method: str, params: dict) -> dict:
        request_id = self.next_id
        self.next_id += 1
        self.writer.write(json.dumps({"id": request_id, "method": method, "params": params}) + "\n")
        self.writer.flush()
        while True:
            line = self.reader.readline()
            if not line:
                raise RuntimeError(f"daemon closed connection while awaiting {method}")
            message = json.loads(line)
            if message.get("id") != request_id:
                continue  # notification/event; this fixture needs no event assertions
            if "error" in message:
                raise RpcError(method, message["error"].get("message", str(message["error"])))
            return message.get("result")

    def close(self) -> None:
        self.reader.close()
        self.writer.close()
        self.sock.close()


class RpcError(RuntimeError):
    def __init__(self, method: str, message: str):
        super().__init__(f"{method}: {message}")
        self.method = method
        self.message = message


def wait_daemon(port: int, process: subprocess.Popen[str]) -> Rpc:
    deadline = time.monotonic() + 15
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"daemon exited early ({process.returncode})")
        try:
            rpc = Rpc("127.0.0.1", port)
            return rpc
        except (OSError, TimeoutError) as error:
            last_error = error
            time.sleep(0.1)
    raise RuntimeError(f"daemon did not accept authenticated RPC: {last_error}")


def launch(binary: Path, data: Path, port: int, env: dict[str, str], log) -> subprocess.Popen[str]:
    process = subprocess.Popen(
        [str(binary), "--listen", f"127.0.0.1:{port}", "--data-dir", str(data), "--token", TOKEN],
        cwd=REPO,
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return process


def stop(process: subprocess.Popen[str]) -> None:
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=8)


def seed_profile(store: Path, workspace: Path, catalog: Path) -> None:
    profile = {
        "id": "contract-generic-acp",
        "revision": 1,
        "name": "Contract ACP",
        "kind": "acp",
        "executable": sys.executable,
        "args": [str(FIXTURE), "--catalog", str(catalog)],
        "workingDirectory": str(workspace),
    }
    con = sqlite3.connect(store)
    try:
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("CREATE TABLE IF NOT EXISTS runtime_profiles(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL)")
        con.execute(
            "INSERT OR REPLACE INTO runtime_profiles(id,revision,payload) VALUES(?,?,?)",
            (profile["id"], profile["revision"], json.dumps(profile, separators=(",", ":"))),
        )
        con.commit()
    finally:
        con.close()


def seed_captures(store: Path, run: dict, evidence: Path) -> tuple[str, str]:
    """Insert two real capture records through the documented schema for comparison RPC tests."""
    instance = run["instances"][0]
    con = sqlite3.connect(store)
    try:
        captures = []
        for i, text in enumerate(("capture one", "capture two"), start=1):
            capture_id = str(uuid.uuid4())
            request_id = str(uuid.uuid4())
            payload = {
                "schemaVersion": 2,
                "id": capture_id,
                "instanceId": instance["id"],
                "requestId": request_id,
                "kind": "manual",
                "turnId": f"fixture-turn-{i}",
                "label": f"fixture capture {i}",
                "createdAt": int(time.time() * 1000),
                "status": "completed",
                "items": [{"type": "text", "text": text}],
                "usage": None,
                "observed": {"fixture": True},
                "git": {},
                "errors": [],
                "complete": True,
            }
            con.execute(
                "INSERT INTO captures(id,instance_id,request_id,payload) VALUES(?,?,?,?)",
                (capture_id, instance["id"], request_id, json.dumps(payload, separators=(",", ":"))),
            )
            captures.append((capture_id, payload))
        raw_instance = con.execute("SELECT payload FROM instances WHERE id=?", (instance["id"],)).fetchone()[0]
        instance_payload = json.loads(raw_instance)
        for capture_id, payload in captures:
            instance_payload.setdefault("captures", []).append({
                "id": capture_id,
                "kind": "manual",
                "turnId": payload["turnId"],
                "label": payload["label"],
                "createdAt": payload["createdAt"],
                "complete": True,
            })
        con.execute("UPDATE instances SET payload=? WHERE id=?", (json.dumps(instance_payload), instance["id"]))
        con.commit()
        write_json(evidence / "seeded-captures.json", [payload for _, payload in captures])
        return captures[0][0], captures[1][0]
    finally:
        con.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon-bin", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path)
    args = parser.parse_args()
    binary = args.daemon_bin.resolve(strict=True)
    evidence = args.evidence_dir or (REPO / ".agent" / f"backend-client-contracts-{time.strftime('%Y%m%d-%H%M%S')}-{uuid.uuid4().hex[:8]}")
    evidence = evidence.resolve()
    if evidence.exists():
        raise SystemExit(f"refusing to reuse existing evidence directory: {evidence}")
    evidence.mkdir(parents=True)
    data = evidence / "data"
    data.mkdir()
    workspace = evidence / "workspace"
    workspace.mkdir()
    # Keep git_snapshot from discovering the repository that contains this fixture and
    # diffing the entire CodexMonitor checkout during the create_run contract call.
    subprocess.run(["git", "init", "--quiet", str(workspace)], check=True)
    binary_asset = bytes((0, 1, 2, 127, 128, 254, 255))
    (workspace / "contract.png").write_bytes(binary_asset)
    catalog_path = evidence / "acp-catalog.json"
    catalog = {"sessions": [{
        "sessionId": "external-contract-session",
        "cwd": str(workspace),
        "title": "external contract session",
        "updates": [
            {"sessionUpdate": "user_message_chunk", "messageId": "user-1", "content": {"type": "text", "text": "external fixture prompt"}},
            {"sessionUpdate": "agent_message_chunk", "messageId": "agent-1", "content": {"type": "text", "text": "external fixture answer"}},
        ],
    }]}
    write_json(catalog_path, catalog)
    write_json(data / "settings.json", {"theme": "dark"})
    write_json(data / "workspaces.json", [{
        "id": "contract-workspace", "name": "contract workspace", "path": str(workspace),
        "kind": "main", "parentId": None, "worktree": None,
        "settings": {"codexAutoConnect": False, "sidebarCollapsed": False},
    }])
    seed_profile(data / "runs.sqlite3", workspace, catalog_path)

    sentinel_dir = evidence / "codex-sentinel"
    sentinel_dir.mkdir()
    sentinel = sentinel_dir / "codex"
    sentinel.write_text("#!/bin/sh\nprintf 'unexpected Codex spawn\\n' >> \"$CODEX_SENTINEL_LOG\"\nexit 97\n", encoding="utf-8")
    sentinel.chmod(0o755)
    env = os.environ.copy()
    env["PATH"] = f"{sentinel_dir}{os.pathsep}{env.get('PATH', '')}"
    env["CODEX_SENTINEL_LOG"] = str(evidence / "codex-spawn.log")
    port = free_port()
    receipt: dict[str, object] = {"daemonBinary": str(binary), "evidenceDir": str(evidence), "port": port}
    log_path = evidence / "daemon.log"
    process: subprocess.Popen[str] | None = None
    rpc: Rpc | None = None
    with log_path.open("w", encoding="utf-8") as log:
        try:
            process = launch(binary, data, port, env, log)
            rpc = wait_daemon(port, process)
            profiles = rpc.call("runtime_request", {"method": "profiles", "input": {}})
            profile = next(p for p in profiles["profiles"] if p["id"] == "contract-generic-acp")
            discovery = rpc.call("runtime_request", {"method": "discover_sessions", "input": {"profileId": profile["id"]}})
            assert discovery["sessions"] and discovery["sessions"][0]["nativeSessionId"] == "external-contract-session", discovery
            catalog_rows = rpc.call("runtime_request", {"method": "catalog", "input": {}})
            assert isinstance(catalog_rows, list), catalog_rows
            row = next(s for s in catalog_rows if s["nativeSessionId"] == "external-contract-session")
            attached = rpc.call("runtime_request", {"method": "attach_session", "input": {
                "catalogId": row["catalogId"], "workspaceAssociation": {"kind": "reuse", "workspaceId": "contract-workspace"},
            }})
            assert attached["workspaceId"] == "contract-workspace", attached
            binary_read = rpc.call("read_workspace_file_binary", {
                "workspaceId": "contract-workspace", "path": "contract.png",
            })
            assert binary_read["mimeType"] == "image/png", binary_read
            assert binary_read["byteLength"] == len(binary_asset), binary_read
            assert base64.b64decode(binary_read["contentBase64"]) == binary_asset, binary_read
            assert not Path(env["CODEX_SENTINEL_LOG"]).exists(), "ACP discovery/import started Codex"

            run = rpc.call("create_run", {"input": {
                "requestId": str(uuid.uuid4()), "workspaceId": "contract-workspace", "title": "contract run",
                "prompt": "durable initial prompt", "mode": "local", "images": [],
                "instances": [{"runtime": {"kind": "acp"}, "model": "fixture"}],
            }})
            left, right = seed_captures(data / "runs.sqlite3", run, evidence)
            comparison = rpc.call("save_run_comparison", {"input": {
                "leftCaptureId": left, "rightCaptureId": right, "choice": "left", "notes": "persisted comparison note", "expectedRevision": 0,
            }})
            assert comparison["revision"] == 1 and comparison["notes"] == "persisted comparison note", comparison
            try:
                rpc.call("save_run_comparison", {"input": {
                    "leftCaptureId": left, "rightCaptureId": right, "choice": "right", "notes": "stale", "expectedRevision": 0,
                }})
            except RpcError as error:
                assert "revision" in error.message.lower() or "changed" in error.message.lower(), error.message
            else:
                raise AssertionError("stale comparison revision was accepted")
            receipt.update({"profile": profile, "discovery": discovery, "attached": attached, "binaryRead": binary_read, "run": run, "comparison": comparison})
            rpc.close()
            rpc = None
            stop(process)
            process = None
            process = launch(binary, data, port, env, log)
            rpc = wait_daemon(port, process)
            persisted = rpc.call("list_run_comparisons", {"input": {"runId": run["id"]}})
            assert any(item["revision"] == 1 and item["notes"] == "persisted comparison note" for item in persisted), persisted
            final_profiles = rpc.call("runtime_request", {"method": "profiles", "input": {}})
            assert any(p["id"] == "contract-generic-acp" for p in final_profiles["profiles"])
            with sqlite3.connect(data / "runs.sqlite3") as con:
                integrity = con.execute("PRAGMA integrity_check").fetchone()[0]
                assert integrity == "ok", integrity
                comparison_revisions = con.execute("SELECT COUNT(*) FROM comparison_revisions").fetchone()[0]
            receipt["restartVerification"] = {
                "comparisons": persisted,
                "profiles": final_profiles["profiles"],
                "sqliteIntegrity": integrity,
                "comparisonRevisionRows": comparison_revisions,
            }
            receipt["codexSpawned"] = False
            write_json(evidence / "receipt.json", receipt)
            print(json.dumps({"status": "passed", "evidenceDir": str(evidence), "comparisonRevision": 1, "codexSpawned": False}, indent=2))
            return 0
        finally:
            if rpc is not None:
                rpc.close()
            if process is not None:
                stop(process)


if __name__ == "__main__":
    raise SystemExit(main())
