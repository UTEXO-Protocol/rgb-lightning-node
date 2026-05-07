"use strict";

/**
 * Poll a predicate until it returns a truthy value or the timeout elapses.
 *
 * Unlike ad-hoc `for/sleep` loops scattered across specs, this:
 *   - logs *what* it was waiting for and *what* it last observed when it times out,
 *   - returns the truthy value the predicate produced,
 *   - has a single, consistent contract for every readiness check in the suite.
 */
async function pollUntil(predicate, opts = {}) {
  const {
    timeoutMs = 60_000,
    intervalMs = 250,
    label = "predicate",
    onTick = null,
  } = opts;

  const deadline = Date.now() + timeoutMs;
  let last;
  let attempt = 0;
  let lastError;

  while (Date.now() < deadline) {
    try {
      last = await predicate({ attempt });
      if (last) return last;
    } catch (err) {
      lastError = err;
      last = undefined;
    }
    if (typeof onTick === "function") {
      try {
        await onTick({ attempt, last, lastError });
      } catch (_e) {
        // onTick failures must not break the polling loop.
      }
    }
    attempt += 1;
    await new Promise((r) => setTimeout(r, intervalMs));
  }

  const detail =
    lastError !== undefined
      ? `last_error=${String(lastError)}`
      : `last=${safeStringify(last)}`;
  throw new Error(`pollUntil timed out: ${label} (${detail})`);
}

function safeStringify(v) {
  try {
    return JSON.stringify(v, (_k, x) => (typeof x === "bigint" ? x.toString() : x));
  } catch (_e) {
    return String(v);
  }
}

module.exports = { pollUntil };
