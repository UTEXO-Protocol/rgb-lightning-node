/**
 * @utexo/minimal-client-sdk — browser/mobile SDK: key management and
 * verify-before-sign PSBT handling. No wasm; < 500KB bundle budget.
 *
 * The mnemonic is the user's only backup and never leaves the client (I1);
 * the gateway sees only account xpubs and the master fingerprint (I2).
 */
export const SDK_NAME = '@utexo/minimal-client-sdk';

export {
  accountPath,
  addressNetwork,
  coinType,
  hdVersions,
  type BitcoinNetwork,
} from './network.js';
export { ClientKeys, InvalidMnemonicError, fingerprintHex, type AccountXpubs } from './keys.js';
export {
  deriveForOriginPath,
  deriveTaproot,
  parseAccountXpub,
  InvalidXpubError,
  type DerivedScript,
} from './derive.js';
export {
  verifyPsbt,
  PSBT_PARSE_OPTIONS,
  type CheckName,
  type CheckResult,
  type IntentAsset,
  type IntentRecipient,
  type IntentUtxos,
  type OnchainIntent,
  type VerifyParams,
  type VerifyVerdict,
} from './verify.js';
export {
  verifyAndSignPsbt,
  SigningError,
  VerificationFailedError,
  type SignParams,
  type SignResult,
} from './sign.js';
export {
  decodeBolt11,
  decodeRgbInvoice,
  InvoiceDecodeError,
  type Bolt11Invoice,
  type RgbInvoice,
} from './invoice.js';
export {
  GatewayClient,
  GatewayError,
  generateIdempotencyKey,
  IntentMismatchError,
  type AddressDerivation,
  type Assignment,
  type Balance,
  type CompleteParams,
  type CreatedUser,
  type FeeEstimate,
  type GatewayClientOptions,
  type LnBalance,
  type LnDepositPrepareParams,
  type LnDepositPrepareResult,
  type LnInvoiceCreateParams,
  type LnInvoiceInfo,
  type LnPayParams,
  type LnPaymentEntry,
  type LnPaymentStatus,
  type LnPayResult,
  type LnWithdrawParams,
  type LnWithdrawResult,
  type OperationStatus,
  type PreparedOp,
  type PrepareCreateUtxosParams,
  type PrepareSendAssetParams,
  type PrepareSendBtcParams,
  type ReceiveParams,
  type ReceiveResult,
  type RegisterXpubsParams,
  type WalletAddress,
  type WalletAsset,
  type WalletBalances,
  type WalletTransfer,
  type WalletUnspent,
} from './gateway.js';
