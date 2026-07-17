import os
from pathlib import Path

# Reproduction Root
REPRO_ROOT = Path(__file__).resolve().parent

# Ports
NODE_A_REST_PORT = int(os.getenv("NODE_A_REST_PORT", "3611"))
NODE_B_REST_PORT = int(os.getenv("NODE_B_REST_PORT", "3612"))
NODE_C_REST_PORT = int(os.getenv("NODE_C_REST_PORT", "3613"))

NODE_A_PEER_PORT = int(os.getenv("NODE_A_PEER_PORT", "9735"))
NODE_B_PEER_PORT = int(os.getenv("NODE_B_PEER_PORT", "9736"))
NODE_C_PEER_PORT = int(os.getenv("NODE_C_PEER_PORT", "9737"))

# Passwords
NODE_A_PASSWORD = os.getenv("NODE_A_PASSWORD", "password123")
NODE_B_PASSWORD = os.getenv("NODE_B_PASSWORD", "password123")
NODE_C_PASSWORD = os.getenv("NODE_C_PASSWORD", "password123")

# Logic Defaults
OPEN_CHANNEL_CAPACITY_SAT = int(os.getenv("OPEN_CHANNEL_CAPACITY_SAT", "500000"))
PAYMENT_MSAT = int(os.getenv("PAYMENT_MSAT", "3100000"))

# RPC/Services
BITCOIND_RPC_URL = os.getenv("BITCOIND_RPC_URL", "http://localhost:18443")
BITCOIND_RPC_USER = os.getenv("BITCOIND_RPC_USER", "user")
BITCOIND_RPC_PASS = os.getenv("BITCOIND_RPC_PASS", "password")
ESPLORA_URL = os.getenv("ESPLORA_URL", "http://localhost:3002")
PROXY_ENDPOINT = os.getenv("PROXY_ENDPOINT", "rpc://proxy:3000/json-rpc")
