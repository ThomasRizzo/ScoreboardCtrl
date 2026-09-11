use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let memory = if env::var("CARGO_FEATURE_OTA").is_ok() {
        println!("cargo:rerun-if-changed=memory-app.x");
        include_bytes!("memory-app.x").as_slice()
    } else {
        println!("cargo:rerun-if-changed=memory-standalone.x");
        include_bytes!("memory-standalone.x").as_slice()
    };
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(memory)
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
