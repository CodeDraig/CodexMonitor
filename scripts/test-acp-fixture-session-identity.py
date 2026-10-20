#!/usr/bin/env python3
"""Verify the ACP fixture gives each session/new a durable, unique identity."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile


FIXTURE = (
    Path(__file__).resolve().parents[1]
    / "src-tauri/src/shared/agent_runtime/fixtures/conversations_acp.py"
)


def rpc(process, request_id, method, params, notifications=None):
    process.stdin.write(
        json.dumps(
            {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        )
        + "\n"
    )
    process.stdin.flush()
    while True:
        line = process.stdout.readline()
        if not line:
            raise AssertionError(f"fixture exited while handling {method}")
        message = json.loads(line)
        if message.get("id") == request_id:
            if "error" in message:
                raise AssertionError(f"fixture {method} error: {message['error']}")
            return message["result"]
        if notifications is not None:
            notifications.append(message)


def start_fixture(catalog):
    process = subprocess.Popen(
        [sys.executable, str(FIXTURE), "--catalog", str(catalog)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    rpc(process, 1, "initialize", {"protocolVersion": 1})
    return process


def stop_fixture(process):
    process.terminate()
    process.wait(timeout=5)


def main():
    receipt = {}
    with tempfile.TemporaryDirectory(prefix="codexmonitor-acp-fixture-") as temporary:
        root = Path(temporary)
        cwd = root / "workspace"
        cwd.mkdir()
        catalog = root / "catalog.json"
        external = {
            "sessionId": "external-fixture",
            "cwd": str(cwd),
            "title": "external fixture",
            "updates": [
                {
                    "sessionUpdate": "agent_message_chunk",
                    "messageId": "external-history",
                    "content": {"type": "text", "text": "preserved external history"},
                }
            ],
        }
        catalog.write_text(json.dumps({"sessions": [external]}))
        first_connection = start_fixture(catalog)
        try:
            first = rpc(
                first_connection,
                2,
                "session/new",
                {"cwd": str(cwd), "mcpServers": []},
            )
            first_id = first["sessionId"]
        finally:
            stop_fixture(first_connection)

        # A separate ACP process/connection must not reuse the previous native ID.
        second_connection = start_fixture(catalog)
        try:
            second = rpc(
                second_connection,
                2,
                "session/new",
                {"cwd": str(cwd), "mcpServers": []},
            )
            second_id = second["sessionId"]
            assert first_id != second_id, (first_id, second_id)
            prompt_notifications = []
            rpc(
                second_connection,
                3,
                "session/prompt",
                {
                    "sessionId": second_id,
                    "prompt": [{"type": "text", "text": "identity probe"}],
                },
                prompt_notifications,
            )
            assert len(prompt_notifications) == 2
        finally:
            stop_fixture(second_connection)

        restarted_connection = start_fixture(catalog)
        try:
            listed = []
            page = rpc(
                restarted_connection,
                2,
                "session/list",
                {"cwd": str(cwd)},
            )
            listed.extend(row["sessionId"] for row in page["sessions"])
            while page.get("nextCursor"):
                page = rpc(
                    restarted_connection,
                    3 + len(listed),
                    "session/list",
                    {"cwd": str(cwd), "cursor": page["nextCursor"]},
                )
                listed.extend(row["sessionId"] for row in page["sessions"])
            assert set(listed) == {"external-fixture", first_id, second_id}

            loaded_updates = []
            loaded = rpc(
                restarted_connection,
                10,
                "session/load",
                {"sessionId": second_id, "cwd": str(cwd)},
                loaded_updates,
            )
            assert loaded["sessionId"] == second_id
            assert any(
                event.get("params", {}).get("update", {}).get("content", {}).get("text")
                == "identity probe"
                for event in loaded_updates
            )

            external_updates = []
            rpc(
                restarted_connection,
                11,
                "session/load",
                {"sessionId": "external-fixture", "cwd": str(cwd)},
                external_updates,
            )
            assert any(
                event.get("params", {}).get("update", {}).get("content", {}).get("text")
                == "preserved external history"
                for event in external_updates
            )
            receipt = {
                "passed": True,
                "firstSessionId": first_id,
                "secondSessionId": second_id,
                "listedSessionIds": sorted(listed),
                "promptHistoryReplayedAfterRestart": True,
                "externalStaticHistoryPreserved": True,
            }
        finally:
            stop_fixture(restarted_connection)

    print(json.dumps(receipt, sort_keys=True))


if __name__ == "__main__":
    main()
