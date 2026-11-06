import { expect, test } from 'bun:test';
import { effectiveRows, slotCapacity } from '../src/components/effectiveSettings';
import { backendReady, backendUpdate } from '../src/components/backendState';
test('automatic, missing and malformed context never claim a capacity', () => {
  for (const ctx_size of [-1, 0, Infinity, NaN, 2.5]) expect(slotCapacity({ ctx_size })).toBeNull();
  expect(slotCapacity({ ctx_size: 8192, llamacpp_args: '--parallel 2' })).toEqual({ total: 8192, slots: 2, perSlot: 4096 });
  expect(slotCapacity({ ctx_size: 8192, llamacpp_args: '--parallel 0' })).toBeNull();
});
test('removing an override restores backend default without changing saved or running', () => {
  const rows = effectiveRows({ model: 'fixture', recipe: 'llamacpp', defaults: { ctx_size: 4096 }, saved: { ctx_size: 8192 }, effective: { ctx_size: 8192 } }, { ctx_size: null }, { ctx_size: 2048 });
  expect(rows[0]).toMatchObject({ saved: 8192, running: 2048, draft: 4096, source: 'Saved override', changed: true });
});
test('optional backend update stays usable; required update does not', () => {
  expect(backendReady('update_available')).toBe(true);
  expect(backendReady('update_required')).toBe(false);
  expect(backendUpdate('update_required')).toBe(true);
  expect(backendReady('unrecognized')).toBe(false);
});
