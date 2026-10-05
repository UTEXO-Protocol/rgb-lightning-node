# Integrating the UTEXO mobile SDK

How to build an Android app on `rgb-sdk-kotlin-light` talking to a `minimal-gateway`
deployment. Written for the app developer; the operator-facing half of the gateway is in
[`packages/gateway/README.md`](../packages/gateway/README.md).

Read [What works today](#2-what-works-today) first. The SDK's surface is deliberately
narrower than the gateway's, and one capability you will probably want — sending BTC —
works but is reachable only through a lower-level entry point, on purpose.

---

## 1. The shape of it

```
Android app
  └── rgb-sdk-kotlin-light          keys, verify-before-sign, local signing, journal
        │                           the seed NEVER leaves the device
        │  HTTPS
     minimal-gateway                per-user auth, idempotency, scoping
        ├── watch-only rgb-lib wallet (one per user, built from your xpubs)
        └── RLN                     shared Lightning node
              └── esplora + RGB proxy
```

The division of labour is the point:

- **The device holds the seed.** It derives the account xpubs and ships only those.
- **The gateway holds no key material.** It builds a watch-only rgb-lib wallet from your
  xpubs and can therefore _prepare_ a transaction but never sign one.
- **The device verifies, then signs.** The gateway returns an unsigned PSBT plus a
  machine-readable statement of intent; the SDK checks the PSBT against the original
  request before a signature exists.
- **The gateway broadcasts.**

So on-chain BTC is self-custodial: those coins cannot move without your user's device.
See [Custody](#6-custody-read-this-before-you-promise-anything) for where that stops
being true.

## 2. Setup

### Gradle

```kotlin
// settings.gradle.kts — until the SDK is on a public repository
dependencyResolutionManagement {
    repositories { mavenLocal(); google(); mavenCentral() }
}

// app/build.gradle.kts
dependencies {
    implementation("com.utexo:rgb-sdk-kotlin-light:0.1.0-SNAPSHOT")
}

android { defaultConfig { minSdk = 24 } }   // SDK minSdk is 24
```

The AAR bundles JNI libraries for `arm64-v8a`, `armeabi-v7a` and `x86_64`. There is no
32-bit-only or RISC-V build; an emulator must be `x86_64` or `arm64-v8a`.

### Gateway credentials

The gateway has no self-service signup. An operator mints each user once:

```sh
curl -X POST https://your-gateway.example/v1/users \
     -H "x-operator-token: $GATEWAY_OPERATOR_TOKEN"
# 201 {"userId":"u_…","token":"utxg_…","createdAt":…}
```

That `token` is returned **exactly once** — only its hash is stored, and there is no
rotation or recovery endpoint. Your backend should mint it during onboarding and deliver
it to the device over a channel you already trust. Treat it as a bearer credential: it
authorizes every call for that user.

Store it with `AndroidKeystoreAPIKeyStore`, which implements `WalletAPIKeyProvider` and
reloads the credential per request, so rotation does not require rebuilding the wallet.

---

## 3. Lifecycle

### Provision the seed first

The seed must exist in the Keystore _before_ the wallet is created.

```kotlin
val walletId = WalletId.of("primary")
val network = Network.SIGNET

// First run only. Back the mnemonic up — it is the user's ONLY recovery path.
SecretMnemonic.fromWords(userEnteredWords).use { mnemonic ->
    AndroidKeystoreSeedStore(context, walletId, network)
        .provisionFromMnemonic(mnemonic)
}
```

`SecretMnemonic`, `SecretSeed` and `WalletAPIKey` are `AutoCloseable` and zeroize on
`close()`. Always use them inside `use { }`. They redact themselves in `toString()`, so
they cannot leak through a log statement.

### Create, init, unlock

```kotlin
val wallet = UTEXOWallet.create(
    context = context,
    configuration = WalletConfiguration(walletId = walletId, network = network),
    service = WalletServiceConfiguration(
        baseUrl = "https://your-gateway.example",   // HTTPS required
        apiKeyProvider = AndroidKeystoreAPIKeyStore(context, walletId, network),
    ),
)

wallet.init()      // registers the xpubs with the gateway (idempotent)
wallet.unlock()    // loads the seed; required before any balance or signing call
```

- `baseUrl` must be **HTTPS** with no path, query or user-info. Plain HTTP is accepted only
  for `127.0.0.1`, `::1`, `localhost` and the emulator host `10.0.2.2`, and only when you
  opt in with `allowInsecureLocalDevelopment = true`.
- The backend permanently binds its credential to the first `WalletId` it sees, and pins
  the gateway `userId` across restarts. A different user behind the same token fails as
  `BackendProtocolMismatch` rather than silently switching wallets.
- Call `shutdown()` when backgrounding and `destroy()` before creating a replacement
  instance — shutdown deliberately retains storage ownership.

### Addresses

`getAddress()` derives locally from the seed and does **not** ask the gateway. That is
deliberate: the gateway holds your xpubs and could otherwise hand back any address it
liked. If you do call `GET /v1/wallet/address` yourself, the response carries a
`derivation` object (`account`, `keychain`, `index`, `derivationPath`, `scriptHex`) so you
can re-derive it and compare before showing it to a user. A `null` derivation means the
gateway could not attribute the address — treat that as unproven, not as safe.

---

## 4. Sending: the verify-before-sign contract

### The gate

**Sending works.** It has been run end to end against a live gateway — prepared, verified,
signed on-device and broadcast. What is gated is _which entry point_ can reach it.

There are two, and they differ by exactly one boolean:

| Entry point                                                   | `allowUnverifiedPsbtProfile`                      | `sendBtc`                         |
| ------------------------------------------------------------- | ------------------------------------------------- | --------------------------------- |
| `UTEXOWallet` (application API)                               | hardcoded `false` in `WalletServiceConfiguration` | throws `SdkException.Unsupported` |
| `RgbLightningNodeGatewayBackend` (`@WalletInfrastructureApi`) | you choose                                        | works                             |

Everything downstream of that flag is identical: the same preflight, the same intent
checks, the same native signer, the same gateway endpoints. The flag is a deliberate hold
on the _convenience_ API until a deployment's exact PSBT profile is qualified — not a sign
that the path is unfinished.

So `wallet.sendBtc(...)` throwing `Unsupported` is expected today. To actually send, build
the backend yourself:

```kotlin
@OptIn(WalletInfrastructureApi::class)
val backend = RgbLightningNodeGatewayBackend(
    RgbLightningNodeGatewayConfiguration(
        baseUrl = "https://your-gateway.example",
        expectedNetwork = Network.SIGNET,
        bearerToken = token,
        allowUnverifiedPsbtProfile = true,   // the gate
    ),
)
```

Do not ship this without qualifying your deployment first. When you do, the flow is:

```
prepare   → gateway returns { opId, psbt, intent, expiresAt }
verify    → the adapter checks kind, recipient, amount, fee rate, canonical Base64, expiry
sign      → the native signer re-validates inputs, outputs, fee and change
            against the ORIGINAL request, then signs
complete  → gateway finalizes and broadcasts
```

Two properties worth relying on. The signer validates against the request your code made,
not against anything the server said — so a server that returns an honest-looking intent
alongside a different PSBT is caught. And signatures never change a txid, so the txid
observed at verification is the txid that broadcasts; the server cannot swap the
transaction between the two steps.

### Idempotency and retries

Every money-moving call carries an `IdempotencyKey`. **Persist it and reuse it on retry** —
that key is the operation's identity:

```kotlin
val request = SendBtcRequest(
    address = BitcoinAddress.of(destination),
    amount = Satoshis.of(100_000),
    feeRate = FeeRate.satPerVbyte("2"),
    idempotencyKey = persistedKey,   // NOT IdempotencyKey.random() on a retry
)
```

A retry with the same key resumes the recorded operation instead of sending twice. A
_different_ request under the same key is rejected with `OperationConflict`.

### Outcomes you must handle

| Exception                 | Meaning                                               | Do                                 |
| ------------------------- | ----------------------------------------------------- | ---------------------------------- |
| `BackendRejected`         | the gateway refused (bad address, insufficient funds) | surface it; the money did not move |
| `OperationConflict`       | same key, different request                           | fix the request or use a fresh key |
| `OperationExpired`        | the prepared PSBT passed its TTL                      | prepare again                      |
| **`OperationAmbiguous`**  | **the outcome is unknown**                            | **see below**                      |
| `BackendUnavailable`      | transient (timeout, 429, 5xx)                         | retry with the _same_ key          |
| `PolicyRejected`          | the PSBT failed local policy — possible tampering     | do not retry blindly; investigate  |
| `BackendProtocolMismatch` | the gateway returned something unmappable             | treat as a bug, not a user error   |

**`OperationAmbiguous` does not mean failure.** rgb-lib broadcasts before it writes its
bookkeeping, so a failure after broadcast is indistinguishable from one before it. Never
present it as "payment failed" and never auto-retry it as a fresh send. The SDK preserves
the operation and resolves it on the next call with the same idempotency key; the gateway
also runs a background worker that finishes such operations server-side. Show the user
"confirming" and re-check.

## 5. Verifying your integration

A reference end-to-end run lives in the SDK repo at
`sdk/src/androidTest/kotlin/com/utexo/rgb/sdk/DeployedGatewayE2eTest.kt`. It no-ops unless
a gateway URL is supplied, and shows the full path — identity derivation, registration,
prepare, local verify-and-sign, completion — against a live gateway on an emulator.

```sh
./gradlew :sdk:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.utexo.rgb.sdk.DeployedGatewayE2eTest \
  -Pandroid.testInstrumentationRunnerArguments.gatewayBaseUrl=https://your-gateway.example \
  -Pandroid.testInstrumentationRunnerArguments.bearerToken=utxg_… \
  -Pandroid.testInstrumentationRunnerArguments.mnemonic=word_word_…   # underscore-separated
```

Fund the registered address between step 1 and step 2. Use a throwaway mnemonic and a test
network.
