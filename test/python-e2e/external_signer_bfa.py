"""External-signer unlock regression tests against a running Bitcoin regtest indexer.

The Ethereum server is a local protocol fixture, not a funded BFA bridge. These
tests exercise the real RLN/rgb-lib unlock path through UniFFI and the C ABI.
"""

import argparse
import ctypes as ct
import json
import os
import secrets
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class EthereumRPC:
    CHAIN_ID = "0x7a69"
    FUNDS_IN_TOPIC = "0xf1a18caea297591892fc07ea412a5e617d8e51e1155912d8871793e1d4e70f87"

    def __init__(self):
        self.requests = []
        self.reject = False
        self.reject_method = None
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                fixture.requests.append(request)
                response = {"jsonrpc": "2.0", "id": request["id"]}
                method = request.get("method")
                valid, result = fixture.answer(request)
                if not valid:
                    response["error"] = {"code": -32601, "message": "unexpected method or params"}
                elif fixture.reject or fixture.reject_method == method:
                    response["error"] = {"code": -32000, "message": "bfa-test-rpc-rejected"}
                else:
                    response["result"] = result
                body = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}/rpc"

    @classmethod
    def answer(cls, request):
        """Only the exact endpoint-admission protocol is simulated, never a mint or burn."""
        method, params = request.get("method"), request.get("params")
        if request.get("jsonrpc") != "2.0" or "id" not in request:
            return False, None
        if method == "eth_chainId" and params == []:
            return True, cls.CHAIN_ID
        if (method == "eth_getBlockByNumber" and isinstance(params, list)
                and len(params) == 2 and params[0] == "finalized" and params[1] is False):
            return True, {"number": "0x64"}
        if method == "eth_getLogs" and params == [{
            "address": "0x" + "00" * 20,
            "fromBlock": "0x0",
            "toBlock": "0x0",
            "topics": [cls.FUNDS_IN_TOPIC, None, "0x" + "00" * 32],
        }]:
            return True, []
        return False, None

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError("Ethereum fixture did not stop")


def init_request(path):
    return dict(
        storage_dir_path=path,
        daemon_listening_port=0,
        ldk_peer_listening_port=0,
        network="regtest",
        max_media_upload_size_mb=5,
        enable_virtual_channels_v0=False,
        virtual_peer_pubkeys=[],
        lsp_base_url=None,
        lsp_bearer_token=None,
    )


def unlock_request():
    indexer = os.getenv("RLN_BFA_TEST_INDEXER", "tcp://127.0.0.1:50001")
    return dict(
        ldk_chain_sync={"mode": "TransactionSync", "config": {"indexer_url": indexer}},
        indexer_url=indexer,
        proxy_endpoint=os.getenv("RLN_BFA_TEST_PROXY", "rpc://127.0.0.1:3000/json-rpc"),
        announce_addresses=[],
        announce_alias="bfa-unlock-test",
    )


class UniffiWallet:
    def __init__(self, path):
        import rgb_lightning_node as rln

        self.rln = rln
        self.node = rln.SdkNode.create(rln.SdkInitRequest(**init_request(path)))
        self.signer = None
        try:
            self.signer = rln.NativeExternalSigner(secrets.token_hex(32), "regtest", False)
            self.node.init_with_native_external_signer(self.signer)
        except BaseException:
            self.close()
            raise

    def unlock(self, request, attached=False, legacy=False, mismatch=False):
        request = dict(request)
        request["ldk_chain_sync"] = self.rln.SdkLdkChainSync.TRANSACTION_SYNC(
            **request["ldk_chain_sync"]["config"]
        )
        signer = (self.rln.NativeExternalSigner(secrets.token_hex(32), "regtest", False)
                  if mismatch else self.signer)
        if attached:
            self.node.attach_native_external_signer(signer)
            if legacy:
                self.node.unlock_with_attached_external_signer(**request)
            else:
                self.node.unlock_with_attached_external_signer_request(
                    self.rln.SdkExternalUnlockRequest(**request)
                )
        elif legacy:
            self.node.unlock_with_native_external_signer(signer, **request)
        else:
            self.node.unlock_with_native_external_signer_request(
                signer, self.rln.SdkExternalUnlockRequest(**request)
            )

    def assert_unlocked(self, test):
        test.assertEqual(self.node.node_info().pubkey, self.signer.bootstrap().node_id)
        self.node.btc_balance(True)
        self.node.list_assets([])

    def assert_locked(self, test):
        with test.assertRaises(self.rln.RlnError.NotInitialized):
            self.node.btc_balance(True)

    def close(self):
        self.node.shutdown()
        self.node = None
        self.signer = None


