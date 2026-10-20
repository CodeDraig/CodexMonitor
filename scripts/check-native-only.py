#!/usr/bin/env python3
"""Check that the repository exposes only the native Rust desktop frontend."""
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[1]
manifest = tomllib.loads((ROOT / 'src-tauri/Cargo.toml').read_text())
assert manifest['package']['default-run'] == 'codex_monitor_native'
assert manifest['features']['default'] == ['native-gui']
assert not {'tauri-host', 'custom-protocol'} & manifest['features'].keys()
assert manifest['lib']['crate-type'] == ['rlib']
assert {entry['name'] for entry in manifest['bin']} == {'codex_monitor_native'}
assert manifest['bin'][0]['required-features'] == ['native-gui']

retired = [
    'src', 'public', 'node_modules', 'dist', 'package.json', 'package-lock.json',
    'tsconfig.json', 'tsconfig.node.json', 'vite.config.ts', 'index.html',
    'src-tauri/build.rs', 'src-tauri/src/main.rs', 'src-tauri/gen',
    'src-tauri/capabilities', 'src-tauri/tauri.conf.json',
    'src-tauri/tauri.ios.conf.json', 'src-tauri/icons/ios', 'src-tauri/icons/android',
    'scripts/build_run_ios.sh', 'scripts/build_run_ios_device.sh',
    'scripts/release_testflight_ios.sh',
]
assert not [name for name in retired if (ROOT / name).exists()], 'Retired frontend files are present'
lock = tomllib.loads((ROOT / 'src-tauri/Cargo.lock').read_text())
for package in lock['package']:
    name = package['name']
    # notify-rust uses this standalone Windows notification backend. It is not a webview host.
    if name == 'tauri-winrt-notification':
        continue
    assert not name.startswith('tauri') and name not in {'wry', 'webkit2gtk', 'javascriptcore-rs'}, name
assert {p.name for p in (ROOT / 'src-tauri/icons').iterdir()} == {'icon.png', 'icon.icns', 'icon.ico'}
for sound in ['success-notification.mp3', 'error-notification.mp3']:
    assert (ROOT / 'src-tauri/assets' / sound).stat().st_size > 0
for name in ['ci.yml', 'release.yml']:
    workflow = (ROOT / '.github/workflows' / name).read_text()
    assert 'npm ' not in workflow and 'setup-node' not in workflow
flake = (ROOT / 'flake.nix').read_text()
assert 'buildNpmPackage' not in flake and 'package.json' not in flake and 'cargo-tauri' not in flake
assert 'npm ' not in (ROOT / '.codex/environments/environment.toml').read_text()
print('Native-only frontend, Cargo, resources, and workflow contracts passed.')
