# Step 4: External Signer Spendable-Output Contract Extension

This document specifies the contract changes required to eliminate the remaining local signing paths in RLN external-signer mode.

Constraint:
- RLN currently aliases signer contract types directly from `signer-external::contract` in `src/signer/types.rs`
- so the wire contract cannot be fully implemented from this repo alone
- the actual contract change must be applied first in `rln-external-signer`

## Why this step is needed

Current `SignSpendableOutputsPsbt` only carries:
- `utxos: Vec<SpendableOutputUtxo>`
- `psbt: String`

That is enough for some wallet/static cases but not enough to fully delegate all descriptor classes.

RLN still signs locally for:
- `StaticOutput` when a derivation match is found
- `StaticPaymentOutput`
- `DelayedPaymentOutput`

See:
- [external-signer-signing-inventory.md](/home/roman-boiko/projects/utexo/rgb-lightning-node/src/signer/external-signer-signing-inventory.md)

## Contract goal

Make one PSBT-based signer operation sufficient for all spendable-output descriptor kinds.

After this change, RLN should:
- build PSBT
- classify inputs
- attach descriptor metadata
- send request
- receive signed/finalized PSBT

RLN should not:
- derive xprivs
- rebuild `KeysManager`
- derive local channel signers
- sign any spendable-output witness locally

## Proposed request shape

Keep the top-level operation name:
- `SignSpendableOutputsPsbt`

Replace the current `utxos`-only payload with richer per-input metadata.

### New per-input metadata object

Suggested Rust contract type in `signer-external`:

```rust
pub enum SpendableDescriptorKind {
    StaticOutput,
    StaticPaymentOutput,
    DelayedPaymentOutput,
}

pub struct WalletDerivationMatch {
    pub account_name: String,
    pub keyindex: u32,
    pub derivation_path: String,
}

pub struct SpendableOutputSignInput {
    pub descriptor_kind: SpendableDescriptorKind,
    pub txid_hex: String,
    pub vout: u32,
    pub amount_sat: u64,
    pub script_pubkey_hex: String,

    // Present for channel-derived descriptor kinds.
    pub channel_keys_id_hex: Option<String>,

    // Present for wallet-owned static outputs when RLN has already matched script -> derivation.
    pub wallet_derivation_match: Option<WalletDerivationMatch>,

    // Optional extra context when signer cannot reconstruct from PSBT alone.
    pub witness_script_hex: Option<String>,
    pub redeem_script_hex: Option<String>,
}
```

### Revised signer request variant

Suggested contract change:

```rust
SignerRequest::SignSpendableOutputsPsbt {
    inputs: Vec<SpendableOutputSignInput>,
    psbt: String,
}
```

This should replace the current:

```rust
SignerRequest::SignSpendableOutputsPsbt {
    utxos: Vec<SpendableOutputUtxo>,
    psbt: String,
}
```

## Descriptor mapping from RLN

RLN should map each `SpendableOutputDescriptor` to one `SpendableOutputSignInput`.

### `StaticOutput`

Use:
- `descriptor_kind = StaticOutput`
- `txid_hex`
- `vout`
- `amount_sat`
- `script_pubkey_hex`
- `wallet_derivation_match` if available from `find_derivation_matches_for_script`
- `channel_keys_id_hex = None`

Signer responsibility:
- derive the correct wallet private key internally
- sign P2WPKH/P2TR/etc as appropriate

### `StaticPaymentOutput`

Use:
- `descriptor_kind = StaticPaymentOutput`
- `txid_hex`
- `vout`
- `amount_sat`
- `script_pubkey_hex`
- `channel_keys_id_hex = Some(...)`

Signer responsibility:
- map channel id to channel signer internally
- produce the `sign_counterparty_payment_input` witness internally

### `DelayedPaymentOutput`

Use:
- `descriptor_kind = DelayedPaymentOutput`
- `txid_hex`
- `vout`
- `amount_sat`
- `script_pubkey_hex`
- `channel_keys_id_hex = Some(...)`
- `witness_script_hex` if required by backend implementation

Signer responsibility:
- derive or restore the correct channel signer internally
- produce the delayed to-local witness internally

## Protobuf extension plan

Current RLN protobuf mirrors the signer contract in `src/signer/proto.rs`.

### Existing message

Current message:
- `SignSpendableOutputsPsbtRequestV1`
- field `utxos: Vec<SpendableOutputUtxoV1>`
- field `psbt: String`

### Proposed message additions

Add:

```protobuf
message WalletDerivationMatchV1 {
  string account_name = 1;
  uint32 keyindex = 2;
  string derivation_path = 3;
}

message SpendableOutputSignInputV1 {
  uint32 descriptor_kind = 1;
  string txid_hex = 2;
  uint32 vout = 3;
  uint64 amount_sat = 4;
  string script_pubkey_hex = 5;
  optional string channel_keys_id_hex = 6;
  optional WalletDerivationMatchV1 wallet_derivation_match = 7;
  optional string witness_script_hex = 8;
  optional string redeem_script_hex = 9;
}

message SignSpendableOutputsPsbtRequestV1 {
  repeated SpendableOutputSignInputV1 inputs = 1;
  string psbt = 2;
}
```

Notes:
- this is a breaking wire change if the existing field numbers are repurposed
- safest migration path is to introduce a new request variant instead of mutating the old one in place

## Safer migration option

Instead of changing the existing variant in-place, introduce a new request:

```rust
SignerRequest::SignSpendableOutputsPsbtV2 {
    inputs: Vec<SpendableOutputSignInput>,
    psbt: String,
}
```

Benefits:
- old clients stay compatible
- RLN can negotiate by `api_level`
- step 5 can switch RLN to V2 once the signer side supports it

Recommended if multiple hosts are already using the v1 contract.

## Required RLN code changes after contract lands

Files:
- `src/signer/types.rs`
- `src/signer/proto.rs`
- `src/signer/vls_adapter.rs`
- `src/signer/external.rs`

### `src/signer/types.rs`
- import new contract types from `signer-external::contract`
- if using V2 request, expose the new request/response aliases

### `src/signer/proto.rs`
- add protobuf mirror messages:
  - `WalletDerivationMatchV1`
  - `SpendableOutputSignInputV1`
- add encode/decode for new signer request variant

### `src/signer/vls_adapter.rs`
- update `ExternalSignerBackend::sign_spendable_outputs_psbt` signature to take richer input metadata
- map to new contract request

Suggested new trait method:

```rust
fn sign_spendable_outputs_psbt(
    &self,
    inputs: Vec<SpendableOutputSignInput>,
    psbt: String,
) -> Result<String, RlnSignerError>;
```

### `src/signer/external.rs`
- change `spendable_descriptor_to_utxo` into `spendable_descriptor_to_sign_input`
- remove local signing branches from `sign_spendable_outputs_psbt`
- always delegate all descriptor kinds to backend

## Required signer-side changes in `rln-external-signer`

- add new contract types / request variant
- extend request decode/encode
- implement signing for:
  - `StaticOutput`
  - `StaticPaymentOutput`
  - `DelayedPaymentOutput`
- keep private key derivation and channel-signer derivation entirely inside signer backend

## Acceptance criteria for step 4

Step 4 is complete when:
- signer contract can carry enough metadata for all three descriptor kinds
- RLN no longer needs to derive local keys to sign spendable outputs
- signer backend can finalize PSBTs for all descriptor kinds remotely
- step 5 can delete the local helper functions from `src/signer/external.rs`
