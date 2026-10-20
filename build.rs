fn main() {
    #[cfg(feature = "uniffi")]
    {
        // A shared target cache may serve upstream and maintained overlay checkouts.
        // Embed this source identity in the build-script compilation and track the
        // absolute UDL input, so older file mtimes cannot reuse another checkout's ABI.
        const UDL: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/bindings/rgb_lightning_node.udl"
        );
        println!("cargo:rerun-if-changed={UDL}");
        uniffi::generate_scaffolding(UDL).expect(
            "failed to generate UniFFI scaffolding from bindings/rgb_lightning_node.udl; \
             verify the UDL syntax and that the bindings path exists",
        );
    }
}
