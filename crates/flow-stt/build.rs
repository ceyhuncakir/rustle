fn main() {
    // A `webgpu` build links Dawn (libwebgpu_dawn.so / .dylib), which ort
    // copies beside each binary; let the examples and tests find it there.
    // src-tauri/build.rs does the same for the app.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        Ok("macos") => println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path"),
        _ => {}
    }
}
