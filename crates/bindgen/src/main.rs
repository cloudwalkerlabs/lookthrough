//! uniffi's binding generator, for the Kotlin bindings of
//! `lookthrough-ffi`. The Android build runs it in library mode.

fn main() {
    uniffi::uniffi_bindgen_main()
}
