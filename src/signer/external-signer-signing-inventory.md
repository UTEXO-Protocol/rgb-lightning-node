# External Signer Boundary Inventory

This document inventories the remaining signing-capable paths that still execute locally in RLN while external-signer mode is active.

Goal:
- RLN builds context and PSBTs
- RLN delegates all signing-capable work to the external signer
- RLN does not derive xprivs, rebuild `KeysManager`, or derive local channel signers in external mode

## Current externalized operations

Already delegated through the external signer backend in `src/signer/vls_adapter.rs`:
- `bootstrap`
- `node_get_node_id`
- `node_get_destination_script`
- `node_get_shutdown_scriptpubkey`
- `generate_channel_keys_id`
- `derive_channel_signer`
- `sign_rgb_psbt`
- `sign_spendable_outputs_psbt`
- `get_wallet_input_metadata`
- `find_derivation_matches_for_script`

These cover node/channel identity, explicit RGB PSBT signing, and one generic spendable-output PSBT path.

## Remaining local signing/derivation paths

All remaining local signing-capable logic is in `src/signer/external.rs`.

### 1. Wallet xpriv derivation from bootstrap seed

Functions:
- `rgb_coin_type`
- `rgb_account_derivation_path`
- `rgb_account_xpriv`
- `derivation_path_from_match`

What they do:
- derive vanilla/colored account xprivs from `self.signer_seed`
- derive child private keys for wallet-owned static outputs

Why this violates the target boundary:
- RLN still holds seed-equivalent material and derives private keys locally in external mode

Target replacement:
- no local xpriv derivation in RLN
- signer operation signs the relevant input directly using signer-owned wallet derivation

Suggested replacement op:
- keep `SignSpendableOutputsPsbt`, but extend request metadata so signer can identify wallet-owned static outputs without RLN deriving child keys

Required request metadata:
- input outpoint
- amount
- script pubkey
- optional wallet derivation match (`account_name`, `keyindex`, `derivation_path`) if signer does not want to rediscover it

### 2. Local signing of `StaticOutput`

Function:
- `sign_static_output_input`

Current behavior:
- RLN derives account xpriv from `signer_seed`
- RLN derives child xpriv from derivation path
- RLN computes sighash and signs P2WPKH locally
- RLN inserts final witness into PSBT

Trigger site:
- `RlnKeysInterface::sign_spendable_outputs_psbt`
- `SpendableOutputDescriptor::StaticOutput`
- branch where `find_derivation_match_for_script(...)` returns a match

Target replacement:
- external signer signs wallet-owned static outputs too
- RLN should not special-case this branch locally

Suggested replacement op:
- extend `SignSpendableOutputsPsbt`

Needed descriptor kind:
- `static_output`

Needed metadata:
- outpoint
- amount
- script pubkey
- optional wallet derivation match if precomputed by RLN

### 3. Local `KeysManager` reconstruction

Function:
- `local_keys_manager`

Current behavior:
- rebuilds LDK `KeysManager` from `self.signer_seed`

Why this violates the target boundary:
- RLN reconstructs a signing-capable local LDK signer root in external mode

Target replacement:
- delete entirely after channel-output signing is delegated

Suggested replacement op:
- none directly
- this function disappears once `StaticPaymentOutput` and `DelayedPaymentOutput` signing are delegated

### 4. Local conversion from VLS dbid-shaped channel id to LDK keys id

Function:
- `local_ldk_channel_keys_id`

Current behavior:
- translates the VLS-style `channel_keys_id` envelope into the real LDK/VLS `keys_id`
- uses `self.signer_seed` + VLS derivation rules to do it locally

Why this violates the target boundary:
- RLN is reproducing channel key derivation logic locally

Target replacement:
- signer backend performs any required mapping internally
- RLN passes the original descriptor/channel metadata only

Suggested replacement op:
- extend `SignSpendableOutputsPsbt`

Needed metadata:
- original `channel_keys_id`
- descriptor kind
- any signer-side channel locator info if needed

### 5. Local channel signer derivation

Function:
- `local_channel_signer`

