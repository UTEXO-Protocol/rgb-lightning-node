import { describe, expect, it } from 'vitest';
import { GATEWAY_NAME } from '../src/index.js';

describe('gateway package smoke', () => {
  it('imports resolve and exports are present', () => {
    expect(GATEWAY_NAME).toBe('@utexo/minimal-gateway');
  });
});
