/**
 * Native rgb-lib backend (Preflight A decision, Task 1): the published
 * `@utexo/rgb-lib` JS wrapper is stale, so this shim calls the compiled
 * platform module directly with the verified signatures. Marshalling rules
 * (documented in minimal-sdk/README.md):
 *  - numeric inputs are passed as strings;
 *  - JSON payloads are camelCase;
 *  - Assignment is externally tagged with a JSON number ({"Fungible": 100})
 *    or a bare string for unit variants ("Any");
 *  - JS null maps to a native NULL only for nullable-typemap params
 *    (asset ids); expiration timestamps must always be concrete strings;
 *  - watch-only construction passes keys JSON with `mnemonic: null`.
 */
import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import {
  WalletBackendError,
  type Balance,
  type CreateUtxosParams,
  type ReceiveData,
  type ReceiveRequest,
  type SendAssetBeginRequest,
  type UnspentAllocation,
  type WalletAsset,
  type WalletBackend,
  type WalletBtcBalance,
  type WalletHandle,
  type WalletIdentity,
  type WalletTransfer,
  type WalletUnspent,
} from './backend.js';

type OpaqueHandle = unknown;

interface NativeRgbLib {
  rgblib_new_wallet(walletDataJson: string, keysJson: string): OpaqueHandle;
  rgblib_go_online(
    wallet: OpaqueHandle,
    skipConsistencyCheck: boolean,
    indexerUrl: string,
  ): OpaqueHandle;
  rgblib_get_address(wallet: OpaqueHandle): string;
  rgblib_get_btc_balance(wallet: OpaqueHandle, online: OpaqueHandle, skipSync: boolean): string;
  rgblib_list_assets(wallet: OpaqueHandle, filterAssetSchemasJson: string): string;
  rgblib_list_unspents(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    settledOnly: boolean,
    skipSync: boolean,
  ): string;
  rgblib_list_transfers(wallet: OpaqueHandle, assetId: string | null): string;
  rgblib_blind_receive(
    wallet: OpaqueHandle,
    assetId: string | null,
    assignmentJson: string,
    expirationTimestamp: string,
    transportEndpointsJson: string,
    minConfirmations: string,
  ): string;
  rgblib_witness_receive(
    wallet: OpaqueHandle,
    assetId: string | null,
    assignmentJson: string,
    expirationTimestamp: string,
    transportEndpointsJson: string,
    minConfirmations: string,
  ): string;
  rgblib_send_btc_begin(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    address: string,
    amount: string,
    feeRate: string,
    skipSync: boolean,
  ): string;
  rgblib_send_btc_end(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    signedPsbt: string,
    skipSync: boolean,
  ): string;
  rgblib_send_begin(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    recipientMapJson: string,
    donation: boolean,
    feeRate: string,
    minConfirmations: string,
    expirationTimestamp: string,
    dryRun: boolean,
  ): string;
  rgblib_send_end(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    signedPsbt: string,
    skipSync: boolean,
  ): string;
  rgblib_create_utxos_begin(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    upTo: boolean,
    num: string,
    size: string,
    feeRate: string,
    skipSync: boolean,
  ): string;
  rgblib_create_utxos_end(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    signedPsbt: string,
    skipSync: boolean,
  ): string;
  rgblib_refresh(
    wallet: OpaqueHandle,
    online: OpaqueHandle,
    assetId: string | null,
    filterJson: string,
    skipSync: boolean,
  ): string;
  rgblib_sync(wallet: OpaqueHandle, online: OpaqueHandle): unknown;
  rgblib_restore_keys(network: string, mnemonic: string): string;
  free_wallet(wallet: OpaqueHandle): void;
  free_online(online: OpaqueHandle): void;
}

const PLATFORM_PACKAGES: Record<string, string> = {
  'linux-x64': '@utexo/rgb-lib-linux-x64',
  'linux-arm64': '@utexo/rgb-lib-linux-arm64',
  'darwin-arm64': '@utexo/rgb-lib-darwin-arm64',
};

let nativeModule: NativeRgbLib | undefined;

