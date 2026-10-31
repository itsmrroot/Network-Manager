#!/usr/bin/env python3
"""Writes gui/assets/third-party.txt: the open-source software inside the app.

Usage: scripts/third-party.py     (after changing Cargo.lock)

The list comes from `cargo metadata` (every crate the app can be built with,
on every system). The test `the_third_party_list_is_current` in gui/src/about.rs
checks that every crate of Cargo.lock is in it.
"""
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEADER = """Open-source software in Network Manager

Network Manager itself is under the PolyForm Noncommercial License 1.0.0 (see LICENSE).
It is built with the software below; each part stays under its own licence, which
you find at the address given (the licence text is in that project's repository
and in the crate's own package).

Fonts (SIL Open Font License 1.1, texts in the app's source folder gui/assets/fonts):
  Noto Sans Arabic, Noto Sans SC (Google / Adobe), Phosphor Icons (MIT), egui's default fonts
  (Ubuntu-Light, Hack, NotoEmoji, emoji-icon-font: Ubuntu Font Licence, MIT, OFL).
Maker names of network devices: the public IEEE OUI registry (standards.ieee.org).

Rust crates (name, version, licence, project address):
"""


def main():
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT, check=True, capture_output=True, text=True
        ).stdout
    )
    members = set(meta["workspace_members"])
    rows = sorted(
        {
            (p["name"], p["version"], p.get("license") or p.get("license_file") or "see the package", p.get("repository") or p.get("homepage") or "")
            for p in meta["packages"]
            if p["id"] not in members
        }
    )
    lines = [f"{n} {v}  {lic}  {url}".rstrip() for n, v, lic, url in rows]
    (ROOT / "gui/assets/third-party.txt").write_text(HEADER + "\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(lines)} crates")


main()
