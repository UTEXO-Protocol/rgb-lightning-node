# External-signer burn tests

Run against disposable local regtest funds only. The tests retain their wallet
directories and print the evidence path. They do not reset existing services.

## IFA and failure handling

Start the repository's regtest services, then run:

```sh
cargo test --locked --features uniffi,vls,test-utils --test lib_sdk \
  external_burn_funded_and_recovery -- --nocapture
```

This covers a funded IFA receipt, native external burn, attached-host burn,
transaction confirmation, balances, burn history, consignment retrieval,
invalid requests, unavailable signer, changed transaction, unfinalized PSBT,
lost signing response, retained input reservations, reopening, explicit
failure reconciliation and concurrent burns competing for the same input. It
also burns from the password-signer issuer.

The injected failures never broadcast; the test can therefore explicitly fail
those transfers. An application must not assume the same after an ambiguous
production error.

Endpoint overrides:

| Variable | Default |
| --- | --- |
| `RLN_BURN_BITCOIND` | `http://127.0.0.1:18443/wallet/miner` |
| `RLN_BURN_RPC_USER` | `user` |
| `RLN_BURN_RPC_PASSWORD` | `password` |
| `RLN_BURN_INDEXER` | `tcp://127.0.0.1:50001` |
| `RLN_BURN_PROXY` | `rpc://127.0.0.1:3000/json-rpc` |

## BFA proof

This opt-in test also needs Anvil (chain ID 31337) and the `TestERC20` and
`BaseBridge` contracts from the pinned rgb-lib source's `tests/contracts`.
Deploy both to local Anvil and approve BaseBridge to spend at least 1000 token
base units from Anvil's standard account
`0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`.

The Anvil container must provide `cast` and listen internally on port 8545.
Set the external RPC URL, bridge address and container name explicitly:

```sh
export RLN_BURN_ETH_RPC=http://127.0.0.1:29545
export RLN_BURN_BRIDGE=<local-deployed-BaseBridge-address>
export RLN_BURN_ANVIL_CONTAINER=<local-anvil-container>
cargo test --locked --features uniffi,vls,test-utils --test lib_sdk \
  external_burn_bfa_proof -- --ignored --nocapture
```

The test checks both chain identities, locks real fixture ERC-20 tokens, bridges
1000 units into an external-signer wallet, then burns 100 and 200 units to two
different 32-byte recipients. It checks Bitcoin confirmation, remaining
spendable balances, exported proof bytes and the recipient committed in each
terminal burn. Missing and malformed recipients must not consume funds.

It does not execute an Ethereum payout or qualify public-network BFA validation.
