#!/usr/bin/env python3
"""Acceptance driver for generic ACP authentication and client callbacks.

Usage: scripts/test-acp-client-services.py --daemon-bin PATH [--evidence-dir PATH]

Each endpoint gets a newly created data directory and workspace. Existing evidence is never
reused. The two endpoint stores deliberately contain distinct bytes at corresponding paths;
this detects callback routing through the wrong workspace/store. This script drives daemon
RPC. Native in-process local runtime acceptance is covered by Rust shared-core tests and the
native desktop run, not inferred from these daemon subprocesses.
"""
from __future__ import annotations

import argparse
import base64
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import subprocess
import sys
import time
import uuid

REPO = Path(__file__).resolve().parents[1]
FIXTURE = REPO / "src-tauri/src/shared/agent_runtime/fixtures/client_callbacks_acp.py"
TOKEN = "acp-client-services-acceptance-token"
LOGIN_READY = "CALLBACK_FIXTURE_LOGIN_READY"
LOGIN_DONE = "CALLBACK_FIXTURE_LOGIN_COMPLETE"


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class RpcError(RuntimeError):
    def __init__(self, method: str, message: str):
        super().__init__(f"{method}: {message}")
        self.method = method
        self.message = message


class Rpc:
    def __init__(self, port: int):
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=10)
        self.sock.settimeout(20)
        self.reader = self.sock.makefile("r", encoding="utf-8")
        self.writer = self.sock.makefile("w", encoding="utf-8")
        self.next_id = 1
        self.notifications: list[dict] = []
        self.call("auth", {"token": TOKEN})

    def call(self, method: str, params: dict) -> dict:
        request_id = self.next_id
        self.next_id += 1
        self.writer.write(json.dumps({"id": request_id, "method": method, "params": params}) + "\n")
        self.writer.flush()
        while True:
            line = self.reader.readline()
            if not line:
                raise RuntimeError(f"daemon disconnected while awaiting {method}")
            message = json.loads(line)
            if message.get("id") != request_id:
                if isinstance(message, dict) and isinstance(message.get("method"), str):
                    self.notifications.append(message)
                continue
            if "error" in message:
                raise RpcError(method, message["error"].get("message", str(message["error"])))
            return message.get("result")

    def runtime(self, method: str, input: dict) -> dict:
        return self.call("runtime_request", {"method": method, "input": input})

    def close(self) -> None:
        self.reader.close()
        self.writer.close()
        self.sock.close()


def launch(binary: Path, data: Path, port: int, log) -> subprocess.Popen[str]:
    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"
    return subprocess.Popen(
        [str(binary), "--listen", f"127.0.0.1:{port}", "--data-dir", str(data), "--token", TOKEN],
        cwd=REPO,
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )


def connect(port: int, process: subprocess.Popen[str]) -> Rpc:
    deadline = time.monotonic() + 20
    last: Exception | None = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"daemon exited with {process.returncode}")
        try:
            return Rpc(port)
        except (OSError, TimeoutError) as exc:
            last = exc
            time.sleep(0.1)
    raise RuntimeError(f"daemon did not become available: {last}")


def stop(process: subprocess.Popen[str]) -> None:
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)


def process_is_running(pid: int) -> bool:
    """Treat a reaped-or-zombie Linux child as gone; no longer executing."""
    proc_stat = Path(f"/proc/{pid}/stat")
    try:
        raw = proc_stat.read_text(encoding="ascii")
    except FileNotFoundError:
        return False
    except OSError:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        return True
    tail = raw.rsplit(")", 1)[-1].strip().split()
    return not tail or tail[0] != "Z"


def wait_process_tree_gone(pid: int, timeout: float = 10) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not process_is_running(pid):
            return
        time.sleep(0.1)
    raise AssertionError(f"terminal child process {pid} remained alive after daemon shutdown")


def prepare_schema3_migration(binary: Path, evidence: Path, data: Path, store: Path, label: str) -> dict:
    """Let the real daemon create a valid store, then make a populated schema-3 fixture."""
    port = free_port()
    process = None
    rpc = None
    with (evidence / "schema-bootstrap.log").open("w", encoding="utf-8") as log:
        try:
            process = launch(binary, data, port, log)
            rpc = connect(port, process)
            rpc.runtime("profiles", {})
            rpc.close()
            rpc = None
            stop(process)
            process = None
        finally:
            if rpc is not None:
                rpc.close()
            if process is not None:
                stop(process)

    run_id = f"legacy-run-{label}"
    instance_id = f"legacy-instance-{label}"
    capture_id = f"legacy-capture-{label}"
    run_payload = '{ "legacyRun" : "byte-for-byte", "number": 1.00 }'
    instance_payload = '{ "legacyInstance" : "byte-for-byte", "number": 2.00 }'
    capture_payload = '{ "legacyCapture" : "byte-for-byte", "number": 3.00 }'
    with closing(sqlite3.connect(store)) as con, con:
        con.execute("PRAGMA foreign_keys=ON")
        con.execute("DROP TABLE IF EXISTS acp_terminal_snapshots")
        con.execute("DROP TABLE IF EXISTS acp_callback_operations")
        con.execute("PRAGMA user_version=3")
        con.execute("INSERT INTO runs(id,request_id,workspace_id,created_at,payload) VALUES(?,?,?,?,?)", (run_id, str(uuid.uuid4()), f"acp-{label}-workspace", 1, run_payload))
        con.execute("INSERT INTO instances(id,run_id,ordinal,payload) VALUES(?,?,?,?)", (instance_id, run_id, 0, instance_payload))
        con.execute("INSERT INTO captures(id,instance_id,request_id,payload) VALUES(?,?,?,?)", (capture_id, instance_id, str(uuid.uuid4()), capture_payload))
        con.commit()
    return {
        "schemaBefore": 3, "runId": run_id, "instanceId": instance_id, "captureId": capture_id,
        "runPayload": run_payload, "instancePayload": instance_payload, "capturePayload": capture_payload,
        "captureSha256": hashlib.sha256(capture_payload.encode()).hexdigest(),
    }


