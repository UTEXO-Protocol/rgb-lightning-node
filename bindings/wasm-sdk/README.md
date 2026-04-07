# RLN WASM SDK (Standalone)

This crate is an isolated wasm package for RLN browser-facing APIs that use `rgb-lib-wasm` directly.

Current status: RGB wallet flows are runtime-backed in wasm. LDK peer/channel runtime is not yet ported in this crate.

## Why separate crate

`rgb-lightning-node` native crate depends on native `rgb-lib` and LDK stack. `rgb-lib-wasm` depends on a wasm-specific RGB stack.
Combining both dependency graphs in one crate currently causes Cargo resolver conflicts.

This crate avoids that by keeping wasm RGB integration in a dedicated package.

## Current APIs

Unified facade:

- `new RlnWasmSdk()`
- `sdk.healthcheck()`
- `sdk.version()`
- `sdk.runtimeCapabilitiesValue()`
- `sdk.runtimeCapabilitiesJson()`
- `sdk.newWallet(walletDataJson)`
- `sdk.createWallet(walletDataJson)` (async)
- `sdk.newNode(proxyUrl)`
- `sdk.newNodeWithRuntimeBackend(proxyUrl, runtimeBackend)`
- `sdk.createNodeHandle(proxyUrl)`
- `sdk.createNodeHandleWithRuntimeBackend(proxyUrl, runtimeBackend)`
- `sdk.createWalletHandle(walletDataJson)`
- `sdk.createWalletHandleAsync(walletDataJson)` (async)
- `sdk.nodeInfoValue(node)`
- `sdk.nodeInfoJson(node)`
- `sdk.listPaymentsValue(node)`
- `sdk.listPaymentsJson(node)`
- `sdk.decodeLnInvoiceValue(node, invoice)`
- `sdk.decodeLnInvoiceJson(node, invoice)`
- `sdk.decodeRgbInvoiceValue(node, invoice)`
- `sdk.decodeRgbInvoiceJson(node, invoice)`
- `sdk.walletGetAddress(wallet)`
- `sdk.walletGetBtcBalanceValue(wallet)`
- `sdk.walletGetBtcBalanceJson(wallet)`
- `sdk.walletListTransactionsValue(wallet)`
- `sdk.walletListTransactionsJson(wallet)`
- `sdk.walletListAssetsValue(wallet, filterAssetSchemas)`
- `sdk.walletListAssetsJson(wallet, filterAssetSchemas)`
- `sdk.connectPeer(node, peerAddr, peerPubkey)` (async)
- `sdk.disconnectPeer(node, peerPubkey)` (async)
- `sdk.openChannelValue(node, peerPubkey, capacitySat, public, assetId?, assetLocalAmount?)`
- `sdk.openChannelJson(node, peerPubkey, capacitySat, public, assetId?, assetLocalAmount?)`
- `sdk.sendPaymentValue(node, invoice, amtMsat?, assetId?, assetAmount?)`
- `sdk.sendPaymentJson(node, invoice, amtMsat?, assetId?, assetAmount?)`
- `sdk.keysendValue(node, destPubkey, amtMsat, assetId?, assetAmount?)`
- `sdk.keysendJson(node, destPubkey, amtMsat, assetId?, assetAmount?)`
- `sdk.listPeersValue(node)`
- `sdk.listPeersJson(node)`
- `sdk.listChannelsValue(node)`
- `sdk.listChannelsJson(node)`
- `sdk.closeChannel(node, channelId)`
- `sdk.getChannelId(node, temporaryChannelId)`
- `sdk.invoiceStatusValue(node, invoice)`
- `sdk.invoiceStatusJson(node, invoice)`
- `sdk.updatePaymentStatus(node, paymentHash, status)`
- `sdk.updatePaymentStatusJson(node, paymentHash, status)`
- `sdk.updatePaymentStatusByInvoice(node, invoice, status)`
- `sdk.updatePaymentStatusByInvoiceJson(node, invoice, status)`
- `sdk.ingestReadEventPayloadHex(node, payloadHex)`
- `sdk.ingestReadEventPayloadHexJson(node, payloadHex)`
- `sdk.failPendingPayments(node)`
- `sdk.installAutoPeerManagerHooks(node)`
- `sdk.clearAutoPeerManagerHooks(node)`

Stateful handle pattern:

