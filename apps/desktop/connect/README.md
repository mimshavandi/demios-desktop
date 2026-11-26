# Connect catalogue (ADR-0018)

`catalogue.json` lists the reviewed tool servers that **Settings › Tools › Connect** offers.
The app reads it only if `catalogue.json.sig`, an Ed25519 signature over its exact bytes, checks
out with a key the build trusts (`apps/desktop/src/connect.rs`). A catalogue that doesn't check
out is refused as a whole: no entries and no older copy.

- **Format:** `format`, `serial` (higher with every published catalogue), `published`, and
  `entries`. Each entry has the service, the vendor, `kind` (only `vendor_remote` for now), the
  server's HTTPS address, its sign-in (`oauth`: the app registers itself, with no client secret),
  the scopes, optional tool risk levels (only stricter than the app's own), the vendor's terms,
  where data goes, and the review (date, the vendor page checked, and `reviewed` or
  `no_longer_reviewed`). `connect.rs` checks every field.
- **Review:** before an entry is listed, the vendor's own docs must confirm that it runs the
  server and how sign-in works, and the vendor's OAuth metadata must allow a client to register
  itself. `review.source` and `review.date` record what was checked.
- **Files:** `catalogue.json.sig` is the owner's signature, made by hand. `catalogue.pub` is the
  owner's public key, which every build compiles in; it is empty until the owner makes one, and
  `build.rs` refuses anything but empty or 64 hex digits. `catalogue.json.test-sig` is the test
  key's signature.
- **Owner key and signing** ([docs/releasing.md](../../../docs/releasing.md)): the private key
  stays on the owner's own hardware, never in the repository or CI. Make it once with
  `node scripts/connect/new-key.mjs <path outside the repo>`, then sign with
  `node scripts/connect/sign-catalogue.mjs <private-key.pem>`. The release workflows run
  `node scripts/connect/check-signature.mjs`, which stops a release whose signature is missing,
  stale or made with another key. While `catalogue.pub` is empty, builds show Connect as not
  available.
- **Test key:** `test-key.pub` is the public half of a throwaway test key; no test private key
  is in the repository. Only debug builds with the `connect-test-key` feature trust it. After
  editing the catalogue, run `node scripts/connect/sign-catalogue.mjs --test`: it makes a new
  key pair, writes `catalogue.json.test-sig` and `test-key.pub`, and drops the private half. A
  test fails until you do.