/** Lazily require the compiled platform module (never loaded by unit tests). */
export function loadNativeRgbLib(): NativeRgbLib {
  if (nativeModule !== undefined) return nativeModule;
  const key = `${process.platform}-${process.arch}`;
  const packageName = PLATFORM_PACKAGES[key];
  if (packageName === undefined) {
    throw new WalletBackendError(
      'rgb-lib native module unavailable',
      `no rgb-lib platform package for ${key}`,
    );
  }
  const require = createRequire(import.meta.url);
  nativeModule = require(`${packageName}/rgblib`) as NativeRgbLib;
  return nativeModule;
}

function toDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * rgb-lib error variants that mean "the REQUEST was wrong", as opposed to a
 * gateway bug or a node/indexer failure. Matched on the variant name because
 * the native binding surfaces Rust debug strings (`RgbLib(InvalidAddress)`).
 * Keep in sync with rgb-lib's `src/error.rs` on version bumps; an unlisted
 * variant degrades to 500, never to a wrong 400.
 */
const CLIENT_ERROR_VARIANTS =
  /\b(AssetNotFound|BitcoinNetworkMismatch|Invalid(Address|AmountZero|Assignment|BitcoinKeys|BitcoinNetwork|Expiration|FeeRate|Fingerprint|ProxyProtocol|Pubkey|RecipientData|RecipientID|RecipientMap|RecipientNetwork|TransportEndpoints?|Txid|VanillaKeychain|WitnessVersion)|MaxFeeExceeded|MinFeeNotMet|NoValidTransportEndpoint|OutputBelowDustLimit|RecipientIDAlreadyUsed|RecipientIDDuplicated|UnknownRgbSchema|UnsupportedSchema|UnsupportedTransportType)\b/;

/** Wrap a native failure; rgb-lib errors are Rust debug strings. Exported for unit tests. */
export function wrapNativeError(operation: string, error: unknown): WalletBackendError {
  const detail = toDetail(error);
  const insufficient = /Insufficient(Bitcoins|AllocationSlots|SpendableAssets|TotalAssets)/.test(
    detail,
  );
  return new WalletBackendError(
    `wallet ${operation} failed`,
    detail,
    insufficient,
    !insufficient && CLIENT_ERROR_VARIANTS.test(detail),
  );
}

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : {};
}

/**
 * A JSON number from rgb-lib that must round-trip losslessly.
 *
 * rgb-lib counts assets in u64 and `JSON.parse` decodes into a double, so any
 * value above 2^53-1 is already rounded by the time this sees it — it cannot be
 * recovered, only detected. Detection is exact: a u64 above the safe range
 * always parses to at least 2^53, which `Number.isSafeInteger` rejects. The
 * alternative was to keep returning the rounded value, which would report a
 * balance the wallet does not hold; a loud failure is the lesser evil, and the
 * caller (`walletHttpError`) maps it to 502 AMOUNT_NOT_REPRESENTABLE.
 *
 * Absence is a separate concern and stays with each caller: some fields are
 * genuinely optional (an amount-less RGB assignment) and some are drift.
 */
function safeInteger(value: unknown, field: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new WalletBackendError(
      'wallet value is not representable',
      `${field} is not a safe integer: ${String(value)}`,
      false,
      false,
      true,
    );
  }
  return value;
}

/** Drift-tolerant read for fields rgb-lib may omit; a present value is still range-checked. */
function safeIntegerOrZero(value: unknown, field: string): number {
  return value === undefined || value === null ? 0 : safeInteger(value, field);
}

function safeIntegerOrNull(value: unknown, field: string): number | null {
  return value === undefined || value === null ? null : safeInteger(value, field);
}

function stringOrNull(value: unknown): string | null {
  return typeof value === 'string' && value !== '' ? value : null;
}

/** Strip JSON quoting if the native layer returned a serialized string. Exported for unit tests. */
export function unquote(value: string): string {
  if (value.startsWith('"') && value.endsWith('"')) {
    try {
      return JSON.parse(value) as string;
    } catch {
      return value;
    }
  }
  return value;
}

