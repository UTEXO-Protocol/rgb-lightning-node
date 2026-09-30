/**
 * Invariant I1 enforcement helper: no gateway API schema may declare a
 * key-material field. Used by the schema-scan test and re-usable at runtime
 * (the e2e suite scans live responses with the same field list).
 */

export const KEY_MATERIAL_FIELD_NAMES: readonly string[] = [
  'mnemonic',
  'seed',
  'xprv',
  'privatekey',
  'signingkey',
  'password',
];

function isKeyMaterialName(name: string): boolean {
  const normalized = name.toLowerCase().replace(/[_-]/g, '');
  return KEY_MATERIAL_FIELD_NAMES.some((forbidden) => normalized.includes(forbidden));
}

/**
 * Walks a JSON schema (or any nested structure) and returns the JSON paths of
 * declared property names that look like key material. Empty array = clean.
 */
export function findKeyMaterialFields(schema: unknown, path = '$'): string[] {
  if (schema === null || typeof schema !== 'object') return [];
  const found: string[] = [];
  if (Array.isArray(schema)) {
    schema.forEach((item, index) => {
      found.push(...findKeyMaterialFields(item, `${path}[${index}]`));
    });
    return found;
  }
  for (const [key, value] of Object.entries(schema as Record<string, unknown>)) {
    if (key === 'properties' && value !== null && typeof value === 'object') {
      for (const [propertyName, propertySchema] of Object.entries(
        value as Record<string, unknown>,
      )) {
        if (isKeyMaterialName(propertyName)) {
          found.push(`${path}.properties.${propertyName}`);
        }
        found.push(...findKeyMaterialFields(propertySchema, `${path}.properties.${propertyName}`));
      }
      continue;
    }
    if (key === 'required' && Array.isArray(value)) {
      value.forEach((entry) => {
        if (typeof entry === 'string' && isKeyMaterialName(entry)) {
          found.push(`${path}.required[${entry}]`);
        }
      });
      continue;
    }
    found.push(...findKeyMaterialFields(value, `${path}.${key}`));
  }
  return found;
}

/**
 * Runtime variant for live payloads: flags any object key in a decoded JSON
 * value that looks like key material.
 */
export function findKeyMaterialInValue(value: unknown, path = '$'): string[] {
  if (value === null || typeof value !== 'object') return [];
  const found: string[] = [];
  if (Array.isArray(value)) {
    value.forEach((item, index) => {
      found.push(...findKeyMaterialInValue(item, `${path}[${index}]`));
    });
    return found;
  }
  for (const [key, nested] of Object.entries(value as Record<string, unknown>)) {
    if (isKeyMaterialName(key)) found.push(`${path}.${key}`);
    found.push(...findKeyMaterialInValue(nested, `${path}.${key}`));
  }
  return found;
}
