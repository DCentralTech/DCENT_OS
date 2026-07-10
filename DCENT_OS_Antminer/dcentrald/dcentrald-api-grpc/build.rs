// Supremacy S5.1 — tonic-build generates Rust prost types + tonic service
// scaffolding from `proto/dcent_v1.proto`. The generated code lands in
// `OUT_DIR/dcent.v1.rs` and is consumed by `lib.rs` via `tonic::include_proto!`.
//
// `file_descriptor_set_path` emits the binary FileDescriptorSet that
// tonic-reflection consumes at runtime so clients can do server reflection
// (grpcurl, postman) without distributing the .proto file.

use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use the vendored `protoc` binary so the build works without an apt-get
    // install on Docker / a chocolatey install on Windows / etc. The
    // `protoc-bin-vendored` crate ships the binary in the build sysroot.
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    env::set_var("PROTOC", &protoc);

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let descriptor_path = out_dir.join("dcent_v1_descriptor.bin");

    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(&["proto/dcent_v1.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/dcent_v1.proto");
    Ok(())
}