def verify_schema4_migration(store: Path, before: dict) -> dict:
    with closing(sqlite3.connect(store)) as con:
        schema = con.execute("PRAGMA user_version").fetchone()[0]
        assert schema == 4, f"expected schema 4 after daemon startup, got {schema}"
        values = {
            "runPayload": con.execute("SELECT payload FROM runs WHERE id=?", (before["runId"],)).fetchone()[0],
            "instancePayload": con.execute("SELECT payload FROM instances WHERE id=?", (before["instanceId"],)).fetchone()[0],
            "capturePayload": con.execute("SELECT payload FROM captures WHERE id=?", (before["captureId"],)).fetchone()[0],
            "integrity": con.execute("PRAGMA integrity_check").fetchone()[0],
            "foreignKeys": con.execute("PRAGMA foreign_key_check").fetchall(),
        }
    assert values["runPayload"] == before["runPayload"]
    assert values["instancePayload"] == before["instancePayload"]
    assert values["capturePayload"] == before["capturePayload"]
    assert hashlib.sha256(values["capturePayload"].encode()).hexdigest() == before["captureSha256"]
    assert values["integrity"] == "ok" and not values["foreignKeys"], values
    prefix = store.name.replace("runs.sqlite3", "runs.schema3-backup-")
    backups = [path for path in store.parent.iterdir() if path.name.startswith(prefix)]
    assert backups, "schema-3 migration did not leave a SQLite backup"
    with closing(sqlite3.connect(backups[0])) as backup:
        assert backup.execute("PRAGMA user_version").fetchone()[0] == 3
        assert backup.execute("SELECT payload FROM captures WHERE id=?", (before["captureId"],)).fetchone()[0] == before["capturePayload"]
        backup_integrity = backup.execute("PRAGMA integrity_check").fetchone()[0]
    assert backup_integrity == "ok", backup_integrity
    return {"schemaAfter": 4, "payloadsByteExact": True, "captureSha256": before["captureSha256"], "backup": str(backups[0]), "integrity": values["integrity"]}


def seed(endpoint: Path, label: str, sentinel: str) -> tuple[Path, Path, Path, str]:
    endpoint.mkdir(parents=True)
    data = endpoint / "data"
    data.mkdir(parents=True)
    workspace = endpoint / "workspace"
    workspace.mkdir()
    subprocess.run(["git", "init", "--quiet", str(workspace)], check=True)
    (workspace / "sentinel.txt").write_text(sentinel, encoding="utf-8")
    (workspace / "peer-id.txt").write_text(label + "-workspace", encoding="utf-8")
    catalog = endpoint / "unused-catalog.json"
    write_json(catalog, {"sessions": []})
    workspace_id = f"acp-{label}-workspace"
    write_json(data / "settings.json", {"theme": "dark"})
    write_json(data / "workspaces.json", [{
        "id": workspace_id, "name": label, "path": str(workspace), "kind": "main",
        "parentId": None, "worktree": None,
        "settings": {"codexAutoConnect": False, "sidebarCollapsed": False},
    }])
    profile_id = f"client-callback-{label}"
    profile = {
        "id": profile_id, "revision": 1, "name": f"{label} ACP fixture", "kind": "acp",
        "executable": sys.executable,
        "args": [str(FIXTURE), "--agent-label", "Provider-neutral callback fixture", "--replay-mutations"],
        "workingDirectory": str(workspace),
        "environment": {},
        "clientPolicy": {
            "filesystem": "read_write", "commandExecution": "allow", "additionalRoots": [],
        },
    }
    store = data / "runs.sqlite3"
    with closing(sqlite3.connect(store)) as con, con:
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("CREATE TABLE IF NOT EXISTS runtime_profiles(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL)")
        con.execute("INSERT OR REPLACE INTO runtime_profiles(id,revision,payload) VALUES(?,?,?)", (profile_id, 1, json.dumps(profile, separators=(",", ":"))))
    return data, workspace, store, profile_id


def poll_operation(rpc: Rpc, ticket: dict, timeout: float = 30, *, include_awaiting_user: bool = True) -> dict:
    operation_id = ticket["operationId"]
    deadline = time.monotonic() + timeout
    latest: dict = ticket
    while time.monotonic() < deadline:
        latest = rpc.runtime("connection_operation", {
            "operationId": operation_id,
            "connectionId": ticket.get("connectionId"),
            "generation": ticket.get("generation"),
        })
        state = str(latest.get("state", "")).lower()
        terminal_states = {"succeeded", "failed", "cancelled", "interrupted"}
        if include_awaiting_user:
            terminal_states.add("awaiting_user")
        if state in terminal_states:
            return latest
        time.sleep(0.1)
    raise TimeoutError(f"connection operation {operation_id} did not settle: {latest}")


def prepare(rpc: Rpc, profile_id: str, cwd: Path, role: str = "conversation") -> tuple[dict, dict]:
    ticket = rpc.runtime("prepare_connection", {
        "profileId": profile_id, "expectedProfileRevision": 1, "cwd": str(cwd),
        "role": role, "requestId": str(uuid.uuid4()),
    })
    operation = poll_operation(rpc, ticket)
    if str(operation.get("state", "")).lower() not in {"succeeded", "awaiting_user"}:
        raise AssertionError(f"prepare failed: {operation}")
    status = rpc.runtime("connection_status", {
        "connectionId": ticket["connectionId"], "generation": ticket["generation"],
    })
    return ticket, status


def authenticate_agent(rpc: Rpc, connection: dict, method_id: str) -> dict:
    ticket = rpc.runtime("authenticate", {
        "connectionId": connection["connectionId"], "generation": connection["generation"],
        "methodId": method_id, "requestId": str(uuid.uuid4()),
    })
    result = poll_operation(rpc, ticket)
    assert str(result.get("state", "")).lower() == "succeeded", result
    return result


