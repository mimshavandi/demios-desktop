// node --test packaging/msix/  (runs on any OS; packing itself needs Windows)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  ASSETS,
  bundleResources,
  checkStoreFeatures,
  manifestValues,
  msixVersion,
  renderManifest,
  resolveIdentity,
  storeBuildEnv,
  VULKAN_CONF,
} from "./build-msix.mjs";

const here = fileURLToPath(new URL(".", import.meta.url));
const conf = JSON.parse(readFileSync(join(here, "..", "..", "apps", "desktop", "tauri.conf.json"), "utf8"));
const template = readFileSync(join(here, "AppxManifest.xml.in"), "utf8");
const placeholders = JSON.parse(readFileSync(join(here, "identity.json"), "utf8"));
const real = {
  MSIX_IDENTITY_NAME: "12345DigiNoteInc.DemiosDesktop",
  MSIX_PUBLISHER: "CN=1A2B3C4D-1111-2222-3333-444455556666",
  MSIX_PUBLISHER_DISPLAY_NAME: "DigiNote Inc.",
};

test("versions become four numbers with a zero revision", () => {
  assert.equal(msixVersion("0.1.0"), "0.1.0.0");
  assert.equal(msixVersion("12.34.56"), "12.34.56.0");
  assert.throws(() => msixVersion("1.0.0-beta.1"), /pre-release/);
  assert.throws(() => msixVersion("1.0"), /major\.minor\.patch/);
  assert.throws(() => msixVersion("1.70000.0"), /65535/);
  assert.doesNotThrow(() => msixVersion(conf.version));
});

test("the placeholder identity builds a test package but not a store one", () => {
  assert.equal(resolveIdentity(placeholders, {}).identityName, placeholders.identityName);
  assert.throws(() => resolveIdentity(placeholders, {}, { store: true }), /identityName is still the placeholder/);
  assert.throws(() => resolveIdentity(placeholders, {}, { store: true }), /publisher is still the placeholder/);
  const id = resolveIdentity(placeholders, real, { store: true });
  assert.equal(id.identityName, real.MSIX_IDENTITY_NAME);
  assert.equal(id.publisher, real.MSIX_PUBLISHER);
  assert.equal(id.displayName, "Demios Desktop");
});

test("bad identity values are refused", () => {
  assert.throws(() => resolveIdentity(placeholders, { MSIX_PUBLISHER: "DigiNote Inc." }), /CN=/);
  assert.throws(() => resolveIdentity(placeholders, { MSIX_IDENTITY_NAME: "has space" }), /identity name/);
  assert.throws(() => resolveIdentity(placeholders, { MSIX_IDENTITY_NAME: "x".repeat(51) }), /identity name/);
});

test("the app is built as the MSIX distribution, with its licences list", () => {
  const env = storeBuildEnv({ PATH: "x", DEMIOS_DISTRIBUTION: "direct" });
  assert.equal(env.DEMIOS_DISTRIBUTION, "msix");
  assert.equal(env.DEMIOS_REQUIRE_LICENSES, "1");
  assert.equal(env.PATH, "x");
});

test("a store build refuses a self-updater", () => {
  assert.throws(() => checkStoreFeatures(["custom-protocol", "updater"]), /must not self-update/);
  assert.doesNotThrow(() => checkStoreFeatures([]));
  assert.doesNotThrow(() => checkStoreFeatures(["custom-protocol"]));
});

test("the manifest is filled, escaped and declares only the agreed capabilities", () => {
  const values = manifestValues(conf, resolveIdentity(placeholders, real));
  const xml = renderManifest(template, { ...values, DISPLAY_NAME: "A & B <x>" });
  assert.match(xml, /Name="12345DigiNoteInc\.DemiosDesktop"/);
  assert.match(xml, /Publisher="CN=1A2B3C4D-1111-2222-3333-444455556666"/);
  assert.match(xml, /Version="0\.1\.0\.0"/);
  assert.match(xml, /Executable="demios-desktop\.exe"/);
  assert.match(xml, /<DisplayName>A &amp; B &lt;x&gt;<\/DisplayName>/);
  assert.match(xml, /EntryPoint="Windows\.FullTrustApplication"/);
  assert.match(xml, /<uap:Protocol Name="demios">/);
  const caps = [...xml.matchAll(/<(?:\w+:)?Capability Name="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(caps.sort(), ["internetClient", "runFullTrust"]);
  assert.doesNotMatch(xml.replace(/<!--[\s\S]*?-->/g, ""), /\{\{|\}\}/);
});

test("a missing manifest value fails", () => {
  assert.throws(() => renderManifest(template, {}), /missing/);
});

test("every asset the manifest names has variants", () => {
  for (const base of ["Square44x44Logo", "Square150x150Logo", "Wide310x150Logo", "StoreLogo"]) {
    assert.match(template, new RegExp(`Assets\\\\${base}\\.png`));
    assert.ok(ASSETS.some(([f]) => f.startsWith(`${base}.scale-100`)), base);
    assert.ok(ASSETS.some(([f]) => f.startsWith(`${base}.scale-200`)), base);
  }
});

test("a vulkan build lays out the Vulkan loader next to the exe, as NSIS does", () => {
  const appDir = join(here, "..", "..", "apps", "desktop");
  const conf = JSON.parse(readFileSync(join(appDir, "tauri.conf.json"), "utf8"));
  const vulkan = JSON.parse(readFileSync(join(appDir, VULKAN_CONF), "utf8"));
  const plain = bundleResources(conf, [], vulkan);
  assert.ok(!Object.values(plain).includes("vulkan-1.dll"));
  const gpu = bundleResources(conf, ["vulkan"], vulkan);
  assert.equal(gpu["../../target/vulkan-loader/vulkan-1.dll"], "vulkan-1.dll");
  // The app's own resources are still there.
  for (const k of Object.keys(plain)) assert.ok(k in gpu, k);
});
