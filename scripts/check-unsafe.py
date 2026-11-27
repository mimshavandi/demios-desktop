#!/usr/bin/env python3
"""ADR-0009 and ADR-0019 §2: who may contain `unsafe` code.

- Every crate uses the workspace lints (`unsafe_code = "forbid"`), except `dn-mobile`.
- `dn-mobile` sets `unsafe_code = "deny"`, so only UniFFI's generated scaffolding (which
  allows it inside the macro expansion) gets through. Its own sources contain no `unsafe`
  and no `allow(unsafe_code)`.
- No source file anywhere turns the lint off with `allow(unsafe_code)`.

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

ALLOW = re.compile(r"allow\s*\(\s*unsafe_code\s*\)")
UNSAFE = re.compile(r"\bunsafe\b")
for source in sorted(ROOT.glob("crates/**/*.rs")) + sorted(ROOT.glob("apps/**/src/**/*.rs")):
    rel = source.relative_to(ROOT).as_posix()
    if "/target/" in rel or "/node_modules/" in rel:
        continue
    for n, line in enumerate(source.read_text().splitlines(), 1):
        code = line.split("//", 1)[0]
        if ALLOW.search(code):
            problems.append(f"{rel}:{n}: allow(unsafe_code) isn't allowed")
        if rel.startswith(EXCEPTION + "/") and UNSAFE.search(code):
            problems.append(f"{rel}:{n}: hand-written `unsafe` in dn-mobile (only UniFFI's generated code may use it)")

if problems:
    print("Unsafe-code check failed:")
    for p in problems:
        print("  " + p)
    sys.exit(1)
print(f"unsafe-code check: ok ({len(manifests)} crates)")
