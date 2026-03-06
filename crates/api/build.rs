fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Safety: build scripts are single-threaded; no other threads can observe this.
    unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?); }
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[
                "../../proto/liquidmetal/v1/service.proto",
                "../../proto/liquidmetal/v1/user.proto",
            ],
            &["../../proto"],
        )?;
    Ok(())
}
