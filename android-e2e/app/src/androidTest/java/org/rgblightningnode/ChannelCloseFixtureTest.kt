package org.rgblightningnode

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class ChannelCloseFixtureTest {
    // Independent fixed v1 vector: display txid is ascending bytes, internal
    // bytes are reversed, and output index2 XORs internal byte31 from00 to02.
    private val funding = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    private val channel = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020102"
    private val closing = "22".repeat(32)
    private val sweep = "33".repeat(32)

    private fun transaction(id: String, vararg parents: Pair<String, Int>): JSONObject =
        JSONObject().put("txid", id).put("vin", JSONArray().apply {
            if (parents.isEmpty()) put(JSONObject().put("coinbase", "00"))
            parents.forEach { (txid, vout) -> put(JSONObject().put("txid", txid).put("vout", vout)) }
        }).put("vout", JSONArray())

    private fun fundingTransaction(): JSONObject = transaction(funding).put("vout", JSONArray().apply {
        put(JSONObject().put("n", 0).put("scriptPubKey", JSONObject().put("type", "nulldata").put("hex", "6a")))
        put(JSONObject().put("n", 1).put("scriptPubKey", JSONObject().put("type", "witness_v0_keyhash").put("hex", "0014" + "aa".repeat(20))))
        put(JSONObject().put("n", 2).put("scriptPubKey", JSONObject().put("type", "witness_v0_scripthash").put("hex", "0020" + "aa".repeat(32))))
    })

    private class RpcFixture(val transactions: MutableMap<String, JSONObject>, val mempool: List<String>) {
        val calls = mutableListOf<String>()
        var advanceClock: (() -> Unit)? = null
        fun call(method: String, params: Array<out Any>, timeout: Int): JSONObject {
            assertTrue(timeout in 1..1_000)
            calls.add(method)
            advanceClock?.invoke()
            val result: Any = when (method) {
                "getblockchaininfo" -> JSONObject().put("chain", "regtest")
                "getrawmempool" -> JSONArray(mempool)
                "getrawtransaction" -> transactions[params[0] as String]
                    ?: return JSONObject().put("result", JSONObject.NULL)
                        .put("error", JSONObject().put("code", -5))
                else -> error("unexpected or mutating RPC: $method")
            }
            return JSONObject().put("result", result).put("error", JSONObject.NULL)
        }
    }

    private fun assertRejected(block: () -> Unit) {
        try {
            block()
            fail("invalid channel sweep observation must fail closed")
        } catch (_: IllegalArgumentException) {
        } catch (_: IllegalStateException) {
        }
    }

    @Test
    fun trustedVirtualSyntheticFundingSkipsOnlyGenericBroadcastBarrier() {
        val fixture = RpcFixture(mutableMapOf(), emptyList())
        for (synthetic in listOf(funding, "fe".repeat(32))) {
            val closed = ClosedChannelContext(synthetic, channel, "trusted_no_broadcast")
            assertFalse(closed.requiresBitcoinBroadcast)
            assertRejected {
                ChannelCloseFixture.relatedMempoolTransactions(closed, rpc = fixture::call)
            }
        }
        assertTrue(fixture.calls.isEmpty())
    }

    @Test
    fun realAndUnknownModesStillObserveExactFundingOutput() {
        for (mode in listOf(null, "", "unrecognized", "TRUSTED_NO_BROADCAST", "trusted_no_broadcast ")) {
            val closed = ClosedChannelContext(funding, channel, mode)
            assertTrue(closed.requiresBitcoinBroadcast)
            val fixture = RpcFixture(mutableMapOf(
                funding to fundingTransaction(),
                closing to transaction(closing, funding to 2),
            ), listOf(closing))
            assertEquals(listOf(closing), ChannelCloseFixture.relatedMempoolTransactions(
                closed, rpc = fixture::call,
            ))
            assertTrue(fixture.calls.contains("getrawtransaction"))
        }
    }

    @Test
    fun realAndUnknownModesCannotBypassFundingIdentityOrP2wsh() {
        for (mode in listOf(null, "unrecognized", "TRUSTED_NO_BROADCAST", "trusted_no_broadcast ")) {
            val wrongIdentity = RpcFixture(mutableMapOf(), emptyList())
            assertRejected {
                ChannelCloseFixture.relatedMempoolTransactions(
                    ClosedChannelContext(funding, "ff".repeat(32), mode), rpc = wrongIdentity::call,
                )
            }
            assertTrue(wrongIdentity.calls.isEmpty())
            val root = fundingTransaction()
            root.getJSONArray("vout").getJSONObject(2).getJSONObject("scriptPubKey")
                .put("type", "witness_v0_keyhash")
            val wrongScript = RpcFixture(mutableMapOf(funding to root), emptyList())
            assertRejected {
                ChannelCloseFixture.relatedMempoolTransactions(
                    ClosedChannelContext(funding, channel, mode), rpc = wrongScript::call,
                )
            }
            assertEquals(listOf("getblockchaininfo", "getrawtransaction"), wrongScript.calls)
        }
    }

    @Test
    fun unfundedGenericCloseCannotBecomeBitcoinSettlementObservation() {
        val fixture = RpcFixture(mutableMapOf(), emptyList())
        for (mode in listOf(null, "unrecognized", "trusted_no_broadcast")) {
            val closed = ClosedChannelContext(null, channel, mode)
            assertFalse(closed.requiresBitcoinBroadcast)
            assertRejected {
                ChannelCloseFixture.relatedMempoolTransactions(closed, rpc = fixture::call)
            }
        }
        assertTrue(fixture.calls.isEmpty())
    }

    @Test
    fun exactFundingOutputAndItsMempoolDescendantsAreObserved() {
        val fixture = RpcFixture(mutableMapOf(
            funding to fundingTransaction(),
            closing to transaction(closing, funding to 2),
            sweep to transaction(sweep, closing to 1),
        ), listOf(sweep, closing))
        assertEquals(listOf(closing, sweep), ChannelCloseFixture.relatedMempoolTransactions(
            ClosedChannelContext(funding, channel), rpc = fixture::call,
        ))
        assertEquals(3, fixture.calls.count { it == "getrawtransaction" })
    }

    @Test
    fun fundingChangeAndSiblingChannelDescendantsDoNotAuthorizeMining() {
        val sibling = "44".repeat(32)
        val child = "55".repeat(32)
        val fixture = RpcFixture(mutableMapOf(
            funding to fundingTransaction(),
            sibling to transaction(sibling, funding to 1),
            child to transaction(child, sibling to 0),
        ), listOf(child, sibling))
        assertEquals(emptyList<String>(), ChannelCloseFixture.relatedMempoolTransactions(
            ClosedChannelContext(funding, channel), rpc = fixture::call,
        ))
    }

    @Test
    fun mismatchedChannelIdentityIsRejectedBeforeRpc() {
        val fixture = RpcFixture(mutableMapOf(), emptyList())
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, "ff".repeat(32)), rpc = fixture::call,
            )
        }
        assertTrue(fixture.calls.isEmpty())
    }

    @Test
    fun missingFundingOutputOrNonP2wshIsRejected() {
        for (missing in listOf(false, true)) {
            val root = fundingTransaction()
            if (missing) root.put("vout", JSONArray())
            else root.getJSONArray("vout").getJSONObject(2).getJSONObject("scriptPubKey")
                .put("type", "witness_v0_keyhash")
            val fixture = RpcFixture(mutableMapOf(funding to root), emptyList())
            assertRejected {
                ChannelCloseFixture.relatedMempoolTransactions(
                    ClosedChannelContext(funding, channel), rpc = fixture::call,
                )
            }
        }
    }

    @Test
    fun excessiveMempoolAndMissingAncestorAreRejected() {
        val oversized = RpcFixture(mutableMapOf(funding to fundingTransaction()),
            (0..256).map { "%064x".format(it + 1000) })
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, channel), rpc = oversized::call,
            )
        }
        val missing = RpcFixture(mutableMapOf(funding to fundingTransaction()), listOf(sweep))
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, channel), rpc = missing::call,
            )
        }
    }

    @Test
    fun cyclicAndTooDeepAncestryAreRejected() {
        val cycle = RpcFixture(mutableMapOf(
            funding to fundingTransaction(),
            closing to transaction(closing, sweep to 0),
            sweep to transaction(sweep, closing to 0),
        ), listOf(sweep))
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, channel), rpc = cycle::call,
            )
        }
        val chain = (0..33).map { "%064x".format(it + 1000) }
        val transactions = mutableMapOf(funding to fundingTransaction())
        chain.forEachIndexed { index, txid ->
            transactions[txid] = transaction(txid, (chain.getOrNull(index + 1) ?: funding) to 2)
        }
        val deep = RpcFixture(transactions, listOf(chain.first()))
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, channel), rpc = deep::call,
            )
        }
    }

    @Test
    fun cacheAndTotalProbeTimeAreBounded() {
        val transactions = mutableMapOf(funding to fundingTransaction())
        val tips = (0..7).map { branch ->
            val chain = (0..31).map { "%064x".format(1000 + branch * 32 + it) }
            chain.forEachIndexed { index, txid ->
                transactions[txid] = if (index == chain.lastIndex) transaction(txid)
                    else transaction(txid, chain[index + 1] to 0)
            }
            chain.first()
        }
        val cache = RpcFixture(transactions, tips)
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, channel), rpc = cache::call,
            )
        }
        var now = 0L
        val timed = RpcFixture(mutableMapOf(funding to fundingTransaction()), emptyList())
        timed.advanceClock = { now += 3_000_000_000L }
        assertRejected {
            ChannelCloseFixture.relatedMempoolTransactions(
                ClosedChannelContext(funding, channel), nowNanos = { now }, rpc = timed::call,
            )
        }
        assertEquals(3, timed.calls.size)
    }
}
