#!/usr/bin/env python3
"""Deterministic, provider-neutral ACP v1 fixture for client callback acceptance.

The same executable can be renamed in a profile without changing behavior. It exercises
bidirectional JSON-RPC while prompts are outstanding, including both numeric and string
request ids. `--auth-terminal` is a separate login invocation: it emits a harmless marker,
waits for an explicit line of input, then exits. It never accepts or writes credentials.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import uuid


parser = argparse.ArgumentParser()
parser.add_argument("--auth-terminal", action="store_true")
parser.add_argument("--never-initialize", action="store_true")
parser.add_argument("--agent-label", default="Callback Fixture")
parser.add_argument("--replay-mutations", action="store_true")
parser.add_argument("--log", type=Path)
parser.add_argument("--client-timeout-seconds", type=float, default=20.0)
args = parser.parse_args()
if not 0 < args.client_timeout_seconds <= 600:
    parser.error("--client-timeout-seconds must be greater than 0 and at most 600")

if args.auth_terminal:
    print("CALLBACK_FIXTURE_LOGIN_READY", flush=True)
    if os.environ.get("FIXTURE_TERMINAL") == "provided":
        print("CALLBACK_FIXTURE_TERMINAL_ENV_OK", flush=True)
    answer = sys.stdin.readline().strip()
    if answer == "ACCEPT_FIXTURE_LOGIN":
        print("CALLBACK_FIXTURE_LOGIN_COMPLETE", flush=True)
        raise SystemExit(0)
    print("CALLBACK_FIXTURE_LOGIN_CANCELLED", flush=True)
    raise SystemExit(3)

if args.never_initialize:
    (Path.cwd() / "fixture.pid").write_text(str(os.getpid()), encoding="ascii")

write_lock = threading.Lock()
pending_lock = threading.Lock()
pending: dict[str, tuple[threading.Event, dict]] = {}
terminals: dict[str, subprocess.Popen[bytes]] = {}
sequence = 0
active_prompt: dict | None = None
cwd = Path.cwd()
log_path = args.log or (cwd / "client-callback-acp.jsonl")
history_root = cwd / ".fixture-history"
history_lock = threading.Lock()


def emit(value: dict) -> None:
    raw = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    with write_lock:
        sys.stdout.write(raw + "\n")
        sys.stdout.flush()


def log(value: dict) -> None:
    try:
        with log_path.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")
    except OSError:
        pass


def reply(request_id: int | str, result: dict) -> None:
    emit({"jsonrpc": "2.0", "id": request_id, "result": result})


def error(request_id: int | str, code: int, message: str) -> None:
    emit({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})


def update(session_id: str, value: dict) -> None:
    message = {"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id, "update": value}}
    # This is the fixture's conversation store: only protocol session/update
    # notifications enter it. Auth, permission, prompt, and client callback
    # messages are never copied into this journal.
    name = hashlib.sha256(session_id.encode("utf-8")).hexdigest() + ".jsonl"
    with history_lock:
        history_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        with (history_root / name).open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n")
            stream.flush()
        emit(message)


def read_history(session_id: str) -> list[dict]:
    name = hashlib.sha256(session_id.encode("utf-8")).hexdigest() + ".jsonl"
    path = history_root / name
    with history_lock:
        if not path.exists():
            return []
        with path.open("r", encoding="utf-8") as stream:
            messages = [json.loads(line) for line in stream if line.strip()]
    # Refuse to replay anything except the exact conversation notification
    # shape; corrupted or foreign fixture data must not become a protocol call.
    for message in messages:
        if message.get("method") != "session/update" or not isinstance(message.get("params"), dict):
            raise ValueError("fixture history contains a non-session/update record")
    return messages


def call_client(method: str, params: dict, request_id: int | str) -> dict:
    key = json.dumps(request_id, separators=(",", ":"))
    event = threading.Event()
    box: dict = {}
    with pending_lock:
        pending[key] = (event, box)
    # Keep only request identity in the fixture trace. Callback parameters can
    # include workspace content or commands and are not needed by this check.
    log({"fixtureCallbackRequest": {"id": request_id, "method": method}})
    emit({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
    if not event.wait(args.client_timeout_seconds):
        with pending_lock:
            pending.pop(key, None)
        raise TimeoutError(f"client did not answer {method} ({key})")
    if "error" in box:
        raise RuntimeError(f"client rejected {method}: {box['error']}")
    return box.get("result", {})


def run_callback_scenario(session_id: str, prompt_id: int | str, text: str) -> None:
    """Issue callbacks concurrently with the still-pending prompt response."""
    global active_prompt
    try:
        seen: dict[str, object] = {}
        if "callbacks" in text:
            # Overlap unlike callback types and preserve distinct JSON-RPC id types.
            concurrent: dict[str, object] = {}
            failures: list[BaseException] = []

            def read_sentinel() -> None:
                try:
                    concurrent["read"] = call_client(
                        "fs/read_text_file", {"sessionId": session_id, "path": str(cwd / "sentinel.txt")}, 101
                    )
                except BaseException as exc:
                    failures.append(exc)

            def start_terminal() -> None:
                try:
                    concurrent["terminal"] = call_client(
                        "terminal/create",
                        {"sessionId": session_id, "command": sys.executable, "args": ["-c", "print('CALLBACK_TERMINAL_OUTPUT')"], "cwd": str(cwd), "env": []},
                        "term-create-1",
                    )
                except BaseException as exc:
                    failures.append(exc)

            readers = [threading.Thread(target=read_sentinel), threading.Thread(target=start_terminal)]
            for reader in readers:
                reader.start()
            for reader in readers:
                reader.join(timeout=args.client_timeout_seconds + 5)
            if any(reader.is_alive() for reader in readers):
                raise TimeoutError("concurrent client callback did not finish")
            if failures:
                raise failures[0]
            seen["read"] = concurrent["read"]
            terminal = concurrent["terminal"]
            seen["write"] = call_client(
                "fs/write_text_file", {"sessionId": session_id, "path": str(cwd / "created-by-acp.txt"), "content": "created by callback fixture\n"}, "fs-write-1"
            )
            seen["edit"] = call_client(
                "fs/write_text_file", {"sessionId": session_id, "path": str(cwd / "sentinel.txt"), "content": "edited by callback fixture\n"}, 103
            )
            terminal_id = terminal["terminalId"]
            seen["terminalId"] = terminal_id
            terminal_tool_call_id = f"callback-terminal-{session_id}"
            update(session_id, {
                "sessionUpdate": "tool_call",
                "toolCallId": terminal_tool_call_id,
                "title": "Read callback terminal output",
                "kind": "execute",
                "status": "in_progress",
                "content": [{"type": "terminal", "terminalId": terminal_id}],
            })
            seen["terminalExit"] = call_client(
                "terminal/wait_for_exit", {"sessionId": session_id, "terminalId": terminal_id}, "term-wait-1"
            )
            seen["terminalOutput"] = call_client(
                "terminal/output", {"sessionId": session_id, "terminalId": terminal_id, "cursor": 0, "limit": 4096}, 105
            )
            seen["terminalReleased"] = call_client(
                "terminal/release", {"sessionId": session_id, "terminalId": terminal_id}, 107
            )
            # The update deliberately omits content: the projection should
            # merge status without discarding the terminal content reference.
            update(session_id, {
                "sessionUpdate": "tool_call_update",
                "toolCallId": terminal_tool_call_id,
                "status": "completed",
            })

            sleeper = call_client(
                "terminal/create",
                {"sessionId": session_id, "command": sys.executable, "args": ["-c", "import time; print('BEFORE_KILL', flush=True); time.sleep(60)"], "cwd": str(cwd), "env": []},
                "term-kill-create",
            )
            kill_id = sleeper["terminalId"]
            call_client("terminal/kill", {"sessionId": session_id, "terminalId": kill_id}, "term-kill-1")
            seen["killedExit"] = call_client(
                "terminal/wait_for_exit", {"sessionId": session_id, "terminalId": kill_id}, 109
            )
            call_client("terminal/release", {"sessionId": session_id, "terminalId": kill_id}, "term-release-1")

        if "terminal-cancel" in text:
            # Keep a callback-owned child alive until the host cancels this prompt. The
            # marker update gives the driver a durable terminal id before teardown.
            terminal = call_client(
                "terminal/create",
                {"sessionId": session_id, "command": sys.executable,
                 "args": ["-c", "import time; print('FINAL_CANCEL_TERMINAL_OUTPUT', flush=True); time.sleep(60)"],
                 "cwd": str(cwd), "env": []},
                "term-cancel-create",
            )
            terminal_id = terminal["terminalId"]
            deadline = time.monotonic() + 10
            output = ""
            while time.monotonic() < deadline and "FINAL_CANCEL_TERMINAL_OUTPUT" not in output:
                snapshot = call_client(
                    "terminal/output",
                    {"sessionId": session_id, "terminalId": terminal_id, "cursor": 0, "limit": 4096},
                    "term-cancel-output",
                )
                output += str(snapshot.get("output", ""))
                if "FINAL_CANCEL_TERMINAL_OUTPUT" not in output:
                    time.sleep(0.05)
            if "FINAL_CANCEL_TERMINAL_OUTPUT" not in output:
                raise TimeoutError("cancel terminal never produced its marker")
            update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {
                "type": "text", "text": "CANCEL_TERMINAL_READY " + terminal_id}})
            deadline = time.monotonic() + 20
            while active_prompt is not None and not active_prompt.get("cancelled") and time.monotonic() < deadline:
                time.sleep(0.03)
            if active_prompt and active_prompt.get("cancelled"):
                reply(prompt_id, {"stopReason": "cancelled"})
                return
            raise TimeoutError("host did not cancel terminal callback prompt")

        if "terminal-after-cancel" in text:
            # A cancelled prompt closes its owned command tree but should leave
            # the connection generation usable for the next prompt.
            terminal = call_client(
                "terminal/create",
                {"sessionId": session_id, "command": sys.executable,
                 "args": ["-c", "print('AFTER_CANCEL_TERMINAL_OUTPUT', flush=True)"],
                 "cwd": str(cwd), "env": []},
                "term-after-cancel-create",
            )
            terminal_id = terminal["terminalId"]
            exit_status = call_client(
                "terminal/wait_for_exit",
                {"sessionId": session_id, "terminalId": terminal_id},
                "term-after-cancel-wait",
            )
            output = call_client(
                "terminal/output",
                {"sessionId": session_id, "terminalId": terminal_id, "cursor": 0, "limit": 4096},
                "term-after-cancel-output",
            )
            call_client(
                "terminal/release",
                {"sessionId": session_id, "terminalId": terminal_id},
                "term-after-cancel-release",
            )
            update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {
                "type": "text", "text": "AFTER_CANCEL_TERMINAL_OK " + json.dumps({
                    "terminalId": terminal_id, "exitStatus": exit_status, "output": output,
                }, ensure_ascii=False)}})
            reply(prompt_id, {"stopReason": "end_turn"})
            return

        if "terminal-daemon-shutdown" in text:
            # Leave an unpolled callback terminal with a nested process alive. The
            # daemon owns the process group and must persist the final snapshot
            # while handling SIGTERM.
            pid_file = cwd / "daemon-shutdown-child.pid"
            child_code = "import time; time.sleep(60)"
            command_code = (
                "import pathlib,subprocess,sys,time; "
                f"child=subprocess.Popen([{sys.executable!r}, '-c', {child_code!r}]); "
                f"pathlib.Path({str(pid_file)!r}).write_text(str(child.pid)); "
                "print('DAEMON_SHUTDOWN_TERMINAL_OUTPUT', flush=True); time.sleep(60)"
            )
            terminal = call_client(
                "terminal/create",
                {"sessionId": session_id, "command": sys.executable,
                 "args": ["-c", command_code], "cwd": str(cwd), "env": []},
                "term-daemon-shutdown-create",
            )
            terminal_id = terminal["terminalId"]
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not pid_file.exists():
                time.sleep(0.02)
            if not pid_file.exists():
                raise TimeoutError("nested terminal child did not start")
            child_pid = pid_file.read_text(encoding="ascii").strip()
            update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {
                "type": "text", "text": f"DAEMON_TERMINAL_READY {terminal_id} {child_pid}"}})
            # Do not call terminal/output, wait_for_exit, kill, or release. The
            # host process is expected to end this prompt during daemon teardown.
            deadline = time.monotonic() + 60
            while active_prompt is not None and not active_prompt.get("cancelled") and time.monotonic() < deadline:
                time.sleep(0.05)
            if active_prompt and active_prompt.get("cancelled"):
                reply(prompt_id, {"stopReason": "cancelled"})
                return
            raise TimeoutError("daemon did not stop the terminal fixture")

        if "elicitation" in text:
            seen["elicitation"] = call_client(
                "elicitation/create",
                {"sessionId": session_id, "mode": "url", "url": "https://example.invalid/device", "elicitationId": "fixture-login", "message": "Open the fixture sign-in page?"},
                "elicitation-url-1",
            )

        if "permission" in text or "gate" in text:
            permission_id = "permission-callback-fixture"
            gate_key = json.dumps(permission_id, separators=(",", ":"))
            event = threading.Event()
            box: dict = {}
            with pending_lock:
                pending[gate_key] = (event, box)
            emit({
                "jsonrpc": "2.0", "id": permission_id, "method": "session/request_permission",
                "params": {"sessionId": session_id, "toolCall": {"toolCallId": "fixture-command", "title": "Fixture command"},
                           "options": [{"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                                       {"optionId": "deny", "name": "Reject", "kind": "reject_once"}]},
            })
            if not event.wait(20):
                raise TimeoutError("permission decision did not arrive")
            seen["permission"] = box

        if "slow" in text or "gate" in text:
            # Cancellation must remain possible while this callback-producing prompt waits.
            deadline = time.monotonic() + 15
            while active_prompt is not None and not active_prompt.get("cancelled") and time.monotonic() < deadline:
                time.sleep(0.03)
            if active_prompt and active_prompt.get("cancelled"):
                reply(prompt_id, {"stopReason": "cancelled"})
                return

        update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "CALLBACK_SCENARIO_OK " + json.dumps(seen, ensure_ascii=False)}})
        reply(prompt_id, {"stopReason": "end_turn"})
    except Exception as exc:  # Keep failure explicit in the conversation and prompt result.
        update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "CALLBACK_SCENARIO_FAILED: " + str(exc)}})
        reply(prompt_id, {"stopReason": "end_turn"})
    finally:
        active_prompt = None


def replay_mutation_probe(session_id: str, load_id: int | str) -> None:
    """Try both mutation classes before session/load's success barrier."""
    outcomes: dict[str, str] = {}
    for name, method, params, request_id in (
        ("write", "fs/write_text_file", {"sessionId": session_id, "path": str(cwd / "replay-must-not-write.txt"), "content": "must not persist"}, 201),
        ("terminal", "terminal/create", {"sessionId": session_id, "command": sys.executable, "args": ["-c", "print('MUST_NOT_RUN')"], "cwd": str(cwd), "env": []}, "replay-terminal"),
    ):
        try:
            call_client(method, params, request_id)
            outcomes[name] = "unexpectedly_allowed"
        except Exception as exc:
            outcomes[name] = "rejected:" + str(exc)
    update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "REPLAY_MUTATION_PROBE " + json.dumps(outcomes)}})
    reply(load_id, {"sessionId": session_id, "configOptions": []})