class Opaque(ct.Structure):
    _fields_ = [("ptr", ct.c_void_p), ("ty", ct.c_uint64)]


class HandleResult(ct.Structure):
    _fields_ = [("result", ct.c_int), ("inner", Opaque)]


class StringResult(ct.Structure):
    _fields_ = [("result", ct.c_int), ("inner", ct.c_void_p)]


class CffiWallet:
    library = None

    def __init__(self, path):
        self.lib = ct.CDLL(self.library)
        signatures = {
            "rln_sdk_node_new": (HandleResult, [ct.c_char_p]),
            "rln_native_external_signer_new": (HandleResult, [ct.c_char_p, ct.c_char_p, ct.c_bool]),
            "rln_sdk_node_init_with_native_external_signer": (StringResult, [ct.POINTER(Opaque)] * 2),
            "rln_sdk_node_attach_native_external_signer": (StringResult, [ct.POINTER(Opaque)] * 2),
            "rln_sdk_node_unlock_with_native_external_signer":
                (StringResult, [ct.POINTER(Opaque), ct.POINTER(Opaque), ct.c_char_p]),
            "rln_sdk_node_unlock_with_attached_external_signer":
                (StringResult, [ct.POINTER(Opaque), ct.c_char_p]),
            "rln_node_info": (StringResult, [ct.POINTER(Opaque)]),
            "rln_btc_balance": (StringResult, [ct.POINTER(Opaque), ct.c_bool]),
            "rln_sdk_node_shutdown": (StringResult, [ct.POINTER(Opaque)]),
            "free_sdk_node": (None, [Opaque]),
            "free_native_external_signer": (None, [Opaque]),
            "rln_free_string": (None, [ct.c_void_p]),
        }
        for name, (result, args) in signatures.items():
            fn = getattr(self.lib, name)
            fn.restype, fn.argtypes = result, args
        self.node = self.handle(self.lib.rln_sdk_node_new(json.dumps(init_request(path)).encode()))
        self.signer = None
        try:
            self.signer = self.new_signer()
            self.string(self.lib.rln_sdk_node_init_with_native_external_signer(
                ct.byref(self.node), ct.byref(self.signer)))
        except BaseException:
            self.close()
            raise

    def handle(self, result):
        if result.result:
            # CResult errors contain an owned CString in the opaque payload.
            message = ct.string_at(result.inner.ptr).decode()
            self.lib.rln_free_string(result.inner.ptr)
            raise RuntimeError(message)
        return result.inner

    def string(self, result):
        try:
            message = ct.string_at(result.inner).decode()
            if result.result:
                raise RuntimeError(message)
            return json.loads(message)
        finally:
            self.lib.rln_free_string(result.inner)

    def new_signer(self):
        return self.handle(self.lib.rln_native_external_signer_new(
            secrets.token_hex(32).encode(), b"regtest", False))

    def unlock(self, request, attached=False, legacy=False, mismatch=False):
        signer = self.new_signer() if mismatch else self.signer
        try:
            body = json.dumps(request).encode()
            if attached:
                self.string(self.lib.rln_sdk_node_attach_native_external_signer(
                    ct.byref(self.node), ct.byref(signer)))
                result = self.lib.rln_sdk_node_unlock_with_attached_external_signer(
                    ct.byref(self.node), body)
            else:
                result = self.lib.rln_sdk_node_unlock_with_native_external_signer(
                    ct.byref(self.node), ct.byref(signer), body)
            self.string(result)
        finally:
            if mismatch:
                self.lib.free_native_external_signer(signer)

    def assert_unlocked(self, test):
        test.assertIn("pubkey", self.string(self.lib.rln_node_info(ct.byref(self.node))))
        self.string(self.lib.rln_btc_balance(ct.byref(self.node), True))

    def assert_locked(self, test):
        with test.assertRaisesRegex(RuntimeError, r"Rln\(NotInitialized\)"):
            self.string(self.lib.rln_btc_balance(ct.byref(self.node), True))

    def close(self):
        try:
            self.string(self.lib.rln_sdk_node_shutdown(ct.byref(self.node)))
        finally:
            self.lib.free_sdk_node(self.node)
            if self.signer is not None:
                self.lib.free_native_external_signer(self.signer)