Current behavior:
- rebuilds an `InMemorySigner` from the local `KeysManager`

Why this violates the target boundary:
- RLN re-creates channel signing authority locally in external mode

Target replacement:
- signer backend derives/signs internally

Suggested replacement op:
- none directly
- this function disappears once delayed/static payment output signing is delegated

### 6. Local signing of `StaticPaymentOutput`

Function:
- `sign_static_payment_output_input`

Current behavior:
- RLN derives local channel signer
- RLN calls `sign_counterparty_payment_input(...)`
- RLN writes witness into PSBT

Trigger site:
- `RlnKeysInterface::sign_spendable_outputs_psbt`
- `SpendableOutputDescriptor::StaticPaymentOutput`

Target replacement:
- signer signs this descriptor class remotely

Suggested replacement op:
- extend `SignSpendableOutputsPsbt`

Needed descriptor kind:
- `static_payment_output`

Needed metadata:
- outpoint
- amount
- script pubkey
- `channel_keys_id`
- any witness script/redeem context the signer cannot reconstruct from PSBT alone

### 7. Local signing of `DelayedPaymentOutput`

Function:
- `sign_delayed_payment_output_input`

Current behavior:
- RLN derives local channel signer
- RLN calls `sign_dynamic_p2wsh_input(...)`
- RLN writes witness into PSBT

Trigger site:
- `RlnKeysInterface::sign_spendable_outputs_psbt`
- `SpendableOutputDescriptor::DelayedPaymentOutput`

Target replacement:
- signer signs this descriptor class remotely

Suggested replacement op:
- extend `SignSpendableOutputsPsbt`

Needed descriptor kind:
- `delayed_payment_output`

Needed metadata:
- outpoint
- amount
- script pubkey
- `channel_keys_id`
- any witness script/redeem/CSV context the signer cannot reconstruct from PSBT alone

## Current mixed behavior in `sign_spendable_outputs_psbt`

Function:
- `RlnKeysInterface::sign_spendable_outputs_psbt`

Current split:
- `StaticOutput` with derivation match: signed locally
- `StaticOutput` without derivation match: delegated via backend `sign_spendable_outputs_psbt`
- `StaticPaymentOutput`: signed locally
- `DelayedPaymentOutput`: signed locally

This mixed behavior is the exact boundary leak.

Target behavior:
- all descriptor kinds are delegated through the backend in external mode
- RLN may still gather metadata, but must not sign or derive signing keys

## Recommended contract shape for step 4

Use one PSBT-based signer operation, not three separate ad hoc RPCs.

Preferred op:
- `SignSpendableOutputsPsbt`

Extend request payload with per-input metadata:
- `descriptor_kind`: `static_output | static_payment_output | delayed_payment_output`
- `outpoint`
- `amount_sat`
- `script_pubkey_hex`
- `channel_keys_id_hex` when applicable
- `wallet_derivation_match` when applicable
- `witness_script_hex` / `redeem_script_hex` when needed
- optional extra descriptor-specific context if VLS cannot reconstruct from PSBT + metadata

Why one op is better:
- keeps RLN/host contract small
- preserves PSBT as the common signing surface
- allows signer to own all descriptor-specific private key derivation internally

## Functions to delete after step 4 is implemented

From `src/signer/external.rs`:
- `rgb_coin_type`
- `rgb_account_derivation_path`
- `rgb_account_xpriv`
- `derivation_path_from_match` (unless still needed only as metadata serialization)
- `sign_static_output_input`
- `local_keys_manager`
- `local_ldk_channel_keys_id`
- `local_channel_signer`
- `find_psbt_input_idx` (if no longer needed)
- `sign_static_payment_output_input`
- `sign_delayed_payment_output_input`

## Definition of completion for this area

This inventory is fully resolved when:
- `ExternalSigner` no longer derives xprivs from `signer_seed`
- `ExternalSigner` no longer rebuilds `KeysManager`
- `ExternalSigner` no longer derives local channel signers
- `sign_spendable_outputs_psbt` delegates all descriptor kinds to the backend
- external mode contains no local spendable-output signing paths