def authenticate_terminal(rpc: Rpc, connection: dict, evidence: Path) -> dict:
    methods = connection["status"].get("authMethods", [])
    method = next(item for item in methods if item.get("id") == "fixture-terminal-auth")
    ticket = rpc.runtime("authenticate", {
        "connectionId": connection["ticket"]["connectionId"],
        "generation": connection["ticket"]["generation"],
        "methodId": method["id"], "requestId": str(uuid.uuid4()),
    })
    operation_id = ticket["operationId"]
    output = ""
    cursor = 0
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and LOGIN_READY not in output:
        chunk = rpc.runtime("read_login_terminal", {
            "operationId": operation_id, "generation": ticket["generation"], "cursor": cursor, "limit": 4096,
        })
        output += str(chunk.get("output", ""))
        cursor = int(chunk.get("nextCursor", cursor))
        time.sleep(0.1)
    assert LOGIN_READY in output, output
    written = rpc.runtime("write_login_terminal", {
        "operationId": operation_id, "generation": ticket["generation"], "inputSequence": 1,
        "data": base64.b64encode(b"ACCEPT_FIXTURE_LOGIN\n").decode("ascii"),
    })
    completed = poll_operation(rpc, ticket, include_awaiting_user=False)
    assert str(completed.get("state", "")).lower() == "succeeded", completed
    final = rpc.runtime("read_login_terminal", {
        "operationId": operation_id, "generation": ticket["generation"], "cursor": cursor, "limit": 4096,
    })
    output += str(final.get("output", ""))
    assert LOGIN_DONE in output, output
    evidence_path = evidence / "login-transcript-redacted.json"
    write_json(evidence_path, {"readyMarkerObserved": LOGIN_READY in output, "completionMarkerObserved": LOGIN_DONE in output, "credentialContentRecorded": False, "writeAck": written})
    return {"state": completed.get("state"), "redactedTranscript": str(evidence_path), "credentialContentRecorded": False}


def create_session(rpc: Rpc, connection: dict, workspace_id: str) -> dict:
    return rpc.runtime("create_session", {
        "profileId": connection["profileId"], "expectedProfileRevision": 1,
        "workspaceId": workspace_id, "connectionId": connection["ticket"]["connectionId"],
        "generation": connection["ticket"]["generation"],
    })


def submit(rpc: Rpc, session_id: str, prompt: str) -> dict:
    return rpc.runtime("submit", {
        "requestId": str(uuid.uuid4()), "sessionId": session_id,
        "prompt": [{"type": "text", "text": prompt}], "options": {}, "runContext": None,
    })


def wait_execution(rpc: Rpc, ticket: dict, timeout: float = 40) -> dict:
    deadline = time.monotonic() + timeout
    last: dict = ticket
    while time.monotonic() < deadline:
        last = rpc.runtime("execution", {"requestId": ticket["requestId"]})
        execution_state = last.get("execution")
        state = str(execution_state.get("state", "") if isinstance(execution_state, dict) else execution_state or last.get("state", "")).lower()
        if state in {"completed", "failed", "cancelled", "interrupted"}:
            return last
        time.sleep(0.1)
    raise TimeoutError(f"execution did not finish: {last}")


def session_events(rpc: Rpc, session_id: str) -> list[dict]:
    result = rpc.runtime("events", {"sessionId": session_id, "after": 0})
    return result if isinstance(result, list) else result.get("events", [])


def event_texts(events: list[dict]) -> list[str]:
    texts: list[str] = []
    for event in events:
        payload = event.get("payload")
        if not isinstance(payload, dict):
            continue
        for candidate in (payload.get("text"), payload.get("content"), payload.get("native")):
            if isinstance(candidate, str):
                texts.append(candidate)
            elif isinstance(candidate, dict):
                if isinstance(candidate.get("text"), str):
                    texts.append(candidate["text"])
                content = candidate.get("content")
                if isinstance(content, dict) and isinstance(content.get("text"), str):
                    texts.append(content["text"])
    return texts


def wait_for_event_text(rpc: Rpc, session_id: str, marker: str, timeout: float = 20) -> list[str]:
    deadline = time.monotonic() + timeout
    latest: list[str] = []
    while time.monotonic() < deadline:
        events = session_events(rpc, session_id)
        latest = event_texts(events)
        if any(marker in text for text in latest):
            return latest
        time.sleep(0.1)
    raise TimeoutError(f"session did not publish {marker}: {latest}")


def load_conversation_snapshot(rpc: Rpc, session_id: str, timeout: float = 20) -> tuple[dict, dict]:
    ticket = rpc.runtime("load_conversation_history", {
        "sessionId": session_id, "requestId": str(uuid.uuid4()),
    })
    deadline = time.monotonic() + timeout
    latest: dict = {}
    while time.monotonic() < deadline:
        latest = rpc.runtime("read_conversation", {"sessionId": session_id, "limit": 1000})
        state = str(latest.get("load", {}).get("state", latest.get("loadState", ""))).lower()
        if state in {"complete", "succeeded", "failed"}:
            break
        time.sleep(0.1)
    state = str(latest.get("load", {}).get("state", latest.get("loadState", ""))).lower()
    assert state in {"complete", "succeeded"}, (ticket, latest)
    return ticket, latest


def fixture_history_updates(workspace: Path, native_session_id: str) -> tuple[Path, bytes, list[dict]]:
    filename = hashlib.sha256(native_session_id.encode("utf-8")).hexdigest() + ".jsonl"
    path = workspace / ".fixture-history" / filename
    raw = path.read_bytes()
    messages = [json.loads(line) for line in raw.decode("utf-8").splitlines() if line]
    assert messages and all(
        message.get("method") == "session/update"
        and message.get("params", {}).get("sessionId") == native_session_id
        for message in messages
    ), messages
    return path, raw, [message["params"]["update"] for message in messages]


def fixture_initializations(workspace: Path) -> list[dict]:
    path = workspace / "client-callback-acp.jsonl"
    if not path.exists():
        return []
    records = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]
    return [record for record in records if record.get("protocolMethod") == "initialize"]


def workspace_files_changed_notifications(
    rpc: Rpc, session_id: str, store_id: str, workspace_id: str,
) -> list[dict]:
    return [
        notification["params"]
        for notification in rpc.notifications
        if notification.get("method") == "runtime-event"
        and isinstance(notification.get("params"), dict)
        and notification["params"].get("kind") == "workspace_files_changed"
        and notification["params"].get("sessionId") == session_id
        and notification["params"].get("storeId") == store_id
        and notification["params"].get("workspaceId") == workspace_id
    ]


