//! Compiles the vendored Signalsmith Stretch (header-only C++, MIT) with a small C
//! wrapper. Plain `cc`, no bindgen: the few declarations are written by hand in lib.rs.

fn main() {
    println!("cargo:rerun-if-changed=src/wrapper.cpp");
    println!("cargo:rerun-if-changed=vendor");
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .file("src/wrapper.cpp")
        .include("vendor")
        .include("vendor/signalsmith-stretch")
        .flag_if_supported("-O3")
        .warnings(false)
        .compile("dap_stretch_cpp");
}
