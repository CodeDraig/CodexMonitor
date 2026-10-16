#!/usr/bin/env python3
"""Deterministic subprocess fixture for Rust recorder acceptance tests."""
import json
from pathlib import Path
import sys
import time

if "app-server" not in sys.argv:
    print("codex-cli 0.162.1")
    sys.exit(0)

def emit(value):
    print(json.dumps(value), flush=True)

def event(method, params):
    emit({"method": method, "params": {"threadId": "thread-1", **params}})

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method, params = request["method"], request.get("params") or {}
    result = {}
    if method == "initialize":
        result = {"userAgent": "fixture"}
    elif method == "thread/start":
        result = {"thread": {"id": "thread-1"}}
    elif method == "turn/start":
        with open("turn-count", "a") as count:
            count.write("turn\n")
        mode = params.get("model")
        if mode == "receipt-lock":
            Path("response-ready").touch()
            while not Path("release-response").exists():
                time.sleep(0.01)
        if mode == "rejected":
            emit({"id": request["id"], "error": {"message": "rejected by fixture"}})
            continue
        if mode == "lost":
            continue
        if mode == "malformed":
            emit({"id": request["id"], "result": {}})
            continue
        turn = "t2" if mode == "followup" else "t1"
        event("turn/started", {"turn": {"id": turn, "status": "inProgress"}})
        event("item/completed", {"turnId": turn, "item": {"id": "output", "type": "agentMessage", "text": "first result"}})
        event("thread/tokenUsage/updated", {"turnId": turn, "tokenUsage": {"last": {"totalTokens": 12}, "total": {"totalTokens": 999}}})
        event("turn/completed", {"turn": {"id": turn, "status": "completed"}})
        result = {"turn": {"id": turn}}
    elif method == "thread/turns/list":
        if params.get("cursor") and Path("fail-history").exists():
            emit({"id": request["id"], "error": {"message": "required history page failed"}})
            continue
        if params.get("cursor"):
            result = {"data": [{"id": "t2", "status": "completed", "items": [{"id": "output2", "type": "agentMessage", "text": "later result"}]}], "nextCursor": None}
        else:
            result = {"data": [{"id": "t1", "status": "inProgress" if Path("running").exists() else "completed", "items": [{"id": "output", "type": "agentMessage", "text": "first result"}]}], "nextCursor": "page2"}
    emit({"id": request["id"], "result": result})
