# Connecting a Native External Signer

This guide mirrors the shape of Breez's "self signer" flow, but it is written for RLN's current
`NativeExternalSigner` API.

By default, RLN uses its internal signer flow (`init(password, mnemonic)` and `unlock(...)`).
If you do not want RLN to own the node seed/mnemonic directly, you can initialize and unlock the
node with an in-process external signer instead.

Current RLN model:

1. Construct `NativeExternalSigner`
2. Create `SdkNode`
3. Call `init_with_native_external_signer(...)`
4. Call `unlock_with_native_external_signer(...)`

Important constraints:

- `NativeExternalSigner` is available only in bindings generated from the compiled library with
  the `vls` feature enabled.
- It is a convenience in-process signer, not a production-grade seed store.
- The host application still owns the 32-byte seed and passes it to the signer in memory.
- Network strings accepted by `NativeExternalSigner` are: `mainnet`, `bitcoin`, `testnet`,
  `testnet4`, `signet`, and `regtest`.

## Rust

```rust
use std::sync::Arc;

use rgb_lightning_node::{
    NativeExternalSigner, RlnError, SdkInitRequest, SdkNode,
};

fn connect_with_native_external_signer() -> Result<Arc<SdkNode>, RlnError> {
    let seed_hex = "11".repeat(32);

    let signer = NativeExternalSigner::new(
        seed_hex,
        "regtest".to_string(),
        Some(true), // permissive_policy
    )?;

    let node = SdkNode::create(SdkInitRequest {
        storage_dir_path: "/tmp/rln-native-signer-rust".to_string(),
        daemon_listening_port: 3001,
        ldk_peer_listening_port: 9736,
        network: "regtest".to_string(),
        max_media_upload_size_mb: 20,
        enable_virtual_channels_v0: Some(false),
        virtual_peer_pubkeys: None,
        lsp_base_url: None,
        lsp_bearer_token: None,
    })?;

    node.init_with_native_external_signer(signer.clone())?;
    node.unlock_with_native_external_signer(
        signer,
        "user".to_string(),
        "password".to_string(),
        "127.0.0.1".to_string(),
        18443,
        Some("127.0.0.1:50001".to_string()),
        Some("rpc://127.0.0.1:3000/json-rpc".to_string()),
        vec![],
        Some("rust_native_signer".to_string()),
    )?;

    Ok(node)
}
```

## Swift

```swift
import RGBLightningNode

func connectWithNativeExternalSigner() throws -> SdkNode {
    let signer = try NativeExternalSigner(
        seedHex: String(repeating: "11", count: 32),
        network: "regtest",
        permissivePolicy: true
    )

    let node = try SdkNode.create(
        request: SdkInitRequest(
            storageDirPath: "/tmp/rln-native-signer-swift",
            daemonListeningPort: 3001,
            ldkPeerListeningPort: 9736,
            network: "regtest",
            maxMediaUploadSizeMb: 20,
            enableVirtualChannelsV0: false,
            virtualPeerPubkeys: nil,
            lspBaseUrl: nil,
            lspBearerToken: nil
        )
    )

    try node.initWithNativeExternalSigner(signer: signer)
    try node.unlockWithNativeExternalSigner(
        signer: signer,
        bitcoindRpcUsername: "user",
        bitcoindRpcPassword: "password",
        bitcoindRpcHost: "127.0.0.1",
        bitcoindRpcPort: 18443,
        indexerUrl: "127.0.0.1:50001",
        proxyEndpoint: "rpc://127.0.0.1:3000/json-rpc",
        announceAddresses: [],
        announceAlias: "swift_native_signer"
    )

    return node
}
```

## Kotlin

```kotlin
import org.utexo.rgblightningnode.NativeExternalSigner
import org.utexo.rgblightningnode.SdkInitRequest
import org.utexo.rgblightningnode.SdkNode

fun connectWithNativeExternalSigner(): SdkNode {
    val signer = NativeExternalSigner("11".repeat(32), "regtest", true)

    val node = SdkNode.create(
        SdkInitRequest(
            storageDirPath = "/tmp/rln-native-signer-kotlin",
            daemonListeningPort = 3001u,
            ldkPeerListeningPort = 9736u,
            network = "regtest",
            maxMediaUploadSizeMb = 20u,
            enableVirtualChannelsV0 = false,
            virtualPeerPubkeys = null,
            lspBaseUrl = null,
            lspBearerToken = null,
        )
    )

    node.initWithNativeExternalSigner(signer)
    node.unlockWithNativeExternalSigner(
        signer,
        "user",
        "password",
        "127.0.0.1",
        18443u,
        "127.0.0.1:50001",
        "rpc://127.0.0.1:3000/json-rpc",
        emptyList(),
        "kotlin_native_signer",
    )

    return node
}
```

## Python

```python
import rgb_lightning_node as rln


def connect_with_native_external_signer() -> rln.SdkNode:
    signer = rln.NativeExternalSigner("11" * 32, "regtest", True)

    node = rln.SdkNode(
        rln.SdkInitRequest(
            storage_dir_path="/tmp/rln-native-signer-python",
            daemon_listening_port=3001,
            ldk_peer_listening_port=9736,
            network="regtest",
            max_media_upload_size_mb=20,
            enable_virtual_channels_v0=False,
            virtual_peer_pubkeys=None,
            lsp_base_url=None,
            lsp_bearer_token=None,
        )
    )

    node.init_with_native_external_signer(signer)
    node.unlock_with_native_external_signer(
        signer,
        "user",
        "password",
        "127.0.0.1",
        18443,
        "127.0.0.1:50001",
        "rpc://127.0.0.1:3000/json-rpc",
        [],
        "python_native_signer",
    )

    return node
```

## Optional: explicit attach flow

`unlock_with_native_external_signer(...)` is the compact helper. Internally it does:

1. `attach_native_external_signer(signer)`
2. `unlock_with_attached_external_signer(...)`

If you want to manage the steps explicitly, you can do that too:

1. `bootstrap = signer.bootstrap()`
2. `node.init_with_external_signer(bootstrap)`
3. `node.attach_native_external_signer(signer)`
4. `node.unlock_with_attached_external_signer(...)`

This is useful when you want the same shape as a custom `ExternalSignerHost` implementation.

## Bootstrap data

`signer.bootstrap()` returns `SdkExternalSignerBootstrap`:

- `node_id`
- `account_xpub_vanilla`
- `account_xpub_colored`
- `master_fingerprint`
- `protocol_version`
- `api_level`

RLN uses this to validate that the attached signer matches the node identity and key source.

## When to use `NativeExternalSigner`

Use it when:

- you want app-owned seed material
- you want RLN to run with an attached signer instead of internal mnemonic handling
- you want an in-process reference signer for tests, demos, or controlled integrations

Do not treat it as:

- a standalone production signer SDK
- a hardware-wallet abstraction
- a durable secure seed vault

Today it is best understood as RLN's built-in reference implementation of an external signer host.

## Related code

- `src/uniffi_api/native_signer.rs`
- `src/uniffi_api/mod.rs`
- `src/uniffi_api/README.md`
- `test/swift-e2e/Tests/SwiftUniffiE2ETests/SwiftExternalSignerSmokeTests.swift`
- `test/kotlin-external-signer-smoke/ExternalSignerSmoke.kt`
- `src/uniffi_api/examples/python-interop/manual_py_external_signer_e2e.py`
