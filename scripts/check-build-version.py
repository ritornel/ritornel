"""Check the package version shared by the main and desktop workspaces and release tags."""

from pathlib import Path
import subprocess
import sys
import tomllib

root = Path(__file__).resolve().parent.parent
with (root / "Cargo.toml").open("rb") as manifest:
    version = tomllib.load(manifest)["workspace"]["package"]["version"]
with (root / "apps/desktop/Cargo.toml").open("rb") as manifest:
    desktop = tomllib.load(manifest)["package"]["version"]
if desktop != version:
    sys.exit(f"apps/desktop/Cargo.toml has version {desktop}, but the workspace has {version}")

tags = subprocess.check_output(["git", "tag", "--points-at", "HEAD", "--list", "v*"], cwd=root, encoding="utf-8")
for tag in tags.splitlines():
    if tag != f"v{version}":
        sys.exit(f"HEAD has release tag {tag}, but Cargo.toml requires v{version}")
print(f"Build version: ok ({version}).")
