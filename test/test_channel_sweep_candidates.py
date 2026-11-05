"""Independent graph vectors for the read-only close-confirmation probe."""
import unittest
import subprocess
from unittest.mock import Mock, patch
from channel_sweep_candidates import candidates, funding_outpoint, main, rpc

ROOT = "".join(f"{n:02x}" for n in range(32))
# LDK v1 vector: transaction consensus bytes with output index 257 XORed into its tail.
CHANNEL = bytearray(bytes.fromhex(ROOT)[::-1])
CHANNEL[30] ^= 1
CHANNEL[31] ^= 1
CHANNEL = CHANNEL.hex()
CLOSE, SWEEP, OTHER, GRANDCHILD = (f"{n:064x}" for n in range(50, 54))


def graph():
    outputs = [{"n": n, "scriptPubKey": {"type": "witness_v0_scripthash"}}
               for n in range(258)]
    return {ROOT: {"vout": outputs},
            CLOSE: {"vin": [{"txid": ROOT, "vout": 257}]},
            SWEEP: {"vin": [{"txid": CLOSE, "vout": 0}]},
            OTHER: {"vin": [{"txid": ROOT, "vout": 0}]},
            GRANDCHILD: {"vin": [{"txid": SWEEP, "vout": 1}]}}


class CandidateTests(unittest.TestCase):
    def test_v1_vector_binds_transaction_byte_order_and_nonzero_output(self):
        self.assertEqual(funding_outpoint(ROOT, CHANNEL), (ROOT, 257))

    def test_only_exact_channel_descendants_including_confirmed_ancestor(self):
        data = graph()
        self.assertEqual(candidates(ROOT, CHANNEL, [OTHER, GRANDCHILD, SWEEP], data.__getitem__),
                         sorted([SWEEP, GRANDCHILD]))

    def test_other_funding_output_never_becomes_this_channel(self):
        data = graph()
        self.assertEqual(candidates(ROOT, CHANNEL, [OTHER], data.__getitem__), [])

    def test_empty_mempool_has_no_mining_candidate(self):
        data = graph()
        self.assertEqual(candidates(ROOT, CHANNEL, [], data.__getitem__), [])

    def test_wrong_channel_binding_and_missing_output_reject(self):
        with self.assertRaises(ValueError):
            candidates(ROOT, "ff" * 32, [], lambda _: {})
        with self.assertRaises(ValueError):
            candidates(ROOT, CHANNEL, [], lambda _: {"vout": []})

    def test_expired_total_budget_never_starts_another_rpc(self):
        with patch("channel_sweep_candidates.time.monotonic", return_value=20.0), \
                patch("channel_sweep_candidates.subprocess.run") as run:
            with self.assertRaises(TimeoutError):
                rpc("getrawmempool", deadline=19.0)
            run.assert_not_called()

    def test_each_rpc_cannot_outlive_remaining_total_budget(self):
        with patch("channel_sweep_candidates.time.monotonic", return_value=20.0), \
                patch("channel_sweep_candidates.subprocess.run", return_value=Mock(stdout="[]")) as run:
            self.assertEqual(rpc("getrawmempool", deadline=20.5), [])
            self.assertLessEqual(run.call_args.kwargs["timeout"], 0.5)

    def test_cycle_cannot_be_a_channel_descendant(self):
        data = graph()
        data[OTHER] = {"vin": [{"txid": GRANDCHILD, "vout": 0}]}
        data[GRANDCHILD] = {"vin": [{"txid": OTHER, "vout": 0}]}
        self.assertEqual(candidates(ROOT, CHANNEL, [OTHER], data.__getitem__), [])

    def test_transient_timeout_retries_identical_read_with_remaining_budget(self):
        with patch("channel_sweep_candidates.time.monotonic", side_effect=[20.0, 27.5, 27.6]), \
                patch("channel_sweep_candidates.subprocess.run", side_effect=[
                    subprocess.TimeoutExpired("read", 2.0), Mock(stdout="[]")]) as run:
            self.assertEqual(rpc("getrawmempool", deadline=28.0), [])
            self.assertEqual(run.call_count, 2)
            self.assertEqual(run.call_args_list[0].args, run.call_args_list[1].args)
            self.assertEqual(run.call_args_list[0].kwargs["timeout"], 2.0)
            self.assertEqual(run.call_args_list[1].kwargs["timeout"], 0.5)

    def test_persistent_timeout_stops_after_three_attempts(self):
        with patch("channel_sweep_candidates.time.monotonic", side_effect=[20.0, 22.0, 24.0]), \
                patch("channel_sweep_candidates.subprocess.run",
                      side_effect=subprocess.TimeoutExpired("read", 2.0)) as run:
            with self.assertRaises(subprocess.TimeoutExpired):
                rpc("getrawtransaction", ROOT, "true", deadline=28.0)
            self.assertEqual(run.call_count, 3)
            self.assertTrue(all(call.kwargs["timeout"] <= 2.0 for call in run.call_args_list))

    def test_timeout_does_not_start_retry_after_shared_deadline(self):
        with patch("channel_sweep_candidates.time.monotonic", side_effect=[20.0, 28.0]), \
                patch("channel_sweep_candidates.subprocess.run",
                      side_effect=subprocess.TimeoutExpired("read", 2.0)) as run:
            with self.assertRaises(TimeoutError):
                rpc("getrawmempool", deadline=28.0)
            self.assertEqual(run.call_count, 1)

    def test_nonzero_rpc_lookup_failure_is_not_retried(self):
        with patch("channel_sweep_candidates.time.monotonic", return_value=20.0), \
                patch("channel_sweep_candidates.subprocess.run",
                      side_effect=subprocess.CalledProcessError(5, "lookup")) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                rpc("getrawtransaction", ROOT, "true", deadline=28.0)
            self.assertEqual(run.call_count, 1)

    def test_malformed_json_is_not_retried(self):
        with patch("channel_sweep_candidates.time.monotonic", return_value=20.0), \
                patch("channel_sweep_candidates.subprocess.run",
                      return_value=Mock(stdout="malformed response")) as run:
            with self.assertRaises(ValueError):
                rpc("getrawmempool", deadline=28.0)
            self.assertEqual(run.call_count, 1)

    def test_non_readonly_rpc_is_rejected_before_subprocess(self):
        with patch("channel_sweep_candidates.subprocess.run") as run:
            with self.assertRaisesRegex(ValueError, "read-only"):
                rpc("generatetoaddress", 1, "unused", deadline=28.0)
            run.assert_not_called()

    def test_successful_rpc_returned_after_deadline_is_rejected(self):
        with patch("channel_sweep_candidates.time.monotonic", side_effect=[20.0, 28.1]), \
                patch("channel_sweep_candidates.subprocess.run", return_value=Mock(stdout="[]")):
            with self.assertRaises(TimeoutError):
                rpc("getrawmempool", deadline=28.0)

    def test_late_candidate_result_is_not_printed(self):
        with patch("channel_sweep_candidates.sys.argv", ["probe", ROOT, CHANNEL]), \
                patch("channel_sweep_candidates.time.monotonic", side_effect=[20.0, 28.1]), \
                patch("channel_sweep_candidates.rpc", side_effect=[{"chain": "regtest"}, []]), \
                patch("channel_sweep_candidates.candidates", return_value=[SWEEP]), \
                patch("builtins.print") as output:
            with self.assertRaises(TimeoutError):
                main()
            output.assert_not_called()


if __name__ == "__main__":
    unittest.main()