function mapBalance(value: unknown, field: string): Balance {
  const record = asRecord(value);
  return {
    settled: safeIntegerOrZero(record['settled'], `${field}.settled`),
    future: safeIntegerOrZero(record['future'], `${field}.future`),
    spendable: safeIntegerOrZero(record['spendable'], `${field}.spendable`),
  };
}

/** Fungible amount of an externally tagged rgb-lib Assignment, if any. */
function assignmentAmount(value: unknown, field: string): number | null {
  const record = asRecord(value);
  return safeIntegerOrNull(record['Fungible'], `${field}.Fungible`);
}

/**
 * An rgb-lib Assignment as a flat pair. The wire form is externally tagged —
 * `{"Fungible": 100}`, `{"InflationRight": 100}`, `{"NonFungible": …}` or the
 * bare string `"Any"` / `"ReplaceRight"` for unit variants — which is awkward
 * to declare in a strict response schema and awkward to consume. `kind` carries
 * the variant name verbatim so a new rgb-lib variant surfaces as itself rather
 * than as a dropped field; `amount` is set only for the fungible-valued ones.
 */
export function mapAssignment(
  value: unknown,
  field: string,
): { kind: string; amount: number | null } {
  if (typeof value === 'string') return { kind: value, amount: null };
  const entries = Object.entries(asRecord(value));
  const first = entries[0];
  if (entries.length !== 1 || first === undefined) {
    throw new WalletBackendError(
      'wallet returned an unreadable assignment',
      `${field} is not an externally tagged assignment: ${JSON.stringify(value)}`,
    );
  }
  const [kind, payload] = first;
  return { kind, amount: typeof payload === 'number' ? safeInteger(payload, field) : null };
}

/** Exported for unit tests (response-shape drift must fail loudly, not corrupt data). */
export function mapAssets(raw: unknown): WalletAsset[] {
  const bySchema = asRecord(raw);
  const assets: WalletAsset[] = [];
  for (const [schemaKey, list] of Object.entries(bySchema)) {
    if (!Array.isArray(list)) continue;
    for (const entry of list) {
      const record = asRecord(entry);
      const assetId = stringOrNull(record['assetId']);
      if (assetId === null) continue;
      // Every rgb-lib asset schema declares a non-optional, non-empty name.
      // Substituting '' here used to hide the drift and then hand a client a
      // nameless asset it cannot display or validate; fail loudly instead.
      const name = stringOrNull(record['name']);
      if (name === null) {
        throw new WalletBackendError(
          'wallet returned an unnamed asset',
          `asset ${assetId} (${schemaKey}) has no name`,
        );
      }
      assets.push({
        assetId,
        schema: schemaKey,
        ticker: stringOrNull(record['ticker']),
        name,
        details: stringOrNull(record['details']),
        precision: safeIntegerOrZero(record['precision'], `asset ${assetId} precision`),
        // UDA has no issued supply; NIA/CFA/IFA do.
        issuedSupply: safeIntegerOrNull(record['issuedSupply'], `asset ${assetId} issuedSupply`),
        timestamp: safeIntegerOrZero(record['timestamp'], `asset ${assetId} timestamp`),
        addedAt: safeIntegerOrZero(record['addedAt'], `asset ${assetId} addedAt`),
        balance: mapBalance(record['balance'], `asset ${assetId} balance`),
      });
    }
  }
  return assets;
}

export function mapUnspents(raw: unknown): WalletUnspent[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((entry) => {
    const record = asRecord(entry);
    const utxo = asRecord(record['utxo']);
    const outpoint = asRecord(utxo['outpoint']);
    const allocationsRaw = record['rgbAllocations'];
    const allocations: UnspentAllocation[] = Array.isArray(allocationsRaw)
      ? allocationsRaw.map((allocation, index) => {
          const allocationRecord = asRecord(allocation);
          const field = `unspent allocation[${index}]`;
          return {
            assetId: stringOrNull(allocationRecord['assetId']),
            amount: assignmentAmount(allocationRecord['assignment'], field),
            assignment: mapAssignment(allocationRecord['assignment'], `${field}.assignment`),
            settled: allocationRecord['settled'] === true,
          };
        })
      : [];
    return {
      txid: typeof outpoint['txid'] === 'string' ? outpoint['txid'] : '',
      vout: safeIntegerOrZero(outpoint['vout'], 'unspent vout'),
      amountSat: safeIntegerOrZero(utxo['btcAmount'], 'unspent btcAmount'),
      colorable: utxo['colorable'] === true,
      // Blind receives already promised against this UTXO. A client choosing
      // inputs or counting free allocation slots cannot do it from
      // `allocations` alone: a pending blind receive reserves a slot without
      // yet holding an allocation.
      pendingBlinded: safeIntegerOrZero(record['pendingBlinded'], 'unspent pendingBlinded'),
      allocations,
    };
  });
}

