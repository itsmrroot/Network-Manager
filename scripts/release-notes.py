#!/usr/bin/env python3
"""Prints the notes of one version from CHANGELOG.md, for its release page.

Usage: scripts/release-notes.py v0.9.8

The section is the lines under "## 0.9.8" up to the next "## ". The release
fails when a version has no section: it must not go out without notes.
"""
import re
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parent.parent / "CHANGELOG.md"


def section(text: str, version: str) -> str | None:
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if re.fullmatch(rf"##\s+v?{re.escape(version)}\s*", line):
            end = next((j for j in range(i + 1, len(lines)) if lines[j].startswith("## ")), len(lines))
            body = "\n".join(lines[i + 1 : end]).strip()
            return body or None
    return None


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    version = sys.argv[1].lstrip("v")
    notes = section(CHANGELOG.read_text(encoding="utf-8"), version)
    if notes is None:
        print(f"CHANGELOG.md has no notes for {version}", file=sys.stderr)
        return 1
    # Bytes, as UTF-8: a Windows console would use its own code page and fail
    # on a character such as an arrow.
    sys.stdout.buffer.write((notes + "\n").encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
