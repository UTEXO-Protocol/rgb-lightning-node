import { describe, expect, it } from 'vitest';
import { SDK_NAME } from '../src/index.js';

describe('client-sdk package smoke', () => {
  it('imports resolve and exports are present', () => {
    expect(SDK_NAME).toBe('@utexo/minimal-client-sdk');
  });
});