export function mapTransfers(raw: unknown, assetId: string | null): WalletTransfer[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((entry) => {
    const record = asRecord(entry);
    let amount = assignmentAmount(record['requestedAssignment'], 'transfer requestedAssignment');
    const rawAssignments = record['assignments'];
    const assignments = Array.isArray(rawAssignments)
      ? rawAssignments.map((assignment, index) =>
          mapAssignment(assignment, `transfer assignments[${index}]`),
        )
      : [];
    if (amount === null && Array.isArray(rawAssignments)) {
      const total = rawAssignments
        .map((assignment, index) => assignmentAmount(assignment, `transfer assignments[${index}]`))
        .filter((value): value is number => value !== null)
        .reduce((sum, value) => sum + value, 0);
      amount = total > 0 ? safeInteger(total, 'transfer assignments total') : null;
    }
    return {
      idx: safeIntegerOrZero(record['idx'], 'transfer idx'),
      // The batch transfer this one belongs to. rgb-lib's own operations
      // (refresh, fail, delete) are keyed by the BATCH index, not `idx`, so a
      // client that only sees `idx` cannot address the transfer it just read.
      batchTransferIdx: safeIntegerOrNull(record['batchTransferIdx'], 'transfer batchTransferIdx'),
      assetId,
      amount,
      assignments,
      kind: typeof record['kind'] === 'string' ? record['kind'] : 'Unknown',
      status: typeof record['status'] === 'string' ? record['status'] : 'Unknown',
      txid: stringOrNull(record['txid']),
      recipientId: stringOrNull(record['recipientId']),
      expiration: safeIntegerOrNull(record['expiration'], 'transfer expiration'),
      createdAt: safeIntegerOrZero(record['createdAt'], 'transfer createdAt'),
      updatedAt: safeIntegerOrZero(record['updatedAt'], 'transfer updatedAt'),
    };
  });
}

export interface NativeWalletBackendOptions {
  /** Gateway-owned directory; each user's wallet lives in `<baseDir>/<userId>`. */
  baseDir: string;
  /** rgb-lib BitcoinNetwork name: Regtest | Testnet | Signet | Mainnet. */
  network: string;
  /** Indexer URL for goOnline (electrum host:port verified in Preflight A). */
  indexerUrl: string;
  maxAllocationsPerUtxo?: number;
}

export class NativeWalletBackend implements WalletBackend {
  constructor(private readonly options: NativeWalletBackendOptions) {}

  async open(identity: WalletIdentity): Promise<WalletHandle> {
    const lib = loadNativeRgbLib();
    const dataDir = join(this.options.baseDir, identity.userId);
    mkdirSync(dataDir, { recursive: true });
    const walletData = JSON.stringify({
      dataDir,
      bitcoinNetwork: this.options.network,
      databaseType: 'Sqlite',
      maxAllocationsPerUtxo: String(this.options.maxAllocationsPerUtxo ?? 1),
      accountXpubVanilla: identity.xpubs.vanilla,
      accountXpubColored: identity.xpubs.colored,
      vanillaKeychain: null,
      supportedSchemas: ['Nia', 'Cfa', 'Uda', 'Ifa'],
    });
    // Watch-only: mnemonic stays null; the client signs externally (I2).
    const keys = JSON.stringify({
      accountXpubVanilla: identity.xpubs.vanilla,
      accountXpubColored: identity.xpubs.colored,
      vanillaKeychain: null,
      masterFingerprint: identity.xpubs.fingerprint,
      mnemonic: null,
    });
    let wallet: OpaqueHandle;
    try {
      wallet = lib.rgblib_new_wallet(walletData, keys);
    } catch (error) {
      throw wrapNativeError('construction', error);
    }
    let online: OpaqueHandle;
    try {
      online = lib.rgblib_go_online(wallet, false, this.options.indexerUrl);
    } catch (error) {
      lib.free_wallet(wallet);
      throw wrapNativeError('goOnline', error);
    }
    return new NativeWalletHandle(lib, wallet, online);
  }
}

