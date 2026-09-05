//! Keeps the macOS library identity independent of the builder's checkout path.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libseekdeep_python_sdk_ffi.dylib"
        );
    }
}
