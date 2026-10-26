# Rust native-mod ABI sample

This Windows x64 Rust `cdylib` implements the five required 00047 lifecycle exports without depending on the C++ SDK. It is intentionally a no-op: it changes no shell UI and installs no hooks.

Build and package from the repository root on Windows with the x64 MSVC Rust toolchain:

```powershell
cargo build --manifest-path samples/native-mod-rust/Cargo.toml --target x86_64-pc-windows-msvc --release --locked
python tools/package_native_mod.py --manifest samples/native-mod-rust/manifest.json --dll samples/native-mod-rust/target/x86_64-pc-windows-msvc/release/example_rust_native_mod.dll --output dist/rust-native-mod.zip
python tools/validate_native_mod.py dist/rust-native-mod.zip
```

The Windows CI job also loads the DLL and checks its export version and lifecycle transitions. Treat all native mods as unsandboxed native code; implement shutdown and unload checks for every resource a real mod owns.
