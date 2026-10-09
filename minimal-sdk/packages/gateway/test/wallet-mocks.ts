/** Configurable in-memory wallet backend for unit tests. */
import type {
  AssetSendResult,
  CreateUtxosParams,
  ReceiveData,
  ReceiveRequest,
  SendAssetBeginRequest,
  WalletAsset,
  WalletBackend,
  WalletBtcBalance,
  WalletHandle,
  WalletIdentity,
  WalletTransfer,
  WalletUnspent,
} from '../src/wallets/backend.js';

export const EMPTY_BALANCE = { settled: 0, future: 0, spendable: 0 };

export interface MockWalletData {
  address?: string;
  btcBalance?: WalletBtcBalance;
  assets?: WalletAsset[];
  unspents?: WalletUnspent[];
  /** Keyed by assetId; 'null' key holds asset-less transfers. */
  transfersByAsset?: Record<string, WalletTransfer[]>;
  /** Unsigned PSBT returned by every *Begin call. */
  preparedPsbt?: string;
  sendBtcTxid?: string;
  sendAssetTxid?: string;
  sendAssetBatchTransferIdx?: number;
  utxosCreated?: number;
}

export class MockWalletHandle implements WalletHandle {
  readonly operations: string[] = [];
  syncCount = 0;
  refreshCount = 0;
  receiveCount = 0;
  closed = false;
  /** Set to make every operation wait (concurrency probes). */
  gate: (() => Promise<void>) | undefined;
  /** Set to make operations fail. */
  failWith: Error | undefined;

  constructor(
    readonly identity: WalletIdentity,
    private readonly data: MockWalletData = {},
  ) {}

  private async op<T>(name: string, result: () => T): Promise<T> {
    this.operations.push(name);
    if (this.closed) throw new Error(`operation ${name} on closed mock wallet`);
    if (this.gate !== undefined) await this.gate();
    if (this.failWith !== undefined) throw this.failWith;
    return result();
  }

  getAddress(): Promise<string> {
    return this.op('getAddress', () => this.data.address ?? `addr-${this.identity.userId}`);
  }

  getBtcBalance(): Promise<WalletBtcBalance> {
    return this.op(
      'getBtcBalance',
      () => this.data.btcBalance ?? { vanilla: EMPTY_BALANCE, colored: EMPTY_BALANCE },
    );
  }

  listAssets(): Promise<WalletAsset[]> {
    return this.op('listAssets', () => this.data.assets ?? []);
  }

  listUnspents(): Promise<WalletUnspent[]> {
    return this.op('listUnspents', () => this.data.unspents ?? []);
  }

  listTransfers(assetId: string | null): Promise<WalletTransfer[]> {
    return this.op(
      `listTransfers:${assetId ?? 'null'}`,
      () => this.data.transfersByAsset?.[assetId ?? 'null'] ?? [],
    );
  }

  lastReceiveRequest: ReceiveRequest | undefined;

  receive(request: ReceiveRequest): Promise<ReceiveData> {
    return this.op(`receive:${request.mode}`, () => {
      this.receiveCount += 1;
      this.lastReceiveRequest = request;
      return {
        invoice: `mock-invoice-${this.identity.userId}-${this.receiveCount}`,
        recipientId: `mock-recipient-${this.identity.userId}-${this.receiveCount}`,
        expirationTimestamp: request.expirationTimestamp,
        batchTransferIdx: this.receiveCount,
      };
    });
  }

  lastSendBtcArgs: { address: string; amountSat: number; feeRateSatPerVb: number } | undefined;
  lastSendAssetRequest: SendAssetBeginRequest | undefined;
  lastCreateUtxosParams: CreateUtxosParams | undefined;
  lastSignedPsbt: string | undefined;

  sendBtcBegin(address: string, amountSat: number, feeRateSatPerVb: number): Promise<string> {
    return this.op('sendBtcBegin', () => {
      this.lastSendBtcArgs = { address, amountSat, feeRateSatPerVb };
      return this.data.preparedPsbt ?? `mock-psbt-${this.identity.userId}`;
    });
  }

  sendBtcEnd(signedPsbt: string): Promise<string> {
    return this.op('sendBtcEnd', () => {
      this.lastSignedPsbt = signedPsbt;
      return this.data.sendBtcTxid ?? 'mock-btc-txid';
    });
  }

  sendAssetBegin(request: SendAssetBeginRequest): Promise<string> {
    return this.op('sendAssetBegin', () => {
      this.lastSendAssetRequest = request;
      return this.data.preparedPsbt ?? `mock-psbt-${this.identity.userId}`;
    });
  }

  sendAssetEnd(signedPsbt: string): Promise<AssetSendResult> {
    return this.op('sendAssetEnd', () => {
      this.lastSignedPsbt = signedPsbt;
      return {
        txid: this.data.sendAssetTxid ?? 'mock-asset-txid',
        batchTransferIdx: this.data.sendAssetBatchTransferIdx ?? 7,
      };
    });
  }

  createUtxosBegin(params: CreateUtxosParams): Promise<string> {
    return this.op('createUtxosBegin', () => {
      this.lastCreateUtxosParams = params;
      return this.data.preparedPsbt ?? `mock-psbt-${this.identity.userId}`;
    });
  }

  createUtxosEnd(signedPsbt: string): Promise<number> {
    return this.op('createUtxosEnd', () => {
      this.lastSignedPsbt = signedPsbt;
      return this.data.utxosCreated ?? 5;
    });
  }

  refresh(): Promise<void> {
    return this.op('refresh', () => {
      this.refreshCount += 1;
    });
  }

  sync(): Promise<void> {
    return this.op('sync', () => {
      this.syncCount += 1;
    });
  }

  async close(): Promise<void> {
    this.operations.push('close');
    this.closed = true;
  }
}

export class MockWalletBackend implements WalletBackend {
  readonly openCalls: WalletIdentity[] = [];
  readonly handles: MockWalletHandle[] = [];
  /** Per-user fixture data applied at open time. */
  dataFor: (identity: WalletIdentity) => MockWalletData = () => ({});
  /** Set to make open() fail (e.g. invalid-xpub simulation). */
  openError: Error | undefined;

  async open(identity: WalletIdentity): Promise<WalletHandle> {
    this.openCalls.push(identity);
    if (this.openError !== undefined) throw this.openError;
    const handle = new MockWalletHandle(identity, this.dataFor(identity));
    this.handles.push(handle);
    return handle;
  }

  handleFor(userId: string): MockWalletHandle | undefined {
    return [...this.handles].reverse().find((handle) => handle.identity.userId === userId);
  }
}
