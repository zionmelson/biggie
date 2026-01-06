use std::{env, path::PathBuf};

// Geyser gRPC plugin protos (from local validator)
const GEYSER_PROTOS: &[&str] = &[
    "geyser/confirmed_block.proto",
    "geyser/entries.proto",
    "geyser/geyser.proto",
    "geyser/transaction_by_addr.proto",
];

// Jito protos for bundle submission
const JITO_PROTOS: &[&str] = &[
    "jito/auth.proto",
    "jito/block_engine.proto",
    "jito/block.proto",
    "jito/bundle.proto",
    "jito/packet.proto",
    "jito/relayer.proto",
    "jito/searcher.proto",
    "jito/shared.proto",
    "jito/shredstream.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=geyser");
    println!("cargo:rerun-if-changed=jito");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let proto_out = out_dir.clone();

    // Build Geyser protos first (primary data source)
    let geyser_cfg = tonic_build::configure()
        .build_server(false)
        .out_dir(&proto_out)
        .compile_well_known_types(true)
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .extern_path(".google.protobuf.Timestamp", "::prost_types::Timestamp");

    let geyser_includes = vec![PathBuf::from("geyser")];
    geyser_cfg.compile_protos(GEYSER_PROTOS, &geyser_includes)?;

    // Build Jito protos (bundle submission)
    let jito_cfg = tonic_build::configure()
        .build_server(false)
        .out_dir(&proto_out)
        .compile_well_known_types(true)
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .extern_path(".google.protobuf.Timestamp", "::prost_types::Timestamp");

    let jito_includes = vec![PathBuf::from("jito")];
    jito_cfg.compile_protos(JITO_PROTOS, &jito_includes)?;

    Ok(())
}
