//! Compiles the vendored Signalsmith Stretch (header-only C++, MIT) with a small C
//! wrapper. Plain `cc`, no bindgen: the few declarations are written by hand in lib.rs.

fn main() {
    println!("cargo:rerun-if-changed=src/wrapper.cpp");
    println!("cargo:rerun-if-changed=vendor");
    let android = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "android");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++14")
        .file("src/wrapper.cpp")
        .include("vendor")
        .include("vendor/signalsmith-stretch")
        .flag_if_supported("-O3")
        .warnings(false);
    if android {
        // cc would link libc++_shared.so, which the APK does not ship: link the C++
        // runtime statically into libdap_api.so instead (passed to the linker only,
        // which finds it in the NDK sysroot).
        build.cpp_link_stdlib(None);
    }
    build.compile("dap_stretch_cpp");
    if android {
        println!("cargo:rustc-link-lib=static:-bundle=c++_static");
        println!("cargo:rustc-link-lib=static:-bundle=c++abi");
    }
}