def wait_for_workspace_file_signals(
    rpc: Rpc, session_id: str, store_id: str, workspace_id: str, connection_id: str,
    expected_paths: set[str], timeout: float = 8,
) -> list[dict]:
    deadline = time.monotonic() + timeout
    latest: list[dict] = []
    while time.monotonic() < deadline:
        latest = workspace_files_changed_notifications(rpc, session_id, store_id, workspace_id)
        paths = [path for event in latest for path in event.get("payload", {}).get("paths", [])]
        if set(paths) == expected_paths and len(paths) == len(expected_paths):
            return latest
        # The test RPC client reads notifications while servicing a response.
        rpc.runtime("connection_status", {"connectionId": connection_id})
        time.sleep(0.05)
    raise AssertionError(f"workspace change notifications did not match {sorted(expected_paths)}: {latest}")


def history_generation_updates(store: Path, session_id: str, generation: str) -> list[dict]:
    sql = "SELECT payload FROM runtime_events WHERE session_id=? ORDER BY sequence"
    try:
        with closing(sqlite3.connect(store)) as con:
            payloads = [row[0] for row in con.execute(sql, (session_id,))]
    except sqlite3.DatabaseError as exc:
        # Preserve the original error and capture read-only diagnostics. This
        # is deliberately not retried: a malformed-database report must stay
        # visible to acceptance rather than be papered over by a later read.
        diagnostics: dict = {
            "store": str(store),
            "sessionId": session_id,
            "generation": generation,
            "query": sql,
            "sqliteError": repr(exc),
            "sqliteErrorCode": getattr(exc, "sqlite_errorcode", None),
            "sqliteErrorName": getattr(exc, "sqlite_errorname", None),
            "files": {},
        }
        for path in sorted(store.parent.glob(store.name + "*")):
            # Stat metadata only. Reading a database/WAL file through a raw
            # descriptor while another SQLite connection is alive can disturb
            # process-wide POSIX locks; the outer harness captures auth bytes
            # only after stopping the daemon.
            info = path.stat()
            diagnostics["files"][path.name] = {"bytes": info.st_size, "mtimeNs": info.st_mtime_ns}
        raise AssertionError(json.dumps(diagnostics, ensure_ascii=False, default=str)) from exc
    return [
        event["raw"]["params"]["update"]
        for payload in payloads
        if (event := json.loads(payload)).get("source") == "history"
        and event.get("historyLoadId") == generation
        and event.get("raw", {}).get("method") == "session/update"
    ]


