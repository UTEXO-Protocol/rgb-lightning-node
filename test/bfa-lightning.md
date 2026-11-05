# BFA Lightning channels

Native REST and UniFFI/SDK channel opens accept BFA as a fungible assignment, like
NIA/CFA/IFA. The internal Lightning wallet uses the node's resolved Ethereum RPC
endpoint to validate BFA funding consignments. Endpoints are mirrored into the
Lightning KV store after a successful unlock and replaced on each subsequent
unlock. An unlock without an endpoint disables BFA and clears the old LN endpoint.
Pass `eth_rpc_url` on every unlock, or configure `[chain].eth_rpc_url`.

Both peers, including forwarding nodes, need this configuration. Nodes running
older versions reject BFA funding. Mainnet remains on-chain only, and the separate
WASM RGB backend is not enabled by this native change.

## Local integration test

Use the repository's regtest bitcoind, electrs and proxy. This test does not reset
the chain. It creates fresh wallets, retains them and prints their location.
It also requires a local Anvil with chain ID 31337, `cast` in its container, and
`TestERC20`/`MinimalBridge` deployed from the pinned bfa.5 rgb-lib `tests/contracts` sources.
Approve the bridge to spend at least 1000 token base units from Anvil account
`0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266` for each run. See the deployment
requirements in [external-burn.md](external-burn.md#bfa-proof).

```sh
export RLN_BFA_ETH_RPC=http://127.0.0.1:29545
export RLN_BFA_BRIDGE=<local-MinimalBridge-address>
export RLN_BFA_ANVIL_CONTAINER=<local-anvil-container>
cargo test --locked --features uniffi,vls,test-utils --test lib_sdk \
  bfa_channels_multihop_restart_close_and_burn -- --ignored --nocapture
```

`RLN_BFA_PROXY` optionally overrides `rpc://127.0.0.1:3000/json-rpc` when that
host port is already in use. Bitcoin RPC and electrs use the repository defaults.

The test locks ERC-20 tokens and mints 1000 BFA, opens private A–B and B–C channels with
initial pushed assets (C learns the contract from funding), pays A→C, restarts all
three nodes, pays C→A, cooperatively closes A–B, force-closes B–C and checks the
recovered on-chain balances total 1000. It then burns 10 of C's recovered units
and validates the recipient in the exported burn proof.
The bridge must expose `getFundsInBlock(uint256,uint256)` and the bfa.5
`fundsIn(uint256,uint256,uint64)` lock call. On Anvil, install the pinned
`MockArbSys` runtime at address `0x0000000000000000000000000000000000000064`
so the Arbitrum L2-height interface returns the local block number. The test
derives one exclusive `before_block` from that fixture's current height and
uses it in both the RGB operation and the EVM lock. Older bridge bytecode is
incompatible; an unchanged EVM chain ID alone does not qualify the deployment.
The fixture explicitly enables private-channel forwarding; production defaults
remain unchanged (`[channels].accept_forwards_to_priv_channels` is opt-in).

Unit tests also cover clearing stale endpoints, legacy stores with no endpoint,
schema selection and preserving inactive mainnet state. A VSS service is not
required by this integration test; it covers local restart, not a full VSS restore.