def read_loop() -> None:
    global sequence, active_prompt
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        log(message)
        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params") or {}
        if method is None and request_id is not None:
            key = json.dumps(request_id, separators=(",", ":"))
            with pending_lock:
                waiter = pending.pop(key, None)
            if waiter:
                event, box = waiter
                box.update(message)
                event.set()
            continue
        if method == "initialize":
            # Safe lifecycle evidence for the acceptance runner; this records
            # neither initialize params nor any auth/client request payload.
            log({"protocolMethod": "initialize", "pid": os.getpid()})
            if not args.never_initialize:
                reply(request_id, {
                    "protocolVersion": 1,
                    "agentInfo": {"name": args.agent_label, "version": "1.0-fixture"},
                    "agentCapabilities": {"loadSession": True, "promptCapabilities": {"image": False}},
                    "authMethods": [
                        {"id": "fixture-agent-auth", "name": "Fixture agent authentication", "type": "agent", "args": ["--ignored-agent-args"]},
                        {"id": "fixture-terminal-auth", "name": "Fixture terminal authentication", "type": "terminal", "args": ["--auth-terminal"], "env": {"FIXTURE_TERMINAL": "provided"}},
                    ],
                })
        elif method == "authenticate":
            reply(request_id, {"secret": "AUTH_RESULT_SHOULD_NOT_ESCAPE"})
        elif method in ("session/new", "session/load"):
            session_id = params.get("sessionId") or f"fixture-session-{uuid.uuid4()}"
            if method == "session/load":
                history = read_history(session_id)
                for message in history:
                    # Replay bypasses update() so a load cannot append its own
                    # transcript or multiply the conversation on later loads.
                    emit(message)
            else:
                history = []
            has_mutation_probe = any("REPLAY_MUTATION_PROBE" in json.dumps(message, ensure_ascii=False) for message in history)
            if method == "session/load" and args.replay_mutations and not has_mutation_probe:
                threading.Thread(target=replay_mutation_probe, args=(session_id, request_id), daemon=True).start()
            else:
                reply(request_id, {"sessionId": session_id, "configOptions": []})
        elif method == "session/prompt":
            text = " ".join(part.get("text", "") for part in params.get("prompt", []) if isinstance(part, dict))
            active_prompt = {"id": request_id, "session": params.get("sessionId"), "cancelled": False}
            update(params.get("sessionId", ""), {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": text}})
            threading.Thread(target=run_callback_scenario, args=(params.get("sessionId", ""), request_id, text), daemon=True).start()
        elif method == "session/cancel":
            if active_prompt:
                active_prompt["cancelled"] = True
            if request_id is not None:
                reply(request_id, {})
        elif method == "session/set_config_option":
            reply(request_id, {"configOptions": []})
        elif method == "session/list":
            reply(request_id, {"sessions": []})
        elif request_id is not None:
            error(request_id, -32601, f"Unsupported fixture method: {method}")


read_loop()