- `RlnWasmSdkNodeHandle` methods:
  - `nodeInfoValue/Json`
  - `connectPeer` / `disconnectPeer` (async)
  - `listPeersValue/Json`
  - `listChannelsValue/Json`
  - `openChannelValue/Json`
  - `closeChannel`
  - `getChannelId`
  - `listPaymentsValue/Json`
  - `sendPaymentValue/Json`
  - `keysendValue/Json`
  - `invoiceStatusValue/Json`
  - `updatePaymentStatus/Json`
  - `updatePaymentStatusByInvoice/Json`
  - `decodeLnInvoiceValue/Json`
  - `decodeRgbInvoiceValue/Json`
  - `ingestReadEventPayloadHex/Json`
  - `failPendingPayments`
  - `installAutoPeerManagerHooks` / `clearAutoPeerManagerHooks`
- `RlnWasmSdkWalletHandle` methods:
  - `getAddress`
  - `getBtcBalanceValue/Json`
  - `listTransactionsValue/Json`
  - `listAssetsValue/Json`
  - `goOnlineValue/Json` (async)
  - `syncOnline` (async)
  - `getFeeEstimation/Json` (async)
  - `createUtxosBegin/End/EndJson` (async)
  - `sendBegin` / `sendEndValue` / `sendEndJson` (async)
  - `sendBtcBegin` / `sendBtcEnd` (async)
  - `refreshValue/Json` (async)
  - `failTransfers/Json` (async)
  - `drainToBegin` / `drainToEnd` (async)
  - `inflateBegin` / `inflateEndValue` / `inflateEndJson` (async)
  - `listUnspentsVanillaValue/Json` (async)
  - `backup` / `restoreBackup` / `backupInfo` / `backupInfoJson`
  - `configureVssBackup` / `disableVssBackup`
  - `vssBackupValue/Json` / `vssRestoreBackup` / `vssBackupInfoValue/Json` (async)

