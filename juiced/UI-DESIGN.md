# Operator console design

This console manages shared model workers. Home Assistant, Pi and other harnesses
consume its endpoints. Service tests are temporary and do not own conversations.

## Foundations

Keep the existing dark T3-based palette, Base UI controls, Lucide icons and
typography. Use the existing color roles and compact controls. Prefer aligned
lists, meaningful state labels and task-specific panes over repeated cards.
Model names and quantization remain readable at normal desktop and phone sizes.
Bundle the unmodified Inter font and its license so host-installed fonts do not
change the layout. Keep source/version/checksums beside the assets.

## Compact console direction

Use short page headers, aligned list rows and a compact summary band. Keep the
sidebar navigation; its footer is one status line with labelled refresh/source
icons. Explanations belong beside the relevant decision, behind a shared Info
popover that works with keyboard and touch. Native titles suit short metadata.
Keep failures, unsaved changes, unavailable capabilities and apply/reload
consequences visible. Do not hide them as help text.
Use one latest-event strip on the dashboard; buffered log warnings are not an
unresolved incident count. Full raw/parsed history belongs in Diagnostics.

Use one level of task tabs instead of repeated accordions: Settings/Advanced in
the model inspector, Snapshots/Storage policy/Model overrides in Cache, and a
protocol selector in Service APIs. Provider grouping remains collapsible because
it organizes a potentially large engine inventory. Show hardware context directly.

The inspected [LM Studio model table](https://lmstudio.ai/docs/app/advanced/per-model)
informs row density, explicit quant/size columns and contextual actions. The
[Docker Desktop workflow](https://docs.docker.com/desktop/use-desktop/container/)
informs the list/detail separation and nearby lifecycle actions. Borrow these
specific qualities within Juiced's existing T3-based visual foundations.

## Workflows and screen purpose

Discover opens a populated native catalog with the bundled Lemonade `hot` picks
ranked first. That metadata is separate from the Hot loading policy. Search
matches name, publisher/checkpoint, runtime and labels. Service, capability,
runtime, weight download size and download status filters compose; reset restores
the default view. Size is a download estimate, not a runtime memory promise.
Model names open the inspector before acquisition; downloading never enables or
loads a model. Hide only exact duplicate `builtin.` aliases in this discovery view.

Hugging Face is a separate chat-GGUF search scope using the existing registry API.
Keep grouped quant choices, show publisher/sort controls for returned results,
retain queries/results after failures and cancel superseded searches. A full model
URL can open file review directly. Preserve the exact quant/projector and proper
registration name. The catalog is the discovery path for other services until
their remote artifact contracts are supported.

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
Cache settings share this draft protection. Validate fields across tabs, reveal
the failing tab and focus the invalid input. Snapshot details use a compact
dialog rather than a full-height model inspector.

Service APIs show routes and connection details for the selected protocol.
Wyoming is a private TCP listener; do not imply HTTPS routes or persistent chat
cache headers apply to it. Show a support-read failure instead of inventing a
working connection. Voice discovery uses the selected eligible speech model.

Image tests inherit saved model dimensions unless the operator chooses a
temporary size override. Changing models or tasks clears generated outputs so
results cannot appear to belong to the next model. Tests never enable a model or
change its saved settings. Manage keeps policy, unload and removal eligibility
in one flow, with explicit typed confirmation before removing weights.

## Model settings contract

The inspector distinguishes saved intent, observed running values and the local
draft. The worker's command is observed data and does not preview an unapplied
change. Save for next load persists intent; Save and reload changes a shared
worker while preserving policy. Busy/Disabled/missing-weight restrictions remain
visible. A conflict retains the draft and provides a way to review current values.
Required variants/projectors stay attached to the exact model registration.
Speech, transcription and image models open their typed native options directly
under Settings. Hide the chat-only context form for these runtimes; show image
dimensions and recognizer settings before generic runtime behavior. Boolean
options such as Include backend defaults remain booleans, even when their native
key ends in `_args`. Only actual string arguments use the argument editor.

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
to the viewport, keyboard-operable and return focus on Escape. The native planner
owns compatible provider/hardware choices; switching profiles preserves the
source registration and weights.

Model Runtime choices expose short native titles on each option and the selected
control. Runtime, Run on and device labels also use Info popovers for keyboard
and touch access. Put routine explanations there; keep incompatibility reasons,
unavailable hardware and experimental precision/feature changes visible.
