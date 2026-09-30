/**
 * Generates TypeScript types for the RLN endpoint subset the gateway uses.
 *
 * Reads the repo-root openapi.yaml (read-only), prunes it down to RLN_PATHS
 * plus every transitively $ref-erenced component, and runs openapi-typescript
 * on the pruned document. Pruning keeps the generated file reviewable and
 * sidesteps spec problems in endpoints the gateway never calls.
 *
 * Run with: pnpm --filter @utexo/minimal-gateway generate:rln-types
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'yaml';
import openapiTS, { astToString } from 'openapi-typescript';

const here = dirname(fileURLToPath(import.meta.url));
const specPath = join(here, '..', '..', '..', '..', 'openapi.yaml');
const outPath = join(here, '..', 'src', 'rln', 'openapi.ts');

/** The RLN endpoints the gateway calls (plan Task 3). */
const RLN_PATHS = [
  '/nodeinfo',
  '/address',
  '/btcbalance',
  '/sendbtc',
  '/createutxos',
  '/rgbinvoice',
  '/decodergbinvoice',
  '/sendrgb',
  '/listtransfers',
  '/refreshtransfers',
  '/lninvoice',
  '/decodelninvoice',
  '/invoicestatus',
  '/sendpayment',
  '/getpayment',
  '/listpayments',
  '/listchannels',
];

const spec = parse(readFileSync(specPath, 'utf8'));

for (const path of RLN_PATHS) {
  if (spec.paths?.[path] === undefined) {
    throw new Error(`endpoint ${path} not found in openapi.yaml`);
  }
}

/** Collect '#/components/schemas/X' names reachable from a node. */
function collectRefs(node, into) {
  if (Array.isArray(node)) {
    for (const item of node) collectRefs(item, into);
    return;
  }
  if (node === null || typeof node !== 'object') return;
  for (const [key, value] of Object.entries(node)) {
    if (key === '$ref' && typeof value === 'string') {
      const match = /^#\/components\/schemas\/(.+)$/.exec(value);
      if (match === null) throw new Error(`unsupported $ref target: ${value}`);
      into.add(match[1]);
    } else {
      collectRefs(value, into);
    }
  }
}

const paths = Object.fromEntries(RLN_PATHS.map((p) => [p, spec.paths[p]]));
const needed = new Set();
collectRefs(paths, needed);
// Fixpoint: schemas referenced by already-needed schemas.
let previousSize = 0;
while (needed.size !== previousSize) {
  previousSize = needed.size;
  for (const name of [...needed]) {
    const schema = spec.components?.schemas?.[name];
    if (schema === undefined) throw new Error(`component schema ${name} not found`);
    collectRefs(schema, needed);
  }
}

const pruned = {
  openapi: spec.openapi,
  info: spec.info,
  paths,
  components: {
    schemas: Object.fromEntries(
      [...needed].sort().map((name) => [name, spec.components.schemas[name]]),
    ),
    securitySchemes: spec.components.securitySchemes,
  },
  security: spec.security,
};

const ast = await openapiTS(pruned);
const banner = `/**
 * GENERATED FILE — do not edit by hand.
 *
 * RLN API types for the endpoint subset the gateway uses, generated from the
 * repo-root openapi.yaml. Regenerate with:
 *   pnpm --filter @utexo/minimal-gateway generate:rln-types
 */
`;
writeFileSync(outPath, banner + astToString(ast));
console.log(`wrote ${outPath} (${needed.size} schemas, ${RLN_PATHS.length} paths)`);
