fn main() {
    #[cfg(feature = "uniffi")]
    {
        // A shared target cache may serve upstream and maintained overlay checkouts.
        // Resolve the active checkout when the script runs: Cargo can reuse a
        // compiled build script from another checkout with the same package ID.
        // A compile-time env! would retain that checkout's incompatible UDL path.
        let udl = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("missing Cargo manifest directory"),
        )
        .join("bindings/rgb_lightning_node.udl");
        println!("cargo:rerun-if-changed={}", udl.display());
        uniffi::generate_scaffolding(udl.to_str().expect("UDL path is not valid UTF-8")).expect(
            "failed to generate UniFFI scaffolding from bindings/rgb_lightning_node.udl; \
             verify the UDL syntax and that the bindings path exists",
        );
    }
}
