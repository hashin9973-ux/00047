fn main() {
    println!("cargo:rerun-if-changed=native/windhawk_blur.cpp");
    println!("cargo:rerun-if-changed=native/signature_verifier.cpp");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        cc::Build::new()
            .cpp(true)
            .file("native/windhawk_blur.cpp")
            .file("native/signature_verifier.cpp")
            .flag_if_supported("/std:c++20")
            .flag_if_supported("/EHsc")
            .compile("windhawk_blur");

        println!("cargo:rustc-link-lib=dylib=d2d1");
        println!("cargo:rustc-link-lib=dylib=dwmapi");
        println!("cargo:rustc-link-lib=dylib=dxguid");
        println!("cargo:rustc-link-lib=dylib=runtimeobject");
        println!("cargo:rustc-link-lib=dylib=windowsapp");
        println!("cargo:rustc-link-lib=dylib=CoreMessaging");
        println!("cargo:rustc-link-lib=dylib=user32");
        println!("cargo:rustc-link-lib=dylib=wintrust");
        println!("cargo:rustc-link-lib=dylib=crypt32");
    }

    tauri_build::build();
}
