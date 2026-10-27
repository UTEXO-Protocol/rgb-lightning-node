#!/usr/bin/env python3
"""Read-only regtest fixture probe: pending descendants of one exact channel outpoint.

This script never mines, starts/stops services, or changes a wallet. The caller
owns its existing test miner and decides when confirmation is appropriate.
"""
import json
from pathlib import Path
import re
import subprocess
import sys
import time

MAX_DEPTH = 32
MAX_TRANSACTIONS = 256
PROBE_SECONDS = 8.0
RPC_SECONDS = 2.0
MAX_RPC_ATTEMPTS = 3
READ_ONLY_METHODS = frozenset({"getblockchaininfo", "getrawmempool", "getrawtransaction"})
TXID = re.compile(r"[0-9a-f]{64}\Z")


def identifier(value):
    if not isinstance(value, str) or not TXID.fullmatch(value):
        raise ValueError("expected a canonical 32-byte hexadecimal identifier")
    return value


def funding_outpoint(funding_txid, channel_id):
    """Invert LDK ChannelId::v1_from_funding_txid; never guess another output."""
    funding = bytes.fromhex(identifier(funding_txid))[::-1]
    channel = bytes.fromhex(identifier(channel_id))
    if funding[:30] != channel[:30]:
        raise ValueError("channel ID does not bind this v1 funding transaction")
    index = ((funding[30] ^ channel[30]) << 8) | (funding[31] ^ channel[31])
    return funding_txid, index


def candidates(funding_txid, channel_id, mempool, transaction):
    root_txid, root_index = funding_outpoint(funding_txid, channel_id)
    root = transaction(root_txid)
    outputs = root.get("vout", [])
    if root_index >= len(outputs) or outputs[root_index].get("n") != root_index:
        raise ValueError("channel funding output is missing")
    if outputs[root_index].get("scriptPubKey", {}).get("type") != "witness_v0_scripthash":
        raise ValueError("fixture requires the exact v1 P2WSH channel funding output")
    cache = {root_txid: root}
    matches = {}

    def descends(txid, depth, active):
        identifier(txid)
        if txid == root_txid:
            return False
        if txid in matches:
            return matches[txid]
        if depth > MAX_DEPTH or txid in active:
            return False
        if txid not in cache:
            if len(cache) >= MAX_TRANSACTIONS:
                raise ValueError("fixture transaction ancestry exceeds its bound")
            cache[txid] = transaction(txid)
        inputs = cache[txid].get("vin", [])
        for entry in inputs:
            parent = entry.get("txid")
            if parent == root_txid:
                if entry.get("vout") == root_index:
                    matches[txid] = True
                    return True
                # A spend of the funding transaction's wallet change is not a
                # channel descendant, even if its other ancestry shares inputs.
                continue
            if parent is not None and descends(parent, depth + 1, active | {txid}):
                matches[txid] = True
                return True
        matches[txid] = False
        return False

    if not isinstance(mempool, list) or len(mempool) > MAX_TRANSACTIONS:
        raise ValueError("fixture mempool exceeds its bound")
    return sorted(txid for txid in mempool if descends(identifier(txid), 0, set()))


def remaining_budget(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("channel sweep probe exceeded its total time budget")
    return remaining


def rpc(method, *arguments, deadline):
    if method not in READ_ONLY_METHODS:
        raise ValueError("channel sweep probe permits only its explicit read-only RPC methods")
    command = ["docker", "compose", "exec", "-T", "-u", "blits", "bitcoind",
               "bitcoin-cli", "-regtest", "-rpcuser=user", "-rpcpassword=password", method]
    command.extend(str(arg) if not isinstance(arg, (dict, list)) else json.dumps(arg)
                   for arg in arguments)
    for attempt in range(MAX_RPC_ATTEMPTS):
        remaining = remaining_budget(deadline)
        try:
            result = subprocess.run(command, cwd=Path(__file__).resolve().parents[1],
                                    capture_output=True, text=True,
                                    timeout=min(RPC_SECONDS, remaining), check=True)
        except subprocess.TimeoutExpired:
            # Retry only the identical idempotent read, within the original probe budget.
            # Nonzero RPC errors, malformed responses and validation failures stay strict.
            if attempt + 1 == MAX_RPC_ATTEMPTS:
                raise
            continue
        value = json.loads(result.stdout)
        # Process creation, scheduling and parsing can outlive a subprocess timeout.
        # Never authorize a result returned after the shared probe deadline.
        remaining_budget(deadline)
        return value


def main():
    if len(sys.argv) != 3:
        raise ValueError("usage: channel_sweep_candidates.py FUNDING_TXID CHANNEL_ID")
    funding_outpoint(*sys.argv[1:])  # Validate input before contacting any service.
    deadline = time.monotonic() + PROBE_SECONDS
    def request(method, *arguments):
        return rpc(method, *arguments, deadline=deadline)
    if request("getblockchaininfo").get("chain") != "regtest":
        raise ValueError("fixture probe is restricted to regtest")
    output = json.dumps(candidates(*sys.argv[1:], request("getrawmempool"),
                                   lambda txid: request("getrawtransaction", txid, "true")))
    remaining_budget(deadline)
    print(output)


if __name__ == "__main__":
    main()