- `rgbGenerateKeysJson(network)`
- `rgbGenerateKeysValue(network)`
- `rgbRestoreKeysJson(network, mnemonic)`
- `rgbRestoreKeysValue(network, mnemonic)`
- `new RlnWasmWallet(walletDataJson)`
- `RlnWasmWallet.create(walletDataJson)` (async, IndexedDB restore)
- `wallet.getWalletDataValue()`
- `wallet.getWalletDataJson()`
- `wallet.getAddress()`
- `wallet.getBtcBalanceValue()`
- `wallet.getBtcBalanceJson()`
- `wallet.listTransactionsValue()`
- `wallet.listTransactionsJson()`
- `wallet.listAssetsValue(filterAssetSchemas)`
- `wallet.listAssetsJson(filterAssetSchemas)`
- `wallet.getAssetMetadataValue(assetId)`
- `wallet.getAssetMetadataJson(assetId)`
- `wallet.getAssetBalanceValue(assetId)`
- `wallet.getAssetBalanceJson(assetId)`
- `wallet.listTransfersValue(assetId?)`
- `wallet.listTransfersJson(assetId?)`
- `wallet.listUnspentsValue(settledOnly)`
- `wallet.listUnspentsJson(settledOnly)`
- `wallet.blindReceiveValue(assetId?, assignment, durationSeconds?, transportEndpoints, minConfirmations)`
- `wallet.blindReceiveJson(assetId?, assignment, durationSeconds?, transportEndpoints, minConfirmations)`
- `wallet.witnessReceiveValue(assetId?, assignment, durationSeconds?, transportEndpoints, minConfirmations)`
- `wallet.witnessReceiveJson(assetId?, assignment, durationSeconds?, transportEndpoints, minConfirmations)`
- `wallet.goOnlineValue(skipConsistencyCheck, indexerUrl)` (async)
- `wallet.goOnlineJson(skipConsistencyCheck, indexerUrl)` (async)
- `wallet.syncOnline(online)` (async)
- `wallet.getFeeEstimation(online, blocks)` (async)
- `wallet.getFeeEstimationJson(online, blocks)` (async)
- `wallet.createUtxosBegin(online, upTo, num?, size?, feeRate, skipSync)` (async)
- `wallet.createUtxosEnd(online, signedPsbt, skipSync)` (async)
- `wallet.createUtxosEndJson(online, signedPsbt, skipSync)` (async)
- `wallet.sendBegin(online, recipientMap, donation, feeRate, minConfirmations)` (async)
- `wallet.sendEndValue(online, signedPsbt, skipSync)` (async)
- `wallet.sendEndJson(online, signedPsbt, skipSync)` (async)
- `wallet.sendBtcBegin(online, address, amount, feeRate, skipSync)` (async)
- `wallet.sendBtcEnd(online, signedPsbt, skipSync)` (async)
- `wallet.refreshValue(online, assetId?, filter, skipSync)` (async)
- `wallet.refreshJson(online, assetId?, filter, skipSync)` (async)
- `wallet.failTransfers(online, batchTransferIdx?, noAssetOnly, skipSync)` (async)
- `wallet.failTransfersJson(online, batchTransferIdx?, noAssetOnly, skipSync)` (async)
- `wallet.drainToBegin(online, address, destroyAssets, feeRate)` (async)
- `wallet.drainToEnd(online, signedPsbt)` (async)
- `wallet.inflateBegin(online, assetId, inflationAmounts, feeRate, minConfirmations)` (async)
- `wallet.inflateEndValue(online, signedPsbt)` (async)
- `wallet.inflateEndJson(online, signedPsbt)` (async)
- `wallet.listUnspentsVanillaValue(online, minConfirmations, skipSync)` (async)
- `wallet.listUnspentsVanillaJson(online, minConfirmations, skipSync)` (async)
- `wallet.backup(password)`
- `wallet.restoreBackup(backupBytes, password)`
- `wallet.backupInfo()`
- `wallet.backupInfoJson()`
- `wallet.configureVssBackup(serverUrl, storeId, signingKeyHex)`
- `wallet.disableVssBackup()`
- `wallet.vssBackupValue()` (async)
- `wallet.vssBackupJson()` (async)
- `wallet.vssRestoreBackup()` (async)
- `wallet.vssBackupInfoValue()` (async)
- `wallet.vssBackupInfoJson()` (async)
- `new RlnWasmInvoice(invoiceString)`
- `invoice.invoiceDataValue()`
- `invoice.invoiceDataJson()`
- `invoice.invoiceString()`
- `checkProxyUrl(proxyUrl)` (async)
- `checkLnPeerWebsocketValue(proxyUrl, peerAddr)` (async)
- `checkLnPeerWebsocketJson(proxyUrl, peerAddr)` (async)
- `lnSocketConnect(proxyUrl, peerAddr)` (async) -> `RlnWasmLnSocket`
- `socket.websocketUrl()`
- `socket.sendHex(payloadHex)` (async)
- `socket.startReadLoop(onMessage)`
- `socket.stopReadLoop()`
- `socket.close()` (async)
- `socket.isClosed()`
- `peerSessionConnect(proxyUrl, peerAddr, peerPubkey, newOutboundConnectionCb, readEventCb, processEventsCb, socketDisconnectedCb)` (async) -> `RlnWasmPeerSession`
- `peerSessionConnectWithErrorCb(proxyUrl, peerAddr, peerPubkey, newOutboundConnectionCb, readEventCb, processEventsCb, socketDisconnectedCb, reportErrorCb)` (async) -> `RlnWasmPeerSession`
- `peerSession.websocketUrl()`
- `peerSession.start()` (async)
- `peerSession.stop()`
- `peerSession.close()` (async)
- `peerSession.isStarted()`
- `new RlnWasmRustPeerManagerBridge(initialOutboundHex?)`
- `bridge.setInitialOutboundHex(hex)`
- `bridge.statsValue()`
- `bridge.connectSession(proxyUrl, peerAddr, peerPubkey)` (async) -> `RlnWasmPeerSession`
- `new RlnWasmNode(proxyUrl)`
- `node.connectPeer(peerAddr, peerPubkey)` (async)
- `node.disconnectPeer(peerPubkey)` (async)
- `node.listPeersValue()`
- `node.listPeersJson()`
- `node.listChannelsValue()`
- `node.listChannelsJson()`
- `node.nodeInfoValue()`
- `node.nodeInfoJson()`
- `node.closeAllPeers()` (async)
- `node.openChannelValue(peerPubkey, capacitySat, public, assetId?, assetLocalAmount?)`
- `node.openChannelJson(peerPubkey, capacitySat, public, assetId?, assetLocalAmount?)`
- `node.closeChannel(channelId)`
- `node.getChannelId(temporaryChannelId)`
- `node.sendPaymentValue(invoice, amtMsat?, assetId?, assetAmount?)`
- `node.sendPaymentJson(invoice, amtMsat?, assetId?, assetAmount?)`
- `node.keysendValue(destPubkey, amtMsat, assetId?, assetAmount?)`
- `node.keysendJson(destPubkey, amtMsat, assetId?, assetAmount?)`
- `node.listPaymentsValue()`
- `node.listPaymentsJson()`
- `node.getPaymentValue(paymentHash)`
- `node.getPaymentJson(paymentHash)`
- `node.decodeLnInvoiceValue(invoice)`
- `node.decodeLnInvoiceJson(invoice)`
- `node.decodeRgbInvoiceValue(invoice)`
- `node.decodeRgbInvoiceJson(invoice)`
- `node.invoiceStatusValue(invoice)`
- `node.invoiceStatusJson(invoice)`
- `node.updatePaymentStatus(paymentHash, status)`
- `node.updatePaymentStatusJson(paymentHash, status)`
- `node.updatePaymentStatusByInvoice(invoice, status)`
- `node.updatePaymentStatusByInvoiceJson(invoice, status)`
- `node.ingestReadEventPayloadHex(payloadHex)`
- `node.ingestReadEventPayloadHexJson(payloadHex)`
- `node.installAutoPeerManagerHooks()`
- `node.clearAutoPeerManagerHooks()`
- `node.failPendingPayments()`

