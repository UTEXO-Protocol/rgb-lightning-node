package org.rgblightningnode

import org.json.JSONArray
import org.json.JSONObject

/** Public identifiers captured before the channel disappears from listChannels. */
internal data class ClosedChannelContext(val fundingTxid: String?, val channelId: String)

/** Test-only observation. It never submits transactions or mines blocks itself. */
internal object ChannelCloseFixture {
    private const val MAX_DEPTH = 32
    private const val MAX_TRANSACTIONS = 256
    private const val PROBE_BUDGET_NANOS = 8_000_000_000L
    private val txidPattern = Regex("[0-9a-f]{64}")
    private val p2wshPattern = Regex("0020[0-9a-f]{64}")

    fun relatedMempoolTransactions(
        closed: ClosedChannelContext,
        nowNanos: () -> Long = System::nanoTime,
        rpc: (String, Array<out Any>, Int) -> JSONObject,
    ): List<String> {
        val fundingTxid = requireNotNull(closed.fundingTxid) {
            "closed-channel settlement needs an observed funding transaction"
        }
        require(txidPattern.matches(fundingTxid) && txidPattern.matches(closed.channelId)) {
            "invalid closed-channel identity"
        }
        val fundingBytes = fundingTxid.chunked(2).map { it.toInt(16) }.reversed()
        val channelBytes = closed.channelId.chunked(2).map { it.toInt(16) }
        require(fundingBytes.take(30) == channelBytes.take(30)) {
            "channel ID does not bind this funding transaction"
        }
        // LDK v1 channel IDs XOR the final two internal-txid bytes with the
        // big-endian funding output index. Display txids use reversed bytes.
        val fundingVout = ((channelBytes[30] xor fundingBytes[30]) shl 8) or
            (channelBytes[31] xor fundingBytes[31])
        val deadline = nowNanos() + PROBE_BUDGET_NANOS
        fun checkBudget() {
            check(nowNanos() < deadline) { "channel sweep probe exceeded its deadline" }
        }
        fun call(method: String, vararg params: Any): Any {
            checkBudget()
            // Separate connection/read timeouts each stay <=1s. Even a final
            // stalled RPC cannot extend this bounded probe beyond about 10s.
            val timeoutMs = ((deadline - nowNanos()) / 1_000_000L)
                .coerceIn(1L, 1_000L).toInt()
            val response = rpc(method, params, timeoutMs)
            checkBudget()
            check(!response.has("error") || response.isNull("error")) {
                "channel sweep RPC failed: $method"
            }
            return response.get("result")
        }
        check((call("getblockchaininfo") as JSONObject).getString("chain") == "regtest") {
            "closed-channel mining is restricted to regtest"
        }
        val transactions = mutableMapOf<String, JSONObject>()
        fun transaction(txid: String): JSONObject {
            checkBudget()
            require(txidPattern.matches(txid)) { "invalid transaction in channel sweep ancestry" }
            transactions[txid]?.let { return it }
            check(transactions.size < MAX_TRANSACTIONS) { "channel sweep ancestry exceeds its bound" }
            val value = call("getrawtransaction", txid, true) as JSONObject
            check(value.getString("txid") == txid) { "channel sweep RPC transaction identity mismatch" }
            transactions[txid] = value
            return value
        }
        val fundingOutputs = transaction(fundingTxid).getJSONArray("vout")
        val output = (0 until fundingOutputs.length())
            .map { fundingOutputs.getJSONObject(it) }
            .singleOrNull { it.getInt("n") == fundingVout }
        check(output != null) { "channel funding output does not exist" }
        val script = output.getJSONObject("scriptPubKey")
        check(script.getString("type") == "witness_v0_scripthash" &&
            p2wshPattern.matches(script.getString("hex"))) {
            "channel funding output is not P2WSH"
        }
        val mempool = call("getrawmempool") as JSONArray
        check(mempool.length() <= MAX_TRANSACTIONS) { "channel sweep mempool exceeds its bound" }
        val candidateIds = (0 until mempool.length()).map { mempool.getString(it) }
        check(candidateIds.all { txidPattern.matches(it) } && candidateIds.distinct().size == candidateIds.size) {
            "invalid channel sweep mempool transaction IDs"
        }
        val related = mutableMapOf<String, Boolean>()
        val visiting = mutableSetOf<String>()
        fun descendsFromFunding(txid: String, depth: Int): Boolean {
            checkBudget()
            check(depth <= MAX_DEPTH) { "channel sweep ancestry exceeds its depth bound" }
            if (txid == fundingTxid) return false
            related[txid]?.let { return it }
            check(visiting.add(txid)) { "cyclic channel sweep ancestry" }
            try {
                val inputs = transaction(txid).getJSONArray("vin")
                val found = (0 until inputs.length()).any { index ->
                    checkBudget()
                    val input = inputs.getJSONObject(index)
                    if (input.has("coinbase")) {
                        false
                    } else {
                        val parent = input.getString("txid")
                        val vout = input.getInt("vout")
                        check(vout >= 0) { "invalid channel sweep previous output" }
                        if (parent == fundingTxid) {
                            // Never recurse through another output of the funding
                            // transaction: change may fund an unrelated channel.
                            vout == fundingVout
                        } else {
                            descendsFromFunding(parent, depth + 1)
                        }
                    }
                }
                related[txid] = found
                return found
            } finally {
                visiting.remove(txid)
            }
        }
        return candidateIds.filter { descendsFromFunding(it, 0) }.sorted()
    }
}
