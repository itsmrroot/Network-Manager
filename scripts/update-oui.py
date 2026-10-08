#!/usr/bin/env python3
"""Rebuilds data/oui.tsv from the IEEE MA-L registry.

Each line is "<6 hex digits>\t<vendor>", sorted, with company suffixes
trimmed so that the table embedded in the binary stays small.

    python3 scripts/update-oui.py
"""

import csv
import io
import re
import urllib.request
from pathlib import Path

URL = "https://standards-oui.ieee.org/oui/oui.csv"
OUT = Path(__file__).resolve().parent.parent / "data" / "oui.tsv"

SUFFIX = re.compile(
    r"[,\s]+(inc|incorporated|corp|corporation|co|company|ltd|limited|llc|gmbh|ag|sa|s\.a|bv|b\.v|"
    r"ab|oy|as|a/s|spa|s\.p\.a|srl|s\.r\.l|kg|plc|pty|pte|sas|nv|n\.v|technology|technologies|"
    r"electronics|communications|co\.,? ?ltd)\.?$",
    re.IGNORECASE,
)


def short(name: str) -> str:
    name = " ".join(name.replace("\t", " ").split()).strip(" .,")
    for _ in range(4):
        trimmed = SUFFIX.sub("", name).strip(" .,")
        if trimmed == name or not trimmed:
            break
        name = trimmed
    return name


def main() -> None:
    req = urllib.request.Request(URL, headers={"User-Agent": "netmgr-oui-update"})
    text = urllib.request.urlopen(req, timeout=60).read().decode("utf-8", "replace")
    rows = {}
    for row in csv.DictReader(io.StringIO(text)):
        prefix = row["Assignment"].strip().upper()
        vendor = short(row["Organization Name"])
        if len(prefix) == 6 and vendor:
            rows[prefix] = vendor
    OUT.write_text("".join(f"{k}\t{v}\n" for k, v in sorted(rows.items())), encoding="utf-8")
    print(f"{len(rows)} vendors -> {OUT} ({OUT.stat().st_size // 1024} KiB)")


if __name__ == "__main__":
    main()
