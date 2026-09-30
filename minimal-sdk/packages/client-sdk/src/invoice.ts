/**
 * Invoice decoding for intent construction:
 *  - BOLT11 (payee, amount, payment hash, description, expiry) — a minimal
 *    decoder over @scure/base bech32 + @noble secp256k1 signature recovery;
 *  - RGB invoices in the rgb-invoicing v0.11 grammar
 *    `rgb:<contract|~>/<schema|~>/<state|~>/<beneficiary>?expiry=&endpoints=`
 *    (asset id, amount, beneficiary kind, transports). In this grammar a
 *    witness beneficiary is a `wvout:` recipient id, not a plain address;
 *    full RGB validation is explicitly out of scope (design doc).
 */
import { bech32, hex } from '@scure/base';
import { sha256 } from '@noble/hashes/sha2.js';
import { secp256k1 } from '@noble/curves/secp256k1.js';
import type { BitcoinNetwork } from './network.js';

export class InvoiceDecodeError extends Error {
  constructor(detail: string) {
    super(`invalid invoice: ${detail}`);
    this.name = 'InvoiceDecodeError';
  }
}

// ---------------------------------------------------------------------------
// BOLT11
// ---------------------------------------------------------------------------

export interface Bolt11Invoice {
  network: BitcoinNetwork;
  /** Invoice amount in millisatoshi; null for any-amount invoices. */
  amountMsat: bigint | null;
  /** 32-byte payment hash, lowercase hex. */
  paymentHash: string;
  /** 32-byte payment secret, lowercase hex (null on legacy invoices). */
  paymentSecret: string | null;
  /** Short description, or null when only a description hash is present. */
  description: string | null;
  descriptionHash: string | null;
  /** Payee node id (33-byte compressed pubkey hex), recovered or from `n`. */
  payeeNodeId: string;
  /** Invoice creation time, unix seconds. */
  timestamp: number;
  /** Seconds until expiry (BOLT11 default 3600). */
  expirySeconds: number;
}

const HRP_NETWORKS: [string, BitcoinNetwork][] = [
  ['bcrt', 'Regtest'],
  ['tbs', 'Signet'],
  ['bc', 'Mainnet'],
  ['tb', 'Testnet'],
];

const MSAT_PER_BTC = 100_000_000_000n;
const MULTIPLIERS: Record<string, bigint> = {
  m: MSAT_PER_BTC / 1_000n,
  u: MSAT_PER_BTC / 1_000_000n,
  n: MSAT_PER_BTC / 1_000_000_000n,
};

function parseHrp(hrp: string): { network: BitcoinNetwork; amountMsat: bigint | null } {
  if (!hrp.startsWith('ln')) throw new InvoiceDecodeError(`not a BOLT11 HRP: ${hrp}`);
  const rest = hrp.slice(2);
  const entry = HRP_NETWORKS.find(([prefix]) => rest.startsWith(prefix));
  if (entry === undefined) throw new InvoiceDecodeError(`unknown network prefix: ${rest}`);
  const [prefix, network] = entry;
  const amountPart = rest.slice(prefix.length);
  if (amountPart === '') return { network, amountMsat: null };
  const match = /^(\d+)([munp])?$/.exec(amountPart);
  if (match === null) throw new InvoiceDecodeError(`malformed amount: ${amountPart}`);
  const value = BigInt(match[1] as string);
  const unit = match[2];
  if (unit === undefined) return { network, amountMsat: value * MSAT_PER_BTC };
  if (unit === 'p') {
    if (value % 10n !== 0n) throw new InvoiceDecodeError('sub-millisatoshi pico amount');
    return { network, amountMsat: value / 10n };
  }
  return { network, amountMsat: value * (MULTIPLIERS[unit] as bigint) };
}

/** Pack 5-bit words into bytes MSB-first, zero-padding the tail (BIP-173). */
function wordsToBytesPadded(words: number[]): Uint8Array {
  const bytes: number[] = [];
  let acc = 0;
  let bits = 0;
  for (const word of words) {
    acc = (acc << 5) | word;
    bits += 5;
    while (bits >= 8) {
      bits -= 8;
      bytes.push((acc >> bits) & 0xff);
    }
  }
  if (bits > 0) bytes.push((acc << (8 - bits)) & 0xff);
  return Uint8Array.from(bytes);
}

