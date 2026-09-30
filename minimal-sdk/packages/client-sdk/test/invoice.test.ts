/**
 * Invoice decoding: official BOLT11 spec vectors (lightning/bolts
 * 11-payment-encoding.md, all signed by the spec's reference key) and RGB
 * invoices in the rgb-invoicing v0.11 grammar, including the real invoice
 * rgb-lib generated in the parity fixture.
 */
import { describe, expect, it } from 'vitest';
import { decodeBolt11, decodeRgbInvoice, InvoiceDecodeError } from '../src/invoice.js';
import { fixture } from './helpers.js';

const SPEC_PAYEE = '03e7156ae33b0a208d0744199163177e909e80176e55d97a2f221ede0f934dd9ad';
const SPEC_HASH = '0001020304050607080900010203040506070809000102030405060708090102';
const SPEC_SECRET = '1111111111111111111111111111111111111111111111111111111111111111';

const VECTOR_ANY_AMOUNT =
  'lnbc1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdpl2pkx2ctnv5sxxmmwwd5kgetjypeh2ursdae8g6twvus8g6rfwvs8qun0dfjkxaq9qrsgq357wnc5r2ueh7ck6q93dj32dlqnls087fxdwk8qakdyafkq3yap9us6v52vjjsrvywa6rt52cm9r9zqt8r2t7mlcwspyetp5h2tztugp9lfyql';
const VECTOR_COFFEE =
  'lnbc2500u1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpu9qrsgquk0rl77nj30yxdy8j9vdx85fkpmdla2087ne0xh8nhedh8w27kyke0lp53ut353s06fv3qfegext0eh0ymjpf39tuven09sam30g4vgpfna3rh';
const VECTOR_UTF8 =
  'lnbc2500u1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdpquwpc4curk03c9wlrswe78q4eyqc7d8d0xqzpu9qrsgqhtjpauu9ur7fw2thcl4y9vfvh4m9wlfyz2gem29g5ghe2aak2pm3ps8fdhtceqsaagty2vph7utlgj48u0ged6a337aewvraedendscp573dxr';
const VECTOR_HASHED =
  'lnbc20m1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqhp58yjmdan79s6qqdhdzgynm4zwqd5d7xmw5fk98klysy043l2ahrqs9qrsgq7ea976txfraylvgzuxs8kgcw23ezlrszfnh8r6qtfpr6cxga50aj6txm9rxrydzd06dfeawfk6swupvz4erwnyutnjq7x39ymw6j38gp7ynn44';
const VECTOR_TESTNET =
  'lntb20m1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygshp58yjmdan79s6qqdhdzgynm4zwqd5d7xmw5fk98klysy043l2ahrqspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqfpp3x9et2e20v6pu37c5d9vax37wxq72un989qrsgqdj545axuxtnfemtpwkc45hx9d2ft7x04mt8q7y6t0k2dge9e7h8kpy9p34ytyslj3yu569aalz2xdk8xkd7ltxqld94u8h2esmsmacgpghe9k8';

describe('decodeBolt11 (spec vectors)', () => {
  it('decodes the any-amount donation invoice', () => {
    const invoice = decodeBolt11(VECTOR_ANY_AMOUNT);
    expect(invoice.network).toBe('Mainnet');
    expect(invoice.amountMsat).toBeNull();
    expect(invoice.paymentHash).toBe(SPEC_HASH);
    expect(invoice.paymentSecret).toBe(SPEC_SECRET);
    expect(invoice.description).toBe('Please consider supporting this project');
    expect(invoice.payeeNodeId).toBe(SPEC_PAYEE);
    expect(invoice.timestamp).toBe(1496314658);
    expect(invoice.expirySeconds).toBe(3600);
  });

  it('decodes the 2500u coffee invoice with 60s expiry', () => {
    const invoice = decodeBolt11(VECTOR_COFFEE);
    expect(invoice.amountMsat).toBe(250_000_000n);
    expect(invoice.description).toBe('1 cup coffee');
    expect(invoice.expirySeconds).toBe(60);
    expect(invoice.payeeNodeId).toBe(SPEC_PAYEE);
  });

  it('decodes a UTF-8 description', () => {
    const invoice = decodeBolt11(VECTOR_UTF8);
    expect(invoice.description).toBe('ナンセンス 1杯');
    expect(invoice.amountMsat).toBe(250_000_000n);
    expect(invoice.payeeNodeId).toBe(SPEC_PAYEE);
  });

  it('decodes a hashed-description 20m invoice', () => {
    const invoice = decodeBolt11(VECTOR_HASHED);
    expect(invoice.amountMsat).toBe(2_000_000_000n);
    expect(invoice.description).toBeNull();
    expect(invoice.descriptionHash).toBe(
      '3925b6f67e2c340036ed12093dd44e0368df1b6ea26c53dbe4811f58fd5db8c1',
    );
    expect(invoice.payeeNodeId).toBe(SPEC_PAYEE);
  });

  it('decodes a testnet invoice (and ignores the fallback tag)', () => {
    const invoice = decodeBolt11(VECTOR_TESTNET);
    expect(invoice.network).toBe('Testnet');
    expect(invoice.amountMsat).toBe(2_000_000_000n);
    expect(invoice.payeeNodeId).toBe(SPEC_PAYEE);
  });

  it('rejects a corrupted invoice (checksum)', () => {
    const corrupted = `${VECTOR_COFFEE.slice(0, -1)}${VECTOR_COFFEE.endsWith('h') ? 'k' : 'h'}`;
    expect(() => decodeBolt11(corrupted)).toThrow(InvoiceDecodeError);
  });

  it('rejects a non-lightning bech32 string', () => {
    expect(() => decodeBolt11('bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4')).toThrow(
      InvoiceDecodeError,
    );
  });

  it('rejects truncated data', () => {
    expect(() => decodeBolt11('lnbc1pvjluez')).toThrow(InvoiceDecodeError);
  });
});

