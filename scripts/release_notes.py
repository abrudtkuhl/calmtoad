#!/usr/bin/env python3
"""Extract one version's user-facing changelog entries, without network access."""

import argparse
from pathlib import Path
import re
import sys


VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?")
HEADING = re.compile(r"## \[([^\]]+)\](?: - \d{4}-\d{2}-\d{2})?\s*")
CATEGORIES = {"Added", "Improved", "Fixed"}


def extract_notes(changelog: str, version: str) -> str:
    if not VERSION.fullmatch(version):
        raise ValueError("Use a version without the v prefix, for example 0.1.1.")
    # Comments are author guidance, never release content or evidence of an entry.
    lines = re.sub(r"<!--.*?-->", "", changelog, flags=re.DOTALL).splitlines()
    starts = [i for i, line in enumerate(lines)
              if (match := HEADING.fullmatch(line)) and match[1] == version]
    if len(starts) != 1:
        raise ValueError(f"Expected exactly one ## [{version}] section; found {len(starts)}.")
    start = starts[0] + 1
    end = next((i for i in range(start, len(lines)) if lines[i].startswith("## ")), len(lines))
    body = lines[start:end]
    category = None
    entries = 0
    for line in body:
        if line.startswith("### "):
            category = line[4:].strip()
            if category not in CATEGORIES:
                raise ValueError(f"Unsupported category: {category}. Use Added, Improved, or Fixed.")
        elif re.match(r"^[-*+]\s+\S", line):
            if category is None:
                raise ValueError("Release entries must be under Added, Improved, or Fixed.")
            entries += 1
    if not entries:
        raise ValueError(f"The {version} section needs at least one user-facing bullet entry.")
    return "\n".join(body).strip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="Cargo version, without v")
    parser.add_argument("--changelog", type=Path,
                        default=Path(__file__).resolve().parents[1] / "CHANGELOG.md")
    args = parser.parse_args()
    try:
        notes = extract_notes(args.changelog.read_text(encoding="utf-8"), args.version)
    except (OSError, ValueError) as error:
        print(f"Release notes: {error}", file=sys.stderr)
        return 1
    sys.stdout.write(notes)
    return 0


if __name__ == "__main__":
    sys.exit(main())
