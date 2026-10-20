#!/usr/bin/env python3
"""PID-scoped native eframe acceptance driver for an isolated CodexMonitor build.

This script never looks up or launches the installed desktop entry. It operates
only on executables and data paths explicitly passed by the caller (or the
task-owned evidence directory under .agent/).
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import time
import uuid


REPO = Path(__file__).resolve().parents[1]
FIXTURE_DIR = REPO / "src-tauri/src/shared/agent_runtime/fixtures"
TOKEN = "egui-native-acceptance-only"


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as destination:
        destination.write(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def replace_verified_pid_pointer(path: Path, pid: int) -> None:
    """Replace the current-role pointer only after its prior PID was stopped.

    Historical PIDs remain recorded in immutable launch and stop receipts.
    """
    temporary = path.with_name(f".{path.name}.{time.time_ns()}.tmp")
    with temporary.open("x", encoding="ascii") as destination:
        destination.write(f"{pid}\n")
    os.replace(temporary, path)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def binary_identity(path: Path) -> dict[str, int]:
    info = path.stat()
    return {"dev": info.st_dev, "ino": info.st_ino, "size": info.st_size, "mtimeNs": info.st_mtime_ns}


def cached_binary_hash(path: Path, root: Path) -> tuple[str, dict[str, int]] | None:
    identity = binary_identity(path)
    for cache_path in root.glob("phase-*/binary-hash-cache.json"):
        try:
            cache = json.loads(cache_path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if cache.get("identity") == identity and isinstance(cache.get("sha256"), str):
            return cache["sha256"], identity
    return None


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def require_tool(name: str) -> str:
    path = shutil.which(name)
    if not path:
        raise RuntimeError(f"required native acceptance tool is missing: {name}")
    return path


def prepare(args: argparse.Namespace) -> None:
    root: Path = args.evidence_dir
    runtime_root: Path = args.runtime_dir or root
    if root.exists():
        unexpected = [item.name for item in root.iterdir() if not (item.is_file() and item.name.startswith("native-preflight-") and item.name.endswith(".json"))]
        if unexpected:
            raise RuntimeError(f"refusing to overwrite existing evidence in {root}: {', '.join(sorted(unexpected))}")
    root.mkdir(parents=True, exist_ok=True)
    if runtime_root != root and runtime_root.exists():
        raise RuntimeError(f"refusing to reuse existing runtime directory {runtime_root}")
    runtime_root.mkdir(parents=True, exist_ok=True)
    workspace = runtime_root / "workspace"
    workspace.mkdir()
    (workspace / "README.md").write_text("# egui acceptance workspace\n\nTask-owned scratch repository.\n")
    base_diff = "".join(f"BASE_{index:05d} before native diff inspection\n" for index in range(50_000))
    (workspace / "bulk-diff.txt").write_text(base_diff)
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    subprocess.run(["git", "-C", str(workspace), "add", "README.md", "bulk-diff.txt"], check=True)
    subprocess.run(["git", "-C", str(workspace), "-c", "user.name=Native acceptance", "-c", "user.email=acceptance@localhost", "commit", "-qm", "Acceptance fixture"], check=True)
    changed_diff = "".join(f"TARGET_{index:05d} after native diff inspection\n" for index in range(50_000))
    (workspace / "bulk-diff.txt").write_text(changed_diff)

    local_data = runtime_root / "local-data"
    remote_data = runtime_root / "remote-data"
    daemon_data = local_data  # local desktop and daemon intentionally share this isolated store
    for directory in (local_data, remote_data, daemon_data):
        directory.mkdir(parents=True, exist_ok=True)
    port = args.port or free_port()
    workspace_entry = [{
        "id": "egui-acceptance-workspace",
        "name": "egui acceptance",
        "path": str(workspace.resolve()),
        "kind": "main",
        "parentId": None,
        "worktree": None,
        "settings": {"codexAutoConnect": False, "sidebarCollapsed": False},
    }]
    opencode_workspace = args.opencode_workspace.resolve() if args.opencode_workspace else None
    if opencode_workspace and args.register_opencode_workspace:
        if not opencode_workspace.is_dir():
            raise RuntimeError(f"OpenCode external-session workspace does not exist: {opencode_workspace}")
        workspace_entry.append({
            "id": "egui-opencode-external-workspace",
            "name": "OpenCode external ACP session",
            "path": str(opencode_workspace),
            "kind": "main",
            "parentId": None,
            "worktree": None,
            "settings": {"codexAutoConnect": False, "sidebarCollapsed": False},
        })

    fake_codex = (FIXTURE_DIR / "fake_codex.py").resolve()
    if not fake_codex.is_file():
        raise RuntimeError(f"fake Codex fixture was not found: {fake_codex}")
    fake_path = runtime_root / "fixture-bin"
    fake_path.mkdir()
    codex_cmd = fake_path / "codex"
    codex_cmd.symlink_to(fake_codex)
    codex_spy = fake_path / "fake-codex-spy"
    codex_spy.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, pathlib, sys\n"
        f"target = {str(fake_codex)!r}\n"
        "home = pathlib.Path(os.environ.get('CODEX_HOME', '.'))\n"
        "home.mkdir(parents=True, exist_ok=True)\n"
        "with (home / 'codex-launches.jsonl').open('a', encoding='utf-8') as out:\n"
        "    out.write(json.dumps({'argv': sys.argv[1:]}) + '\\n')\n"
        "os.execv(sys.executable, [sys.executable, target, *sys.argv[1:]])\n",
        encoding="utf-8",
    )
    codex_spy.chmod(0o755)
    common_settings = {
        # Explicitly pin the app-server executable. The shared Codex resolver
        # can prefer its configured path over PATH, so a PATH-only shim does
        # not protect this test from the user's installed Codex binary.
        "codexBin": str(codex_spy.resolve()),
        "automaticAppUpdateChecksEnabled": False,
        "threadTitleAutogenerationEnabled": False,
        "notificationSoundsEnabled": False,
        "systemNotificationsEnabled": False,
        "theme": "dark",
        "uiScale": 1,
    }
    for role, data, mode in (("local", local_data, "local"), ("remote", remote_data, "remote")):
        data.mkdir(parents=True, exist_ok=True)
        write_json(data / "settings.json", {
            **common_settings,
            "backendMode": mode,
            **({
                "remoteBackendHost": f"127.0.0.1:{port}",
                "remoteBackendToken": TOKEN,
                "remoteBackends": [{"id": "egui-acceptance", "name": "egui acceptance daemon", "provider": "tcp", "host": f"127.0.0.1:{port}", "token": TOKEN}],
                "activeRemoteBackendId": "egui-acceptance",
            } if mode == "remote" else {}),
        })
        write_json(data / "workspaces.json", workspace_entry)

    catalog = {
        "sessions": [{
            "sessionId": "egui-fixture-existing",
            "cwd": str(workspace.resolve()),
            "title": "egui existing fixture conversation",
            "updates": [
                {"sessionUpdate": "user_message_chunk", "messageId": "existing-user-1", "content": {"type": "text", "text": "EXTERNAL_EGUI_HISTORY"}},
                {"sessionUpdate": "agent_message_chunk", "messageId": "existing-agent-1", "content": {"type": "text", "text": "EXTERNAL_EGUI_ANSWER"}},
            ],
        }],
    }
    write_json(root / "generic-catalog.json", catalog)
    large_catalog_path = runtime_root / "performance-catalog.json"
    large_catalog = {
        "sessions": [{
            "sessionId": "egui-fixture-10000",
            "cwd": str(workspace.resolve()),
            "title": "10000 item native conversation",
            "updates": [
                update
                for index in range(5_000)
                for update in (
                    {"sessionUpdate": "user_message_chunk", "messageId": f"perf-user-{index:05d}", "content": {"type": "text", "text": f"PERF_EGUI_USER_{index:05d} — summarize item {index}."}},
                    {"sessionUpdate": "agent_message_chunk", "messageId": f"perf-agent-{index:05d}", "content": {"type": "text", "text": f"PERF_EGUI_AGENT_{index:05d} — response for item {index}."}},
                )
            ],
        }],
    }
    write_json(large_catalog_path, large_catalog)

    # Profiles are seeded only in the new, task-owned SQLite stores. The native
    # application remains responsible for creating the schema and all records.
    profiles = [
        {
            "id": "egui-generic-acp",
            "revision": 1,
            "name": "Independent ACP",
            "kind": "acp",
            "executable": sys.executable,
            "args": [str((FIXTURE_DIR / "conversations_acp.py").resolve()), "--catalog", str((root / "generic-catalog.json").resolve())],
            "workingDirectory": str(REPO),
        },
        {
            "id": "egui-large-acp",
            "revision": 1,
            "name": "Large ACP Performance",
            "kind": "acp",
            "executable": sys.executable,
            "args": [str((FIXTURE_DIR / "conversations_acp.py").resolve()), "--catalog", str(large_catalog_path.resolve())],
            "workingDirectory": str(workspace.resolve()),
        },
    ]
    open_code = Path("/home/cadwaladyr/.opencode/bin/opencode")
    if open_code.is_file():
        profiles.append({
            "id": "egui-opencode-acp",
            "revision": 1,
            "name": "OpenCode",
            "kind": "open_code_acp",
            "executable": str(open_code.resolve()),
            "args": [],
            "workingDirectory": str(opencode_workspace or workspace.resolve()),
        })
    for data in (local_data, remote_data):
        db_path = data / "runs.sqlite3"
        conn = sqlite3.connect(db_path)
        try:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("CREATE TABLE IF NOT EXISTS runtime_profiles(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL)")
            for profile in profiles:
                conn.execute("INSERT OR REPLACE INTO runtime_profiles(id,revision,payload) VALUES(?,?,?)", (profile["id"], profile["revision"], json.dumps(profile, separators=(",", ":"))))
            conn.commit()
        finally:
            conn.close()

    context = {
        "createdAt": utc_now(),
        "repository": str(REPO),
        "evidenceDir": str(root.resolve()),
        "runtimeDir": str(runtime_root.resolve()),
        "agentXdgDir": str(args.agent_xdg_dir.resolve()) if args.agent_xdg_dir else str((runtime_root / "runtime-home" / "agent-runtime").resolve()),
        "externalOpenCodeWorkspace": str(opencode_workspace) if opencode_workspace else None,
        "workspace": str(workspace.resolve()),
        "localDataDir": str(local_data.resolve()),
        "daemonDataDir": str(daemon_data.resolve()),
        "remoteDataDir": str(remote_data.resolve()),
        "port": port,
        "token": TOKEN,
        "fixturePath": str(fake_path.resolve()),
        "genericAcpProfileId": profiles[0]["id"],
        "performanceAcpProfileId": "egui-large-acp",
        "performanceCatalog": str(large_catalog_path.resolve()),
        "openCodeProfileId": next((profile["id"] for profile in profiles if profile["kind"] == "open_code_acp"), None),
        "codexFixture": str(fake_codex),
        "codexLaunchSpy": str(codex_spy.resolve()),
        "codexLaunchLogs": [str((runtime_root / "runtime-home" / role / "codex" / "codex-launches.jsonl").resolve()) for role in ("local", "daemon", "remote")],
        "genericAcpFixture": str((FIXTURE_DIR / "conversations_acp.py").resolve()),
        "openCodeExecutable": "/home/cadwaladyr/.opencode/bin/opencode" if Path("/home/cadwaladyr/.opencode/bin/opencode").is_file() else None,
        "note": "Only task-owned data and fixture workspaces were created. No installed application was launched or edited.",
    }
    write_json(root / "context.json", context)
    print(json.dumps(context, indent=2))


def isolated_env(root: Path, role: str) -> dict[str, str]:
    context = json.loads((root / "context.json").read_text(encoding="utf-8"))
    runtime_root = Path(context.get("runtimeDir", root))
    env = os.environ.copy()
    fixture = runtime_root / "fixture-bin"
    env["PATH"] = f"{fixture}:/usr/local/bin:/usr/bin:/bin"
    env["CODEX_MONITOR_ACCEPTANCE"] = "1"
    env["EGUI_INSPECTION"] = env.get("EGUI_INSPECTION", "1")
    # Runtime configuration must stay inside this test directory. App settings
    # can update Codex config, while OpenCode honors XDG config/data locations.
    # A real ACP executable may keep its native session catalog outside the
    # Monitor SQLite database. The in-process local backend and the daemon
    # therefore need the same task-owned XDG roots when testing an existing
    # native session through both transports. The remote desktop is just a
    # client; its UI settings stay separate while its daemon-side ACP process
    # uses the daemon's shared local/daemon roots.
    xdg_role = "agent-runtime" if role in ("local", "daemon") else role
    runtime_home = runtime_root / "runtime-home" / role
    runtime_xdg_home = Path(context.get("agentXdgDir", runtime_root / "runtime-home" / xdg_role))
    for variable, directory in (
        ("CODEX_HOME", runtime_home / "codex"),
        ("XDG_CONFIG_HOME", runtime_xdg_home / "xdg-config"),
        ("XDG_DATA_HOME", runtime_xdg_home / "xdg-data"),
        ("XDG_STATE_HOME", runtime_xdg_home / "xdg-state"),
        ("XDG_CACHE_HOME", runtime_xdg_home / "xdg-cache"),
    ):
        directory.mkdir(parents=True, exist_ok=True)
        env[variable] = str(directory.resolve())
    # Keep the caller's native display/session variables intact.
    return env


def launch(args: argparse.Namespace) -> None:
    root: Path = args.evidence_dir
    ctx_path = root / "context.json"
    if not ctx_path.is_file():
        raise RuntimeError(f"run prepare first; missing {ctx_path}")
    ctx = json.loads(ctx_path.read_text())
    if args.role in ("local", "remote"):
        binary = args.binary.resolve()
        if not binary.is_file():
            raise RuntimeError(f"native binary not found: {binary}")
        data_dir = Path(ctx["localDataDir"] if args.role == "local" else ctx["remoteDataDir"])
        command = [str(binary), "--data-dir", str(data_dir)]
    else:
        binary = args.daemon_binary.resolve()
        if not binary.is_file():
            raise RuntimeError(f"daemon binary not found: {binary}")
        data_dir = Path(ctx["daemonDataDir"])
        command = [str(binary), "--listen", f"127.0.0.1:{ctx['port']}", "--data-dir", str(data_dir), "--token", ctx["token"]]
    pid_path = root / f"{args.role}.pid"
    if pid_path.exists():
        old_pid = int(pid_path.read_text().strip())
        try:
            os.kill(old_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise RuntimeError(f"task-owned {args.role} process {old_pid} is still running")
        has_verified_stop = False
        for receipt_path in root.glob(f"{args.role}-stopped-*.json"):
            try:
                receipt = json.loads(receipt_path.read_text())
            except (OSError, json.JSONDecodeError):
                continue
            if receipt.get("pid") == old_pid and receipt.get("stopped") is True:
                has_verified_stop = True
                break
        if not has_verified_stop:
            raise RuntimeError(f"previous {args.role} process {old_pid} lacks a matching verified stop receipt; preserve and inspect it before relaunch")
    stamp = time.time_ns()
    log_path = root / f"{args.role}-{stamp}.log"
    receipt_path = root / f"{args.role}-launch-{stamp}.json"
    if log_path.exists() or receipt_path.exists():
        raise RuntimeError(f"refusing to reuse generated launch paths for {args.role}: {log_path}, {receipt_path}")
    log = log_path.open("xb")
    child_env = isolated_env(root, args.role)
    proc = subprocess.Popen(command, cwd=REPO, env=child_env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    log.close()
    cached = cached_binary_hash(binary, root)
    binary_hash = cached[0] if cached else sha256(binary)
    binary_stat = cached[1] if cached else binary_identity(binary)
    launch_receipt = {"pid": proc.pid, "command": command, "binarySha256": binary_hash, "binaryIdentity": binary_stat, "binaryHashSource": "immutable-snapshot-cache" if cached else "launch-time-read", "binaryPath": str(binary), "dataDir": str(data_dir), "isolatedEnvironment": {key: child_env[key] for key in ("CODEX_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")}, "startedAt": utc_now(), "log": str(log_path)}
    write_json(receipt_path, launch_receipt)
    if pid_path.exists():
        replace_verified_pid_pointer(pid_path, proc.pid)
    else:
        with pid_path.open("x", encoding="ascii") as destination:
            destination.write(f"{proc.pid}\n")
    with (root / f"{args.role}-processes.jsonl").open("a", encoding="utf-8") as history:
        history.write(json.dumps(launch_receipt, ensure_ascii=False) + "\n")
    if proc.poll() is not None:
        raise RuntimeError(f"{args.role} exited during launch with status {proc.returncode}; see {log_path}")
    print(json.dumps({"role": args.role, "pid": proc.pid, "pgid": os.getpgid(proc.pid), "binary": str(binary), "sha256": binary_hash, "hashSource": launch_receipt["binaryHashSource"], "binaryIdentity": binary_stat, "dataDir": str(data_dir), "log": str(log_path)}, indent=2))


def process_pid(root: Path, role: str) -> int:
    path = root / f"{role}.pid"
    if not path.exists():
        raise RuntimeError(f"no task-owned PID receipt for {role}: {path}")
    pid = int(path.read_text().strip())
    if not process_alive(pid):
        raise RuntimeError(f"task-owned {role} process {pid} is no longer running")
    launches = []
    for receipt_path in root.glob(f"{role}-launch-*.json"):
        try:
            receipt = json.loads(receipt_path.read_text())
            if receipt.get("pid") == pid:
                launches.append(receipt)
        except (OSError, json.JSONDecodeError):
            continue
    if len(launches) != 1:
        raise RuntimeError(f"cannot identify one launch receipt for task-owned {role} PID {pid}")
    proc_exe = Path(f"/proc/{pid}/exe")
    try:
        exe_identity = binary_identity(proc_exe)
        receipt = launches[0]
        expected_identity = receipt.get("binaryIdentity")
        if expected_identity is not None and exe_identity != expected_identity:
            raise RuntimeError(f"PID {pid} executable inode differs from the task-owned {role} binary path")
        # The launch receipt hashes the executable once. If the running /proc
        # image still has the exact same device/inode/size/mtime identity, that
        # digest remains bound to this process. Re-reading a large immutable
        # binary through mergerfs on every mouse action can block for minutes.
        if expected_identity is not None and isinstance(receipt.get("binarySha256"), str):
            return pid
        running_hash = sha256(proc_exe)
    except OSError as error:
        raise RuntimeError(f"cannot inspect task-owned {role} executable for PID {pid}: {error}") from error
    if running_hash != launches[0].get("binarySha256"):
        raise RuntimeError(f"PID {pid} executable hash differs from the task-owned {role} launch receipt")
    return pid


def accessibility_app(pid: int, timeout: float = 10.0):
    try:
        import pyatspi
    except ImportError as error:
        raise RuntimeError("pyatspi is required for AT-SPI native acceptance") from error
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for app in pyatspi.Registry.getDesktop(0):
            try:
                if app.get_process_id() == pid:
                    return pyatspi, app
            except Exception:
                continue
        time.sleep(0.1)
    raise RuntimeError(f"no AT-SPI application is registered for task-owned PID {pid}; inspect EGUI_INSPECTION output and native window diagnostics")


def walk(node):
    if node is None:
        return
    yield node
    try:
        children = list(node)
    except Exception:
        return
    for child in children:
        yield from walk(child)


def window_id(pid: int) -> str:
    matches = subprocess.check_output(["xdotool", "search", "--all", "--pid", str(pid)], text=True).splitlines()
    if not matches:
        raise RuntimeError(f"xdotool found no X11 window for PID {pid}")
    return matches[-1]


def focus(pid: int) -> str:
    wid = window_id(pid)
    subprocess.run(["xdotool", "windowactivate", "--sync", wid], check=True)
    active_pid = subprocess.check_output(["xdotool", "getactivewindow", "getwindowpid"], text=True).strip()
    if active_pid != str(pid):
        raise RuntimeError(f"foreground guard failed: expected PID {pid}, got {active_pid}")
    return wid


def window_geometry(wid: str) -> tuple[int, int, int, int]:
    geometry = subprocess.check_output(["xdotool", "getwindowgeometry", "--shell", wid], text=True)
    values = dict(line.split("=", 1) for line in geometry.splitlines() if "=" in line)
    return tuple(int(values[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))


def require_window_point(wid: str, x: int, y: int, *, local: bool = False) -> None:
    """Reject clipped AccessKit or screenshot targets outside the task window."""
    root_x, root_y, width, height = window_geometry(wid)
    point_x, point_y = (x, y) if local else (x - root_x, y - root_y)
    if not (0 <= point_x < width and 0 <= point_y < height):
        raise RuntimeError(
            f"native target point ({point_x}, {point_y}) is outside task window "
            f"{width}x{height}; scroll the control into view before clicking"
        )


def physical_center(app, target) -> tuple[int, int, list[int]]:
    """Translate AccessKit bounds through nested native windows to X11 roots.

    Root controls report app-local bounds. Controls in egui floating panels
    report bounds local to the panel frame, so include every intermediate
    frame's origin before adding the main X11 window origin.
    """
    frame = next((item for item in walk(app) if item.getRoleName() in ("frame", "window")), None)
    if frame is None:
        raise RuntimeError("the task-owned native window has no accessible frame")
    frame_x, frame_y, _, _ = frame.queryComponent().getExtents(2)
    x, y, width, height = target.queryComponent().getExtents(2)
    parent = target.parent
    while parent is not None and parent != frame:
        try:
            if parent.getRoleName() in ("frame", "window"):
                parent_x, parent_y, _, _ = parent.queryComponent().getExtents(2)
                x += parent_x
                y += parent_y
            parent = parent.parent
        except Exception:
            break
    return frame_x + x + width // 2, frame_y + y + height // 2, [x, y, width, height]


def dump_accessibility(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    _, app = accessibility_app(pid, args.timeout)
    output = []
    for item in walk(app):
        try:
            role = item.getRoleName()
            name = item.name or ""
            try:
                text = item.queryText().getText(0, -1)
            except Exception:
                text = ""
            try:
                x, y, width, height = item.queryComponent().getExtents(2)
                bounds = [x, y, width, height]
            except Exception:
                bounds = None
            if name or text or role in ("frame", "window"):
                output.append({"role": role, "name": name, "text": text, "bounds": bounds})
        except Exception:
            continue
    snapshot = {"capturedAt": utc_now(), "role": args.role, "pid": pid, "windowId": window_id(pid), "items": output}
    name = args.output or f"{args.role}-accessibility-{time.time_ns()}.json"
    path = args.evidence_dir / name
    write_json(path, snapshot)
    print(json.dumps({"path": str(path), "items": len(output), "pid": pid}, indent=2))


def find_accessible(app, label: str, role: str | None, timeout: float, nth: int | None = None):
    deadline = time.monotonic() + timeout
    while True:
        matches = []
        for item in walk(app):
            try:
                item_role, item_name = item.getRoleName(), item.name or ""
                if item_name == label and (role is None or item_role == role):
                    matches.append(item)
            except Exception:
                continue
        if nth is not None and nth < len(matches):
            return matches[nth]
        if nth is None and len(matches) == 1:
            return matches[0]
        if time.monotonic() >= deadline:
            expectation = f"item index {nth}" if nth is not None else "exactly one accessible item"
            raise RuntimeError(f"expected {expectation} named {label!r} role={role!r}; found {len(matches)}")
        time.sleep(0.1)


def act(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    pyatspi, app = accessibility_app(pid, args.timeout)
    wid = focus(pid)
    target = find_accessible(app, args.label, args.widget_role, args.timeout, args.nth)
    action = target.queryAction()
    if action.nActions < 1:
        raise RuntimeError(f"accessible item has no action: {args.label!r}")
    if not action.doAction(0):
        raise RuntimeError(f"AT-SPI action was rejected: {args.label!r}")
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "invoke", "label": args.label, "widgetRole": target.getRoleName()}
    path = args.evidence_dir / f"{args.role}-action-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def assert_native(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    pyatspi, app = accessibility_app(pid, args.timeout)
    matches = []
    for item in walk(app):
        try:
            if item.name == args.label and (args.widget_role is None or item.getRoleName() == args.widget_role):
                matches.append(item)
        except Exception:
            continue
    if args.nth is not None:
        if args.nth >= len(matches):
            raise RuntimeError(f"native accessible item {args.label!r} index {args.nth} missing; found {len(matches)}")
        item = matches[args.nth]
    elif len(matches) == 1:
        item = matches[0]
    else:
        raise RuntimeError(f"expected one native accessible item {args.label!r}, found {len(matches)}")
    states = item.getState()
    checks = {
        "enabled": pyatspi.STATE_ENABLED,
        "selected": pyatspi.STATE_SELECTED,
        "expanded": pyatspi.STATE_EXPANDED,
        "focused": pyatspi.STATE_FOCUSED,
        "showing": pyatspi.STATE_SHOWING,
    }
    state_values = {key: bool(states.contains(value)) for key, value in checks.items()}
    for state in args.require_state:
        if not state_values[state]:
            raise RuntimeError(f"native widget {args.label!r} did not have required state {state}: {state_values}")
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "label": args.label, "widgetRole": item.getRoleName(), "states": state_values, "result": "passed"}
    path = args.evidence_dir / f"{args.role}-assert-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def physical_click(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    _, app = accessibility_app(pid, args.timeout)
    wid = focus(pid)
    target = find_accessible(app, args.label, args.widget_role, args.timeout, args.nth)
    point_x, point_y, bounds = physical_center(app, target)
    point = (point_x, point_y)
    require_window_point(wid, *point)
    subprocess.run(["xdotool", "mousemove", str(point[0]), str(point[1]), "click", "1"], check=True)
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "physical_click", "label": args.label, "widgetRole": target.getRoleName(), "point": point, "appLocalBounds": bounds}
    path = args.evidence_dir / f"{args.role}-click-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def physical_click_point(args: argparse.Namespace) -> None:
    """Click a screenshot-derived window-local point if AT-SPI omits a widget."""
    pid = process_pid(args.evidence_dir, args.role)
    wid = focus(pid)
    x, y, width, height = window_geometry(wid)
    if not (0 <= args.x < width and 0 <= args.y < height):
        raise RuntimeError(
            f"screenshot point ({args.x}, {args.y}) is outside task window "
            f"{width}x{height}; choose a point visible in the retained screenshot"
        )
    root_point = [x + args.x, y + args.y]
    subprocess.run(["xdotool", "mousemove", str(root_point[0]), str(root_point[1]), "click", "1"], check=True)
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "physical_click_point", "appLocalPoint": [args.x, args.y], "rootPoint": root_point}
    path = args.evidence_dir / f"{args.role}-point-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def type_text(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    _, app = accessibility_app(pid, args.timeout)
    wid = focus(pid)
    target = find_accessible(app, args.label, args.widget_role, args.timeout, args.nth)
    if not target.queryComponent().grabFocus():
        raise RuntimeError(f"could not focus native control {args.label!r}")
    subprocess.run(["xdotool", "key", "--clearmodifiers", "ctrl+a"], check=True)
    subprocess.run(["xdotool", "type", "--clearmodifiers", "--delay", "2", args.value], check=True)
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        try:
            value = target.queryText().getText(0, -1)
        except Exception:
            value = None
        if value == args.value:
            receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "type", "label": args.label, "value": args.value}
            path = args.evidence_dir / f"{args.role}-type-{time.time_ns()}.json"
            write_json(path, receipt)
            print(json.dumps(receipt | {"evidence": str(path)}, indent=2))
            return
        time.sleep(0.05)
    raise RuntimeError(f"typed value did not reach AT-SPI text field {args.label!r}")


def screenshot(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    wid = focus(pid)
    out = args.output or f"{args.role}-screen-{time.time_ns()}.png"
    path = args.evidence_dir / out
    if path.exists():
        raise RuntimeError(f"refusing to overwrite existing screenshot: {path}")
    subprocess.run(["import", "-window", wid, str(path)], check=True)
    print(json.dumps({"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "screenshot": str(path)}, indent=2))


def hold(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    _, app = accessibility_app(pid, args.timeout)
    wid = focus(pid)
    target = find_accessible(app, args.label, args.widget_role, args.timeout)
    x, y, bounds = physical_center(app, target)
    require_window_point(wid, x, y)
    subprocess.run(["xdotool", "mousemove", str(x), str(y), "click", "1"], check=True)
    history_before = load_count(args.database, "history_loads") if args.database else None
    started = time.monotonic()
    time.sleep(args.seconds)
    active_pid = subprocess.check_output(["xdotool", "getactivewindow", "getwindowpid"], text=True).strip()
    if active_pid != str(pid):
        raise RuntimeError(f"native window lost focus during hold: expected {pid}, got {active_pid}")
    screenshot_args = argparse.Namespace(evidence_dir=args.evidence_dir, role=args.role, output=f"{args.role}-{args.slug}-held.png")
    screenshot(screenshot_args)
    # A foreground window alone does not prove that an egui popup survived.
    # Re-query the native tree after the idle interval and require the exact
    # control to remain exposed before recording a successful hold.
    _, refreshed_app = accessibility_app(pid, args.timeout)
    held = find_accessible(refreshed_app, args.label, args.widget_role, args.timeout, args.nth)
    hx, hy, hw, hh = held.queryComponent().getExtents(2)
    if hw <= 0 or hh <= 0:
        raise RuntimeError(f"native control {args.label!r} was not visible after the hold interval")
    history_after = load_count(args.database, "history_loads") if args.database else None
    if args.database and history_after != history_before:
        raise RuntimeError(f"history load count changed while native control was held: before={history_before}, after={history_after}")
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "hold_open", "label": args.label, "seconds": time.monotonic() - started, "foregroundPid": int(active_pid), "controlBoundsAfter": [hx, hy, hw, hh], "historyLoadsBefore": history_before, "historyLoadsAfter": history_after, "status": "foreground retained; accessible control remained exposed; screenshot retained; history count unchanged when a database was supplied"}
    path = args.evidence_dir / f"{args.role}-{args.slug}-held.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def load_count(database: Path, table: str) -> int:
    conn = sqlite3.connect(f"file:{database.resolve()}?mode=ro", uri=True)
    try:
        exists = conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone()
        if not exists:
            raise RuntimeError(f"required acceptance table is absent: {table}")
        return int(conn.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0])
    finally:
        conn.close()


def choose(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    _, app = accessibility_app(pid, args.timeout)
    wid = focus(pid)
    control = find_accessible(app, args.control, args.control_role, args.timeout, args.control_nth)
    x, y, control_bounds = physical_center(app, control)
    require_window_point(wid, x, y)
    subprocess.run(["xdotool", "mousemove", str(x), str(y), "click", "1"], check=True)
    option = find_accessible(app, args.option, args.option_role, args.timeout, args.option_nth)
    ox, oy, option_bounds = physical_center(app, option)
    require_window_point(wid, ox, oy)
    subprocess.run(["xdotool", "mousemove", str(ox), str(oy), "click", "1"], check=True)
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "choose", "control": args.control, "option": args.option, "controlPoint": [x, y], "optionPoint": [ox, oy], "controlAppLocalBounds": control_bounds, "optionAppLocalBounds": option_bounds, "input": "physical xdotool mouse clicks"}
    path = args.evidence_dir / f"{args.role}-choose-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def press(args: argparse.Namespace) -> None:
    pid = process_pid(args.evidence_dir, args.role)
    wid = focus(pid)
    subprocess.run(["xdotool", "key", "--clearmodifiers", *args.keys], check=True)
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "windowId": wid, "action": "physical_keypress", "keys": args.keys}
    path = args.evidence_dir / f"{args.role}-keys-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def verify_store(args: argparse.Namespace) -> None:
    store = args.database.resolve()
    uri = f"file:{store}?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    try:
        integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
        foreign_keys = conn.execute("PRAGMA foreign_key_check").fetchall()
        tables = [row[0] for row in conn.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
        counts = {}
        for table in tables:
            if table.startswith("sqlite_"):
                continue
            safe = table.replace('"', '""')
            counts[table] = conn.execute(f'SELECT COUNT(*) FROM "{safe}"').fetchone()[0]
        result = {"at": utc_now(), "database": str(store), "sha256": sha256(store), "integrity": integrity, "foreignKeyViolations": foreign_keys, "rowCounts": counts}
        if integrity != "ok" or foreign_keys:
            raise RuntimeError(json.dumps(result))
        path = args.evidence_dir / (args.output or f"store-check-{time.time_ns()}.json")
        write_json(path, result)
        print(json.dumps(result | {"evidence": str(path)}, indent=2))
    finally:
        conn.close()


def snapshot_store(args: argparse.Namespace) -> None:
    source = args.database.resolve()
    destination = args.output.resolve()
    if not source.is_file():
        raise RuntimeError(f"source store does not exist: {source}")
    if destination.exists():
        raise RuntimeError(f"refusing to overwrite existing snapshot: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    src = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
    dst = sqlite3.connect(destination)
    try:
        src.backup(dst)
        integrity = dst.execute("PRAGMA integrity_check").fetchone()[0]
        if integrity != "ok":
            raise RuntimeError(f"SQLite backup failed integrity check: {integrity}")
    finally:
        dst.close()
        src.close()
    receipt = {"at": utc_now(), "source": str(source), "snapshot": str(destination), "sha256": sha256(destination), "integrity": "ok"}
    path = args.evidence_dir / f"snapshot-{destination.stem}-{time.time_ns()}.json"
    write_json(path, receipt)
    print(json.dumps(receipt | {"evidence": str(path)}, indent=2))


def stop(args: argparse.Namespace) -> None:
    root: Path = args.evidence_dir
    pid = process_pid(root, args.role)
    pgid = os.getpgid(pid)
    if pgid != pid:
        raise RuntimeError(f"refusing to signal non-task-owned process group: pid={pid} pgid={pgid}")
    os.killpg(pgid, signal.SIGTERM)
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        if not process_alive(pid):
            break
        time.sleep(0.1)
    else:
        raise RuntimeError(f"task-owned process group {pgid} did not stop within {args.timeout}s; no SIGKILL sent")
    receipt = {"at": utc_now(), "role": args.role, "pid": pid, "processGroup": pgid, "signal": "SIGTERM", "stopped": True}
    stamp = time.time_ns()
    write_json(root / f"{args.role}-stopped-{stamp}.json", receipt)
    with (root / f"{args.role}-processes.jsonl").open("a", encoding="utf-8") as history:
        history.write(json.dumps(receipt, ensure_ascii=False) + "\n")
    print(json.dumps(receipt, indent=2))


def process_alive(pid: int) -> bool:
    stat = Path(f"/proc/{pid}/stat")
    if stat.exists():
        try:
            # A zombie cannot service requests or hold a window; it is stopped
            # even if the kernel still exposes its PID until reaping.
            state = stat.read_text().split()[2]
            return state != "Z"
        except (OSError, IndexError):
            return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def preflight(args: argparse.Namespace) -> None:
    report = {"at": utc_now(), "platform": sys.platform, "display": {k: os.environ.get(k) for k in ("DISPLAY", "WAYLAND_DISPLAY", "XDG_SESSION_TYPE", "DBUS_SESSION_BUS_ADDRESS")}, "tools": {}, "nativeAcceptance": {"status": "preflight_only", "installedAppTouched": False}}
    for tool in ("xdotool", "import"):
        try:
            report["tools"][tool] = require_tool(tool)
        except RuntimeError as error:
            report["tools"][tool] = str(error)
    try:
        import pyatspi  # noqa: F401
        report["tools"]["pyatspi"] = "available"
        report["tools"]["atspiDesktopApplications"] = len(list(pyatspi.Registry.getDesktop(0)))
    except Exception as error:
        report["tools"]["pyatspi"] = f"unavailable: {error}"
    try:
        report["tools"]["opencode"] = subprocess.check_output(["/home/cadwaladyr/.opencode/bin/opencode", "--version"], text=True, stderr=subprocess.STDOUT).strip()
    except (OSError, subprocess.CalledProcessError) as error:
        report["tools"]["opencode"] = f"unavailable: {error}"
    path = args.output or (args.evidence_dir / f"native-preflight-{time.time_ns()}.json")
    write_json(path, report)
    print(json.dumps(report | {"evidence": str(path)}, indent=2))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path, required=True, help="task-owned isolated evidence directory (must be explicit)")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("preflight", help="inspect native automation tools and display session")
    p.add_argument("--output", type=Path, help="optional explicit new receipt path; existing files are never replaced")
    p.set_defaults(run=preflight)
    p = sub.add_parser("prepare", help="create isolated local/daemon data and fixture repositories")
    p.add_argument("--port", type=int)
    p.add_argument("--runtime-dir", type=Path, help="fresh task-owned runtime root, useful for ext4 performance runs; evidence stays under --evidence-dir")
    p.add_argument("--agent-xdg-dir", type=Path, help="existing task-owned OpenCode XDG root to share with a public ACP fixture session")
    p.add_argument("--opencode-workspace", type=Path, help="existing task-owned directory used as the OpenCode profile cwd; it stays outside Monitor until imported")
    p.add_argument("--register-opencode-workspace", action="store_true", help="pre-register the OpenCode path in Monitor (not used for outside-workspace discovery acceptance)")
    p.set_defaults(run=prepare)
    p = sub.add_parser("launch", help="launch only a supplied native/daemon binary with isolated data")
    p.add_argument("role", choices=("local", "remote", "daemon"))
    p.add_argument("--binary", type=Path, default=REPO / "src-tauri/target/debug/codex_monitor_native")
    p.add_argument("--daemon-binary", type=Path, default=REPO / "src-tauri/target/debug/codex_monitor_daemon")
    p.set_defaults(run=launch)
    p = sub.add_parser("tree", help="save native AT-SPI tree for a task-owned PID")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("--timeout", type=float, default=20)
    p.add_argument("--output")
    p.set_defaults(run=dump_accessibility)
    for name, callback in (("invoke", act), ("click", physical_click)):
        p = sub.add_parser(name)
        p.add_argument("role", choices=("local", "remote"))
        p.add_argument("label")
        p.add_argument("--widget-role")
        p.add_argument("--nth", type=int, help="zero-based match index when an accessible label appears more than once")
        p.add_argument("--timeout", type=float, default=15)
        p.set_defaults(run=callback)
    p = sub.add_parser("click-point", help="physically click a screenshot-derived native control absent from AT-SPI")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("x", type=int, help="window-local X from a retained screenshot")
    p.add_argument("y", type=int, help="window-local Y from a retained screenshot")
    p.set_defaults(run=physical_click_point)
    p = sub.add_parser("assert-ui", help="assert one named AT-SPI widget and optional states")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("label")
    p.add_argument("--widget-role")
    p.add_argument("--nth", type=int)
    p.add_argument("--require-state", choices=("enabled", "selected", "expanded", "focused", "showing"), action="append", default=[])
    p.add_argument("--timeout", type=float, default=15)
    p.set_defaults(run=assert_native)
    p = sub.add_parser("type")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("label")
    p.add_argument("value")
    p.add_argument("--widget-role", default="text")
    p.add_argument("--nth", type=int)
    p.add_argument("--timeout", type=float, default=15)
    p.set_defaults(run=type_text)
    p = sub.add_parser("screenshot")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("--output")
    p.set_defaults(run=screenshot)
    p = sub.add_parser("hold")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("label")
    p.add_argument("slug")
    p.add_argument("--widget-role")
    p.add_argument("--nth", type=int)
    p.add_argument("--seconds", type=float, default=35)
    p.add_argument("--database", type=Path, help="read-only store used to assert no replay occurred during the hold")
    p.add_argument("--timeout", type=float, default=15)
    p.set_defaults(run=hold)
    p = sub.add_parser("choose", help="physically click a named choice in a native accessible popup")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("control")
    p.add_argument("option")
    p.add_argument("--control-role")
    p.add_argument("--option-role")
    p.add_argument("--control-nth", type=int)
    p.add_argument("--option-nth", type=int)
    p.add_argument("--timeout", type=float, default=15)
    p.set_defaults(run=choose)
    p = sub.add_parser("press", help="send physical key events to the focused task-owned window")
    p.add_argument("role", choices=("local", "remote"))
    p.add_argument("keys", nargs="+")
    p.set_defaults(run=press)
    p = sub.add_parser("verify-store")
    p.add_argument("database", type=Path)
    p.add_argument("--output")
    p.set_defaults(run=verify_store)
    p = sub.add_parser("snapshot-store", help="create a consistent non-overwriting SQLite backup")
    p.add_argument("database", type=Path)
    p.add_argument("output", type=Path)
    p.set_defaults(run=snapshot_store)
    p = sub.add_parser("stop")
    p.add_argument("role", choices=("local", "remote", "daemon"))
    p.add_argument("--timeout", type=float, default=15)
    p.set_defaults(run=stop)
    return parser


def main() -> None:
    args = build_parser().parse_args()
    args.evidence_dir = args.evidence_dir.resolve()
    try:
        args.run(args)
    except Exception as error:
        print(f"native-egui acceptance command failed: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
