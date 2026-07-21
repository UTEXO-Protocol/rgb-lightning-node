# Reproduction: Private Multihop Routing Failure

This directory contains an automated testing environment to reproduce a routing failure in `rgb-lightning-node` during multihop payments where the final hop is a private channel.

## Concept and Overview
In a Lightning Network multihop payment, the sender (Node A) must find a route to the receiver (Node C). If the last hop (Node B -> Node C) is a private channel, Node C must provide "route hints" in its invoice so that Node A knows how to reach it through Node B.

This test verifies if `rgb-lightning-node` correctly:
1.  Includes route hints when generating an invoice if it only has private channels.
2.  Can use those route hints to route a payment through an intermediate node.

### Topology
- **Node A** (Sender) - REST: 3611, LDK: 9735
- **Node B** (Intermediate) - REST: 3612, LDK: 9736
- **Node C** (Receiver) - REST: 3613, LDK: 9737

**Channels:**
- `A -> B`: Public
- `B -> C`: Private (`public: false`)

## Prerequisites
- Docker & Docker Compose
- Python 3
- **Python dependencies**:
  ```bash
  pip install -r requirements.txt
  ```
- **Install RLN binary**: Run `cargo install --locked --path .` in the project root. This ensures the binary is built and available.

## Automated Execution
1.  **Build and start the environment** (always build to pick up source changes):
    ```bash
    docker compose up -d --build
    ```
2.  **Run the reproduction script**:
    ```bash
    python3 test_routing.py
    ```

> **Note:** Always use `--build` when running after source code changes. Without it, Docker reuses the cached image and the binary will not reflect any modifications to `src/`.

## Verifying Node B Configuration

Node B should start with the accept_forwards_to_priv_channels configuration `data/nodeB/config.toml`:

```
[channels] 
accept_forwards_to_priv_channels = true
```

## Manual Debugging Workflow
If you want to manually interact with the nodes to debug:

1.  **Start base services**:
    You can use the local `docker-compose.yml` or the root `regtest.sh`.
    ```bash
    docker-compose up -d bitcoind electrs
    ```

2.  **Run RLN nodes locally** (optional, for logs/debugging):
    ```bash
    # Node A
    cargo run -- --network regtest --storage-dir /tmp/nodeA --ldk-peer-listening-port 9735 --rest-port 3611
    # Node B
    cargo run -- --network regtest --storage-dir /tmp/nodeB --ldk-peer-listening-port 9736 --rest-port 3612
    # Node C
    cargo run -- --network regtest --storage-dir /tmp/nodeC --ldk-peer-listening-port 9737 --rest-port 3613
    ```

## Expected Results
- **Success**: Node A finds the route to Node C using the route hints provided in the invoice and the payment status becomes `Succeeded`.
- **Failure**: Node A fails to find a route or the payment remains `Pending` / transitions to `Failed`.

## Debugging Errors
To inspect the logs for specific errors from the nodes:

1. **View latest errors from Node A (Sender)**:
   ```bash
   docker-compose logs node-a | grep ERROR
   ```

2. **Follow logs for Node B (Intermediate)**:
   ```bash
   docker-compose logs -f node-b
   ```

3. **Check for missing route hints in Node C's invoice**:
   If you suspect Node C (Receiver) is not generating route hints, you can decode the invoice (e.g., at lightningdecoder.com or using `lightning-cli decode`) to see if `route_hints` are present.
