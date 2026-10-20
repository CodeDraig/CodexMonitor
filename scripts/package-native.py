#!/usr/bin/env python3
"""Stage and archive an unsigned, native CodexMonitor desktop distribution."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shlex
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ICONS = ROOT / "src-tauri" / "icons"
EGUI_FONTS = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "registry" / "src"


def bundled_egui_font_resources() -> Path:
    candidates = sorted(EGUI_FONTS.glob("*/epaint_default_fonts-0.37.0/fonts"))
    if len(candidates) != 1:
        raise SystemExit(
            "Could not identify the locked epaint_default_fonts 0.37.0 license resources"
        )
    return candidates[0]


def copy_font_licenses(destination: Path) -> None:
    fonts = bundled_egui_font_resources()
    destination.mkdir(parents=True, exist_ok=True)
    # These files correspond to the defaults compiled by epaint 0.37.0 with
    # default_fonts enabled and monochrome_emoji_fonts disabled.
    for name in (
        "Hack-Regular.txt",
        "UFL.txt",
        "emoji-icon-font-mit-license.txt",
        "egui-icons.txt",
    ):
        source = fonts / name
        if not source.is_file():
            raise SystemExit(f"Bundled egui font license resource is missing: {source}")
        shutil.copy2(source, destination / name)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def current_target() -> tuple[str, str, str]:
    system = platform.system().lower()
    target = {"darwin": "darwin", "windows": "windows", "linux": "linux"}.get(system)
    if target is None:
        raise SystemExit(f"Unsupported native package platform: {system}")
    machine = platform.machine().lower()
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(machine)
    if arch is None:
        raise SystemExit(f"Unsupported native package architecture: {machine}")
    fmt = "tar.gz" if target == "linux" else "zip"
    return target, arch, fmt


def version() -> str:
    import tomllib

    with (ROOT / "src-tauri" / "Cargo.toml").open("rb") as source:
        return tomllib.load(source)["package"]["version"]


def copy_executable(source: Path, destination: Path) -> None:
    if not source.is_file():
        raise SystemExit(f"Required executable is missing: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def file_inventory(stage: Path) -> list[dict[str, object]]:
    files = []
    for path in sorted(p for p in stage.rglob("*") if p.is_file()):
        rel = path.relative_to(stage).as_posix()
        files.append({"path": rel, "size": path.stat().st_size, "sha256": sha256(path)})
    return files


def linux_desktop_entry(prefix: Path) -> str:
    prefix = prefix.expanduser().resolve(strict=True)
    launcher = prefix / "codex-monitor-native"
    icon = prefix / "share" / "icons" / "hicolor" / "256x256" / "apps" / "codex-monitor-native.png"
    if not launcher.is_file() or not launcher.stat().st_mode & stat.S_IXUSR:
        raise SystemExit(f"Linux bundle launcher is missing or not executable: {launcher}")
    if not icon.is_file():
        raise SystemExit(f"Linux bundle icon is missing: {icon}")
    if any(c in str(path) for path in (launcher, icon) for c in "\n\r"):
        raise SystemExit("Linux bundle paths cannot contain newlines")
    # Freedesktop Exec uses its own quoting rules; shlex.quote is unsuitable
    # because it emits shell syntax that desktop launchers do not interpret.
    def quote_exec(value: str) -> str:
        return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$") + '"'

    # TryExec and Icon are key-file path values, not Exec command lines.
    # In particular, spaces are literal path characters and must not be quoted.
    def escape_path_value(value: str) -> str:
        escaped = value.replace("\\", "\\\\").replace("\t", "\\t")
        if escaped.startswith(" "):
            escaped = "\\s" + escaped[1:]
        if escaped.endswith(" "):
            escaped = escaped[:-1] + "\\s"
        return escaped

    return (
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Codex Monitor (Native)\n"
        "Comment=Native Rust desktop build of Codex Monitor\n"
        f"Exec={quote_exec(str(launcher))}\n"
        f"TryExec={escape_path_value(str(launcher))}\n"
        f"Icon={escape_path_value(str(icon))}\n"
        "Terminal=false\n"
        "Categories=Development;\n"
    )


def write_linux_desktop_entry(prefix: Path, output: Path) -> Path:
    output = output.expanduser()
    if output.exists():
        raise SystemExit(f"Refusing to overwrite desktop entry: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(linux_desktop_entry(prefix), encoding="utf-8")
    return output


def stage_package(native: Path, daemon: Path, daemonctl: Path, stage: Path, target: str) -> Path:
    stage.mkdir(parents=True)
    suffix = ".exe" if target == "windows" else ""

    if target == "darwin":
        app = stage / "Codex Monitor.app"
        contents = app / "Contents"
        macos = contents / "MacOS"
        resources = contents / "Resources"
        resources.mkdir(parents=True, exist_ok=True)
        copy_executable(native, macos / "codex_monitor_native")
        copy_executable(daemon, macos / "codex_monitor_daemon")
        copy_executable(daemonctl, macos / "codex_monitor_daemonctl")
        shutil.copy2(ICONS / "icon.icns", resources / "AppIcon.icns")
        shutil.copy2(ICONS / "icon.png", resources / "icon.png")
        info = {
            "CFBundleDevelopmentRegion": "en",
            "CFBundleExecutable": "codex_monitor_native",
            "CFBundleIconFile": "AppIcon.icns",
            "CFBundleIdentifier": "com.dimillian.codexmonitor.native",
            "CFBundleName": "Codex Monitor",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": version(),
            "CFBundleVersion": version(),
            "LSMinimumSystemVersion": "11.0",
            "NSMicrophoneUsageDescription": "Allow access to the microphone for dictation.",
            "NSHighResolutionCapable": True,
        }
        (contents / "Info.plist").write_bytes(__import__("plistlib").dumps(info, sort_keys=True))
        (resources / "FONT-NOTE.txt").write_text(
            "The native application embeds egui's default Ubuntu Light and Hack fonts plus its small special-icon font. "
            "eframe system-font fallback is enabled for glyphs those embedded fonts do not cover. "
            "Applicable font license and attribution texts are in Licenses/egui-fonts/.\n",
            encoding="utf-8",
        )
        copy_font_licenses(resources / "Licenses" / "egui-fonts")
        launcher = "Codex Monitor.app/Contents/MacOS/codex_monitor_native"
    else:
        bin_dir = stage / "bin"
        copy_executable(native, bin_dir / f"codex_monitor_native{suffix}")
        copy_executable(daemon, bin_dir / f"codex_monitor_daemon{suffix}")
        copy_executable(daemonctl, bin_dir / f"codex_monitor_daemonctl{suffix}")
        share = stage / "share" / "codex-monitor"
        share.mkdir(parents=True, exist_ok=True)
        for icon_name in ("icon.png", "icon.ico", "icon.icns"):
            shutil.copy2(ICONS / icon_name, share / icon_name)
        (share / "FONT-NOTE.txt").write_text(
            "The native application embeds egui's default Ubuntu Light and Hack fonts plus its small special-icon font. "
            "eframe system-font fallback is enabled for glyphs those embedded fonts do not cover. "
            "Applicable font license and attribution texts are in ../licenses/egui-fonts/.\n",
            encoding="utf-8",
        )
        copy_font_licenses(stage / "share" / "licenses" / "egui-fonts")
        if target == "linux":
            launcher_path = stage / "codex-monitor-native"
            launcher_path.write_text(
                '#!/bin/sh\n'
                'set -eu\n'
                'HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"\n'
                'exec "$HERE/bin/codex_monitor_native" "$@"\n',
                encoding="utf-8",
            )
            launcher_path.chmod(0o755)
            applications = stage / "share" / "applications"
            applications.mkdir(parents=True, exist_ok=True)
            (applications / "codex-monitor-native.desktop.in").write_text(
                "[Desktop Entry]\n"
                "Type=Application\n"
                "Name=Codex Monitor (Native)\n"
                "Comment=Native Rust desktop build of Codex Monitor\n"
                "Exec=\"@BUNDLE_PREFIX@/codex-monitor-native\"\n"
                "TryExec=@BUNDLE_PREFIX@/codex-monitor-native\n"
                "Icon=@BUNDLE_PREFIX@/share/icons/hicolor/256x256/apps/codex-monitor-native.png\n"
                "Terminal=false\n"
                "Categories=Development;\n",
                encoding="utf-8",
            )
            icons_dir = stage / "share" / "icons" / "hicolor" / "256x256" / "apps"
            icons_dir.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ICONS / "icon.png", icons_dir / "codex-monitor-native.png")
            launcher = "codex-monitor-native"
        else:
            launcher = f"bin/codex_monitor_native{suffix}"

    (stage / "native-package.json").write_text(
        json.dumps(
            {
                "product": "Codex Monitor Native",
                "version": version(),
                "target": target,
                "architecture": current_target()[1],
                "format": current_target()[2],
                "launcher": launcher,
                "font_policy": "egui-embedded-defaults-with-system-font-fallback",
                "files": file_inventory(stage),
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return stage


def validate_stage(
    stage: Path,
    target: str,
    native_source: Path | None = None,
    execute_launcher: bool = False,
) -> None:
    manifest_path = stage / "native-package.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest["target"] != target:
        raise SystemExit("Package target does not match staged platform")
    for item in manifest["files"]:
        path = stage / item["path"]
        if not path.is_file() or path.stat().st_size != item["size"] or sha256(path) != item["sha256"]:
            raise SystemExit(f"Staged package file failed hash/size validation: {item['path']}")

    launcher = manifest["launcher"]
    if target == "linux":
        desktop = stage / "share" / "applications" / "codex-monitor-native.desktop.in"
        text = desktop.read_text(encoding="utf-8")
        if "@BUNDLE_PREFIX@/codex-monitor-native" not in text or "@BUNDLE_PREFIX@/share/icons/" not in text:
            raise SystemExit("Linux desktop-entry template does not resolve launcher and icon from the bundle prefix")
        wrapper = stage / launcher
        if not wrapper.is_file() or "bin/codex_monitor_native" not in wrapper.read_text(encoding="utf-8"):
            raise SystemExit("Linux launcher does not dispatch to the packaged native executable")
        native_path = stage / "bin" / "codex_monitor_native"
        icon = stage / "share" / "icons" / "hicolor" / "256x256" / "apps" / "codex-monitor-native.png"
        if not icon.is_file():
            raise SystemExit("Linux launcher icon is missing from the bundle")
    elif target == "darwin":
        native_path = stage / launcher
        plist = __import__("plistlib").loads((stage / "Codex Monitor.app" / "Contents" / "Info.plist").read_bytes())
        if plist.get("CFBundleExecutable") != "codex_monitor_native":
            raise SystemExit("macOS app bundle executable does not match the packaged native binary")
        if plist.get("NSMicrophoneUsageDescription") != "Allow access to the microphone for dictation.":
            raise SystemExit("macOS app bundle is missing the microphone permission usage description")
    else:
        native_path = stage / launcher
    if not native_path.is_file():
        raise SystemExit(f"Package launcher executable is missing: {native_path}")
    if native_source is not None and sha256(native_path) != sha256(native_source):
        raise SystemExit("Packaged launcher executable differs from the native build output")
    if execute_launcher:
        result = subprocess.run(
            [str(wrapper if target == "linux" else native_path), "--help"],
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        help_text = result.stdout + result.stderr
        if result.returncode != 0 or "codex-monitor-native" not in help_text:
            raise SystemExit(
                "Packaged native launcher did not identify itself through --help "
                f"(exit {result.returncode}): {help_text.strip()}"
            )


def archive_stage(stage: Path, output: Path, fmt: str) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists():
        raise SystemExit(f"Refusing to overwrite existing package archive: {output}")
    if fmt == "tar.gz":
        with tarfile.open(output, "w:gz") as archive:
            archive.add(stage, arcname=stage.name)
    else:
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    archive.write(path, Path(stage.name) / path.relative_to(stage))


def write_sidecar(archive: Path, target: str, arch: str, fmt: str, app_version: str) -> Path:
    sidecar = archive.with_suffix(archive.suffix + ".manifest.json")
    payload = {
        "target": target,
        "architecture": arch,
        "format": fmt,
        "version": app_version,
        "sha256": sha256(archive),
        "signature": None,
        "signature_status": "unsigned",
    }
    sidecar.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return sidecar


def run_self_test() -> None:
    """Exercise staging, launcher identity, and hashes with disposable fixtures."""
    target, arch, fmt = current_target()
    with tempfile.TemporaryDirectory(prefix="codex-monitor-package-test-") as temporary:
        base = Path(temporary)
        fixtures = base / "fixtures"
        fixtures.mkdir()
        extension = ".exe" if target == "windows" else ""
        sources = []
        for name in ("native", "daemon", "daemonctl"):
            path = fixtures / f"{name}{extension}"
            if target == "linux" and name == "native":
                path.write_text("#!/bin/sh\nprintf 'codex-monitor-native fixture\\n'\n", encoding="utf-8")
                path.chmod(0o755)
            else:
                path.write_bytes(b"fixture executable identity\n" + name.encode())
            sources.append(path)
        stage_root = base / "extracted bundle with spaces"
        stage_root.mkdir()
        stage = stage_root / "CodexMonitor"
        stage_package(*sources, stage, target)
        validate_stage(stage, target, sources[0], execute_launcher=(target == "linux"))
        # Exercise the macOS microphone-usage key on every host. The plist
        # payload is platform data, so its generation can be checked without
        # compiling or launching a macOS bundle.
        mac_stage_parent = base / "macOS plist test"
        mac_stage_parent.mkdir()
        mac_stage = mac_stage_parent / "Codex Monitor.app bundle test"
        stage_package(*sources, mac_stage, "darwin")
        mac_plist = __import__("plistlib").loads(
            (mac_stage / "Codex Monitor.app" / "Contents" / "Info.plist").read_bytes()
        )
        if mac_plist.get("NSMicrophoneUsageDescription") != "Allow access to the microphone for dictation.":
            raise SystemExit("Generated macOS app bundle has no microphone permission usage description")
        if target == "linux":
            wrapper = stage / "codex-monitor-native"
            xdg_data = base / "xdg-data"
            applications = xdg_data / "applications"
            applications.mkdir(parents=True)
            desktop_entry = applications / "codex-monitor-native-fixture.desktop"
            write_linux_desktop_entry(stage, desktop_entry)
            subprocess.run(["desktop-file-validate", str(desktop_entry)], check=True, timeout=10)
            entry = dict(
                line.split("=", 1)
                for line in desktop_entry.read_text(encoding="utf-8").splitlines()
                if "=" in line and not line.startswith("[")
            )
            icon_path = Path(entry["Icon"])
            exec_argv = shlex.split(entry["Exec"])
            if Path(entry["TryExec"]) != wrapper or not icon_path.is_file() or exec_argv != [str(wrapper)]:
                raise SystemExit("Generated Linux desktop entry does not resolve its bundle launcher and icon")
            # Exercise the same GLib parser used by desktop launchers when
            # PyGObject is available, while keeping it optional for packagers.
            try:
                from gi.repository import Gio, GioUnix, GLib
            except (ImportError, ValueError):
                pass
            else:
                key_file = GLib.KeyFile.new()
                key_file.load_from_file(str(desktop_entry), GLib.KeyFileFlags.NONE)
                app_info = GioUnix.DesktopAppInfo.new_from_keyfile(key_file)
                if app_info is None:
                    raise SystemExit("GLib rejected the generated Linux desktop entry or its launcher path")
            launched = subprocess.run(
                [*exec_argv, "--help"],
                check=False,
                capture_output=True,
                text=True,
                timeout=15,
            )
            if launched.returncode != 0 or "codex-monitor-native" not in launched.stdout + launched.stderr:
                raise SystemExit(
                    "Launcher resolved from the generated Linux desktop entry did not start "
                    f"(exit {launched.returncode}): {(launched.stdout + launched.stderr).strip()}"
                )
        archive = base / f"CodexMonitor-{version()}-{target}-{arch}.{fmt}"
        archive_stage(stage, archive, fmt)
        sidecar = write_sidecar(archive, target, arch, fmt, version())
        manifest = json.loads(sidecar.read_text(encoding="utf-8"))
        if manifest["sha256"] != sha256(archive) or manifest["signature"] is not None:
            raise SystemExit("Package sidecar hash/signature fields failed self-test")
        print(f"Native package self-test passed: staged paths, hashes, launcher identity, {archive.name}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path)
    parser.add_argument("--daemon", type=Path)
    parser.add_argument("--daemonctl", type=Path)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "native-artifacts")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--write-linux-desktop-entry", action="store_true")
    parser.add_argument("--prefix", type=Path, help="Extracted Linux bundle directory")
    parser.add_argument("--desktop-entry-output", type=Path)
    args = parser.parse_args()
    if args.write_linux_desktop_entry:
        if not args.prefix or not args.desktop_entry_output:
            parser.error("--write-linux-desktop-entry requires --prefix and --desktop-entry-output")
        write_linux_desktop_entry(args.prefix, args.desktop_entry_output)
        print(f"Wrote Linux desktop entry: {args.desktop_entry_output}")
        return
    if args.self_test:
        run_self_test()
        return
    if not all((args.native, args.daemon, args.daemonctl)):
        parser.error("--native, --daemon, and --daemonctl are required unless --self-test is used")

    target, arch, fmt = current_target()
    app_version = version()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    stage = args.output_dir / f"CodexMonitor-{app_version}-{target}-{arch}"
    if stage.exists():
        raise SystemExit(f"Refusing to overwrite existing staging directory: {stage}")
    stage_package(args.native, args.daemon, args.daemonctl, stage, target)
    validate_stage(stage, target, args.native, execute_launcher=True)
    archive = args.output_dir / f"CodexMonitor-{app_version}-{target}-{arch}.{fmt}"
    if archive.exists():
        raise SystemExit(f"Refusing to overwrite existing package archive: {archive}")
    archive_stage(stage, archive, fmt)
    sidecar = write_sidecar(archive, target, arch, fmt, app_version)
    print(f"Staged package: {stage}")
    print(f"Archive: {archive}")
    print(f"Unsigned package manifest: {sidecar}")


if __name__ == "__main__":
    main()