Note: LN decode in this crate currently reports only standard BOLT11 fields; RGB
invoice extension fields (`asset_id`, `asset_amount`) are returned as `null`.

Payment status transition note:
- `sendPayment` / `keysend` set initial status (`pending` when peer connectivity exists, otherwise `failed`).
- Use `updatePaymentStatus*` APIs from your event/callback path to transition into `succeeded` / `failed` / `expired`.
- `installAutoPeerManagerHooks()` installs a default Rust hook adapter that marks all pending payments as failed on socket disconnect/error callbacks.
- Auto hook `read_event` also attempts targeted payment updates from UTF-8 payloads encoded as hex:
  - JSON: `{"payment_hash":"<hex>","status":"succeeded"}`
  - JSON aliases: `{"event":"PaymentSent","payment_hash":"<hex>"}` / `{"event":"PaymentFailed","payment_hash":"<hex>"}` / `{"event":"PaymentExpired","payment_hash":"<hex>"}`
  - Text: `payment_status:<payment_hash>:<status>`
  - Text aliases: `payment_succeeded:<payment_hash>` / `payment_failed:<payment_hash>` / `payment_expired:<payment_hash>`
- Auto hook `read_event` also applies transport/channel payloads:
  - `peer_disconnected:<peer_pubkey>`
  - `peer_reconnected:<peer_pubkey>`
  - `channel_closed:<channel_id>`
  - `channel_usable:<channel_id>`
  - `channel_unusable:<channel_id>`
- `ingestReadEventPayloadHex*` applies the same parser directly for deterministic browser tests/examples.

Rust integration hook points (non-JS API, for embedding this crate from Rust):

- `install_rln_ldk_peer_manager_hooks(...)`
- `clear_rln_ldk_peer_manager_hooks()`

WASM bootstrap helpers for hook installation:

- `installPeerManagerHooksFromJs(newOutboundConnectionCb, readEventCb, processEventsCb, socketDisconnectedCb, reportErrorCb)`
- `clearPeerManagerHooks()`
- `hasPeerManagerHooks()`

Supported networks: `mainnet`, `testnet`, `testnet4`, `signet`, `regtest`.

## Build

```bash
cd bindings/wasm-sdk
cargo check --target wasm32-unknown-unknown
```

## Example

Browser interop example (similar role to Python interop example):

- `bindings/wasm-sdk/examples/wasm-interop/README.md`

## Dependency note

This crate pins `rgb-consensus` / `rgb-ops` / `rgb-invoicing` /
`rgb-strict-encoding` through `[patch.crates-io]` to vendored snapshots under
`bindings/wasm-sdk/vendor`.

Pinned snapshots:

1. `rgb-consensus`: `d0e157ae6a19edc08829cf4b52bbda2f10b4301e`
2. `rgb-ops` / `rgb-invoicing`: `b9d44825a95593429dfd6a2bd059d3d83749a063`
3. `rgb-strict-encoding`: `062dccfb88196ba2a100e76fef6f3e5adc228a9c`

These are the known-good revisions for the current `rgb-lib-wasm` API
expectations (`ChainNet::BitcoinSignetCustom`, `chain_hash()`).

## Port Plan

Full SDK wasm port plan and progress tracking live in:

- `bindings/wasm-sdk/PORTING_PLAN.md`
- `bindings/wasm-sdk/ERROR_CONTRACT.md`
- `bindings/wasm-sdk/MUTINY_LDK_REFERENCE.md`
- `bindings/wasm-sdk/SDK_WASM_PARITY_PLAN.md`
