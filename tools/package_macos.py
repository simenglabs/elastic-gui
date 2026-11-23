#!/usr/bin/env python3
"""Package a native Rust release binary as an ad-hoc signed .app and DMG."""

import argparse
import hashlib
import json
import plistlib
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


TARGETS = {
    "aarch64-apple-darwin": "arm64",
    "x86_64-apple-darwin": "x86_64",
}


def run(*args):
    subprocess.run(args, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--binary", type=Path, default=Path("target/release/elastic-gui"))
    parser.add_argument("--output", type=Path, default=Path("dist"))
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("DMG packaging requires macOS (codesign, lipo, and hdiutil).")
    if not args.binary.is_file():
        parser.error(f"Binary not found: {args.binary}. Run cargo build --locked --release first.")

    arch = TARGETS[args.target]
    binary = args.binary.resolve()
    architectures = subprocess.check_output(["lipo", "-archs", str(binary)], text=True).split()
    if architectures != [arch]:
        parser.error(f"Expected a native {arch} binary, found: {' '.join(architectures)}")

    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version=1"], text=True,
    ))
    package = next(p for p in metadata["packages"] if p["name"] == "elastic-gui")
    version = package["version"]
    bundle_version = version.split("+")[0].split("-")[0]
    args.output.mkdir(parents=True, exist_ok=True)
    dmg = args.output.resolve() / f"Elastic-GUI-macos-{arch}.dmg"

    with tempfile.TemporaryDirectory(prefix="elastic-gui-dmg-") as tmp:
        staging = Path(tmp) / "staging"
        contents = staging / "Elastic GUI.app" / "Contents"
        executable = contents / "MacOS" / "elastic-gui"
        executable.parent.mkdir(parents=True)
        shutil.copy2(binary, executable)
        executable.chmod(0o755)
        info = {
            "CFBundleName": "Elastic GUI",
            "CFBundleDisplayName": "Elastic GUI",
            "CFBundleIdentifier": "id.menglabs.elastic-gui",
            "CFBundleExecutable": "elastic-gui",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": bundle_version,
            "CFBundleVersion": bundle_version,
            "LSApplicationCategoryType": "public.app-category.developer-tools",
            "LSMinimumSystemVersion": "13.0",
            "NSHighResolutionCapable": True,
        }
        (contents / "Info.plist").write_bytes(plistlib.dumps(info))
        (contents / "PkgInfo").write_bytes(b"APPL????")
        app = contents.parent
        run("plutil", "-lint", str(contents / "Info.plist"))
        run("codesign", "--force", "--sign", "-", str(app))
        run("codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app))
        (staging / "Applications").symlink_to("/Applications", target_is_directory=True)
        run("hdiutil", "create", "-volname", "Elastic GUI", "-srcfolder", str(staging),
            "-ov", "-format", "UDZO", "-fs", "HFS+", str(dmg))
        run("hdiutil", "verify", str(dmg))

    digest = hashlib.sha256(dmg.read_bytes()).hexdigest()
    dmg.with_suffix(".dmg.sha256").write_text(f"{digest}  {dmg.name}\n")
    print(f"Packaged Elastic GUI {version}: {dmg}")


if __name__ == "__main__":
    main()