class EthereumRPCProtocolTests(unittest.TestCase):
    def test_exact_endpoint_admission_shapes(self):
        calls = [
            ("eth_chainId", [], EthereumRPC.CHAIN_ID),
            ("eth_getBlockByNumber", ["finalized", False], {"number": "0x64"}),
            ("eth_getLogs", [{
                "address": "0x" + "00" * 20,
                "fromBlock": "0x0",
                "toBlock": "0x0",
                "topics": [EthereumRPC.FUNDS_IN_TOPIC, None, "0x" + "00" * 32],
            }], []),
        ]
        for method, params, result in calls:
            with self.subTest(method=method):
                self.assertEqual(EthereumRPC.answer({
                    "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
                }), (True, result))

    def test_unknown_methods_and_inexact_params_are_not_successes(self):
        calls = [
            ("web3_clientVersion", []),
            ("eth_chainId", {}),
            ("eth_chainId", None),
            ("eth_getBlockByNumber", ["latest", False]),
            ("eth_getBlockByNumber", ["finalized", 0]),
            ("eth_getLogs", [{}]),
            ("eth_getLogs", []),
        ]
        for method, params in calls:
            with self.subTest(method=method, params=params):
                self.assertEqual(EthereumRPC.answer({
                    "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
                }), (False, None))