class NativeWalletHandle implements WalletHandle {
  private closed = false;

  constructor(
    private readonly lib: NativeRgbLib,
    private readonly wallet: OpaqueHandle,
    private readonly online: OpaqueHandle,
  ) {}

  private call<T>(operation: string, fn: () => T): T {
    if (this.closed) {
      throw new WalletBackendError(`wallet ${operation} failed`, 'wallet handle is closed');
    }
    try {
      return fn();
    } catch (error) {
      throw wrapNativeError(operation, error);
    }
  }

  async getAddress(): Promise<string> {
    return this.call('getAddress', () => this.lib.rgblib_get_address(this.wallet));
  }

  async getBtcBalance(): Promise<WalletBtcBalance> {
    const raw = this.call('getBtcBalance', () =>
      JSON.parse(this.lib.rgblib_get_btc_balance(this.wallet, this.online, false)),
    ) as Record<string, unknown>;
    return {
      vanilla: mapBalance(raw['vanilla'], 'btc balance vanilla'),
      colored: mapBalance(raw['colored'], 'btc balance colored'),
    };
  }

  async listAssets(): Promise<WalletAsset[]> {
    const raw = this.call('listAssets', () =>
      JSON.parse(this.lib.rgblib_list_assets(this.wallet, JSON.stringify([]))),
    );
    return mapAssets(raw);
  }

  async listUnspents(): Promise<WalletUnspent[]> {
    const raw = this.call('listUnspents', () =>
      JSON.parse(this.lib.rgblib_list_unspents(this.wallet, this.online, false, false)),
    );
    return mapUnspents(raw);
  }

  async listTransfers(assetId: string | null): Promise<WalletTransfer[]> {
    const raw = this.call('listTransfers', () =>
      JSON.parse(this.lib.rgblib_list_transfers(this.wallet, assetId)),
    );
    return mapTransfers(raw, assetId);
  }

  async receive(request: ReceiveRequest): Promise<ReceiveData> {
    const assignment =
      request.amount === null
        ? JSON.stringify('Any')
        : JSON.stringify({ Fungible: request.amount });
    const raw = this.call(`${request.mode}Receive`, () => {
      const fn =
        request.mode === 'blind' ? this.lib.rgblib_blind_receive : this.lib.rgblib_witness_receive;
      return JSON.parse(
        fn(
          this.wallet,
          request.assetId,
          assignment,
          String(request.expirationTimestamp),
          JSON.stringify(request.transportEndpoints),
          String(request.minConfirmations),
        ),
      ) as Record<string, unknown>;
    });
    const invoice = stringOrNull(raw['invoice']);
    const recipientId = stringOrNull(raw['recipientId']);
    if (invoice === null || recipientId === null) {
      throw new WalletBackendError(
        'wallet receive failed',
        `unexpected receive response shape: ${Object.keys(raw).join(',')}`,
      );
    }
    return {
      invoice,
      recipientId,
      expirationTimestamp: safeIntegerOrNull(
        raw['expirationTimestamp'],
        'receive expirationTimestamp',
      ),
      // rgb-lib keys refresh/fail/delete by batch index, so a client that wants
      // to manage the receive it just created needs this. It is non-optional in
      // rgb-lib's ReceiveData; null here only means shape drift.
      batchTransferIdx: safeIntegerOrNull(raw['batchTransferIdx'], 'receive batchTransferIdx'),
    };
  }

  async sendBtcBegin(address: string, amountSat: number, feeRateSatPerVb: number): Promise<string> {
    return this.call('sendBtcBegin', () =>
      unquote(
        this.lib.rgblib_send_btc_begin(
          this.wallet,
          this.online,
          address,
          String(amountSat),
          String(feeRateSatPerVb),
          false,
        ),
      ),
    );
  }

