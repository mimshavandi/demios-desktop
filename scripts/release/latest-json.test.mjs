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
  "Demios-Desktop-0.2.0-macos-arm64.app.tar.gz": "",
  "Demios-Desktop-0.2.0-macos-arm64.app.tar.gz.sig": "sig-mac-arm\n",
  "Demios-Desktop-0.2.0-macos-x64.app.tar.gz": "",
  "Demios-Desktop-0.2.0-macos-x64.app.tar.gz.sig": "sig-mac-x64",
  "Demios-Desktop-0.2.0-windows-x64-setup.exe": "",
  "Demios-Desktop-0.2.0-windows-x64-setup.exe.sig": "sig-win",
  "Demios-Desktop-0.2.0-linux-x86_64.AppImage": "",
  "Demios-Desktop-0.2.0-linux-x86_64.AppImage.sig": "sig-linux",
  "Demios-Desktop-0.2.0-macos-arm64.dmg": "",
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
    "https://github.com/o/r/releases/download/v0.2.0/Demios-Desktop-0.2.0-windows-x64-setup.exe",
  );
  assert.equal(m.version, "0.2.0");
  assert.equal(m.pub_date, "2027-01-15T10:00:00Z");
});

test("--platforms keeps code-signed platforms only and fails for a missing one", () => {
  const m = latestJson({ ...base, platforms: ["linux-x86_64", "windows-x86_64"] });
  assert.deepEqual(Object.keys(m.platforms).sort(), ["linux-x86_64", "windows-x86_64"]);
  const noLinux = { ...files };
  delete noLinux["Demios-Desktop-0.2.0-linux-x86_64.AppImage"];
  assert.throws(() => latestJson({ ...base, files: noLinux, platforms: ["linux-x86_64"] }), /no update bundle/);
  assert.throws(() => latestJson({ ...base, platforms: ["beos-x86"] }), /unknown platform/);
  assert.throws(() => latestJson({ ...base, platforms: [] }), /no platform/);
});

test("a bundle without a signature is an error, never an unsigned entry", () => {
  const f = { ...files, "Demios-Desktop-0.2.0-windows-x64-setup.exe.sig": "  " };
  assert.throws(() => latestJson({ ...base, files: f }), /windows-x86_64: .* has no \.sig/);
  const g = { ...files };
  delete g["Demios-Desktop-0.2.0-macos-x64.app.tar.gz.sig"];
  assert.throws(() => latestJson({ ...base, files: g }), /darwin-x86_64/);
});

test("bundles of another version are ignored; a bad version is refused", () => {
  const f = { "Demios-Desktop-0.1.9-linux-x86_64.AppImage": "", "Demios-Desktop-0.1.9-linux-x86_64.AppImage.sig": "s" };
  assert.throws(() => latestJson({ ...base, files: f }), /no platform/);
  assert.throws(() => latestJson({ ...base, version: "v0.2" }), /not a version/);
});