/** Pack 5-bit words into bytes, DROPPING trailing pad bits (tagged fields). */
function wordsToBytesTrimmed(words: number[]): Uint8Array {
  const full = wordsToBytesPadded(words);
  return full.slice(0, Math.floor((words.length * 5) / 8));
}

function wordsToInt(words: number[]): number {
  return words.reduce((acc, word) => acc * 32 + word, 0);
}

export function decodeBolt11(invoice: string): Bolt11Invoice {
  let decoded: { prefix: string; words: number[] };
  try {
    decoded = bech32.decode(invoice.toLowerCase() as `${string}1${string}`, false);
  } catch (error) {
    throw new InvoiceDecodeError(
      `bech32 decode failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  const { network, amountMsat } = parseHrp(decoded.prefix);
  if (decoded.words.length < 104 + 7) throw new InvoiceDecodeError('data part too short');
  const signatureWords = decoded.words.slice(-104);
  const dataWords = decoded.words.slice(0, -104);
  const timestamp = wordsToInt(dataWords.slice(0, 7));

  let paymentHash: string | null = null;
  let paymentSecret: string | null = null;
  let description: string | null = null;
  let descriptionHash: string | null = null;
  let payeeFromTag: string | null = null;
  let expirySeconds = 3600;

  let cursor = 7;
  while (cursor < dataWords.length) {
    const type = dataWords[cursor] as number;
    const high = dataWords[cursor + 1];
    const low = dataWords[cursor + 2];
    if (high === undefined || low === undefined) throw new InvoiceDecodeError('truncated tag');
    const length = high * 32 + low;
    const data = dataWords.slice(cursor + 3, cursor + 3 + length);
    if (data.length !== length) throw new InvoiceDecodeError('truncated tag data');
    cursor += 3 + length;
    switch (type) {
      case 1: // p — payment hash
        if (length === 52 && paymentHash === null)
          paymentHash = hex.encode(wordsToBytesTrimmed(data));
        break;
      case 16: // s — payment secret
        if (length === 52 && paymentSecret === null)
          paymentSecret = hex.encode(wordsToBytesTrimmed(data));
        break;
      case 13: // d — description
        description = new TextDecoder().decode(wordsToBytesTrimmed(data));
        break;
      case 23: // h — description hash
        if (length === 52) descriptionHash = hex.encode(wordsToBytesTrimmed(data));
        break;
      case 19: // n — payee node id
        if (length === 53) payeeFromTag = hex.encode(wordsToBytesTrimmed(data));
        break;
      case 6: // x — expiry
        expirySeconds = wordsToInt(data);
        break;
      default:
        break; // features, route hints, fallback, cltv — not needed for intent
    }
  }
  if (paymentHash === null) throw new InvoiceDecodeError('missing payment hash');

  const signature = wordsToBytesTrimmed(signatureWords);
  if (signature.length !== 65) throw new InvoiceDecodeError('malformed signature');
  const recoveryId = signature[64] as number;
  if (recoveryId > 3) throw new InvoiceDecodeError(`invalid recovery id ${recoveryId}`);
  const hrpBytes = new TextEncoder().encode(decoded.prefix);
  const message = new Uint8Array(hrpBytes.length + Math.ceil((dataWords.length * 5) / 8));
  message.set(hrpBytes, 0);
  message.set(wordsToBytesPadded(dataWords), hrpBytes.length);
  const digest = sha256(message);

  let payeeNodeId: string;
  try {
    if (payeeFromTag !== null) {
      const valid = secp256k1.verify(signature.slice(0, 64), digest, hex.decode(payeeFromTag), {
        prehash: false,
      });
      if (!valid) throw new InvoiceDecodeError('signature does not match payee node id');
      payeeNodeId = payeeFromTag;
    } else {
      const recovered = secp256k1.Signature.fromBytes(signature.slice(0, 64), 'compact')
        .addRecoveryBit(recoveryId)
        .recoverPublicKey(digest);
      payeeNodeId = hex.encode(recovered.toBytes(true));
    }
  } catch (error) {
    if (error instanceof InvoiceDecodeError) throw error;
    throw new InvoiceDecodeError(
      `signature recovery failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }

  return {
    network,
    amountMsat,
    paymentHash,
    paymentSecret,
    description,
    descriptionHash,
    payeeNodeId,
    timestamp,
    expirySeconds,
  };
}

// ---------------------------------------------------------------------------
// RGB invoices
// ---------------------------------------------------------------------------

export interface RgbInvoice {
  /** Full asset id (`rgb:…`), or null for asset-agnostic invoices. */
  assetId: string | null;
  /** Schema id segment, or null when omitted. */
  schema: string | null;
  /** Fungible amount, or null (omitted / non-fungible state). */
  amount: bigint | null;
  /** Raw assignment-state segment when present and non-numeric. */
  assignmentRaw: string | null;
  /**
   * Chain-qualified beneficiary — exactly the `recipientId` the gateway's
   * send-asset prepare expects (e.g. `bcrt:utxob:…` or `bcrt:wvout:…`).
   */
  recipientId: string;
  /** blind = utxob seal on an existing UTXO; witness = wvout, paid by a new output. */
  beneficiaryKind: 'blind' | 'witness' | 'unknown';
  /** Chain prefix of the beneficiary (bc, tb, bcrt, …). */
  chain: string | null;
  /** Consignment transport endpoints from the `endpoints` query param. */
  transportEndpoints: string[];
  /** Unix-seconds expiry from the `expiry` query param. */
  expiryTimestamp: number | null;
  assignmentName: string | null;
}

const OMITTED = '~';

function percentDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

export function decodeRgbInvoice(invoice: string): RgbInvoice {
  if (!invoice.startsWith('rgb:')) throw new InvoiceDecodeError('missing rgb: scheme');
  const body = invoice.slice('rgb:'.length);
  const queryIndex = body.indexOf('?');
  const path = queryIndex === -1 ? body : body.slice(0, queryIndex);
  const query = queryIndex === -1 ? '' : body.slice(queryIndex + 1);

  const segments = path.split('/');
  if (segments.length !== 4) {
    throw new InvoiceDecodeError(
      `expected contract/schema/state/beneficiary, got ${segments.length} segment(s)`,
    );
  }
  const [contractSeg, schemaSeg, stateSeg, beneficiarySeg] = segments as [
    string,
    string,
    string,
    string,
  ];
  if (beneficiarySeg === '') throw new InvoiceDecodeError('empty beneficiary');

  const assetId = contractSeg === OMITTED ? null : `rgb:${contractSeg}`;
  const schema = schemaSeg === OMITTED ? null : schemaSeg;
  let amount: bigint | null = null;
  let assignmentRaw: string | null = null;
  if (stateSeg !== OMITTED) {
    if (/^\d+$/.test(stateSeg)) amount = BigInt(stateSeg);
    else assignmentRaw = stateSeg;
  }

  const beneficiaryMatch = /^([a-z0-9]+):(utxob|wvout):(.+)$/.exec(beneficiarySeg);
  const chain = beneficiaryMatch === null ? null : (beneficiaryMatch[1] as string);
  const beneficiaryKind =
    beneficiaryMatch === null ? 'unknown' : beneficiaryMatch[2] === 'utxob' ? 'blind' : 'witness';

  let transportEndpoints: string[] = [];
  let expiryTimestamp: number | null = null;
  let assignmentName: string | null = null;
  if (query !== '') {
    for (const pair of query.split('&')) {
      const eq = pair.indexOf('=');
      if (eq === -1) throw new InvoiceDecodeError(`malformed query parameter: ${pair}`);
      const key = percentDecode(pair.slice(0, eq));
      const value = percentDecode(pair.slice(eq + 1));
      if (key === 'endpoints') {
        transportEndpoints = value.split(',').filter((endpoint) => endpoint !== '');
      } else if (key === 'expiry') {
        if (!/^\d+$/.test(value)) throw new InvoiceDecodeError(`invalid expiry: ${value}`);
        expiryTimestamp = Number(value);
      } else if (key === 'assignment_name') {
        assignmentName = value;
      }
    }
  }

  return {
    assetId,
    schema,
    amount,
    assignmentRaw,
    recipientId: beneficiarySeg,
    beneficiaryKind,
    chain,
    transportEndpoints,
    expiryTimestamp,
    assignmentName,
  };
}
