//! Build script for sky-proto.
//!
//! Compiles the project's .proto files into Rust types via tonic-build.
//! Invoked automatically by Cargo before compiling this crate's source.
//! Generated output lands in OUT_DIR and is included via the
//! tonic::include_proto! macro in src/lib.rs.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Tell Cargo to re-run this build script if any .proto file changes.
    // Without this, changes to schema files wouldn't trigger regeneration.
    println!("cargo:rerun-if-changed=../../proto");

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                "../../proto/hello.proto",
                "../../proto/worker_control.proto",
            ],
            &["../../proto"],
        )?;

    Ok(())
}