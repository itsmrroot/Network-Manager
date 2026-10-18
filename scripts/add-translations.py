#!/usr/bin/env python3
"""Adds translations to gui/src/translations.rs.

Usage: scripts/add-translations.py new.json

new.json maps a language code to {English text: translation}:

    {"de": {"Send": "Senden"}, "es": {...}, "fr": {...}, "tr": {...},
     "ru": {...}, "ar": {...}, "zh": {...}}

Every language must have the same English texts, each translation must keep
the {placeholders}, and none may contain direction marks (U+200E, U+200F).
Texts already in a table are replaced. Run `cargo fmt` afterwards, and
gui/assets/fonts/subset-chinese.py when Chinese texts brought new characters.
"""
import json
import re
import sys
from pathlib import Path

TABLES = {"de": "GERMAN", "es": "SPANISH", "fr": "FRENCH", "tr": "TURKISH", "ru": "RUSSIAN", "ar": "ARABIC", "zh": "CHINESE"}
FILE = Path(__file__).resolve().parent.parent / "gui/src/translations.rs"


def placeholders(s):
    return sorted(re.findall(r"\{[a-z_]*\}", s))


def rust(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def main():
    new = json.load(open(sys.argv[1], encoding="utf-8"))
    assert set(new) == set(TABLES), f"languages must be exactly {sorted(TABLES)}"
    keys = set(next(iter(new.values())))
    for code, table in new.items():
        assert set(table) == keys, f"{code}: not the same texts as the others"
        for en, t in table.items():
            assert placeholders(en) == placeholders(t), f"{code}: placeholders differ in {t!r}"
            assert "‎" not in t and "‏" not in t, f"{code}: direction mark in {t!r}"
            assert t.strip(), f"{code}: empty translation of {en!r}"
    text = FILE.read_text(encoding="utf-8")
    for code, const in TABLES.items():
        start = text.index(f"pub const {const}: &[(&str, &str)] = &[")
        end = text.index("\n];", start)
        body = text[start:end]
        for en in new[code]:
            # An existing entry for this text goes first.
            body = re.sub(r"\n    \(\s*" + re.escape(rust(en)) + r",\s*\"(?:[^\"\\]|\\.)*\",?\s*\),", "", body)
        rows = "".join(f"\n    ({rust(en)}, {rust(t)})," for en, t in new[code].items())
        text = text[:start] + body + rows + text[end:]
    FILE.write_text(text, encoding="utf-8")
    print(f"added {len(keys)} texts to {len(TABLES)} languages")


if __name__ == "__main__":
    main()
