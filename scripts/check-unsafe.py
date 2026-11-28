#!/usr/bin/env python3
"""ADR-0009 and ADR-0019 §2: who may contain `unsafe` code.

- Every crate uses the workspace lints (`unsafe_code = "forbid"`), except `dn-mobile`.
- `dn-mobile` sets `unsafe_code = "deny"`, so only UniFFI's generated scaffolding (which
  allows it inside the macro expansion) gets through. Its own sources contain no `unsafe`
  and no `allow(unsafe_code)`.
- `dn-sandbox` sets `unsafe_code = "deny"` too, for one call in its launcher binary
  (ADR-0021 §2, approved by the owner): in `src/bin/dn-sandbox-launch.rs` only, exactly one
  `#[allow(unsafe_code)]`, directly on `fn close_inherited_fds`, and exactly one `unsafe {`
  block, inside that function. No other dn-sandbox file has `unsafe` or lowers the lint, and
  dn-sandbox uses no `#[path]`.
- No other source file anywhere lowers the lint (`allow`, `expect` or `warn` naming
  `unsafe_code`), and dn-mobile uses no `#[path]`.

`--self-test` checks the patterns against known bypasses.

Exits 1 and names every problem. Run from the repository root.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXCEPTION = "crates/dn-mobile"
SANDBOX = "crates/dn-sandbox"
LAUNCHER = SANDBOX + "/src/bin/dn-sandbox-launch.rs"
problems = []

manifests = sorted(ROOT.glob("crates/*/Cargo.toml")) + sorted(ROOT.glob("apps/*/Cargo.toml"))
for manifest in manifests:
    rel = manifest.parent.relative_to(ROOT).as_posix()
    text = manifest.read_text()
    if rel in (EXCEPTION, SANDBOX):
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
    in_sandbox = rel.startswith(SANDBOX + "/")
    if rel == LAUNCHER:
        return launcher_problems(rel, text)
    for n, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("//"):
            continue
        if LOWERED.search(line):
            found.append(f"{rel}:{n}: lowering the unsafe_code lint isn't allowed")
        if in_mobile and UNSAFE.search(line):
            found.append(f"{rel}:{n}: hand-written `unsafe` in dn-mobile (only UniFFI's generated code may use it)")
        if in_sandbox and UNSAFE.search(line):
            found.append(f"{rel}:{n}: `unsafe` in dn-sandbox outside the launcher's close_inherited_fds")
        if (in_mobile or in_sandbox) and PATH_ATTR.search(line):
            found.append(f"{rel}:{n}: #[path] in dn-mobile could pull in code this check doesn't see")
    return found


def launcher_problems(rel, text):
    """The launcher: one `#[allow(unsafe_code)]` directly on `fn close_inherited_fds`, and one
    `unsafe {` block inside that function; nothing else."""
    found = []
    lines = text.splitlines()
    code = [(n, l) for n, l in enumerate(lines, 1) if not l.lstrip().startswith("//")]
    allows = [(n, l) for n, l in code if LOWERED.search(l)]
    unsafes = [(n, l) for n, l in code if UNSAFE.search(l)]
    if any(PATH_ATTR.search(l) for _, l in code):
        found.append(f"{rel}: #[path] in dn-sandbox could pull in code this check doesn't see")
    if len(allows) != 1 or allows[0][1].strip() != "#[allow(unsafe_code)]":
        found.append(f"{rel}: exactly one plain `#[allow(unsafe_code)]` line is allowed")
        return found
    after = lines[allows[0][0]:]  # the lines after the allow
    if not after or not after[0].strip().startswith("fn close_inherited_fds("):
        found.append(f"{rel}:{allows[0][0]}: `#[allow(unsafe_code)]` must sit directly on fn close_inherited_fds")
        return found
    # The function's body: from its line to the first line that is just "}".
    start = allows[0][0] + 1
    end = next((start + i for i, l in enumerate(lines[start - 1:]) if l == "}"), len(lines))
    if len(unsafes) != 1:
        found.append(f"{rel}: exactly one `unsafe` block is allowed, found {len(unsafes)}")
    for n, l in unsafes:
        if not (start < n < end) or not l.strip().startswith("unsafe {"):
            found.append(f"{rel}:{n}: `unsafe` only as one `unsafe {{` block inside close_inherited_fds")
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
    good = (
        "fn main() {}\n#[cfg(unix)]\n#[allow(unsafe_code)]\nfn close_inherited_fds() -> Result<(), String> {\n"
        "    for n in open_fds()? {\n        // SAFETY: ...\n        unsafe { rustix::io::close(n) };\n    }\n    Ok(())\n}\n"
    )
    assert not launcher_problems(LAUNCHER, good), launcher_problems(LAUNCHER, good)
    assert problems_in(LAUNCHER, good + "fn x() { unsafe { } }\n"), "a second unsafe block"
    assert problems_in(LAUNCHER, good.replace("fn close_inherited_fds", "fn other")), "allow on another fn"
    assert problems_in(LAUNCHER, good + "#[allow(unsafe_code)]\nfn y() {}\n"), "a second allow"
    assert problems_in(LAUNCHER, good.replace("#[allow(unsafe_code)]", "#![allow(unsafe_code)]")), "an inner allow"
    assert problems_in(LAUNCHER, good.replace("unsafe { rustix", "unsafe fn z() {} unsafe { rustix")), "unsafe fn"
    assert problems_in(SANDBOX + "/src/lib.rs", "unsafe { }"), "unsafe elsewhere in dn-sandbox"
    assert problems_in(SANDBOX + "/src/lib.rs", "#[allow(unsafe_code)]"), "allow elsewhere in dn-sandbox"
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
