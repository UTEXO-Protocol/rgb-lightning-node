import json
import requests
import time
import subprocess
import os
from config import (
    NODE_A_REST_PORT, NODE_B_REST_PORT, NODE_C_REST_PORT,
    NODE_A_PEER_PORT, NODE_B_PEER_PORT, NODE_C_PEER_PORT,
    NODE_A_PASSWORD, NODE_B_PASSWORD, NODE_C_PASSWORD,
    OPEN_CHANNEL_CAPACITY_SAT, PAYMENT_MSAT,
    BITCOIND_RPC_URL, BITCOIND_RPC_USER, BITCOIND_RPC_PASS,
    ESPLORA_URL, PROXY_ENDPOINT
)

# Configuration Mapping
NODES = {
    "A": {"url": f"http://localhost:{NODE_A_REST_PORT}", "name": "node-a", "ldk_port": NODE_A_PEER_PORT, "password": NODE_A_PASSWORD},
    "B": {"url": f"http://localhost:{NODE_B_REST_PORT}", "name": "node-b", "ldk_port": NODE_B_PEER_PORT, "password": NODE_B_PASSWORD},
    "C": {"url": f"http://localhost:{NODE_C_REST_PORT}", "name": "node-c", "ldk_port": NODE_C_PEER_PORT, "password": NODE_C_PASSWORD},
}

# Path to docker-compose file for this test
COMPOSE_FILE = os.path.abspath(os.path.join(os.path.dirname(__file__), "docker-compose.yml"))

def bitcoin_rpc_call(method, params, wallet=None):
    """Call bitcoind RPC directly."""
    base_url = BITCOIND_RPC_URL
    auth = (BITCOIND_RPC_USER, BITCOIND_RPC_PASS)
    
    def _call(m, p, w=None):
        url = base_url
        if w:
            url = f"{url}/wallet/{w}"
        payload = {"jsonrpc": "1.0", "id": "repro-test", "method": m, "params": p}
        res = requests.post(url, json=payload, auth=auth, timeout=10)
        return res.status_code, res.json()

    for _ in range(30):
        try:
            status, res_json = _call(method, params, wallet)
            if status == 200:
                return res_json.get("result")
            
            error = res_json.get("error")
            if error and error.get("code") == -18: # Wallet not found
                # Try to create/load the wallet
                _call("createwallet", ["miner"])
                _call("loadwallet", ["miner"])
                # After trying to create/load, we'll try the original call again in the next loop iteration
                wallet = "miner"
                continue
            
            if error:
                raise Exception(f"RPC {method} failed: {error}")
            raise Exception(f"RPC {method} failed with status {status}")
            
        except requests.exceptions.ConnectionError:
            time.sleep(2)
    raise Exception(f"Failed to connect to bitcoind at {base_url}")

def wait_for_service(url, name):
    print(f"Waiting for {name} at {url}...")
    for _ in range(60):
        try:
            requests.get(url)
            print(f"{name} is up!")
            return
        except:
            time.sleep(2)
    raise Exception(f"Timeout waiting for {name}")

def init_node(node_key):
    node = NODES[node_key]
    url = node["url"]
    label = f"Node {node_key}"
    password = node["password"]
    print(f"Initializing {label}...")
    requests.post(f"{url}/init", json={"password": password})
    payload = {
        "password": password,
        "indexer_url": "http://esplora:3002",
        "proxy_endpoint": PROXY_ENDPOINT,
        "announce_addresses": []
    }

    res = requests.post(f"{url}/unlock", json=payload)
    print(f"{label} unlock: {res.status_code}")

def get_address(node_key):
    return requests.post(f"{NODES[node_key]['url']}/address").json()["address"]

def mine_blocks(count=1):
    miner_addr = bitcoin_rpc_call("getnewaddress", [], wallet="miner")
    bitcoin_rpc_call("generatetoaddress", [count, miner_addr], wallet="miner")

def fund_node(node_key, amount_btc="0.1"):
    addr = get_address(node_key)
    label = f"Node {node_key}"
    print(f"Funding {label} ({addr}) with {amount_btc} BTC...")
    
    # 1. Get miner address
    miner_addr = bitcoin_rpc_call("getnewaddress", [], wallet="miner")
    
    # 2. Mine enough blocks to mature (101 blocks) if we haven't already
    # For regtest, we need at least 100 blocks to spend coinbase.
    # We'll just mine 103 to be safe.
    bitcoin_rpc_call("generatetoaddress", [103, miner_addr], wallet="miner")
    
    # 3. Send funds
    bitcoin_rpc_call("sendtoaddress", [addr, amount_btc], wallet="miner")
    
    # 4. Confirm (6 blocks)
    bitcoin_rpc_call("generatetoaddress", [6, miner_addr], wallet="miner")
    
    print(f"Waiting for {label} to see funds...")
    for _ in range(30):
        try:
            res = requests.post(f"{NODES[node_key]['url']}/btcbalance", json={
                "skip_sync": False
            })
            balance = res.json()["vanilla"]["spendable"]
            if int(balance) > 0:
                print(f"{label} funds detected: {balance} sats")
                return
        except:
            pass
        time.sleep(2)
    raise Exception(f"Timeout: {label} never received funds")