describe('decodeRgbInvoice', () => {
  it('decodes the real rgb-lib witness invoice from the parity fixture', () => {
    const invoice = decodeRgbInvoice(fixture.witnessReceive.invoice);
    expect(invoice.assetId).toBeNull();
    expect(invoice.schema).toBeNull();
    expect(invoice.amount).toBeNull();
    expect(invoice.recipientId).toBe(fixture.witnessReceive.recipientId);
    expect(invoice.beneficiaryKind).toBe('witness');
    expect(invoice.chain).toBe('bcrt');
    expect(invoice.expiryTimestamp).toBe(fixture.witnessReceive.expirationTimestamp);
    expect(invoice.transportEndpoints).toEqual(['rpc://localhost:3000/json-rpc']);
  });

  it('decodes an asset-specific blind invoice with amount and endpoints', () => {
    const invoice = decodeRgbInvoice(
      'rgb:erRCLIhl-nBu1DdF-M8YCRNH-Y3rB0W3-hLZH1DP-1AY9zpQ/RGB20Fixed/100/bcrt:utxob:4vm1CX2Z-K8hMo59-e7dgGBS-Jka7mYn-Xe~yP85-yUiHHxr-aVlYa?expiry=1700000000&endpoints=rpc://proxy-a/json-rpc,rpcs://proxy-b/json-rpc',
    );
    expect(invoice.assetId).toBe('rgb:erRCLIhl-nBu1DdF-M8YCRNH-Y3rB0W3-hLZH1DP-1AY9zpQ');
    expect(invoice.schema).toBe('RGB20Fixed');
    expect(invoice.amount).toBe(100n);
    expect(invoice.beneficiaryKind).toBe('blind');
    expect(invoice.recipientId).toBe(
      'bcrt:utxob:4vm1CX2Z-K8hMo59-e7dgGBS-Jka7mYn-Xe~yP85-yUiHHxr-aVlYa',
    );
    expect(invoice.expiryTimestamp).toBe(1700000000);
    expect(invoice.transportEndpoints).toEqual([
      'rpc://proxy-a/json-rpc',
      'rpcs://proxy-b/json-rpc',
    ]);
  });

  it('keeps non-numeric assignment state raw and parses assignment_name', () => {
    const invoice = decodeRgbInvoice(
      'rgb:3NoxsLum-cRPebTV-gTZY8qY-KS20lx7-OqgtBls-t7muan4/~/BF/bc:utxob:4vm1CX2Z-K8hMo59-e7dgGBS-Jka7mYn-Xe~yP85-yUiHHxr-aVlYa?assignment_name=assetOwner',
    );
    expect(invoice.amount).toBeNull();
    expect(invoice.assignmentRaw).toBe('BF');
    expect(invoice.assignmentName).toBe('assetOwner');
    expect(invoice.chain).toBe('bc');
  });

  it('rejects malformed invoices', () => {
    expect(() => decodeRgbInvoice('rgbx:~/~/~/bcrt:utxob:x')).toThrow(InvoiceDecodeError);
    expect(() => decodeRgbInvoice('rgb:~/~/bcrt:utxob:x')).toThrow(InvoiceDecodeError);
    expect(() => decodeRgbInvoice('rgb:~/~/~/')).toThrow(InvoiceDecodeError);
    expect(() => decodeRgbInvoice('rgb:~/~/~/bcrt:utxob:x?expiry=soon')).toThrow(
      InvoiceDecodeError,
    );
    expect(() => decodeRgbInvoice('rgb:~/~/~/bcrt:utxob:x?flag')).toThrow(InvoiceDecodeError);
  });
});
