# SDK vs WASM Architecture (UML)

This document compares the native SDK model with the browser WASM model.

## 1) Component Comparison

```mermaid
flowchart LR
  subgraph Native["Native SDK (UniFFI / Rust host)"]
    NClient["SDK Client"]
    NSdk["rln-sdk facade"]
    NNode["Native LN Runtime + Peer Manager"]
    NChain["Chain backend / indexer integration"]
    NRgb["RGB wallet + transport endpoints"]
    NClient --> NSdk --> NNode
    NSdk --> NRgb
    NNode --> NChain
  end

  subgraph Wasm["WASM SDK (Browser)"]
    WClient["Browser App"]
    WSdk["RlnWasmSdk / RlnWasmNode"]
    WLdk["WASM Native LDK Runtime Manager"]
    WCore["NativeLnRuntimeCore (event queue)"]
    WProxy["LN Proxy WebSocket endpoint"]
    WRgbProxy["RGB Proxy HTTP endpoint"]
    WIndexer["Esplora indexer"]
    WStore["LocalStorage + IndexedDB"]

    WClient --> WSdk
    WSdk --> WLdk
    WSdk --> WCore
    WSdk --> WProxy
    WSdk --> WRgbProxy
    WSdk --> WIndexer
    WSdk --> WStore
  end
```

## 2) Open Channel Flow Comparison

```mermaid
flowchart LR
  subgraph N["Native SDK diagram"]
    n1["1. App -> SDK: connect_peer(peer_pubkey@peer_addr)"]
    n2["2. SDK -> Native Runtime: establish peer session"]
    n3["3. App -> SDK: open_channel(peer_pubkey, capacity, ...)"]
    n4["4. SDK -> Runtime: create channel intent/state"]
    n5["5. Runtime -> Chain backend: publish/finalize tx flow"]
    n6["6. Runtime -> SDK: channel state update"]
    n7["7. SDK -> App: channel data"]
    n1 --> n2 --> n3 --> n4 --> n5 --> n6 --> n7
  end

  subgraph W["WASM diagram"]
    w1["1. App -> RlnWasmNode: connectPeer(peer_addr, peer_pubkey)"]
    w2["2. Node -> LN Proxy WS: connect to /v1/{host}/{port} (WS↔TCP bridge)"]
    w3["3. App -> Node: openChannel*(peer_pubkey, capacity, ...)"]
    w4["4. WASM Runtime: create channel; wait for FundingGenerationReady"]
    w5["5. App -> Gateway (HTTP dev): request funding tx for output_script + value"]
    w6["6. App -> Node: submitFundingTransaction*(temporary_channel_id, tx_hex)"]
    w7["7. WASM Runtime: process events until channel becomes usable"]
    w8["8. Node -> Browser store: persist runtime event/state (reload-safe)"]
    w9["9. Node -> App: channel/payment state via runtime-backed reads"]
    w1 --> w2 --> w3 --> w4 --> w5 --> w6 --> w7 --> w8 --> w9
  end
```

## 3) Virtual Channel Flow Comparison

```mermaid
flowchart LR
  subgraph N["Native SDK diagram"]
    n1["1. App -> SDK: open_channel_with_options(... trusted_no_broadcast)"]
    n2["2. SDK -> Native Runtime: reserve virtual intent + session"]
    n3["3. Runtime -> Native store: persist virtual session/scope"]
    n4["4. App -> SDK: close_channel_with_options(channel_id, peer_pubkey, force=false)"]
    n5["5. SDK -> Runtime: validate cleanup guards + update session status"]
    n6["6. SDK -> App: close finalized"]
    n1 --> n2 --> n3 --> n4 --> n5 --> n6
  end

  subgraph W["WASM diagram"]
    w1["1. App -> Node: openChannel*WithOptions(... trusted_no_broadcast)"]
    w2["2. Node -> WASM Runtime: virtual_channel_add_intent + session_add_from_open"]
    w3["3. Node -> NativeLnRuntimeCore: enqueue channel_usable event"]
    w4["4. Node -> Browser store: persist virtual scope/session state"]
    w5["5. App -> Node: closeChannelWithOptions(channel_id, peer_pubkey, force=false)"]
    w6["6. Node -> WASM Runtime: validate guards + update session status"]
    w7["7. Node -> Browser store: persist event/session state"]
    w8["8. Node -> App: close finalized"]
    w1 --> w2 --> w3 --> w4 --> w5 --> w6 --> w7 --> w8
  end
```

## 4) RGB-over-LN Flow Comparison

```mermaid
flowchart LR
  subgraph N["Native SDK diagram"]
    n1["1. App -> SDK: blind_receive / witness_receive(transport_endpoints)"]
    n2["2. SDK -> Native RGB wallet: execute receive flow"]
    n3["3. Wallet -> Native transport endpoints: exchange consignment data"]
    n4["4. App -> SDK: send_payment(invoice, asset_id?, asset_amount?)"]
    n5["5. SDK -> App: payment lifecycle updates"]
    n1 --> n2 --> n3 --> n4 --> n5
  end

  subgraph W["WASM diagram"]
    w1["1. App -> Wallet: setRgbProxyTransport(endpoint, auth_token?, node_id?)"]
    w2["2. Wallet -> Browser store: persist wallet-scoped config"]
    w3["3. App -> Wallet: blindReceive* / witnessReceive*"]
    w4["4. Wallet -> RGB Proxy HTTP: receive via resolved transport endpoint"]
    w5["5. App -> Node: sendPayment*(invoice, asset_id?, asset_amount?)"]
    w6["6. Node -> Browser store: persist payment/runtime event state"]
    w7["7. Node -> App: payment lifecycle updates"]
    w1 --> w2 --> w3 --> w4 --> w5 --> w6 --> w7
  end
```

## 5) Chain Sync Flow Comparison

```mermaid
flowchart LR
  subgraph N["Native SDK diagram"]
    n1["1. App -> SDK: start runtime/sync operations"]
    n2["2. SDK -> Native Runtime: enable chain watchers"]
    n3["3. Runtime -> Chain backend: query tip/confirmations/tx status (loop)"]
    n4["4. Runtime -> Native store: persist runtime snapshots"]
    n5["5. SDK -> App: synced channel/payment view"]
    n1 --> n2 --> n3 --> n4 --> n5
  end

  subgraph W["WASM diagram"]
    w1["1. App -> Node: chainSyncStart(indexer_url, poll_interval_ms?)"]
    w2["2. Node -> WasmChainSyncDriver: start()"]
    w3["3. Driver -> Esplora indexer: fetch tip/tx status (poll loop)"]
    w4["4. Driver -> Browser store: persist snapshot"]
    w5["5. App -> Node: chainSyncTick() or chainSyncStop()"]
    w1 --> w2 --> w3 --> w4 --> w5
  end
```
