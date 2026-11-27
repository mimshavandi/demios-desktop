//! Writes the Swift bindings for dn-mobile (apps/ios/build-xcframework.sh). A build tool,
//! built only with `--features bindgen`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
