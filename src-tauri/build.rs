fn main() {
    // A `webgpu` build links Dawn (libwebgpu_dawn.so / .dylib) as a shared
    // library that ships with the app: beside the program for
    // scripts/install-app.sh and in /usr/lib/flow for the deb, rpm and
    // AppImage on Linux, in Contents/Frameworks in the macOS bundle. Windows
    // looks beside the .exe on its own.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/flow"),
        Ok("macos") => {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path");
        }
        _ => {}
    }
    tauri_build::build()
}