class ExternalSignerBfaTests(unittest.TestCase):
    wallet_class = UniffiWallet

    def setUp(self):
        self.rpc = EthereumRPC()
        self.addCleanup(self.rpc.close)

    def wallet(self):
        directory = tempfile.TemporaryDirectory(prefix="rln-external-bfa-")
        self.addCleanup(directory.cleanup)
        wallet = self.wallet_class(directory.name)
        self.addCleanup(wallet.close)
        return wallet

    def assert_rpc_admission(self):
        methods = [r["method"] for r in self.rpc.requests]
        self.assertTrue(methods)
        self.assertEqual(methods[0], "eth_chainId")  # RLN discovers the endpoint's actual chain.
        self.assertGreaterEqual(methods.count("eth_chainId"), 2)  # RGB verifies the configured chain.
        self.assertIn("eth_getBlockByNumber", methods)
        self.assertIn("eth_getLogs", methods)
        for request in self.rpc.requests:
            self.assertTrue(EthereumRPC.answer(request)[0], "unexpected EVM admission request")

    def assert_rpc_rejected(self, wallet, request, attached, expected_stage):
        error_type = (wallet.rln.RlnError.InvalidRequest
                      if isinstance(wallet, UniffiWallet) else RuntimeError)
        with self.assertRaises(error_type) as error:
            wallet.unlock(request, attached=attached)
        message = str(error.exception)
        if isinstance(wallet, CffiWallet):
            self.assertTrue(message.startswith("Rln(InvalidRequest): "))
        self.assertIn("Invalid Ethereum RPC URL:", message)
        self.assertIn(expected_stage, message)
        wallet.assert_locked(self)

    def test_both_unlock_paths_forward_eth_rpc(self):
        for attached in [False, True]:
            with self.subTest(attached=attached):
                self.rpc.requests.clear()
                wallet = self.wallet()
                wallet.unlock(dict(unlock_request(), eth_rpc_url=self.rpc.url), attached=attached)
                wallet.assert_unlocked(self)
                self.assert_rpc_admission()

    def test_existing_callers_without_eth_rpc_still_unlock(self):
        for attached in [False, True]:
            with self.subTest(attached=attached):
                wallet = self.wallet()
                wallet.unlock(unlock_request(), attached=attached, legacy=True)
                wallet.assert_unlocked(self)
                self.assertEqual(self.rpc.requests, [])

    def test_request_accepts_omitted_and_null_eth_rpc(self):
        for attached in [False, True]:
            for explicit_null in [False, True]:
                with self.subTest(attached=attached, explicit_null=explicit_null):
                    wallet = self.wallet()
                    request = unlock_request()
                    if explicit_null:
                        request["eth_rpc_url"] = None
                    wallet.unlock(request, attached=attached)
                    wallet.assert_unlocked(self)
                    self.assertEqual(self.rpc.requests, [])

    def test_rpc_failure_does_not_unlock_and_allows_retry(self):
        for attached in [False, True]:
            with self.subTest(attached=attached):
                wallet = self.wallet()
                self.rpc.requests.clear()
                request = dict(unlock_request(), eth_rpc_url=self.rpc.url)
                self.rpc.reject = True
                try:
                    self.assert_rpc_rejected(wallet, request, attached, "EVM RPC returned an error")
                    self.assertEqual([r["method"] for r in self.rpc.requests], ["eth_chainId"])
                finally:
                    self.rpc.reject = False
                self.rpc.requests.clear()
                wallet.unlock(request, attached=attached)
                wallet.assert_unlocked(self)
                self.assert_rpc_admission()

    def test_mint_support_failures_leave_wallet_locked_and_allow_valid_retry(self):
        stages = {
            "eth_getBlockByNumber": "cannot get the latest finalized block",
            "eth_getLogs": "cannot get the logs of the oldest blocks",
        }
        for attached in [False, True]:
            for method, expected_stage in stages.items():
                with self.subTest(attached=attached, rejected_method=method):
                    wallet = self.wallet()
                    self.rpc.requests.clear()
                    self.rpc.reject_method = method
                    request = dict(unlock_request(), eth_rpc_url=self.rpc.url)
                    try:
                        self.assert_rpc_rejected(wallet, request, attached, expected_stage)
                        methods = [r["method"] for r in self.rpc.requests]
                        self.assertEqual(methods[-1], method)
                        if method == "eth_getBlockByNumber":
                            self.assertNotIn("eth_getLogs", methods)
                    finally:
                        self.rpc.reject_method = None
                    self.rpc.requests.clear()
                    wallet.unlock(request, attached=attached)
                    wallet.assert_unlocked(self)
                    self.assert_rpc_admission()

    def test_invalid_rpc_url_is_not_ignored(self):
        for attached in [False, True]:
            for endpoint in ["", "not a URL"]:
                with self.subTest(attached=attached, endpoint=endpoint):
                    wallet = self.wallet()
                    with self.assertRaisesRegex(Exception, "Ethereum RPC"):
                        wallet.unlock(dict(unlock_request(), eth_rpc_url=endpoint), attached=attached)
                    wallet.assert_locked(self)
                    self.assertEqual(self.rpc.requests, [])

    def test_signer_mismatch_is_rejected_before_rpc(self):
        for attached in [False, True]:
            with self.subTest(attached=attached):
                wallet = self.wallet()
                with self.assertRaisesRegex(Exception, "does not match|Mismatch"):
                    wallet.unlock(dict(unlock_request(), eth_rpc_url=self.rpc.url),
                                  attached=attached, mismatch=True)
                wallet.assert_locked(self)
                self.assertEqual(self.rpc.requests, [])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cffi-library", help="Test this C-FFI shared library instead of UniFFI")
    args, remaining = parser.parse_known_args()
    if args.cffi_library:
        CffiWallet.library = os.path.abspath(args.cffi_library)
        ExternalSignerBfaTests.wallet_class = CffiWallet
    unittest.main(argv=[__file__, *remaining], verbosity=2)
