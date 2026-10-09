/**
 * Unit tests for the pure marshalling layer of the native rgb-lib backend:
 * error classification (which decides 400 INSUFFICIENT_FUNDS vs opaque 500)
 * and the camelCase JSON → gateway-shape mappers. No native module needed —
 * loadNativeRgbLib() is lazy and never called here.
 */
import { describe, expect, it } from 'vitest';
import {
  mapAssets,
  mapAssignment,
  mapTransfers,
  mapUnspents,
  unquote,
  wrapNativeError,
} from '../src/wallets/rgblib.js';
import { walletHttpError, WalletBackendError } from '../src/wallets/backend.js';
import { HttpError } from '../src/errors.js';

/** Smallest integer a u64 above the safe range can decode to. */
const UNSAFE_AMOUNT = Number.MAX_SAFE_INTEGER + 1;

describe('wrapNativeError', () => {
  it.each([
    'RgbLib(InsufficientBitcoins { needed: 15000, available: 4000 })',
    'RgbLib(InsufficientAllocationSlots)',
    'RgbLib(InsufficientSpendableAssets { asset_id: "rgb:abc" })',
    'RgbLib(InsufficientTotalAssets { asset_id: "rgb:abc" })',
  ])('classifies %s as insufficient funds', (detail) => {
    const wrapped = wrapNativeError('sendBtcBegin', new Error(detail));
    expect(wrapped.insufficientFunds).toBe(true);
    expect(wrapped.message).toBe('wallet sendBtcBegin failed');
    expect(wrapped.detail).toContain(detail);
  });

  it.each([
    'RgbLib(InvalidAddress { details: "bad checksum" })',
    'RgbLib(InvalidRecipientID)',
    'RgbLib(InvalidRecipientNetwork)',
    'RgbLib(InvalidRecipientData { details: "witness data on a blind recipient" })',
    'RgbLib(AssetNotFound { asset_id: "rgb:abc" })',
    'RgbLib(InvalidTransportEndpoint { details: "bad scheme" })',
    'RgbLib(RecipientIDDuplicated)',
    'RgbLib(InvalidFeeRate { details: "below minimum" })',
    // Key material the caller registered: their request, their 400. Without
    // these, a genuinely bad xpub was indistinguishable from a missing rgb-lib
    // module or an unreachable indexer at the registration route.
    'RgbLib(InvalidBitcoinKeys)',
    'RgbLib(InvalidFingerprint)',
    'RgbLib(InvalidPubkey { details: "bad point" })',
    'RgbLib(InvalidVanillaKeychain)',
  ])('classifies %s as a client error, not insufficient funds', (detail) => {
    const wrapped = wrapNativeError('sendAssetBegin', new Error(detail));
    expect(wrapped.insufficientFunds).toBe(false);
    expect(wrapped.clientError).toBe(true);
  });

  it.each([
    'RgbLib(Internal { details: "stash corrupted" })',
    'RgbLib(FailedBdkSync { details: "indexer unreachable" })',
    'RgbLib(Electrum { details: "connection reset" })',
  ])('leaves %s unclassified so it surfaces as a server error', (detail) => {
    const wrapped = wrapNativeError('sync', new Error(detail));
    expect(wrapped.insufficientFunds).toBe(false);
    expect(wrapped.clientError).toBe(false);
  });

  it('stringifies non-Error throwables into the detail', () => {
    const wrapped = wrapNativeError('sync', 'plain-string failure');
    expect(wrapped.detail).toBe('plain-string failure');
    expect(wrapped.insufficientFunds).toBe(false);
    expect(wrapped.clientError).toBe(false);
  });
});

describe('unquote', () => {
  it('strips JSON string quoting', () => {
    expect(unquote('"cHNidP8BAA=="')).toBe('cHNidP8BAA==');
  });

  it('passes through unquoted and malformed values unchanged', () => {
    expect(unquote('cHNidP8BAA==')).toBe('cHNidP8BAA==');
    expect(unquote('"unterminated')).toBe('"unterminated');
  });
});

