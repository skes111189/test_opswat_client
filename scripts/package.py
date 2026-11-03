#!/usr/bin/env python3
"""Package already built native binaries; never resolves dependencies or publishes."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-pc-windows-msvc": ("Windows", {"amd64", "x86_64"}),
    "aarch64-apple-darwin": ("Darwin", {"arm64", "aarch64"}),
    "x86_64-unknown-linux-gnu": ("Linux", {"x86_64", "amd64"}),
}


def package(target, output=None, verify_native=True):
    expected_os, architectures = TARGETS[target]
    if verify_native and (platform.system() != expected_os or platform.machine().lower() not in architectures):
        raise RuntimeError(f"Expected native {target}, got {platform.system()} {platform.machine()}")
    output = Path(output or ROOT / "dist")
    output.mkdir(parents=True, exist_ok=True)
    name = f"metadefender-0.1.0-{target}"
    stage = output / name
    if stage.exists():
        raise RuntimeError(f"Package staging directory already exists: {stage}")
    binaries = ROOT / "target" / target / "release"
    extension = ".exe" if expected_os == "Windows" else ""
    for binary in ["client", "receiver"]:
        if not (binaries / (binary + extension)).is_file():
            raise RuntimeError(f"Build first: cargo build --workspace --release --target {target}")
    if not (ROOT / "Cargo.lock").is_file():
        raise RuntimeError("Cargo.lock is required to record resolved dependencies")
    (stage / "bin").mkdir(parents=True)
    for binary in ["client", "receiver"]:
        shutil.copy2(binaries / (binary + extension), stage / "bin" / (binary + extension))
    (stage / "config").mkdir()
    for example in (ROOT / "config").glob("*.example.toml"):
        shutil.copy2(example, stage / "config" / example.name)
    shutil.copytree(ROOT / "docs", stage / "docs")
    shutil.copytree(ROOT / "scripts", stage / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
    for filename in ["README.md", "Cargo.lock"]:
        shutil.copy2(ROOT / filename, stage / filename)
    shutil.copy2(ROOT / "docs" / "QUICKSTART.md", stage / "開始使用.md")
    if expected_os == "Windows":
        (stage / "start-client.cmd").write_text('@echo off\ncd /d "%~dp0"\n"%~dp0bin\\client.exe" "%~dp0config\\client.toml"\n')
        (stage / "start-receiver.cmd").write_text('@echo off\ncd /d "%~dp0"\n"%~dp0bin\\receiver.exe" "%~dp0config\\receiver.toml"\n')
    else:
        for binary in ["client", "receiver"]:
            launcher = stage / f"start-{binary}.sh"
            launcher.write_text(f'#!/bin/sh\nset -eu\ncd -- "$(dirname -- "$0")"\nexec ./bin/{binary} ./config/{binary}.toml\n')
            launcher.chmod(0o755)
    if expected_os == "Darwin":
        for binary, title in [("client", "開啟用戶端"), ("receiver", "啟動接收服務")]:
            launcher = stage / f"{title}.command"
            launcher.write_text(f'#!/bin/sh\nset -eu\ncd -- "$(dirname -- "$0")"\nexec ./start-{binary}.sh\n')
            launcher.chmod(0o755)
        app = stage / "MetaDefender.app" / "Contents"
        (app / "MacOS").mkdir(parents=True)
        (app / "Resources").mkdir()
        shutil.copy2(stage / "bin" / "client", app / "MacOS" / "client")
        with (app / "Info.plist").open("wb") as stream:
            plistlib.dump({"CFBundleExecutable": "client", "CFBundleIdentifier": "com.example.metadefender.transfer",
                          "CFBundleName": "MetaDefender 安全傳檔工具", "CFBundleVersion": "0.1.0",
                          "CFBundleShortVersionString": "0.1.0", "CFBundlePackageType": "APPL",
                          "NSHighResolutionCapable": True}, stream)
    checksums = {}
    for path in sorted((stage / "bin").iterdir()):
        checksums[str(path.relative_to(stage))] = hashlib.sha256(path.read_bytes()).hexdigest()
    (stage / "SHA256.json").write_text(json.dumps(checksums, indent=2) + "\n")
    try:
        compiler = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    except (OSError, subprocess.SubprocessError):
        compiler = "rustc version unavailable at package time\n"
    (stage / "BUILD.txt").write_text(f"Target: {target}\n{compiler}")
    if expected_os == "Linux":
        archive = output / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(stage, arcname=name)
    else:
        archive = output / f"{name}.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    bundle.write(path, Path(name) / path.relative_to(stage))
    print(archive)
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--output", type=Path)
    options = parser.parse_args()
    package(options.target, options.output)
