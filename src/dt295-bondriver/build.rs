//! Builds the C++ IBonDriver2 shim (cpp/bonshim.cpp) with MSVC: TVTest
//! `dynamic_cast`s the object `CreateBonDriver` returns to `IBonDriver2`, which needs
//! a real MSVC class with RTTI (and thiscall on x86).
//!
//! The shim becomes a static library whose `bonshim_new` (referenced by the crate's
//! `CreateBonDriver`) pulls the shim object into the DLL. RTTI (/GR) is on; /MT vs
//! /MD follows the target's `crt-static` feature (the workspace sets +crt-static),
//! which `cc` reads itself.
//!
//! Also the version of the build: `DT295_VERSION`, and the resources of the DLL and of
//! bontest.exe.

#[path = "../version.rs"]
mod version;

fn main() {
    let v = version::version();
    version::resource(&v, "BonDriver_DT295.dll", "PIX-DT295 BonDriver");
    version::resource(&v, "bontest.exe", "PIX-DT295 BonDriver test host");

    println!("cargo:rerun-if-changed=cpp/bonshim.cpp");
    println!("cargo:rerun-if-changed=cpp/bonshim_test.cpp");
    println!("cargo:rerun-if-changed=cpp/bonhost.cpp");
    println!("cargo:rerun-if-changed=cpp/IBonDriver2.h");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        // The DLL exists only for Windows/MSVC; elsewhere the pure parts still
        // build and test (src/ffi.rs is compiled out).
        return;
    }

    // bonshim_test.cpp (a TVTest-like caller for the unit tests) and bonhost.cpp (the
    // vtable calls of the bontest tool) are separate objects of the library: only the
    // test executable / bontest reference them, so the DLL never links them.
    cc::Build::new()
        .cpp(true)
        .file("cpp/bonshim.cpp")
        .file("cpp/bonshim_test.cpp")
        .file("cpp/bonhost.cpp")
        .include("cpp")
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("NOMINMAX", None)
        .flag("/EHsc")
        .flag("/GR")
        .warnings(true)
        .compile("bonshim");
}
