#!/usr/bin/env python3
"""Fail when a change edits a released section of CHANGELOG.md.

New lines belong under "## [Unreleased]". A branch made before a release can still add its
line under the old "### Added" heading, and git then merges it into the released section
without a conflict. This check compares everything from the first released heading
("## [x.y.z…]" other than Unreleased) to the end of the file, between two commits.

It also fails when a change removes more than half of the file's lines (an emptied or
truncated CHANGELOG.md), even if no released section exists yet.

Usage: changelog-guard.py <base-rev> <head-rev>
In CI on a pull request, the checkout is GitHub's merge commit: base is HEAD^1 (main), head is
HEAD (main with the pull request merged in). Set CHANGELOG_EDIT_RELEASED=1 for a pull request
that is meant to correct a released section.
"""

import os
import re
import subprocess
import sys

RELEASED = re.compile(r"^## \[(?!Unreleased\])[^\]]+\]", re.MULTILINE)


def changelog(rev: str) -> str:
    out = subprocess.run(
        ["git", "show", f"{rev}:CHANGELOG.md"], capture_output=True, text=True
    )
    return out.stdout if out.returncode == 0 else ""


def released(text: str) -> str:
    m = RELEASED.search(text)
    return text[m.start():] if m else ""


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    base_text, head_text = (changelog(r) for r in sys.argv[1:])
    base_lines, head_lines = len(base_text.splitlines()), len(head_text.splitlines())
    allowed = os.environ.get("CHANGELOG_EDIT_RELEASED") == "1"
    if base_lines >= 10 and head_lines < base_lines / 2:
        if allowed:
            print("CHANGELOG.md: most of the file removed, allowed by CHANGELOG_EDIT_RELEASED=1.")
            return 0
        print(
            f"CHANGELOG.md: this change removes most of the file ({base_lines} lines before, "
            f"{head_lines} after). Restore it from main and add only your own lines."
        )
        print("If the removal is meant, add the label 'changelog: edit released'.")
        return 1
    base, head = released(base_text), released(head_text)
    if base == head:
        print("CHANGELOG.md: released sections unchanged.")
        return 0
    if allowed:
        print("CHANGELOG.md: released sections changed, allowed by CHANGELOG_EDIT_RELEASED=1.")
        return 0
    diff = difflines(base, head)
    added = [l[1:] for l in diff if l.startswith("+") and not l.startswith("+++")]
    removed = [l[1:] for l in diff if l.startswith("-") and not l.startswith("---")]
    print("CHANGELOG.md: this change edits a released section. New lines go under '## [Unreleased]'.")
    for line in added[:10]:
        print(f"  added in a released section: {line[:160]}")
    for line in removed[:10]:
        print(f"  removed from a released section: {line[:160]}")
    print("If the edit is meant (a correction), add the label 'changelog: edit released'.")
    return 1


def difflines(a: str, b: str) -> list[str]:
    import difflib

    return list(difflib.unified_diff(a.splitlines(), b.splitlines(), lineterm=""))


if __name__ == "__main__":
    sys.exit(main())