def connect_peer(src_key, dest_key, dest_pubkey):
    src_url = NODES[src_key]["url"]
    dest_node = NODES[dest_key]
    peer_info = f"{dest_pubkey}@{dest_node['name']}:{dest_node['ldk_port']}"
    print(f"Connecting {src_key} to {dest_key} ({peer_info})...")
    res = requests.post(f"{src_url}/connectpeer", json={"peer_pubkey_and_addr": peer_info})
    return res.status_code == 200

def open_channel(src_key, dest_key, dest_pubkey, capacity_sat, is_public=True):
    src_url = NODES[src_key]["url"]
    if not connect_peer(src_key, dest_key, dest_pubkey):
        return None
    dest_node = NODES[dest_key]
    peer_info = f"{dest_pubkey}@{dest_node['name']}:{dest_node['ldk_port']}"
    print(f"Opening {'public' if is_public else 'private'} channel from {src_key} to {dest_key}")
    payload = {
        "peer_pubkey_and_opt_addr": peer_info,
        "capacity_sat": capacity_sat,
        "push_msat": 0,
        "public": is_public,
        "with_anchors": True
    }
    res = requests.post(f"{src_url}/openchannel", json=payload)
    return res.json().get("temporary_channel_id") if res.status_code == 200 else None

def wait_for_channel(node_key):
    print(f"Waiting for channel on Node {node_key} to become usable...")
    url = NODES[node_key]['url']
    for _ in range(30):
        res = requests.get(f"{url}/listchannels")
        channels = res.json()["channels"]
        if any(c["ready"] and c["is_usable"] for c in channels):
            print(f"Channel on Node {node_key} is ready!")
            return True
        time.sleep(5)
    return False

def main():
    # 1. Wait for Nodes and Init
    for key in NODES:
        wait_for_service(NODES[key]["url"], f"Node {key}")
        init_node(key)

    # 3. Fund Nodes via Gateway Faucet
    fund_node("A")
    fund_node("B")

    # 4. Get Pubkeys
    node_infos = {key: requests.get(f"{NODES[key]['url']}/nodeinfo").json() for key in NODES}
    pubkeys = {key: info["pubkey"] for key, info in node_infos.items()}

    # 5. Connect B to C (Private)
    open_channel("B", "C", pubkeys["C"], OPEN_CHANNEL_CAPACITY_SAT, is_public=False)
    # Mine blocks to confirm channel
    mine_blocks(6)
    wait_for_channel("B")

    # 6. Connect A to B (Public)
    open_channel("A", "B", pubkeys["B"], OPEN_CHANNEL_CAPACITY_SAT, is_public=True)
    mine_blocks(6)
    wait_for_channel("A")

    # 7. Attempt Payment A -> C
    print("Requesting invoice from Node C...")
    invoice_res = requests.post(f"{NODES['C']['url']}/lninvoice", json={
        "amt_msat": PAYMENT_MSAT,
        "expiry_sec": 3600
    })
    invoice = invoice_res.json()["invoice"]
    print(f"Invoice from Node C: {invoice}")

    print("Attempting payment from Node A to Node C via Node B...")
    payment_res = requests.post(f"{NODES['A']['url']}/sendpayment", json={"invoice": invoice})
    print(f"Payment result: {json.dumps(payment_res.json(), indent=2)}")

    if payment_res.status_code == 200:
        payment_id = payment_res.json()["payment_id"]
        for _ in range(20):
            status_res = requests.get(f"{NODES['A']['url']}/listpayments")
            p = next((x for x in status_res.json()["payments"] if x["payment_hash"] == payment_id), None)
            if p:
                print(f"Status: {p['status']}")
                if p['status'] == 'Succeeded':
                    print("SUCCESS! Multihop over private channel worked.")
                    break
                if p['status'] == 'Failed':
                    print("REPRODUCED: Payment failed to route over private hop.")
                    try:
                        # Fetch logs and filter for ERROR, taking the last 5
                        log_cmd = f"docker-compose -f {COMPOSE_FILE} logs --tail=500 node-a | grep ERROR | tail -n 5"
                        subprocess.run(log_cmd, shell=True, check=False)
                    except: pass
                    break
            time.sleep(2)

if __name__ == "__main__":
    main()
