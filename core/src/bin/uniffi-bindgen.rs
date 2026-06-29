//! Library-mode UniFFI binding generator, pinned to this crate's `uniffi` dep.
//!
//! Generate Kotlin bindings from the built cdylib, e.g.:
//!   cargo run --bin uniffi-bindgen -- generate \
//!     --library target/debug/libpigeon_mobile_core.so \
//!     --language kotlin --out-dir target/bindings/kotlin
fn main() {
    uniffi::uniffi_bindgen_main()
}
