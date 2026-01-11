use std::fs;
use std::path::PathBuf;
use tonic_prost_build::configure;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);

    // Create proto directory structure in OUT_DIR
    let proto_dir = out_dir.join("pkg/lib/pb/model/protos");
    fs::create_dir_all(&proto_dir)?;

    // Pre-process: Fix naming conflicts in models.proto
    // The proto has `oneof content` + `message Content` in the same scope,
    // which causes prost to generate both `enum Content` and `struct Content`.
    // Rename the oneofs to avoid the conflict.
    let models_proto = fs::read_to_string("./pb/pkg/lib/pb/model/protos/models.proto")?;
    let models_proto = models_proto
        // In Block message: rename `oneof content` to `oneof content_value`
        .replace("oneof content {", "oneof content_value {")
        // In Metadata message: rename `oneof payload` to `oneof payload_value`
        .replace(
            "oneof payload {\n        Payload.IdentityPayload",
            "oneof payload_value {\n        Payload.IdentityPayload",
        );
    fs::write(proto_dir.join("models.proto"), models_proto)?;

    // Copy localstore.proto (it imports models.proto)
    fs::copy(
        "./pb/pkg/lib/pb/model/protos/localstore.proto",
        proto_dir.join("localstore.proto"),
    )?;

    // Stage 1: Compile both model protos
    configure()
        .build_client(false)
        .build_server(false)
        .compile_protos(
            &[
                proto_dir.join("models.proto").to_str().unwrap(),
                proto_dir.join("localstore.proto").to_str().unwrap(),
            ],
            &[&out_dir.to_string_lossy()],
        )?;

    // Stage 2: Compile the service protos, using extern_path to reference
    // the model types we just generated
    configure()
        .build_client(true)
        .build_server(false)
        .extern_path(".anytype.model", "crate::model")
        .compile_protos(
            &[
                "./pb/protos/service/service.proto",
                "./pb/protos/commands.proto",
                "./pb/protos/events.proto",
                "./pb/protos/snapshot.proto",
                "./pb/protos/changes.proto",
            ],
            &[".", "./pb"],
        )?;

    Ok(())
}