  async sendBtcEnd(signedPsbt: string): Promise<string> {
    return this.call('sendBtcEnd', () =>
      unquote(this.lib.rgblib_send_btc_end(this.wallet, this.online, signedPsbt, false)),
    );
  }

  async sendAssetBegin(request: SendAssetBeginRequest): Promise<string> {
    const recipientMap = JSON.stringify({
      [request.assetId]: [
        {
          recipientId: request.recipientId,
          witnessData:
            request.witnessAmountSat === null
              ? null
              : { amountSat: request.witnessAmountSat, blinding: null },
          assignment: { Fungible: request.amount },
          transportEndpoints: request.transportEndpoints,
        },
      ],
    });
    // send_begin returns JSON {psbt, batchTransferIdx, details}; only the
    // PSBT leaves this layer (details carry gateway-internal paths).
    const raw = this.call('sendAssetBegin', () =>
      JSON.parse(
        this.lib.rgblib_send_begin(
          this.wallet,
          this.online,
          recipientMap,
          request.donation,
          String(request.feeRateSatPerVb),
          String(request.minConfirmations),
          String(request.expirationTimestamp),
          false,
        ),
      ),
    ) as Record<string, unknown>;
    const psbt = stringOrNull(raw['psbt']);
    if (psbt === null) {
      throw new WalletBackendError(
        'wallet sendAssetBegin failed',
        `unexpected send-begin response shape: ${Object.keys(raw).join(',')}`,
      );
    }
    return psbt;
  }

  async sendAssetEnd(signedPsbt: string): Promise<string> {
    const raw = this.call('sendAssetEnd', () =>
      JSON.parse(this.lib.rgblib_send_end(this.wallet, this.online, signedPsbt, false)),
    ) as Record<string, unknown>;
    const txid = stringOrNull(raw['txid']);
    if (txid === null) {
      throw new WalletBackendError(
        'wallet sendAssetEnd failed',
        `unexpected send result shape: ${Object.keys(raw).join(',')}`,
      );
    }
    return txid;
  }

  async createUtxosBegin(params: CreateUtxosParams): Promise<string> {
    return this.call('createUtxosBegin', () =>
      unquote(
        this.lib.rgblib_create_utxos_begin(
          this.wallet,
          this.online,
          params.upTo,
          String(params.num),
          String(params.size),
          String(params.feeRateSatPerVb),
          false,
        ),
      ),
    );
  }

  async createUtxosEnd(signedPsbt: string): Promise<number> {
    const raw = this.call('createUtxosEnd', () =>
      unquote(this.lib.rgblib_create_utxos_end(this.wallet, this.online, signedPsbt, false)),
    );
    const created = Number(raw);
    if (!Number.isFinite(created)) {
      throw new WalletBackendError(
        'wallet createUtxosEnd failed',
        `unexpected create-utxos result: ${raw}`,
      );
    }
    return created;
  }

  async refresh(): Promise<void> {
    this.call('refresh', () =>
      this.lib.rgblib_refresh(this.wallet, this.online, null, JSON.stringify([]), false),
    );
  }

  async sync(): Promise<void> {
    this.call('sync', () => this.lib.rgblib_sync(this.wallet, this.online));
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.lib.free_online(this.online);
    this.lib.free_wallet(this.wallet);
  }
}

/**
 * Test-fixture helper: deterministic xpubs from a mnemonic. Used ONLY by the
 * integration/e2e suites to build a client-side signer fixture — mnemonics are
 * never a gateway input (I1/I2).
 */
export function restoreKeysForTests(
  network: string,
  mnemonic: string,
): { vanilla: string; colored: string; fingerprint: string; xpub: string } {
  const lib = loadNativeRgbLib();
  const keys = JSON.parse(lib.rgblib_restore_keys(network, mnemonic)) as Record<string, unknown>;
  return {
    vanilla: String(keys['accountXpubVanilla']),
    colored: String(keys['accountXpubColored']),
    fingerprint: String(keys['masterFingerprint']),
    xpub: String(keys['xpub']),
  };
}
