# Operator console design

This console manages shared model workers. Home Assistant, Pi and other harnesses
consume its endpoints. Service tests are temporary and do not own conversations.

## Foundations

Keep the existing dark T3-based palette, Base UI controls, Lucide icons and
typography. Use the existing color roles and compact controls. Prefer aligned
lists, meaningful state labels and task-specific panes over repeated cards.
Model names and quantization remain readable at normal desktop and phone sizes.

## Workflows and screen purpose

| Screen | Operator decision | Layout |
| --- | --- | --- |
| Models | What is ready, and which worker needs attention? | Runtime summary, compact model list, persistent selected-model inspector |
| Loading policy | What stays Hot, loads On demand or remains Disabled? | Existing drag lanes and equivalent menus; actual state stays secondary |
| Library | Which weights/quant should I acquire or inspect? | Dense discovery/downloaded list, variants and file review; existing imports/download jobs |
| Model inspector | Which exact variant is this, and what should change? | Sticky identity/actions, Settings/Effective/README/Weights/Manage, explicit save/apply footer |
| Diagnostics | What is consuming resources and how are requests behaving? | Performance, Requests and Logs; existing hardware footer remains global |
| Backends & hardware | Which installed engine/device can run these weights? | Compact expandable hardware context, provider groups with execution variants, explicit lifecycle feedback |
| Test services | Does this selected loaded model behave correctly? | Temporary task-specific inputs/results; selected model is explicit |

On wide screens the selected model does not cover or disable the list. On narrow
screens it becomes the focused main pane with a clear Back to models action.
Changing selection or leaving with edits requires Keep editing or Discard changes.
Do not use browser confirm as the primary navigation/recovery interface.

## Model settings contract

The inspector distinguishes saved intent, observed running values and the local
draft. The worker's command is observed data and does not preview an unapplied
change. Save for next load persists intent; Save and reload changes a shared
worker while preserving policy. Busy/Disabled/missing-weight restrictions remain
visible. A conflict retains the draft and provides a way to review current values.
Required variants/projectors stay attached to the exact model registration.

## Diagnostics contract

Performance graphs use native telemetry. Live rate and last completion are
different observations. Shared GPU memory overlaps host RAM. Missing/stale data
is unavailable rather than zero. Request history is bounded server memory and
contains timing/status/model metadata only, never prompts, output, headers,
credentials, query strings or private session identifiers.

## Acceptance

Exercise model/quant selection, draft edits, save/reload, conflict recovery,
Disabled guards, library/search/import/delete guards, diagnostics, downloads and
temporary tests through the rendered UI. Include desktop, tablet and phone
widths, large text, keyboard focus, long names, empty/loading/error states,
unavailable data, no horizontal body overflow and independently bounded scroll
regions. Native fixtures isolate mutations from production model settings.

## Backend provider hierarchy

Group native recipes once, with execution variants beneath each provider. Keep
actual device selection separate in model settings. Headers retain ready/total
counts, loaded-model counts and installation/update/error state. Prefer attention,
then installed/in-use providers in the list. Expand in-use or attention groups by
default and retain explicit collapse choices locally. Search and status filters
reveal matching variants while preserving provider identity and ordinary collapse
preferences. Filter counts always mean variants. Hardware detection is compact
and expandable, with explicit CPU/GPU/NPU labels and no implied compatibility.

## Hardware selection menus

Run on and accelerator-device choices use the shared Base UI SelectField rather
than OS-rendered menus. Popup backgrounds, selected/highlighted states and
disabled reasons use app tokens even with a light system theme. Menus are bounded
to the viewport, keyboard-operable and return focus on Escape. Runtime recipe is
a fixed registration value; only supported execution/device choices are editable.
