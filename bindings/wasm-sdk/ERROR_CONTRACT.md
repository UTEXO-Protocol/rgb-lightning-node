# RLN WASM SDK Error/Unsupported Contract

This document defines stable error behavior for `bindings/wasm-sdk`.

## Principles

1. Validation failures return deterministic human-readable strings.
2. Native-only RLN runtime operations are not exposed in this crate.
3. If native-only operations are introduced in wasm surface, they must return a stable unsupported error string:
   - `unsupported on wasm: <operation>`

## Validation Error Strings (current)

1. `unsupported network: <value>`
2. `mnemonic cannot be empty`
3. `asset_id cannot be empty`
4. `asset_id cannot be empty if provided`
5. `address cannot be empty`
6. `indexer_url cannot be empty`
7. `password cannot be empty`
8. `proxy_url cannot be empty`
9. `invoice_string cannot be empty`
10. `server_url cannot be empty`
11. `store_id cannot be empty`
12. `signing_key_hex must be exactly 64 hex chars (32 bytes), got <len>`
13. `Invalid WalletData JSON: <serde_error>`
14. `Invalid Online object: <serde_error>`
15. `Invalid assignment: <serde_error>`
16. `Invalid transport endpoints: <serde_error>`
17. `Invalid recipient map: <serde_error>`
18. `Invalid filter: <serde_error>`
19. `Invalid schemas: <serde_error>`
20. `Invalid inflation_amounts array: <serde_error>`
21. `relay_auth_token and relay_node_id must be provided together`
22. `relay_auth_token cannot be empty`
23. `relay_node_id cannot be empty`
24. `invalid relay_node_id`
25. `invalid peer_pubkey`
26. `rgb_proxy_endpoint cannot be empty`
27. `rgb_proxy_endpoint must use http:// or https://`
28. `rgb_proxy_auth_token and rgb_proxy_node_id must be provided together`
29. `rgb_proxy_auth_token cannot be empty`
30. `rgb_proxy_node_id cannot be empty`
31. `invalid rgb_proxy_node_id`
32. `transport_endpoints must be provided or setRgbProxyTransport must be configured`

## Unsupported Surface (not exposed by design)

If an RLN-native operation is intentionally unavailable in WASM, it must return a stable unsupported error string:

- `unsupported on wasm: <operation>`

Current examples of intentionally unsupported-by-design functionality are documented in
`SDK_WASM_ENDPOINT_MATRIX.md` (for example UDA issuance).