describe('mapAssignment', () => {
  it('flattens externally tagged fungible and unit variants', () => {
    expect(mapAssignment({ Fungible: 42 }, 'a')).toEqual({ kind: 'Fungible', amount: 42 });
    expect(mapAssignment('Any', 'a')).toEqual({ kind: 'Any', amount: null });
  });

  it('carries an unknown variant through by name instead of dropping it', () => {
    expect(mapAssignment({ InflationRight: 7 }, 'a')).toEqual({
      kind: 'InflationRight',
      amount: 7,
    });
    expect(mapAssignment({ NonFungible: { token: 1 } }, 'a')).toEqual({
      kind: 'NonFungible',
      amount: null,
    });
  });

  it('rejects a payload that is not a single-key tagged object', () => {
    expect(() => mapAssignment({}, 'a')).toThrow(WalletBackendError);
    expect(() => mapAssignment({ Fungible: 1, Any: 2 }, 'a')).toThrow(WalletBackendError);
  });

  it('refuses a fungible amount outside the safe integer range', () => {
    expect(() => mapAssignment({ Fungible: UNSAFE_AMOUNT }, 'a')).toThrow(WalletBackendError);
  });
});

describe('mapAssets', () => {
  it('flattens the per-schema camelCase asset map', () => {
    const raw = {
      nia: [
        {
          assetId: 'rgb:asset-1',
          ticker: 'TST',
          name: 'Test Asset',
          details: 'a test asset',
          precision: 0,
          issuedSupply: 1000,
          timestamp: 1_700_000_000,
          addedAt: 1_700_000_100,
          balance: { settled: 100, future: 100, spendable: 100 },
        },
      ],
      cfa: [
        // No assetId → dropped rather than emitted half-formed.
        { ticker: 'BAD', name: 'No Id', precision: 0 },
      ],
      uda: null,
    };
    expect(mapAssets(raw)).toEqual([
      {
        assetId: 'rgb:asset-1',
        schema: 'nia',
        ticker: 'TST',
        name: 'Test Asset',
        details: 'a test asset',
        precision: 0,
        issuedSupply: 1000,
        timestamp: 1_700_000_000,
        addedAt: 1_700_000_100,
        balance: { settled: 100, future: 100, spendable: 100 },
      },
    ]);
  });

  it('leaves issuance metadata null when the schema has none (UDA)', () => {
    const [asset] = mapAssets({
      uda: [
        {
          assetId: 'rgb:uda-1',
          ticker: 'UDA',
          name: 'Unique',
          precision: 0,
          balance: { settled: 1, future: 1, spendable: 1 },
        },
      ],
    });
    expect(asset?.issuedSupply).toBeNull();
    expect(asset?.details).toBeNull();
    expect(asset?.timestamp).toBe(0);
  });

  it('refuses an asset with no name rather than emitting an empty one', () => {
    // A nameless asset is undisplayable and unvalidatable by a client; the old
    // '' substitution hid rgb-lib shape drift behind a usable-looking response.
    expect(() =>
      mapAssets({
        nia: [{ assetId: 'rgb:asset-1', ticker: 'TST', precision: 0 }],
      }),
    ).toThrow(WalletBackendError);
  });

  it('refuses a balance outside the safe integer range instead of rounding it', () => {
    let thrown: unknown;
    try {
      mapAssets({
        nia: [
          {
            assetId: 'rgb:asset-1',
            ticker: 'TST',
            name: 'Test Asset',
            precision: 0,
            balance: { settled: UNSAFE_AMOUNT, future: 0, spendable: 0 },
          },
        ],
      });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(WalletBackendError);
    expect((thrown as WalletBackendError).unrepresentable).toBe(true);
    // The route answer is an explicit 502, not a silently wrong balance.
    const mapped = walletHttpError(thrown);
    expect(mapped).toBeInstanceOf(HttpError);
    expect((mapped as HttpError).statusCode).toBe(502);
    expect((mapped as HttpError).code).toBe('AMOUNT_NOT_REPRESENTABLE');
  });

  it('returns an empty list for a non-object payload', () => {
    expect(mapAssets(null)).toEqual([]);
    expect(mapAssets('garbage')).toEqual([]);
  });
});

describe('mapUnspents', () => {
  it('maps utxos with their rgb allocations', () => {
    const raw = [
      {
        utxo: {
          outpoint: { txid: 'txid-1', vout: 2 },
          btcAmount: 998,
          colorable: true,
          exists: true,
        },
        pendingBlinded: 1,
        rgbAllocations: [
          { assetId: 'rgb:asset-1', assignment: { Fungible: 42 }, settled: true },
          { assetId: null, assignment: 'Any', settled: false },
        ],
      },
    ];
    expect(mapUnspents(raw)).toEqual([
      {
        txid: 'txid-1',
        vout: 2,
        amountSat: 998,
        colorable: true,
        exists: true,
        pendingBlinded: 1,
        allocations: [
          {
            assetId: 'rgb:asset-1',
            amount: 42,
            assignment: { kind: 'Fungible', amount: 42 },
            settled: true,
          },
          {
            assetId: null,
            amount: null,
            assignment: { kind: 'Any', amount: null },
            settled: false,
          },
        ],
      },
    ]);
  });

  it('defaults a missing pendingBlinded to zero', () => {
    const [unspent] = mapUnspents([
      { utxo: { outpoint: { txid: 't', vout: 0 }, btcAmount: 1, colorable: false } },
    ]);
    expect(unspent?.pendingBlinded).toBe(0);
    // Absent `exists` must read as false, never as an optimistic true.
    expect(unspent?.exists).toBe(false);
  });

  it('returns an empty list for a non-array payload', () => {
    expect(mapUnspents({ not: 'an array' })).toEqual([]);
  });
});

describe('mapTransfers', () => {
  it('prefers the requested assignment amount and falls back to summed assignments', () => {
    const raw = [
      {
        idx: 1,
        batchTransferIdx: 11,
        requestedAssignment: { Fungible: 50 },
        assignments: [],
        kind: 'ReceiveBlind',
        status: 'Settled',
        txid: 'txid-1',
        recipientId: 'utxob:recipient',
        expiration: 1_700_000_000,
        createdAt: 1,
        updatedAt: 2,
      },
      {
        idx: 2,
        batchTransferIdx: 12,
        requestedAssignment: null,
        assignments: [{ Fungible: 10 }, { Fungible: 15 }, 'Any'],
        kind: 'Send',
        status: 'WaitingConfirmations',
        txid: null,
        recipientId: null,
        expiration: null,
        createdAt: 3,
        updatedAt: 4,
      },
    ];
    const mapped = mapTransfers(raw, 'rgb:asset-1');
    expect(mapped).toEqual([
      {
        idx: 1,
        batchTransferIdx: 11,
        assetId: 'rgb:asset-1',
        amount: 50,
        assignments: [],
        kind: 'ReceiveBlind',
        status: 'Settled',
        txid: 'txid-1',
        recipientId: 'utxob:recipient',
        expiration: 1_700_000_000,
        createdAt: 1,
        updatedAt: 2,
      },
      {
        idx: 2,
        batchTransferIdx: 12,
        assetId: 'rgb:asset-1',
        amount: 25,
        assignments: [
          { kind: 'Fungible', amount: 10 },
          { kind: 'Fungible', amount: 15 },
          { kind: 'Any', amount: null },
        ],
        kind: 'Send',
        status: 'WaitingConfirmations',
        txid: null,
        recipientId: null,
        expiration: null,
        createdAt: 3,
        updatedAt: 4,
      },
    ]);
  });

  it('refuses a transfer amount outside the safe integer range', () => {
    expect(() =>
      mapTransfers([{ idx: 1, requestedAssignment: { Fungible: UNSAFE_AMOUNT } }], null),
    ).toThrow(WalletBackendError);
  });

  it('defaults missing fields instead of throwing on shape drift', () => {
    const mapped = mapTransfers([{}], null);
    expect(mapped).toEqual([
      {
        idx: 0,
        batchTransferIdx: null,
        assetId: null,
        amount: null,
        assignments: [],
        kind: 'Unknown',
        status: 'Unknown',
        txid: null,
        recipientId: null,
        expiration: null,
        createdAt: 0,
        updatedAt: 0,
      },
    ]);
  });
});
