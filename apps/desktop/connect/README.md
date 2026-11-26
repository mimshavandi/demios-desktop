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
- **Signing:** after any change, sign again with a new throwaway test key:
  `node scripts/connect/sign-catalogue.mjs --test`. It writes `catalogue.json.sig` and the key's
  public half to `test-key.pub`, and keeps no private key. A test fails if the signature no
  longer matches.
- **Keys:** `test-key.pub` is the public half of a throwaway **test** key; no private key is in
  the repository. Only debug builds with the `connect-test-key` feature trust it. Other builds trust no key yet, so Connect says it isn't
  available. The release key and who signs with it are still to be decided by the owner.
