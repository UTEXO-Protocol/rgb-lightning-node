# RLN - RGB Lightning Node

RGB-enabled LN node daemon ported from [rgb-lightning-sample], which is based
on [ldk-sample].

The node enables the possibility to create payment channels containing assets
issued using the RGB protocol, as well as routing RGB asset denominated
payments across multiple channels, given that they all possess the necessary
liquidity. In this way, RGB assets can be transferred with the same user
experience and security assumptions of regular Bitcoin Lightning Network
payments. This is achieved by adding to each lightning commitment transaction a
dedicated extra output containing the anchor to the RGB state transition.

More context on how RGB works on the Lightning Network can be found
[here](https://docs.rgb.info/lightning-network-compatibility).

The RGB functionality for now can be tested only in regtest or testnet
environments, but an advanced user may be able to apply changes in order to use
it also on other networks.
Please be careful, this software is early alpha, we do not take any
responsibility for loss of funds or any other issue you may encounter.

Also note that [rust-lightning] has been changed in order to support RGB
channels,
[here](https://github.com/RGB-Tools/rust-lightning/compare/v0.2...rgb)
a comparison with `v0.2`, the version we applied the changes to.

## Install

Clone the project, including (shallow) submodules:
```sh
git clone https://github.com/RGB-Tools/rgb-lightning-node --recurse-submodules --shallow-submodules
```

Then, from the project root, install the `rgb-lightning-node` binary by
running:
```sh
cargo install --locked --path .
```

The docker image can be built with:
```sh
docker build -t rgb-lightning-node .
```

## UniFFI Bindings

REST is **not** part of the SDK surface. REST endpoints remain a compatibility
wrapper for existing integrations and tests. The SDK surface is exposed through
the Rust library + UniFFI bindings.

Current lifecycle/threading model:
- UniFFI API is single-node-per-process (global state slot).
- State is registered by daemon startup and cleared on daemon shutdown.
- Use `uniffiIsInitialized()` from generated bindings to check readiness before SDK calls.
- Calls made before registration return `NotInitialized`.

Build with UniFFI enabled:
```sh
cargo check --features uniffi
```

Generate all bindings:
```sh
./scripts/uniffi_bindgen_generate.sh
```

Generate per language:
```sh
./scripts/uniffi_bindgen_generate_kotlin.sh
./scripts/uniffi_bindgen_generate_kotlin_android.sh
./scripts/uniffi_bindgen_generate_python.sh
./scripts/uniffi_bindgen_generate_swift.sh
```

Quick SDK smoke tests:
```sh
cargo test --features uniffi --lib uniffi_smoke_tests:: -- --test-threads=1
```

Kotlin/JVM + Rust UniFFI smoke test (local toolchain):
```sh
./scripts/kotlin_uniffi_smoke.sh
```

Kotlin/JVM + Rust UniFFI smoke test (Docker):
```sh
./scripts/docker_kotlin_uniffi_smoke.sh
```

Kotlin/JVM manual UniFFI checks (error mapping + custom type validation):
```sh
./scripts/kotlin_manual_test.sh
```

Faster rerun when artifacts are already built:
```sh
SKIP_BUILD=1 SKIP_BINDINGS=1 ./scripts/kotlin_manual_test.sh
```

Kotlin/Android manual artifact checks (requires `ANDROID_NDK_HOME` + `cargo-ndk`):
```sh
./scripts/kotlin_android_manual_test.sh
```

Kotlin/Android manual artifact checks in Docker:
```sh
./scripts/docker_kotlin_android_manual_test.sh
```

Swift manual checks on macOS (host smoke + iOS XCFramework packaging):
```sh
./scripts/swift_manual_test.sh
```

If you hit CMake generator/cache conflicts, force a clean Android target dir:
```sh
CLEAN_ANDROID_TARGET=1 ./scripts/kotlin_android_manual_test.sh
# or in docker:
docker run --rm -v "$PWD:/work" -w /work rln-kotlin-android-uniffi:local \
  bash -lc "source /usr/local/cargo/env && CLEAN_ANDROID_TARGET=1 ./scripts/kotlin_android_manual_test.sh"
```

If needed, override target dir explicitly to guarantee no old CMake cache is reused:
```sh
CARGO_TARGET_DIR="$PWD/target/android-uniffi-make-fresh" ./scripts/docker_kotlin_android_manual_test.sh
```

Local Android NDK setup (Ubuntu, CLI):
```sh
# 1) Install Java + Rust helper
sudo apt-get update
sudo apt-get install -y openjdk-17-jdk unzip cmake clang pkg-config build-essential ninja-build
cargo install cargo-ndk --version 3.5.4 --locked
cargo install bindgen-cli --version 0.71.1 --locked

# 2) Install Android cmdline-tools (pick your own SDK root if needed)
export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
mkdir -p "$ANDROID_SDK_ROOT/cmdline-tools"
cd /tmp
wget https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip -O cmdline-tools.zip
unzip -q cmdline-tools.zip
mkdir -p "$ANDROID_SDK_ROOT/cmdline-tools/latest"
mv cmdline-tools/* "$ANDROID_SDK_ROOT/cmdline-tools/latest/"

# 3) Install NDK + build tools used by local checks/CI
"$ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager" --licenses
"$ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager" \
  "ndk;26.3.11579264" "platform-tools" "build-tools;34.0.0"

# 4) Export env vars for this shell
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/26.3.11579264"
export PATH="$ANDROID_SDK_ROOT/cmdline-tools/latest/bin:$ANDROID_SDK_ROOT/platform-tools:$PATH"
```

Then run:
```sh
./scripts/kotlin_android_manual_test.sh
```

Parity tests used in CI:
```sh
cargo test zero_amount_invoice -- --test-threads=1
cargo test send_receive -- --test-threads=1
```

CI artifact packaging workflow:
- `.github/workflows/uniffi-artifacts.yaml`
- Artifacts produced:
  - Swift: `RGBLightningNode.xcframework`
  - Kotlin Android: `jniLibs` + generated Kotlin sources
  - Python: generated module + host shared library bundle

## Run

In order to operate, the node will need:
- a bitcoind node
- an indexer instance (electrum or esplora)
- an [RGB proxy server] instance

Once services are running, daemons can be started.
Each daemon needs to be started in a separate shell with `rgb-lightning-node`,
specifying:
- bitcoind user, password, host and port
- node data directory
- node listening port
- LN peer listening port
- network

### Regtest

To easily start the required services on a regtest network, run:
```sh
./regtest.sh start
```

This command will create the directories needed by the services, start the
docker containers and mine some blocks. The test environment will always start
in a clean state, taking down previous running services (if any) and
re-creating data directories.

Here's an example of how to start three regtest nodes, each one using the
shared regtest services provided by docker compose:
```sh
# 1st shell
rgb-lightning-node dataldk0/ --daemon-listening-port 3001 \
    --ldk-peer-listening-port 9735 --network regtest \
    --disable-authentication

# 2nd shell
rgb-lightning-node dataldk1/ --daemon-listening-port 3002 \
    --ldk-peer-listening-port 9736 --network regtest \
    --disable-authentication

# 3rd shell
rgb-lightning-node dataldk2/ --daemon-listening-port 3003 \
    --ldk-peer-listening-port 9737 --network regtest \
    --disable-authentication
```

To instead run node in docker use the following template:
```sh
docker run \
    --rm -it \
    -p 3001:3001 \
    -v RLNdata1:/RLNdata \
    --network rgb-lightning-node_default \
    rgb-lightning-node \
        --daemon-listening-port 3001 \
        --ldk-peer-listening-port 9735 \
        --network regtest \
        --disable-authentication \
        RLNdata
```
Note: this persists data across runs in the `RLNdata1` volume; to start from
scratch delete it with `docker volume rm RLNdata1`

To send some bitcoins to a node, first get a bitcoin address with the POST
`/address` API, then run:
```sh
./regtest.sh sendtoaddress <address> <amount>
```

To mine, run:
```sh
./regtest.sh mine <blocks>
```

To stop running services and to cleanup data directories, run:
```sh
./regtest.sh stop
```

For more info about regtest utility commands, run:
```sh
./regtest.sh -h
```

When unlocking regtest nodes use the following local services:
- bitcoind_rpc_username: user
- bitcoind_rpc_password: password
- bitcoind_rpc_host: localhost
- bitcoind_rpc_port: 18433
- indexer_url: 127.0.0.1:50001
- proxy_endpoint: rpc://127.0.0.1:3000/json-rpc

To unlock a regtest nodes running in docker use the following local services:
- bitcoind_rpc_username: user
- bitcoind_rpc_password: password
- bitcoind_rpc_host: bitcoind
- bitcoind_rpc_port: 18433
- indexer_url: electrs:50001
- proxy_endpoint: rpc://proxy:3000/json-rpc

### Testnet

#### Testnet3

When running the node on the testnet3 network the docker services are not needed
because the node will use some public services.

Here's an example of how to start three testnet3 nodes, each one using the
external testnet3 services:

```sh
# 1st shell
rgb-lightning-node dataldk0/ --daemon-listening-port 3001 \
    --ldk-peer-listening-port 9735 --network testnet \
    --disable-authentication

# 2nd shell
rgb-lightning-node dataldk1/ --daemon-listening-port 3002 \
    --ldk-peer-listening-port 9736 --network testnet \
    --disable-authentication

# 3rd shell
rgb-lightning-node dataldk2/ --daemon-listening-port 3003 \
    --ldk-peer-listening-port 9737 --network testnet \
    --disable-authentication
```

When unlocking testnet3 nodes you can use the following services:
- bitcoind_rpc_username: user
- bitcoind_rpc_username: password
- bitcoind_rpc_host: electrum.iriswallet.com
- bitcoind_rpc_port: 18332
- indexer_url: ssl://electrum.iriswallet.com:50013
- proxy_endpoint: rpcs://proxy.iriswallet.com/0.2/json-rpc

#### Testnet4

To run testnet4 use the same options as testnet3 except for:
- CLI arg: `--network testnet4`
- bitcoind_rpc_port: 18443
- indexer_url: ssl://electrum.iriswallet.com:50053

## Use

Once daemons are running, they can be operated via REST JSON APIs.

For example, using curl:
```bash
curl -X POST -H "Content-type: application/json" \
    -d '{"ticker": "USDT", "name": "Tether", "amounts": [666], "precision": 0}' \
    http://localhost:3001/issueasset
```

The node currently exposes the following APIs:
- `/address` (POST)
- `/assetbalance` (POST)
- `/assetmetadata` (POST)
- `/backup` (POST)
- `/btcbalance` (POST)
- `/changepassword` (POST)
- `/checkindexerurl` (POST)
- `/checkproxyendpoint` (POST)
- `/closechannel` (POST)
- `/connectpeer` (POST)
- `/createutxos` (POST)
- `/decodelninvoice` (POST)
- `/decodergbinvoice` (POST)
- `/disconnectpeer` (POST)
- `/estimatefee` (POST)
- `/failtransfers` (POST)
- `/getassetmedia` (POST)
- `/getchannelid` (POST)
- `/getpayment` (POST)
- `/getswap` (POST)
- `/init` (POST)
- `/invoicestatus` (POST)
- `/issueassetcfa` (POST)
- `/issueassetnia` (POST)
- `/issueassetuda` (POST)
- `/keysend` (POST)
- `/listassets` (POST)
- `/listchannels` (GET)
- `/listpayments` (GET)
- `/listpeers` (GET)
- `/listswaps` (GET)
- `/listtransactions` (POST)
- `/listtransfers` (POST)
- `/listunspents` (POST)
- `/lninvoice` (POST)
- `/lock` (POST)
- `/makerexecute` (POST)
- `/makerinit` (POST)
- `/networkinfo` (GET)
- `/nodeinfo` (GET)
- `/openchannel` (POST)
- `/postassetmedia` (POST)
- `/refreshtransfers` (POST)
- `/restore` (POST)
- `/revoketoken` (POST)
- `/rgbinvoice` (POST)
- `/sendbtc` (POST)
- `/sendonionmessage` (POST)
- `/sendpayment` (POST)
- `/sendrgb` (POST)
- `/shutdown` (POST)
- `/signmessage` (POST)
- `/sync` (POST)
- `/taker` (POST)
- `/unlock` (POST)

To get more details about the available APIs see the [OpenAPI specification].
A Swagger UI for the `master` branch is generated from the specification and
available at https://rgb-tools.github.io/rgb-lightning-node.
Otherwise you can can browse a local copy exposing it with a web server.  As a
quick example, from the project root you can run:
```bash
python3 -m http.server
```
Then point a browser to `http://localhost:8000`.

If a daemon is running on your machine on one of the example ports
given above, you can even call the APIs directly from the Swagger UI.

To stop the daemon, exit with the `/shutdown` API (or press `Ctrl+C`).

### Authentication

RLN provides API authentication via [Biscuit tokens].

#### One-time setup

First, generate a root keypair. This keypair is your issuer key: the private
half signs new tokens, and the public half is shared with your node so it can
verify them.

```sh
# install the biscuit CLI (or download a prebuilt binary from the Biscuit releases page)
cargo install biscuit-cli

# generate a root keypair (prints both keys)
biscuit keypair

# alternatively, you can export just the private key
biscuit keypair --only-private-key > private-key-file
# and later derive the public key from it
biscuit keypair --from-file private-key-file --only-public-key
```

Save the private key in a secure way (e.g. in a secret manager).

When starting the node, pass the public key with:
```sh
--root-public-key <public_key>
```
To **disable** authentication provide the explicit `--disable-authentication`
arg and do not provide any key.

#### Minting tokens

You can now create Biscuit tokens that will allow calling the authenticated
APIs.

Tokens must carry a **role**, these are the available roles:

- **admin** token (full access):
    ```sh
    echo 'role("admin");' \
      | biscuit generate --private-key-file private-key-file -
    ```

- **read-only** token (allows access only to endpoints that do not make any
  write operations):
    ```sh
    echo 'role("read-only");' \
      | biscuit generate --private-key-file private-key-file -
    ```
- **custom** token (allows access only to the specified API paths), for
  example:
    ```sh
    echo 'role("custom");
          right("api", "/nodeinfo");
          right("api", "/networkinfo");' \
      | biscuit generate --private-key-file private-key-file -
    ```

Tokens can also carry an **expiry** date. Add a `check` clause to enforce
expiration, for example:
```sh
echo 'role("admin");
      check if time($t), $t <= 2025-08-30T00:00:00Z;' \
  | biscuit generate --private-key-file private-key-file -
```

#### Using tokens

All authenticated requests must include the Biscuit token in the
`Authorization` header:

```sh
curl -H "Authorization: Bearer <token>" [...] http://<node-address>/networkinfo
```

In the Swagger UI you can add the token by clicking the Authorize button (lock
icon) at the top right, pasting the token and clicking Authorize.

#### Revoking tokens

A token can be revoked before its expiration.
When you revoke a token, the node will reject any future request carrying that
token.
The node exposes a `/revoketoken` endpoint for this purpose.
Internally, the node extracts the token’s revocation identifiers and adds them
to its revocation list. Every request checks this list before authenticating.

## Test

Tests for a few scenarios using the regtest network are included. The same
services and data directories as the regtest.sh script are used, so the two
cannot run at the same time.

Tests can be executed with:
```sh
cargo test
```

## Maintenance

`rgb-lightning-node` is a fork and we keep feature parity with upstream.
UniFFI support adds a required manual sync checklist when upstream changes:

1. Sync upstream changes into this fork.
2. Re-check SDK/REST parity for changed endpoints in `src/routes.rs` and `src/sdk/mod.rs`.
3. Update `bindings/rgb_lightning_node.udl` for any public API shape changes.
4. Add/update converters in `src/ffi/types.rs` for new structured identifiers.
5. Regenerate bindings:
   - `./scripts/uniffi_bindgen_generate.sh`
6. Re-run required tests:
   - `cargo test -- --test-threads=1`
   - `cargo test --features uniffi --lib uniffi_smoke_tests:: -- --test-threads=1`
   - `cargo test zero_amount_invoice -- --test-threads=1`
   - `cargo test send_receive -- --test-threads=1`

## Release checklist

1. Run Rust checks:
   - `cargo check`
   - `cargo check --features uniffi`
2. Run core tests:
   - `cargo test -- --test-threads=1`
   - `cargo test --features uniffi --lib uniffi_smoke_tests:: -- --test-threads=1`
3. Regenerate bindings and verify changed output:
   - `./scripts/uniffi_bindgen_generate.sh`
4. Ensure CI workflows pass:
   - `.github/workflows/test.yaml`
   - `.github/workflows/uniffi-artifacts.yaml`

## Projects using RLN

Here is a list of projects using RLN, in alphabetical order:
- [Iris Wallet desktop]
- [KaleidoSwap]
- [Lnfi]
- [Spectrum]
- [Thunderstack]
- [Tiramisu Wallet]


[Biscuit tokens]: https://www.biscuitsec.org/
[RGB proxy server]: https://github.com/RGB-Tools/rgb-proxy-server
[ldk-sample]: https://github.com/lightningdevkit/ldk-sample
[OpenAPI specification]: /openapi.yaml
[rgb-lightning-sample]: https://github.com/RGB-Tools/rgb-lightning-sample
[rust-lightning]: https://github.com/lightningdevkit/rust-lightning
[Iris Wallet desktop]: https://github.com/RGB-Tools/iris-wallet-desktop
[KaleidoSwap]: https://kaleidoswap.com/
[Lnfi]: https://www.lnfi.network/
[Spectrum]: https://rgbspectrum.pages.dev/
[Thunderstack]: https://thunderstack.org/
[Tiramisu Wallet]: https://mainnet.tiramisuwallet.com/
