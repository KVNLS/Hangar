//! Compiles the vendored Android emulator gRPC proto with protox (pure Rust, no `protoc`).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "proto/emulator_controller.proto";
    println!("cargo:rerun-if-changed={proto}");
    let fds = protox::compile([proto], ["proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .build_transport(false)
        // Upstream comments contain indented snippets that rustdoc would run as doctests.
        .disable_comments(["."])
        .compile_fds(fds)?;
    Ok(())
}
