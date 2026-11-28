import assert from "node:assert/strict";
import { test } from "node:test";
import { changelogSection, latestJson } from "./latest-json.mjs";

const CHANGELOG = `# Changelog

## [Unreleased]

## [0.2.0] - 2027-01-15

### Added

- A thing ([#1](https://example.test/1)).

## [0.1.0] - 2026-11-25

- Older.

[0.2.0]: https://example.test/compare
`;

const files = {
  "Almend-Desktop-0.2.0-macos-arm64.app.tar.gz": "",
  "Almend-Desktop-0.2.0-macos-arm64.app.tar.gz.sig": "sig-mac-arm\n",
  "Almend-Desktop-0.2.0-macos-x64.app.tar.gz": "",
  "Almend-Desktop-0.2.0-macos-x64.app.tar.gz.sig": "sig-mac-x64",
  "Almend-Desktop-0.2.0-windows-x64-setup.exe": "",
  "Almend-Desktop-0.2.0-windows-x64-setup.exe.sig": "sig-win",
  "Almend-Desktop-0.2.0-linux-x86_64.AppImage": "",
  "Almend-Desktop-0.2.0-linux-x86_64.AppImage.sig": "sig-linux",
  "Almend-Desktop-0.2.0-macos-arm64.dmg": "",
  "SHA256SUMS.txt": "",
};

const base = { version: "0.2.0", tag: "v0.2.0", repo: "o/r", notes: "n", pubDate: "2027-01-15T10:00:00Z", files };

test("changelog section is the version's body, without the next section or links", () => {
  assert.equal(changelogSection(CHANGELOG, "0.2.0"), "### Added\n\n- A thing ([#1](https://example.test/1)).");
  assert.equal(changelogSection(CHANGELOG, "0.1.0"), "- Older.");
  assert.throws(() => changelogSection(CHANGELOG, "9.9.9"), /no section/);
  assert.throws(() => changelogSection("## [1.0.0] - x\n\n## [0.9.0]\n- a", "1.0.0"), /empty/);
});

test("every platform with a signed bundle, with release download URLs", () => {
  const m = latestJson(base);
  assert.deepEqual(Object.keys(m.platforms), ["darwin-aarch64", "darwin-x86_64", "windows-x86_64", "linux-x86_64"]);
  assert.equal(m.platforms["darwin-aarch64"].signature, "sig-mac-arm");
  assert.equal(
    m.platforms["windows-x86_64"].url,
    "https://github.com/o/r/releases/download/v0.2.0/Almend-Desktop-0.2.0-windows-x64-setup.exe",
  );
  assert.equal(m.version, "0.2.0");
  assert.equal(m.pub_date, "2027-01-15T10:00:00Z");
});

test("--platforms keeps code-signed platforms only and fails for a missing one", () => {
  const m = latestJson({ ...base, platforms: ["linux-x86_64", "windows-x86_64"] });
  assert.deepEqual(Object.keys(m.platforms).sort(), ["linux-x86_64", "windows-x86_64"]);
  const noLinux = { ...files };
  delete noLinux["Almend-Desktop-0.2.0-linux-x86_64.AppImage"];
  assert.throws(() => latestJson({ ...base, files: noLinux, platforms: ["linux-x86_64"] }), /no update bundle/);
  assert.throws(() => latestJson({ ...base, platforms: ["beos-x86"] }), /unknown platform/);
  assert.throws(() => latestJson({ ...base, platforms: [] }), /no platform/);
});

test("a bundle without a signature is an error, never an unsigned entry", () => {
  const f = { ...files, "Almend-Desktop-0.2.0-windows-x64-setup.exe.sig": "  " };
  assert.throws(() => latestJson({ ...base, files: f }), /windows-x86_64: .* has no \.sig/);
  const g = { ...files };
  delete g["Almend-Desktop-0.2.0-macos-x64.app.tar.gz.sig"];
  assert.throws(() => latestJson({ ...base, files: g }), /darwin-x86_64/);
});

test("bundles of another version are ignored; a bad version is refused", () => {
  const f = { "Almend-Desktop-0.1.9-linux-x86_64.AppImage": "", "Almend-Desktop-0.1.9-linux-x86_64.AppImage.sig": "s" };
  assert.throws(() => latestJson({ ...base, files: f }), /no platform/);
  assert.throws(() => latestJson({ ...base, version: "v0.2" }), /not a version/);
});

test("exact bundle names only; tag and repo are checked", () => {
  const f = {
    "Almend-Desktop-0.2.0-rc.1-linux-x86_64.AppImage": "",
    "Almend-Desktop-0.2.0-rc.1-linux-x86_64.AppImage.sig": "s",
    "other-0.2.0-linux-x86_64.AppImage": "",
    "other-0.2.0-linux-x86_64.AppImage.sig": "s",
  };
  assert.throws(() => latestJson({ ...base, files: f }), /no platform/);
  assert.throws(() => latestJson({ ...base, tag: "v0.2.1" }), /doesn't match/);
  assert.throws(() => latestJson({ ...base, repo: "o/r/../x?y" }), /not a repository/);
});

test("CLI: a flag without a value fails instead of including every platform", async () => {
  const { execFileSync } = await import("node:child_process");
  const { fileURLToPath } = await import("node:url");
  const script = fileURLToPath(new URL("./latest-json.mjs", import.meta.url));
  for (const args of [
    ["--tag", "v0.2.0", "--dir", ".", "--repo", "o/r", "--platforms"],
    ["--tag", "v0.2.0", "--dir", "--repo", "o/r"],
    ["--tag", "v0.2.0", "--dir", ".", "--repo", "o/r", "--platforms", ""],
  ]) {
    let err = "";
    try {
      execFileSync(process.execPath, [script, ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    } catch (e) {
      err = String(e.stderr);
    }
    assert.match(err, /needs a value/, args.join(" "));
  }
});
