# Host suitability observations

`GET /v1/juiced/suitability?model=MODEL` assesses the selected saved profile without
loading or enabling it. Native prefix aliases provide the same operation. The
response separates artifact completeness, declared runtime compatibility, policy,
context and resource suitability. Unknown values remain unknown. No mutation or
resource reservation follows from an observation.

ResourceAdmission owns the resource projection and uses the same checkpoint/options
match and pool-capacity calculation as actual admission. Schema version 1 binds the
admission configuration revision and observation time. It requires explicit load,
resident and request ceilings. Text models also require a positive saved context.
It samples each required physical pool once and reports its source, ceiling,
observed availability, existing reservations and applicable essential headroom.

The new-worker peak is the greater of load bytes and resident plus request plus
state bytes in each physical pool. The latter includes a concurrent state operation
when the profile declares one. Resident weights are included once in that peak.
Other workers retain their own reservations. This scope describes an additional
worker, including a replacement that must reserve resources before retiring its
predecessor. It does not promise extra capacity for a running worker's next request.

| Fit | Meaning |
| --- | --- |
| `fits` | Declared peaks fit configured ceilings, current reservations and sampled headroom |
| `limited_fit` | Declared peaks fit nominal ceilings, but current reservations or sampled headroom prevent this new-worker peak |
| `does_not_fit` | A declared peak exceeds a configured physical ceiling or protected essential headroom |
| `unknown` | Exact settings, context, complete ceilings or observed capacity are unavailable |

Pause, shutdown, configuration transitions and maintenance remain separate
admission blockers. Inspection does not resume them, consume a queue position or
increment admission/rejection counters. Actual loads recheck current policy,
resource and worker identity through their normal owners.

These estimates are operator-declared upper bounds. Their identity currently covers
the declared checkpoint and options, not complete immutable artifact/device identity.
The response therefore reports hardware and recommendation qualification as false.
It is not a measured memory peak, universal memory formula, automatic selection
permission or model-quality receipt. External variants without a matching profile
remain unknown; their download size never becomes their serving-memory estimate.

The model workspace exposes **Host fit → Check saved settings**. Draft edits cancel
and invalidate an earlier observation. A failed check can retry without losing the
draft or changing loading policy. Capacity details remain optional and compact.

The standalone `ResourceSuitabilityTest` covers resource boundaries without a model.
`juiced/tests/test_model_suitability.py` additionally captures a real CPU worker's
settings, unloads it, and checks read-only HTTP assessment, Disabled preservation,
profile mismatch, unknown automatic context and synthetic pressure. Its synthetic
ceilings do not qualify real peak memory. UI fixtures cover stale responses, retry
and desktop/tablet/phone layouts. Full P1 interference and device gates remain separate.