def exercise_endpoint(binary: Path, evidence: Path, label: str, sentinel: str) -> dict:
    data, workspace, store, profile_id = seed(evidence, label, sentinel)
    legacy = prepare_schema3_migration(binary, evidence, data, store, label)
    port = free_port()
    log_path = evidence / "daemon.log"
    process = None
    rpc = None
    with log_path.open("w", encoding="utf-8") as log:
        try:
            process = launch(binary, data, port, log)
            rpc = connect(port, process)
            profiles = rpc.runtime("profiles", {})
            profile = next(item for item in profiles["profiles"] if item["id"] == profile_id)
            ticket, status = prepare(rpc, profile_id, workspace)
            status_auth = str(status.get("authState", "")).lower()
            auth_methods = status.get("authMethods", [])
            agent_method = next((item for item in auth_methods if item.get("id") == "fixture-agent-auth"), None)
            terminal_method = next((item for item in auth_methods if item.get("id") == "fixture-terminal-auth"), None)
            assert agent_method and terminal_method, status

            # Exercise agent-mediated auth and an isolated terminal-login operation.
            auth_result = authenticate_agent(rpc, {**ticket, "status": status}, agent_method["id"])
            terminal_auth = authenticate_terminal(rpc, {"ticket": ticket, "status": status}, evidence)
            status_after_auth = rpc.runtime("connection_status", {"connectionId": ticket["connectionId"]})

            # Terminal login replaces the ACP process and increments its connection generation.
            authenticated_ticket = {**ticket, "generation": status_after_auth["generation"]}
            connection = {"ticket": authenticated_ticket, "status": status_after_auth, "profileId": profile_id}
            session = create_session(rpc, connection, f"acp-{label}-workspace")
            session_id = session["sessionId"]
            initialize_before_session_status = fixture_initializations(workspace)
            assert initialize_before_session_status, "the fixture did not record its ACP initialization"
            foreground_status = rpc.runtime("connection_status", {"sessionId": session_id})
            initialize_after_session_status = fixture_initializations(workspace)
            assert foreground_status.get("connectionId") == authenticated_ticket["connectionId"], foreground_status
            assert foreground_status.get("generation") == authenticated_ticket["generation"], foreground_status
            assert initialize_after_session_status == initialize_before_session_status, {
                "before": initialize_before_session_status,
                "after": initialize_after_session_status,
            }
            initial_load = rpc.runtime("load_conversation_history", {"sessionId": session_id, "requestId": str(uuid.uuid4())})
            load_deadline = time.monotonic() + 20
            replay = {}
            while time.monotonic() < load_deadline:
                replay = rpc.runtime("read_conversation", {"sessionId": session_id, "limit": 100})
                state = str(replay.get("load", {}).get("state", replay.get("loadState", ""))).lower()
                if state in {"succeeded", "complete", "failed"}:
                    break
                time.sleep(0.1)
            assert str(replay.get("load", {}).get("state", replay.get("loadState", ""))).lower() in {"succeeded", "complete"}, (initial_load, replay)
            store_id = replay.get("storeId")
            assert isinstance(store_id, str) and store_id, replay
            assert not (workspace / "replay-must-not-write.txt").exists(), "history replay wrote a file"
            execution_ticket = submit(rpc, session_id, "callbacks")
            execution = wait_execution(rpc, execution_ticket)
            workspace_id = f"acp-{label}-workspace"
            workspace_changes = wait_for_workspace_file_signals(
                rpc, session_id, store_id, workspace_id, authenticated_ticket["connectionId"],
                {"created-by-acp.txt", "sentinel.txt"},
            )
            all_session_workspace_changes = [
                notification.get("params")
                for notification in rpc.notifications
                if notification.get("method") == "runtime-event"
                and isinstance(notification.get("params"), dict)
                and notification["params"].get("kind") == "workspace_files_changed"
                and notification["params"].get("sessionId") == session_id
            ]
            assert all_session_workspace_changes == workspace_changes, all_session_workspace_changes
            observed_paths = [
                path for event in workspace_changes
                for path in event.get("payload", {}).get("paths", [])
            ]
            assert len(workspace_changes) == 2 and sorted(observed_paths) == ["created-by-acp.txt", "sentinel.txt"], workspace_changes
            assert all(
                event.get("kind") == "workspace_files_changed"
                and event.get("storeId") == store_id
                and event.get("sessionId") == session_id
                and event.get("workspaceId") == workspace_id
                and event.get("payload") == {"paths": [path]}
                for event in workspace_changes
                for path in event.get("payload", {}).get("paths", [])
            ), workspace_changes
            notification_text = json.dumps(rpc.notifications, ensure_ascii=False)
            for forbidden in ("AUTH_RESULT_SHOULD_NOT_ESCAPE", "ACCEPT_FIXTURE_LOGIN"):
                assert forbidden not in notification_text, f"credential marker appeared in RPC notification stream: {forbidden}"
            events = session_events(rpc, session_id)
            result_text = json.dumps({"execution": execution, "events": events}, ensure_ascii=False)
            assert "CALLBACK_SCENARIO_OK" in result_text, result_text
            assert sentinel in result_text, f"callback did not return local sentinel {sentinel!r}: {result_text}"
            assert (workspace / "created-by-acp.txt").read_text(encoding="utf-8") == "created by callback fixture\n"
            assert (workspace / "sentinel.txt").read_text(encoding="utf-8") == "edited by callback fixture\n"
            assert "CALLBACK_TERMINAL_OUTPUT" in result_text, result_text
            callback_text = next((text for text in event_texts(events) if text.startswith("CALLBACK_SCENARIO_OK ")), None)
            assert callback_text is not None, result_text
            callback_result = json.loads(callback_text.removeprefix("CALLBACK_SCENARIO_OK "))
            terminal_id = callback_result["terminalId"]
            retained_terminal = rpc.runtime("read_acp_terminal", {"sessionId": session_id, "terminalId": terminal_id, "cursor": 0, "limit": 4096})
            assert "CALLBACK_TERMINAL_OUTPUT" in json.dumps(retained_terminal, ensure_ascii=False), retained_terminal

            # Keep one callback-owned terminal alive while the host cancels its prompt.
            cancel_ticket = submit(rpc, session_id, "terminal-cancel")
            cancel_events = wait_for_event_text(rpc, session_id, "CANCEL_TERMINAL_READY")
            cancel_terminal_match = next((re.search(r"CANCEL_TERMINAL_READY\s+([A-Za-z0-9_-]+)", text) for text in cancel_events if "CANCEL_TERMINAL_READY" in text), None)
            assert cancel_terminal_match, cancel_events
            cancel_terminal_id = cancel_terminal_match.group(1)
            cancel_result = rpc.runtime("cancel", {"sessionId": session_id})
            cancelled_terminal_execution = wait_execution(rpc, cancel_ticket)
            final_cancel_terminal = rpc.runtime("read_acp_terminal", {
                "sessionId": session_id, "terminalId": cancel_terminal_id, "cursor": 0, "limit": 4096,
            })
            final_cancel_terminal_text = json.dumps(final_cancel_terminal, ensure_ascii=False)
            assert "FINAL_CANCEL_TERMINAL_OUTPUT" in final_cancel_terminal_text, final_cancel_terminal
            assert final_cancel_terminal.get("released") or final_cancel_terminal.get("exitCode") is not None or final_cancel_terminal.get("signal") is not None, final_cancel_terminal

            after_cancel_ticket = submit(rpc, session_id, "terminal-after-cancel")
            after_cancel_execution = wait_execution(rpc, after_cancel_ticket)
            after_cancel_events = session_events(rpc, session_id)
            after_cancel_text = next((text for text in event_texts(after_cancel_events) if text.startswith("AFTER_CANCEL_TERMINAL_OK ")), None)
            assert after_cancel_text is not None, json.dumps({"execution": after_cancel_execution, "events": after_cancel_events}, ensure_ascii=False)
            after_cancel_terminal = json.loads(after_cancel_text.removeprefix("AFTER_CANCEL_TERMINAL_OK "))
            assert "AFTER_CANCEL_TERMINAL_OUTPUT" in json.dumps(after_cancel_terminal, ensure_ascii=False), after_cancel_terminal

            # The callback store keys numeric and string JSON-RPC ids distinctly.
            with closing(sqlite3.connect(store)) as con:
                callback_ids = con.execute(
                    "SELECT request_id_type,request_id_json,method FROM acp_callback_operations"
                ).fetchall()
            typed_ids = {(row[0], row[1], row[2]) for row in callback_ids}
            assert ("number", "103", "fs/write_text_file") in typed_ids, callback_ids
            assert ("string", '"fs-write-1"', "fs/write_text_file") in typed_ids, callback_ids
            fixture_log = workspace / "client-callback-acp.jsonl"
            fixture_messages = [json.loads(line) for line in fixture_log.read_text(encoding="utf-8").splitlines() if line]
            wire_ids = {
                (type(message["fixtureCallbackRequest"].get("id")),
                 message["fixtureCallbackRequest"].get("id"),
                 message["fixtureCallbackRequest"].get("method"))
                for message in fixture_messages if isinstance(message.get("fixtureCallbackRequest"), dict)
            }
            assert (int, 101, "fs/read_text_file") in wire_ids, wire_ids
            assert (str, "term-create-1", "terminal/create") in wire_ids, wire_ids

            elicitation_ticket = submit(rpc, session_id, "elicitation")
            deadline = time.monotonic() + 10
            pending_elicitation = None
            while time.monotonic() < deadline:
                state = rpc.runtime("connection_status", {"connectionId": authenticated_ticket["connectionId"]})
                pending_elicitation = next((item for item in state.get("pendingClientRequests", []) if item.get("method") == "elicitation/create"), None)
                if pending_elicitation:
                    break
                time.sleep(0.1)
            assert pending_elicitation is not None, "URL elicitation was not exposed as a pending user decision"
            assert pending_elicitation.get("url") == "https://example.invalid/device", pending_elicitation
            elicitation_decision = rpc.runtime("respond_client_request", {
                "connectionId": authenticated_ticket["connectionId"], "generation": authenticated_ticket["generation"],
                "clientRequestId": pending_elicitation["clientRequestId"], "decision": {"action": "accept"},
            })
            elicitation_execution = wait_execution(rpc, elicitation_ticket)

            gate_ticket = submit(rpc, session_id, "gate permission then wait for cancellation")
            deadline = time.monotonic() + 10
            paused = {}
            permission_event = None
            while time.monotonic() < deadline:
                paused = rpc.runtime("execution", {"requestId": gate_ticket["requestId"]})
                events_now = session_events(rpc, session_id)
                permission_event = next((event for event in events_now if event.get("kind") == "permission"), None)
                if permission_event:
                    break
                time.sleep(0.1)
            paused_state = paused.get("execution")
            paused_state = paused_state.get("state", "") if isinstance(paused_state, dict) else paused_state or paused.get("state", "")
            assert permission_event is not None and str(paused_state).lower() not in {"completed", "failed"}, (paused, permission_event)
            rpc.runtime("permission", {
                "sessionId": session_id, "requestId": permission_event.get("payload", {}).get("requestId"), "optionId": "allow",
            })
            cancel = rpc.runtime("cancel", {"sessionId": session_id})
            gate_execution = wait_execution(rpc, gate_ticket)
            assert "cancel" in json.dumps(gate_execution, ensure_ascii=False).lower() or cancel, (cancel, gate_execution)

            event_cursor = max((int(event.get("sequence", 0)) for event in session_events(rpc, session_id)), default=0)
            denied_ticket = submit(rpc, session_id, "permission denial probe")
            deadline = time.monotonic() + 10
            denied_permission = None
            while time.monotonic() < deadline:
                denied_permission = next((event for event in session_events(rpc, session_id) if event.get("kind") == "permission" and int(event.get("sequence", 0)) > event_cursor), None)
                if denied_permission:
                    break
                time.sleep(0.1)
            assert denied_permission is not None, "denial probe did not produce a permission request"
            rpc.runtime("permission", {
                "sessionId": session_id, "requestId": denied_permission.get("payload", {}).get("requestId"), "optionId": "deny",
            })
            denied_execution = wait_execution(rpc, denied_ticket)

            # A daemon SIGTERM must reap an owned terminal process tree and
            # commit its final output even when the ACP agent never polls it.
            # Cancellation closes the active terminal generation. Use a fresh
            # connection/session so the daemon-shutdown case independently
            # exercises teardown while an owned terminal is still active.
            shutdown_prepare, shutdown_status = prepare(rpc, profile_id, workspace)
            if str(shutdown_status.get("authState", "")).lower() == "required":
                shutdown_terminal_auth = authenticate_terminal(
                    rpc, {"ticket": shutdown_prepare, "status": shutdown_status}, evidence
                )
                shutdown_status = rpc.runtime("connection_status", {"connectionId": shutdown_prepare["connectionId"]})
                shutdown_prepare = {**shutdown_prepare, "generation": shutdown_status["generation"]}
            shutdown_connection = {"ticket": shutdown_prepare, "status": shutdown_status, "profileId": profile_id}
            shutdown_session = create_session(rpc, shutdown_connection, f"acp-{label}-workspace")
            shutdown_session_id = shutdown_session["sessionId"]
            shutdown_ticket = submit(rpc, shutdown_session_id, "terminal-daemon-shutdown")
            shutdown_events = wait_for_event_text(rpc, shutdown_session_id, "DAEMON_TERMINAL_READY")
            shutdown_match = next((re.search(r"DAEMON_TERMINAL_READY\s+([A-Za-z0-9_-]+)\s+(\d+)", text) for text in shutdown_events if "DAEMON_TERMINAL_READY" in text), None)
            assert shutdown_match, shutdown_events
            shutdown_terminal_id = shutdown_match.group(1)
            shutdown_child_pid = int(shutdown_match.group(2))
            pid_file = workspace / "daemon-shutdown-child.pid"
            assert int(pid_file.read_text(encoding="ascii").strip()) == shutdown_child_pid
            rpc.close()
            rpc = None
            process.terminate()  # SIGTERM exercises the real daemon signal hook.
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired as exc:
                raise AssertionError("daemon did not finish graceful SIGTERM shutdown") from exc
            assert process.returncode == 0, f"daemon did not exit cleanly after SIGTERM: {process.returncode}"
            process = None
            wait_process_tree_gone(shutdown_child_pid)
            with closing(sqlite3.connect(store)) as con:
                row = con.execute(
                    "SELECT payload FROM acp_terminal_snapshots WHERE session_id=? AND terminal_id=? ORDER BY generation DESC LIMIT 1",
                    (shutdown_session_id, shutdown_terminal_id),
                ).fetchone()
            assert row is not None, "daemon shutdown did not persist the unpolled terminal snapshot"
            shutdown_snapshot = json.loads(row[0])
            assert "DAEMON_SHUTDOWN_TERMINAL_OUTPUT" in json.dumps(shutdown_snapshot, ensure_ascii=False), shutdown_snapshot
            assert shutdown_snapshot.get("released") or shutdown_snapshot.get("exitCode") is not None or shutdown_snapshot.get("signal") is not None, shutdown_snapshot

            # Ensure evidence and comparison/runtime files never contain the login control line.
            login_marker_hits = []
            for path in data.rglob("*"):
                if path.is_file() and path.name not in {"-wal", "-shm"}:
                    try:
                        if b"ACCEPT_FIXTURE_LOGIN" in path.read_bytes():
                            login_marker_hits.append(str(path.relative_to(data)))
                    except OSError:
                        pass
            try:
                if b"ACCEPT_FIXTURE_LOGIN" in log_path.read_bytes():
                    login_marker_hits.append(str(log_path.relative_to(evidence)))
            except OSError:
                pass
            assert not login_marker_hits, f"authentication input leaked into durable data: {login_marker_hits}"

            receipt = {
                "endpoint": label, "profileId": profile_id, "agentLabel": profile.get("name"), "storeId": store_id,
                "authStateBefore": status_auth, "advertisedAuthMethods": [m["id"] for m in auth_methods],
                "agentAuthentication": auth_result, "terminalAuthentication": terminal_auth,
                "connectionAfterAuth": status_after_auth, "session": session, "callbackExecution": execution,
                "foregroundSessionStatus": foreground_status,
                "sessionStatusDidNotInitializeACP": True,
                "workspaceFileChanges": workspace_changes,
                "permissionAndCancel": {"approval": "allow", "pausedBeforeDecision": True, "cancel": cancel, "cancelledExecution": gate_execution, "denial": "deny", "deniedExecution": denied_execution},
                "urlElicitation": {"destination": pending_elicitation["url"], "decision": elicitation_decision, "execution": elicitation_execution},
                "filesystem": {"sentinel": sentinel, "created": str(workspace / "created-by-acp.txt"), "edited": True, "peerWorkspaceId": f"acp-{label}-workspace"},
                "callbackTerminalOutputObserved": "CALLBACK_TERMINAL_OUTPUT" in result_text,
                "retainedTerminal": retained_terminal,
                "typedCallbackIds": {"numericWireReadId": "101", "stringWireTerminalCreateId": "term-create-1",
                                     "numericWriteId": "103", "stringWriteId": "fs-write-1", "distinctInStore": True},
                "cancelledTerminal": {"terminalId": cancel_terminal_id, "cancel": cancel_result,
                                      "execution": cancelled_terminal_execution, "snapshot": final_cancel_terminal},
                "terminalAfterCancel": {"execution": after_cancel_execution, "terminal": after_cancel_terminal,
                                        "sameConnectionAndSession": True},
                "daemonShutdownTerminal": {"sessionId": shutdown_session_id, "terminalId": shutdown_terminal_id, "childPid": shutdown_child_pid,
                                           "childExited": True, "signalPath": "SIGTERM", "snapshot": shutdown_snapshot},
                "replayMutationProbe": {"load": initial_load, "snapshot": replay, "writeApplied": (workspace / "replay-must-not-write.txt").exists()},
                "loginInputPersisted": False, "sqliteIntegrity": None,
            }
            # Restart and verify durable callback effects plus absence of login content.
            process = launch(binary, data, port, log)
            rpc = connect(port, process)
            sessions_restart = rpc.runtime("sessions", {"workspaceId": f"acp-{label}-workspace"})
            assert any(row.get("sessionId") == session_id for row in sessions_restart), sessions_restart
            restart_events = session_events(rpc, session_id)
            retained_terminal_after_restart = rpc.runtime("read_acp_terminal", {"sessionId": session_id, "terminalId": terminal_id, "cursor": 0, "limit": 4096})
            assert "CALLBACK_TERMINAL_OUTPUT" in json.dumps(retained_terminal_after_restart, ensure_ascii=False), retained_terminal_after_restart
            cancelled_terminal_after_restart = rpc.runtime("read_acp_terminal", {
                "sessionId": session_id, "terminalId": cancel_terminal_id, "cursor": 0, "limit": 4096,
            })
            assert "FINAL_CANCEL_TERMINAL_OUTPUT" in json.dumps(cancelled_terminal_after_restart, ensure_ascii=False), cancelled_terminal_after_restart
            assert cancelled_terminal_after_restart.get("released") or cancelled_terminal_after_restart.get("exitCode") is not None or cancelled_terminal_after_restart.get("signal") is not None, cancelled_terminal_after_restart
            shutdown_terminal_after_restart = rpc.runtime("read_acp_terminal", {
                "sessionId": shutdown_session_id, "terminalId": shutdown_terminal_id, "cursor": 0, "limit": 4096,
            })
            shutdown_after_restart_text = json.dumps(shutdown_terminal_after_restart, ensure_ascii=False)
            assert "DAEMON_SHUTDOWN_TERMINAL_OUTPUT" in shutdown_after_restart_text, shutdown_terminal_after_restart
            assert shutdown_terminal_after_restart.get("released") or shutdown_terminal_after_restart.get("exitCode") is not None or shutdown_terminal_after_restart.get("signal") is not None, shutdown_terminal_after_restart

            # Re-open the ordinary conversation through session/load twice.
            # The fixture journal is keyed by native session id and contains
            # only session/update messages; replay must preserve their exact
            # order without appending a second copy to the fixture journal.
            native_session_id = session["nativeSessionId"]
            fixture_journal, fixture_journal_before, expected_updates = fixture_history_updates(
                workspace, native_session_id
            )
            ordinary_texts = [
                update["content"]["text"]
                for update in expected_updates
                if isinstance(update.get("content"), dict)
                and isinstance(update["content"].get("text"), str)
            ]
            assert any(text == "callbacks" for text in ordinary_texts), ordinary_texts
            assert any(text.startswith("CALLBACK_SCENARIO_OK ") for text in ordinary_texts), ordinary_texts
            assert any(text == "terminal-after-cancel" for text in ordinary_texts), ordinary_texts
            assert any(text.startswith("AFTER_CANCEL_TERMINAL_OK ") for text in ordinary_texts), ordinary_texts
            tool_calls = [item for item in expected_updates if item.get("sessionUpdate") == "tool_call"]
            tool_call_updates = [item for item in expected_updates if item.get("sessionUpdate") == "tool_call_update"]
            assert len(tool_calls) == len(tool_call_updates) == 1, {"toolCalls": tool_calls, "updates": tool_call_updates}
            terminal_tool_call = tool_calls[0]
            terminal_tool_call_id = terminal_tool_call["toolCallId"]
            terminal_content = terminal_tool_call.get("content")
            assert terminal_content == [{"type": "terminal", "terminalId": callback_result["terminalId"]}], terminal_tool_call
            assert tool_call_updates[0].get("toolCallId") == terminal_tool_call_id and tool_call_updates[0].get("status") == "completed", tool_call_updates
            assert "content" not in tool_call_updates[0], tool_call_updates[0]
            first_history_ticket, first_history_snapshot = load_conversation_snapshot(rpc, session_id)
            assert first_history_snapshot.get("items"), first_history_snapshot
            first_items_json = json.dumps(first_history_snapshot["items"], ensure_ascii=False)
            assert "CALLBACK_SCENARIO_OK" in first_items_json and sentinel in first_items_json and terminal_tool_call_id in first_items_json and callback_result["terminalId"] in first_items_json, first_items_json
            first_tool_items = [item for item in first_history_snapshot["items"] if item.get("id") == terminal_tool_call_id]
            assert len(first_tool_items) == 1 and first_tool_items[0].get("kind") == "tool", first_tool_items
            assert first_tool_items[0].get("status") == "completed" and first_tool_items[0].get("content") == terminal_content, first_tool_items
            first_generation = str(first_history_snapshot["generation"])
            assert history_generation_updates(store, session_id, first_generation) == expected_updates

            second_history_ticket, second_history_snapshot = load_conversation_snapshot(rpc, session_id)
            assert second_history_snapshot.get("items"), second_history_snapshot
            second_items_json = json.dumps(second_history_snapshot["items"], ensure_ascii=False)
            assert "CALLBACK_SCENARIO_OK" in second_items_json and sentinel in second_items_json and terminal_tool_call_id in second_items_json and callback_result["terminalId"] in second_items_json, second_items_json
            second_tool_items = [item for item in second_history_snapshot["items"] if item.get("id") == terminal_tool_call_id]
            assert len(second_tool_items) == 1 and second_tool_items[0].get("kind") == "tool", second_tool_items
            assert second_tool_items[0].get("status") == "completed" and second_tool_items[0].get("content") == terminal_content, second_tool_items
            second_generation = str(second_history_snapshot["generation"])
            assert history_generation_updates(store, session_id, second_generation) == expected_updates
            assert fixture_journal.read_bytes() == fixture_journal_before, "repeated session/load appended duplicate fixture history"
            # Both snapshots above came through the restarted daemon. Stop it
            # before inspecting the raw event journal so this read cannot race
            # a live SQLite writer.
            rpc.close()
            rpc = None
            stop(process)
            process = None
            with closing(sqlite3.connect(store)) as con:
                integrity = con.execute("PRAGMA integrity_check").fetchone()[0]
                assert integrity == "ok", integrity
                foreign_keys = con.execute("PRAGMA foreign_key_check").fetchall()
                assert not foreign_keys, foreign_keys
                payloads = [row[0] for row in con.execute("SELECT payload FROM runtime_events")]
            assert not any("ACCEPT_FIXTURE_LOGIN" in payload for payload in payloads)
            migration = verify_schema4_migration(store, legacy)
            receipt["restart"] = {"sessions": sessions_restart, "filesystemCreated": (workspace / "created-by-acp.txt").exists(), "filesystemContent": (workspace / "sentinel.txt").read_text(encoding="utf-8"), "eventRows": len(payloads), "runtimeEvents": restart_events, "retainedTerminal": retained_terminal_after_restart, "cancelledTerminal": cancelled_terminal_after_restart, "daemonShutdownTerminal": shutdown_terminal_after_restart,
                                  "historyReplay": {"fixtureJournal": str(fixture_journal), "updateCount": len(expected_updates),
                                                    "firstTicket": first_history_ticket, "firstGeneration": first_generation,
                                                    "secondTicket": second_history_ticket, "secondGeneration": second_generation,
                                                    "exactUpdatesPreserved": True, "repeatLoadDidNotAppend": True}}
            receipt["schemaMigration"] = migration
            receipt["sqliteIntegrity"] = integrity
            return receipt
        finally:
            if rpc is not None:
                rpc.close()
            if process is not None:
                stop(process)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon-bin", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path)
    args = parser.parse_args()
    daemon = args.daemon_bin.resolve(strict=True)
    evidence = (args.evidence_dir or (REPO / ".agent" / f"acp-client-services-{time.strftime('%Y%m%d-%H%M%S')}-{uuid.uuid4().hex[:8]}")).resolve()
    if evidence.exists():
        raise SystemExit(f"refusing to reuse existing evidence directory: {evidence}")
    evidence.mkdir(parents=True)
    receipt: dict = {"daemonBinary": str(daemon), "evidenceDir": str(evidence), "acceptance": "running"}
    try:
        endpoints = [
            exercise_endpoint(daemon, evidence / "endpoint-local", "local", "LOCAL_ENDPOINT_SENTINEL"),
            exercise_endpoint(daemon, evidence / "endpoint-daemon", "daemon", "DAEMON_ENDPOINT_SENTINEL"),
        ]
        assert "LOCAL_ENDPOINT_SENTINEL" not in json.dumps(endpoints[1], ensure_ascii=False)
        assert "DAEMON_ENDPOINT_SENTINEL" not in json.dumps(endpoints[0], ensure_ascii=False)
        assert len({endpoint["storeId"] for endpoint in endpoints}) == len(endpoints), endpoints
        for endpoint in endpoints:
            assert all(event.get("storeId") == endpoint["storeId"] for event in endpoint["workspaceFileChanges"]), endpoint
        receipt.update({"acceptance": "passed", "endpoints": endpoints, "scopeNote": "Two independently launched daemon processes and stores on this host; native in-process local and physically remote-host acceptance are separate checks."})
        write_json(evidence / "receipt.json", receipt)
        print(json.dumps({"status": "passed", "evidenceDir": str(evidence), "endpoints": [r["endpoint"] for r in endpoints]}, indent=2))
        return 0
    except Exception as exc:
        receipt.update({"acceptance": "failed", "error": repr(exc)})
        write_json(evidence / "receipt.json", receipt)
        raise


if __name__ == "__main__":
    raise SystemExit(main())
