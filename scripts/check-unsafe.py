#!/usr/bin/env python3
"""ADR-0009 and ADR-0019 §2: who may contain `unsafe` code.

- Every crate uses the workspace lints (`unsafe_code = "forbid"`), except `dn-mobile`.
- `dn-mobile` sets `unsafe_code = "deny"`, so only UniFFI's generated scaffolding (which
  allows it inside the macro expansion) gets through. Its own sources contain no `unsafe`
  and no `allow(unsafe_code)`.
- No source file anywhere lowers the lint (`allow`, `expect` or `warn` naming
  `unsafe_code`), and dn-mobile uses no `#[path]`.

`--self-test` checks the patterns against known bypasses.

Exits 1 and names every problem. Run from the repository root.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXCEPTION = "crates/dn-mobile"
problems = []

manifests = sorted(ROOT.glob("crates/*/Cargo.toml")) + sorted(ROOT.glob("apps/*/Cargo.toml"))
for manifest in manifests:
    rel = manifest.parent.relative_to(ROOT).as_posix()
    text = manifest.read_text()
    if rel == EXCEPTION:
        if not re.search(r'^\[lints\.rust\]\s*\nunsafe_code\s*=\s*"deny"', text, re.M):
            problems.append(f'{rel}/Cargo.toml: [lints.rust] must set unsafe_code = "deny"')
    elif not re.search(r"^\[lints\]\s*\nworkspace\s*=\s*true", text, re.M):
        problems.append(f"{rel}/Cargo.toml: must use the workspace lints ([lints] workspace = true)")

# The lint lowered anywhere: allow, expect or warn, alone or in a list, also inside cfg_attr.
LOWERED = re.compile(r"\b(allow|expect|warn)\s*\([^)]*\bunsafe_code\b")
UNSAFE = re.compile(r"\bunsafe\b")
PATH_ATTR = re.compile(r"#\s*\[\s*path\b")


def problems_in(rel, text):
    """The problems in one source file. Only whole-line `//` comments are skipped, so a
    string such as "//" can't hide code; a comment after code is checked too (a false
    alarm there is cheap)."""
    found = []
    in_mobile = rel.startswith(EXCEPTION + "/")
    for n, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("//"):
            continue
        if LOWERED.search(line):
            found.append(f"{rel}:{n}: lowering the unsafe_code lint isn't allowed")
        if in_mobile and UNSAFE.search(line):
            found.append(f"{rel}:{n}: hand-written `unsafe` in dn-mobile (only UniFFI's generated code may use it)")
        if in_mobile and PATH_ATTR.search(line):
            found.append(f"{rel}:{n}: #[path] in dn-mobile could pull in code this check doesn't see")
    return found


if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
    bad = {
        "a": "#[allow(dead_code, unsafe_code)]",
        "b": "#[expect(unsafe_code)]",
        "c": '#![cfg_attr(test, allow(unsafe_code))]',
        "d": 'fn f() { let _ = "//"; unsafe { } }',
        "e": '#[path = "../../dn-sync/src/x.rs"] mod x;',
        "f": "/* x */ unsafe fn g() {}",
    }
    for key, line in bad.items():
        assert problems_in(EXCEPTION + "/src/lib.rs", line), f"missed {key}: {line}"
    assert problems_in("crates/dn-sync/src/lib.rs", bad["a"]), "lowering outside dn-mobile"
    assert not problems_in(EXCEPTION + "/src/lib.rs", "//! No `unsafe` is written here.")
    assert not problems_in("crates/dn-sync/src/lib.rs", "// the word unsafe in a comment")
    print("check-unsafe self-test: ok")
    sys.exit(0)

for source in sorted(ROOT.glob("crates/**/*.rs")) + sorted(ROOT.glob("apps/**/src/**/*.rs")):
    rel = source.relative_to(ROOT).as_posix()
    if "/target/" in rel or "/node_modules/" in rel:
        continue
    problems.extend(problems_in(rel, source.read_text()))

if problems:
    print("Unsafe-code check failed:")
    for p in problems:
        print("  " + p)
    sys.exit(1)
print(f"unsafe-code check: ok ({len(manifests)} crates)")
