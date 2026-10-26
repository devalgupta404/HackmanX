// Compiles the BPF program to an object file when the `ebpf` feature is on. Without the feature
// (the default), this does nothing, so the crate builds as pure Rust with no clang dependency.
#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/bpf/tracker.bpf.c");
    if std::env::var_os("CARGO_FEATURE_EBPF").is_none() {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set during build"));
    let object = out_dir.join("tracker.bpf.o");
    let clang = std::env::var("CLANG").unwrap_or_else(|_| "clang".to_string());

    let status = Command::new(&clang)
        .args(["-target", "bpf", "-D__TARGET_ARCH_x86", "-O2", "-g", "-c"])
        .arg("src/bpf/tracker.bpf.c")
        .arg("-o")
        .arg(&object)
        .status()
        .expect("running clang to compile the BPF program");
    if !status.success() {
        panic!("clang failed to compile src/bpf/tracker.bpf.c (status {status})");
    }
}
