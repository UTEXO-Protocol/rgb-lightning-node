/**
 * Fee-rate estimates from the shared esplora instance.
 *
 * Clients cannot reach esplora (it sits behind the gateway with RLN and the RGB
 * proxy), so without this route a client has no fee source at all and either
 * hardcodes a rate or sends none and accepts the gateway's 2 sat/vB default.
 *
 * Two deliberate roundings. Esplora reports fractional sat/vB, while every
 * gateway fee input is an integer 1..1000 — so the answer is rounded UP, never
 * down: a client that signs at exactly this rate must not end up below the
 * estimate it asked for. And esplora publishes a fixed ladder of confirmation
 * targets (1..25, 144, 504, 1008), so a target it does not publish resolves to
 * the largest published target at or below it, which is the more urgent — and
 * therefore never cheaper — neighbour.
 */
import { HttpError } from './errors.js';

/** Widest target esplora publishes; also the ceiling this route accepts. */
export const MAX_TARGET_BLOCKS = 1008;
/** Matches the `feeRateSatPerVb` bounds on every fee-taking route. */
export const MIN_FEE_RATE_SAT_PER_VB = 1;
export const MAX_FEE_RATE_SAT_PER_VB = 1000;
/** Estimates move slowly; a short cache keeps a burst of clients off esplora. */
export const CACHE_TTL_MS = 30_000;
const ESPLORA_TIMEOUT_MS = 10_000;

export interface FeeEstimate {
  /** Confirmation target the answer is for (as requested). */
  blocks: number;
  /** Whole sat/vB, rounded up and clamped into the accepted range. */
  feeRateSatPerVb: number;
  /** Esplora target the estimate was actually read from. */
  sourceBlocks: number;
}

export interface FeeEstimatorOptions {
  esploraUrl: string;
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
  now?: () => number;
}

/** Esplora `GET /fee-estimates`: confirmation target (as a string) → sat/vB. */
type EsploraFeeEstimates = Record<string, unknown>;

export class FeeEstimator {
  private readonly esploraUrl: string;
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;
  private readonly now: () => number;
  private cached: { at: number; estimates: Map<number, number> } | undefined;

  constructor(options: FeeEstimatorOptions) {
    this.esploraUrl = options.esploraUrl.replace(/\/+$/, '');
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.timeoutMs = options.timeoutMs ?? ESPLORA_TIMEOUT_MS;
    this.now = options.now ?? Date.now;
  }

  async estimate(blocks: number): Promise<FeeEstimate> {
    const estimates = await this.load();
    // Largest published target at or below the request: more urgent, so its
    // rate is an upper bound on the requested target's rate.
    let sourceBlocks: number | undefined;
    for (const target of estimates.keys()) {
      if (target > blocks) continue;
      if (sourceBlocks === undefined || target > sourceBlocks) sourceBlocks = target;
    }
    // Nothing at or below the request means the ladder starts above it, which
    // only happens on a truncated or empty payload — the fastest target
    // published is then the closest honest answer.
    if (sourceBlocks === undefined) {
      sourceBlocks = [...estimates.keys()].sort((a, b) => a - b)[0];
    }
    const raw = sourceBlocks === undefined ? undefined : estimates.get(sourceBlocks);
    if (sourceBlocks === undefined || raw === undefined) {
      throw new HttpError(502, 'FEE_ESTIMATE_UNAVAILABLE', 'no fee estimate is available');
    }
    const clamped = Math.min(
      MAX_FEE_RATE_SAT_PER_VB,
      Math.max(MIN_FEE_RATE_SAT_PER_VB, Math.ceil(raw)),
    );
    return { blocks, feeRateSatPerVb: clamped, sourceBlocks };
  }

  private async load(): Promise<Map<number, number>> {
    const cached = this.cached;
    if (cached !== undefined && this.now() - cached.at < CACHE_TTL_MS) return cached.estimates;
    const estimates = this.parse(await this.fetchEstimates());
    if (estimates.size === 0) {
      throw new HttpError(
        502,
        'FEE_ESTIMATE_UNAVAILABLE',
        'the indexer returned no usable fee estimates',
      );
    }
    this.cached = { at: this.now(), estimates };
    return estimates;
  }

  /** Keep only finite, positive numeric entries under integer targets. */
  private parse(raw: EsploraFeeEstimates): Map<number, number> {
    const estimates = new Map<number, number>();
    for (const [key, value] of Object.entries(raw)) {
      const target = Number(key);
      if (!Number.isInteger(target) || target < 1 || target > MAX_TARGET_BLOCKS) continue;
      if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) continue;
      estimates.set(target, value);
    }
    return estimates;
  }

  /**
   * Bounded esplora GET. Same shape as the deposits watcher's: without an
   * explicit deadline a stalled indexer holds the request open for undici's
   * 300s default, and this route runs on the caller's request.
   */
  private async fetchEstimates(): Promise<EsploraFeeEstimates> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const response = await this.fetchImpl(`${this.esploraUrl}/fee-estimates`, {
        signal: controller.signal,
      });
      if (!response.ok) {
        throw new HttpError(
          502,
          'FEE_ESTIMATE_UNAVAILABLE',
          'the indexer rejected the fee-estimate request',
          { cause: new Error(`esplora /fee-estimates status ${response.status}`) },
        );
      }
      const body = (await response.json()) as unknown;
      if (body === null || typeof body !== 'object' || Array.isArray(body)) {
        throw new HttpError(
          502,
          'FEE_ESTIMATE_UNAVAILABLE',
          'the indexer returned an unreadable fee-estimate payload',
        );
      }
      return body as EsploraFeeEstimates;
    } catch (error) {
      if (error instanceof HttpError) throw error;
      throw new HttpError(
        controller.signal.aborted ? 504 : 502,
        controller.signal.aborted ? 'UPSTREAM_TIMEOUT' : 'FEE_ESTIMATE_UNAVAILABLE',
        controller.signal.aborted
          ? 'the indexer did not answer the fee-estimate request in time'
          : 'the fee-estimate request failed',
        { cause: error },
      );
    } finally {
      clearTimeout(timer);
    }
  }
}
