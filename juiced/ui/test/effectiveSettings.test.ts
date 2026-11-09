import { expect, test } from 'bun:test';
import { effectiveRows, observedCapacity, discoveryContextLabel } from '../src/components/effectiveSettings';
import { backendReady, backendUpdate } from '../src/components/backendState';
test('observed capacity rejects unknown, automatic and malformed values without dividing requested settings', () => {
  for (const value of [-1, 0, Infinity, NaN, 2.5, null]) expect(observedCapacity({ per_request_tokens: value })).toBeNull();
  expect(observedCapacity({ per_request_tokens: 128, slots: 2 })).toEqual({ perSlot: 128, slots: 2 });
  expect(observedCapacity({ per_request_tokens: 128, slots: -1 })).toEqual({ perSlot: 128, slots: null });
  expect(observedCapacity()).toBeNull();
});
test('discovery context labels preserve observation and configuration provenance', () => {
  const context = { declared_tokens: 128, configured_tokens: 2048, applied_tokens: 1024, observed_per_request_tokens: 128, observed_slots: 2, advertised_tokens: 128, advertised_source: 'runtime_observation' };
  expect(discoveryContextLabel({ context })).toBe('128 tokens per chat');
  expect(discoveryContextLabel({ context: { ...context, advertised_tokens: 2048, advertised_source: 'configured_options' } })).toBe('2,048 configured context tokens');
  expect(discoveryContextLabel({ context: { ...context, advertised_source: 'model_metadata' } })).toBe('128 declared context tokens');
  expect(discoveryContextLabel({ context_length: 4096 })).toBe('4,096 reported context tokens');
  expect(discoveryContextLabel({ context_length: -1 })).toBeNull();
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
