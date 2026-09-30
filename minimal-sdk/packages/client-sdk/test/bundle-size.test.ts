/**
 * Bundle-size gate (Task 6): a minified browser production bundle of the
 * whole SDK must stay under 500KB. Fails the suite when exceeded.
 */
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { describe, expect, it } from 'vitest';

const BUDGET_BYTES = 500 * 1024;

describe('bundle-size gate', () => {
  it('minified browser bundle stays under 500KB', async () => {
    const result = await build({
      entryPoints: [fileURLToPath(new URL('../src/index.ts', import.meta.url))],
      bundle: true,
      minify: true,
      format: 'esm',
      platform: 'browser',
      target: 'es2022',
      write: false,
      logLevel: 'silent',
    });
    const bytes = result.outputFiles.reduce((sum, file) => sum + file.contents.byteLength, 0);
    console.info(
      `client-sdk bundle: ${(bytes / 1024).toFixed(1)}KB of ${BUDGET_BYTES / 1024}KB budget`,
    );
    expect(bytes).toBeGreaterThan(0);
    expect(bytes).toBeLessThan(BUDGET_BYTES);
  });
});
