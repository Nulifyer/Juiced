# Native settings ownership and import

Status: native import remains next-batch design only. This record does not establish native settings migration, complete backup, previous-release restore or execution qualification. No import implementation, service-binary builds or daemon runs were performed here. The separate observed configuration redaction gap and its bounded library fix are recorded below.

## Scope and ownership

Add a bounded import of explicitly supplied copied native settings. Preserve native settings authority, profile definitions, unknown keys, and original document bytes. Keep the import under the rewrite configuration publication owner; do not introduce another writable option store.

The initial execution lane, if included, is one explicitly bound llama.cpp profile. Import does not discover executable authority, infer model paths, install runtimes, download weights, convert artifacts, load workers, or enable registrations. Other recipes and unsupported option semantics remain preserved but blocked.

Do not import live production configuration implicitly. Shared PLAN, ledger, and catalog handoff remain outside this record's ownership.

## Committed reference

Pinned reference commit: `db55ada48de0280132547555d249103c075a38e6` from the native main repository, refreshed from the earlier `fb3a33ae` design. Main may advance later; this record remains pinned. The review used committed objects through `git show` and `git grep`, not dirty main-checkout files.

Read exact source with:

```sh
git show db55ada48de0280132547555d249103c075a38e6:juiced/RESOURCE-ADMISSION.md
git show db55ada48de0280132547555d249103c075a38e6:src/cpp/server/model_manager.cpp
git show db55ada48de0280132547555d249103c075a38e6:test/cpp/test_model_settings_publication.cpp
git show db55ada48de0280132547555d249103c075a38e6:test/cpp/test_persistence_boundaries.cpp
```

| Committed source | Evidence |
| --- | --- |
| `juiced/RESOURCE-ADMISSION.md:205–230` | First profile selection transfers active saved-option authority into one settings-version-1 row. Legacy bytes and unknown saved keys survive. Unsupported/incomplete ownership rejects activation. Internal settings stay out of public profiles. Older executables require matching complete settings backups. |
| `src/cpp/server/model_manager.cpp:1466,188,2290` | Startup loads recipe and profile documents, validates owned profile rows, and performs a native legacy built-in-key migration. The importer must not guess this migration without the exact registry or reset malformed input. |
| `src/cpp/server/model_manager.cpp:2376,2404` | `owns_saved_options_locked` validates a complete owned row. `saved_options_locked` selects the authoritative document. Publication writes the selected authority before updating memory. |
| `src/cpp/server/model_manager.cpp:2522` | Saved-option writers resolve the committed recipe, retain unknown keys during merge, erase keys on null, and publish through the current authority. |
| `src/cpp/server/model_manager.cpp:2554,2584` | Profile application, public projection, and selection. Selection publishes definitions, selection, and active saved options together. Public projection removes internal settings. |
| `src/cpp/server/model_manager.cpp:7668` | Cleanup considers profile-owned active saved-option references, including rows whose model registration is gone. |
| `test/cpp/test_model_settings_publication.cpp:71–239` | Controlled before/after replacement interruptions; failed selection/options publication; legacy fallback; unknown keys; clearing overrides; explicit negative context; unsupported/incomplete rows; missing-versus-damaged documents, recognized option rows and redacted parser errors. |
| `test/cpp/test_persistence_boundaries.cpp:521–539,658–679` | Metadata publication boundaries and failed authoritative settings replacement preserving selection/options. |
| `juiced/RESOURCE-ADMISSION.md:625–682` | Required missing/corrupt native settings distinction; fixed legacy cohort and ownership; staging/prepared/active publication; bounds, permissions/free space; modern-store refusal and qualification limits. |
| `src/cpp/server/config_directory_migration.cpp:20,125,137,279` | Exact migration limits/cohort, complete-generation reader gate, transport owner and final active publication. |
| `test/cpp/test_config_dir_migration.cpp` and `juiced/tests/test_config_directory_migration.py` | Native migration interruption/injected-fault and actual-entrypoint acceptance references. Their source is not Rust acceptance. |
| `src/cpp/tray/main.cpp:181` and `juiced/RESOURCE-ADMISSION.md:591–597` | Intentional shutdown during readiness is successful only without startup failure; manager exit never establishes prior-helper termination. |
| `juiced/RESOURCE-ADMISSION.md:228–245` | Matching complete settings backup is required for older-executable restore. A last-committed state checkpoint is not a consistent multi-writer volume backup or complete model/runtime revision recovery. |

## Native authority rules to retain

Native optional documents have a distinct absence contract: missing registration, recipe-option or execution-profile files use defaults. Present unreadable/nonregular files, malformed JSON and non-object top-level values reject activation with `model_settings_recovery_required`. Recognized canonical saved-option rows and legacy built-in rows must be objects before migration, reads or edits. Preserve unknown bare metadata without assigning model semantics. Do not reset damaged storage to an empty object, publish an empty replacement, or disclose parser input in errors or native application logs. Use static recovery reasons and known file roles.

A model without profile-owned settings reads saved options from its `recipe_options.json` row. A legacy execution-profile row without `settings` also retains that authority.

A profile-owned row must contain:

```json
{
  "selected": "profile-id",
  "profiles": {
    "profile-id": {"recipe": "native-recipe"}
  },
  "settings": {"version": 1, "saved_options": {}}
}
```

The selected identifier and recipe must be nonempty strings; the selected profile must exist. The version must be integer 1, and saved options must be an object. Unsupported versions or incomplete owned rows reject activation without resetting documents or falling back to stale legacy values.

First successful selection transfers authority atomically without rewriting the legacy file. Later merged edits preserve unknown saved keys; null removes an override rather than writing a null value. Clearing an override restores the registered/default resolution, which may be nondefault globally. Profile selection and active saved options must never publish as independent versions.

Preserve complete profile definitions, unknown row/option fields, stale canonical IDs, and unregistered rows. Do not silently prefix unknown bare IDs with `builtin.`. Keep internal settings out of public profile projections.

## Current rewrite seams and blockers

- `service/src/config.rs`: `Config2`, `ModelSpec`, strict wire decoding, canonical bounded `read_bound`, validation, relative-path resolution, and bounded publication. Canonical compact JSON plus its newline is counted against the startup bound without constructing a serialized buffer; readable legacy input is not rejected merely because its canonical form is larger. Schema 1 rewrites are decoded into schema 2 in memory. Native documents are a different format.
- `service/src/execution_profile.rs`: strict profile schema 1 with one execution profile per model, explicit adapter/implementation, command, arguments, typed options, and unresolved placement.
- `service/src/profiles.rs`: `LaunchOptions` accepts a bounded unsigned context, threads, GPU layers, and physical slots. Native `ctx_size: -1` cannot be represented faithfully as a positive typed context or silently converted to absent.
- `service/src/supervisor.rs`: configuration writer, revision checks, registration, saved-option publication, worker replacement, and managed artifact references. This remains the publication owner.
- `service/src/catalog.rs`: documented native recipe/backend identity resolution. It does not authorize an executable, supply complete launch settings, qualify a device, or establish package support.
- `service/src/storage.rs`: retained directory reads, immutable per-manager `LiveRoots`, typed root-unavailable errors, and guarded atomic JSON replacement. These are point checks, not a complete writer fence.

Direct serde import into current strict models would reject or discard required state. A second mutable map alongside independently writable `ExecutionProfile.options` would create competing authority. Neither approach satisfies the contract.

## Proposed implementation boundary

### Native settings owner

Add `service/src/native_settings.rs` for the concrete native document format and authority rules. It should own bounded parsing, authority resolution, merge/clear semantics, profile selection, and redacted import findings. Preserve full objects for recognized native rows and complete JSON values for unknown bare recipe-option metadata; do not assign those unknown values model semantics. Reject duplicate keys. Keep original bytes separately as immutable provenance/rollback material; those bytes are not another live writable option source.

Keep pure authority transformation separate from filesystem publication. No generic migration registry, repository layer, scheduler, callback framework, or arbitrary filesystem API is needed.

### Two concrete publication arrangements

Both arrangements keep the existing 16-MiB rewrite configuration boundary, support the native 64-MiB cohort through separately bounded documents, preserve original bytes and unknown keys, and have one atomic activation point. General model artifacts are unsuitable for private metadata: current artifact creation follows umask, its inventory treats blobs as model candidates, and its authority does not promise native configuration confidentiality. Reusing its public owner and chmodding afterward would create an exposure window. Reuse low-level bounded streaming/hash practices only.

| Arrangement | Commit owner and stored authority | Cost and risk |
| --- | --- | --- |
| A: Config3 descriptor plus private generation blobs, recommended | The ordinary canonical `config.json` contains existing local profiles and a small native generation descriptor. That descriptor names private completed document blobs. One Config3 replacement activates both registration bindings and their complete native settings generation. There is no native-store head. | Keeps current configuration publication and readable local profiles. Adds one concrete private metadata owner and tagged imported-model bindings. Needs secure nested creation and retained document leases. |
| B: private root commit manifest plus immutable config and native snapshots | A new small root manifest selects both a rewrite-config snapshot and a native-settings snapshot; both reference private document blobs. Every writer publishes through the root manifest. Existing local profile options remain authoritative only inside the selected config snapshot. | Also prevents partial activation, but replaces the bootstrap/read/write path for every existing configuration mutation, adds another snapshot per ordinary edit and makes the operator's configuration path an indirect manifest. Its additional composition owner is unnecessary for this selected import. |

Choose A. A direct mutable sidecar plus separately saved Config3 binding is not a third viable option: interruption could combine a new binding with old options or expose an uncommitted selection. Embedding raw source text or all native documents inside Config3 is also rejected because the cohort can exceed its startup limit.

### Selected Config3 authority

Config3 retains non-imported models with their existing `ExecutionProfile1` authority. Imported models use a mutually exclusive native-settings binding: native canonical model ID plus explicit runtime/artifact binding. They cannot contain writable `execution_profile.options` or another saved selection. The selected profile and saved options exist only in the referenced native document row. Reject mixed representations during decoding.

The inline native descriptor is capped at 64 KiB and names a fixed role set: six legacy documents and conditional aliases, with explicit absence, byte sizes, SHA256 values, source permissions, store identity and generation identity. It references both the original import generation and the current generation. Original bytes are immutable provenance/rollback inputs; they are never replayed as another live options authority. A role blob is at most 16 MiB and a logical generation at most 64 MiB, including conditional aliases. Raw initial copies preserve exact bytes rather than re-encoding a full-boundary source.

The descriptor identifies one complete target metadata generation; it does not certify that an externally produced source backup captured all appliance writers consistently. Keep generation/storage identity separate from semantic execution identity. A worker/cache fingerprint binds its selected model/profile/options/runtime inputs, not unrelated private configuration, helper records, another model's edits or a global generation counter.

Unchanged document references are reused. An edit writes only the changed document, not another full 64-MiB cohort. Preserve unknown JSON values through raw-preserving parsing/writing rather than coercing them through typed launch fields or normalizing unknown numbers. Typed launch views are derived and generation-bound in memory. For imported models, existing option APIs must dispatch through the native owner; they cannot overwrite the derived view. Existing local-model writes remain unchanged.

### Private store and retained authority

The fixed private root is beneath the claimed configuration root, outside the public artifact/model inventory. No descriptor supplies an arbitrary destination path. Blob and transaction names are generated opaque IDs; role-to-source filenames are fixed by the owner. A store identity file has no current-generation pointer.

Create the private root and each generation directory owner-only from the first creation, using retained-directory-relative operations. On POSIX, create directories with mode 0700 and manifests/intents with mode 0600; do not create a public directory and chmod it afterward. Preserve source file permission information in each immutable descriptor. Rust opaque blobs, intents and identity metadata use0600 from exclusive creation, under0700 directories; source modes are not applied to opaque blobs. This keeps every blob readable by its private owner even when the original file relied on group permissions. A later native-format export or native metadata transport must restore its source permission contract before writing role-file contents; that separate operation is not implemented here. Refuse unexpectedly permissive/foreign existing private roots rather than silently repairing them. Windows needs creation-time private ACLs and native evidence; until implemented, reject that import lane explicitly instead of relying on Unix mode bits.

Initial import is offline: acquire the existing configuration owner, preclaim all target roots, including the private store, and freeze the shared `LiveRoots` before constructor or payload writes. Existing Config2 managers do not gain a mutable scope during operation. Config3 startup opens its bound private root as existing-only; a missing active root must not be recreated. Later generations create bounded nested children under the already claimed private root, without adding per-generation directory claims to the immutable scope. Retain child directory/file identities, refuse symlinks/reparse points and verify the shared scope before effects. Existing path-based JSON helpers alone do not establish this anchored creation guarantee.

Completed blobs are never edited in place by this owner. This is an immutable publication rule, not proof that an external filesystem writer cannot change bytes. Retained handles, hashes and freshness checks still apply. Worker generations and readers retain the exact selected document leases. Drops restore counters without throwing or deleting paths; worker termination remains possible after storage refusal.

### Publication and interruption order

1. Inspect an explicitly authorized copied source through a read-only retained directory authority. Opening it must not create a writer control file. Parse/hash within the native bounds and distinguish absent optional files from damaged present storage. Capture and recheck source/ancestor/file identities. Do not infer a writer fence from successful observation.
2. Resolve only explicit imported-model runtime bindings, reject identity collisions and unsupported active semantics, and build the small candidate Config3 descriptor. Check its canonical 16-MiB limit before effects. Preserve cheap not-found/revision/busy errors. Reserve actual metadata storage and apply the native free-space requirements: twice the cohort plus 1 MiB at configuration, and relocated alias bytes plus 1 MiB at cache. Do not count unknown capacity as free.
3. Persist a private versioned transaction intent before creating payload staging. It binds old config generation/digest, candidate generation, source identities, role hashes/sizes/permissions and reserved bytes. All deferred work retains the same live-root scope. A failed/uncertain attempt retains its charge and candidate data; it is not a successful rollback or cleanup.
4. Copy new blobs through exclusive, anchored, private creation; stream and validate size/hash; sync the completed files. Finish a complete prepared descriptor only after source and target revalidation. Reused blobs must also match their retained identity, hash, size and permission contract. No reader discovers a generation by scanning for the newest directory.
5. Reacquire the existing publication owner and recheck config/native generation CAS, registration, model revision, policy, unload and active-work guards after preparation. Preparation must not hold admission locks or the configuration read mutex across the bounded blob copy; a changed cohort rejects rather than overwriting another model's intervening edit. Replace canonical Config3 once through the existing serialized publication owner. This replacement alone makes a prepared generation active. Selection/options and explicit registration bindings cannot activate independently. The transaction journal may later record observation of activation, but no second active pointer or mandatory post-commit flag exists. This is a distinct Rust format; do not masquerade as the native migration marker while changing its semantics.
6. With `committed=false`, keep the last observed active configuration and retain uncertain candidate ownership. With `committed=true`, publish the new in-memory view and report durability uncertainty; never pretend the old configuration remains authoritative. Retain both generations where the outcome is uncertain. Restart follows the actual Config3 descriptor, validates its entire referenced closure, and accepts that complete old or new generation; missing/conflicting active data requires recovery. Never guess from intent state or fall back to legacy options.
7. Keep original, active, worker-held and pending blobs referenced. Initial scope has no automatic generation pruning or generic repair. Unresolved candidates remain charged, and physical limits may block another edit. Later reclamation needs this owner's exact references, confirmed writer settlement and root checks. It must not reinterpret native helper records as stopped. Post-active edits derive from the active target; the retained source is not read back over them.

The six-file native transport and native-to-Rust semantic adoption remain distinct. Native jobs/helper/config/alias data is preserved with its schema owner; parsing an object does not qualify replay, helper termination, authentication-epoch migration or full appliance activation. A source modern `juiced/` store and unrelated destination state remain refusal cases. Full backup, power-loss durability and prior-manager recovery remain open.

### Concrete interfaces and implementation ownership

- `native_settings.rs` owns bounded native document parsing, row authority, merge/selection edits, private prepared generations and redacted findings. A small interface is `inspect_source(read_only_source, limits)`, `open(binding, private_store, deadline)` and `prepare_edit(edit, expected_model_revision, deadline) -> PreparedNativeGeneration`. The prepared result supplies a descriptor and retained view; it never commits configuration itself.
- `config.rs` owns Config3 structural decoding and size checks. Its model wire is explicitly local or imported; local models keep the current profile owner. It does not parse a 64-MiB blob into the main configuration object.
- `supervisor.rs` remains the only configuration writer and registry publication owner. It claims/fixes roots at bootstrap, checks revisions and idle state, derives imported execution plans from the native owner, then commits one descriptor. Same-model edits obey existing unload/active guards; unrelated model settings do not invalidate another model's semantic identity.
- `storage.rs` needs narrowly scoped existing-only read authority and anchored private-child creation/read primitives. These remain managed operations, not arbitrary filesystem administration. No OS I/O occurs under admission mutexes. Native Windows ACL support is a separate selected platform batch.
- Native/filesystem tests own corruption, permissions, exact-boundary and interruption seams. The Rust process harness owns actual activation/restart/CLI behavior after fresh binaries are authorized. Existing generic `ArtifactStore` is not the metadata storage authority.

Before implementation, confirm the exact descriptor/role schema, physical byte/count reservation limits and Windows refusal code. Do not add a second commit head or expose raw private rows while settling those details.

### Explicit execution bindings

Activation needs explicit model/artifact identity, executable or verified bundle, arguments, reservation, operations, and physical slots. Catalog identity mapping may explain a native recipe but cannot supply those authorities.

Unknown active execution keys and unsupported sentinel semantics remain blockers until the selected adapter handles them. Do not silently drop them or claim equivalent execution. Unselected definitions remain preserved. Native `pinned` does not automatically become Rust Hot policy; imported registrations start Disabled.

Unknown or unregistered file references do not grant cleanup authority. Preserve them and block any cleanup decision that depends on interpreting unbound references. Managed-artifact references must use the existing reference/lease owner before configuration publication.

## Required legacy cohort transport

Native cohort relocation and native-to-Rust schema import are separate operations. Native ConfigFile owns transport; existing native schema owners interpret its files. The Rust import owner must not treat a transported native `config.json` as rewrite Config2 or duplicate the model registry. A successful copied-cohort transport does not establish Rust activation or full-generation restore.

The fixed legacy configuration cohort is `config.json`, `jobs.json`, `recipe_options.json`, `user_models.json`, `execution_profiles.json` and `native_helpers.json`. Include `aliases.json` only when the cache root also moves. A configuration-only split leaves aliases with their unchanged cache authority. Runtime binaries, model weights and other payloads remain at source; do not recreate the retired recursive cache-tree mover.

The `.juiced-configuration-migration.json` version-1 intent binds canonical source/config/cache roots, file roles, sizes, permissions and SHA256 hashes. States are `staging`, `prepared` and `active`. Persist intent before creating payload staging; retain a private `.juiced-configuration-generation`; finish complete staging before target publication; revalidate source and target before active publication. Every participating reader must reject incomplete or unsupported generations. Reconstruct missing staging only from unchanged source; reject conflicts and unknown generation contents. Preserve source bytes. Once active, ordinary schema owners write the target; restart never replays retained source over later edits. Missing active metadata blocks activation.

Required bounds are 16 MiB per JSON file, 64 MiB combined, a 64 KiB manifest, depth 128, 100000 parser events and a 30-second cooperative deadline between phases. That is not a hard deadline for blocked filesystem calls. Configuration free space must cover twice the cohort plus 1 MiB. A separately relocated alias needs its own bytes plus 1 MiB at its cache destination. Retain source file permissions, an owner-only generation directory and an owner-readable/writable manifest; Windows permission behavior needs actual OS evidence.

A source `juiced/` store or unrelated existing destination state blocks this lane before publication. Modern snapshot bindings, imports, credential epochs and unresolved writers require separate validated recovery through their owners. Preserve the source, destination and generation after `configuration_migration_recovery_required`; do not delete markers or merge isolated files. This is not permission to repair a store or create a replacement modern state directory.

## Source reads, target publication, and limits

Use explicitly supplied copied source documents. The source reader must not acquire a writer lease that creates a control file. Reuse the retained-directory-relative read implementation through a narrow read-only authority if necessary, rather than adding another verify-then-path-read parser.

Revalidate source root, ancestor, and file identities after reading all documents. Record absence explicitly. Missing optional documents and malformed present documents are different results. Reject symlinks, special files, traversal, excessive bytes/records/keys, duplicate keys, and changes during observation.

For the target, reuse `DirectoryLease::read_bytes` for canonical configuration input and `LiveRoots` for publication. Claim all target roots before writes. Preserve typed `storage_root_unavailable`; ordinary I/O, validation, conflicts, and cancellation retain their own errors.

Enforce the selected cohort limits and separate limits on the fully serialized target rewrite configuration. Existing target reads have a 16-MiB bound; an import must not create a configuration that its next startup cannot read. Reject overbound imports before publication; never truncate unknown fields or backup bytes. The selected private-blob arrangement supports the full 64-MiB source cohort without embedding it in Config3. The main configuration still rejects a descriptor/model inventory that exceeds 16 MiB. Document and aggregate limits also apply to later edits; overbound candidates leave the previous active generation intact.

## Backup authority and open limits

Two JSON files, stable hashes, a retained handle, or a directory lease do not establish a consistent full generation. Source revalidation is a non-atomic observation. It does not prove that a native writer was stopped or that every writer joined a maintenance fence.

Require explicitly supplied copied-backup provenance and state exactly what has been validated. Do not advertise `backup_complete` merely because recipe/profile settings parse. A complete native restore also needs the matching executable/settings generation and all referenced dependencies, including relevant registration/config/state files, weights, runtime bundles, and snapshots. Credentials remain governed by their existing process/environment authority; do not serialize them into an import receipt.

An immutable copy of original settings bytes can preserve the pre-import settings version. It does not provide a current native-format export after subsequent Rust edits, a full-volume backup, last-complete-generation recovery, or rollback of the whole appliance. Copying an older `recipe_options.json` over profile-owned settings cannot restore its active options.

Full-generation backup, previous-release restore, native snapshot-format migration, foreign-manager ownership compatibility, and Windows/macOS runtime qualification remain separate gates. The first import batch must state these limits even when its selected settings fixtures pass.

## Acceptance sequence

1. Port the committed native authority fixtures: no profile; legacy profile without settings; first transfer; profile-owned edits; registered/alternate selection; unknown keys; null clears; registered nondefault fallback; explicit `-1`; malformed and unsupported owned rows.
2. Prove source bytes remain exact and public projections omit internal settings. Preserve unknown/stale IDs and unknown nested option values. Reject duplicates, invalid encodings/shapes, bounds, symlinks, special files, alias retargeting, and observed source replacement without rewriting source data.
3. Test that only one writable authority exists per imported model. Typed edits and profile selection must resolve through that owner; stale recipe/revision validation must fail before publication.
4. Interrupt target publication before and after atomic replacement. Restart must observe one complete selection/options authority. Failed publication retains the previous committed view; uncertain outcomes retain ownership and references.
5. Verify old rewrite config decoding remains unchanged. Import an exact-boundary 64-MiB cohort through private blobs while keeping the Config3 descriptor/configuration below its own limits; reject a 64-MiB-plus-one cohort and a 16-MiB-plus-one document without target activation. No whole-cohort serialized buffer is needed. An overbound descriptor/model inventory still rejects before commit.
6. Test one explicitly mapped positive-context llama.cpp fixture through supplied bindings and the existing native process harness. Unsupported recipes, unknown active execution keys, automatic sentinels, missing bindings, and inferred placement remain blocked. Assertions concern identity, configuration, transport, and lifecycle rather than model wording.
7. Add copied-cohort fixtures for the six fixed roles, cache-move aliases versus configuration-only split, permission retention and both free-space requirements. Verify intent precedes staging, complete staging precedes target publication, interrupted stages resume only from matching source, all readers reject incomplete/unsupported generations, and active startup preserves later target edits. Missing active files, source changes, target conflicts and unknown generation contents must reject without destructive repair.
8. Exercise per-file/cohort/manifest/depth/event/deadline limits with preserved source/target trees. Deadline checks are cooperative; do not claim blocked-I/O preemption. Verify source `juiced/` and unrelated destination-state refusal before publication, and that runtime/weight payloads never move. No successful modern-store relocation follows from these rejection tests.
9. Keep actual Windows embedded-readiness acceptance separate: intentional shutdown succeeds after clean owned completion; timeout/bind/listen/worker failures remain failures. Preserve native-helper journal ownership across successful manager exit and check both stderr and application logs for parser-input disclosure. A zero exit code is not helper cleanup acknowledgement.
10. Restore the matching copied settings backup with the frozen native executable and compare selection/effective/saved views. Scope the result to that selected settings fixture. Complete generation restore remains open until all referenced dependencies and writers are covered.

11. Prove privacy from creation: restrictive root/generation/manifest modes despite permissive umask, no public-mode staging window, source permission retention and no blobs in model/artifact inventory. Replace parent/child directories between checkpoints; reject before foreign writes/unlinks and retain uncertain charges. Windows must refuse the lane until its private ACL implementation and native acceptance exist.
12. Crash before intent, during copy, after preparation, immediately before Config3 replacement, after replacement but before acknowledgement, and with missing/changed active blobs. Restart selects only the descriptor-bound complete generation. Test both committed outcomes, original/worker/pending reference retention, no source replay, no old-head fallback and no independent native head.
13. Save native options through the imported binding and prove no writable typed-options copy exists. Clear an override back to the registered baseline, preserve unknown values, and keep unrelated model fingerprints stable. Existing local Config2 profiles retain their authority after Config3 adoption.

No implementation acceptance is claimed by this record. The checks above are required future work.

The frozen performance baseline f6336ea4 is identified by binary/resource hashes, not a proven source commit. Do not assume it implements fb3a33ae settings ownership or use it as the matching restore oracle without establishing that behavior. A settings-restore acceptance run needs a native reference whose implementation and copied settings cohort are explicitly matched; preserve the concurrent main checkout.

## Startup configuration redaction audit

The native `db55ada4` damaged-metadata contract is an error boundary, not permission to dump input for diagnosis. At the probed library revision, `Config::read_bound` and `Config::read` shared `Config::decode`, which propagated both serde parse errors and dynamic semantic-validation strings. `ConfigurationBinding::read` and `Manager::open` propagate that error to `lemond` main's `Result<ExitCode, Box<dyn Error>>`. The standard [Result termination implementation](https://doc.rust-lang.org/src/std/process.rs.html#2719-2726) formats failures with Debug. A static Display alone would not fix the boundary.

An authorized standalone probe was compiled against the existing cached library `target/debug/deps/libjuiced_service-cc5f3ec78cc91ed2.rlib`, SHA256 `fbd9f41ce6d0f057c206d3b9b022391b0b1e9037f9ed551bbf947def6676d7c7`. It called the actual public `Config::read` on temporary synthetic files; no service binary was rebuilt or daemon launched. Probe exit was 0, all input files remained unchanged, and all temporary source/binary/fixture files were removed.

| Synthetic fixture | Observed error formatting |
| --- | --- |
| Top-level JSON string containing `JUICED_SYNTHETIC_CONFIG_REDACTION_MARKER` | Both Display and Debug contain the marker; invalid type/string/ConfigWire, line 1 column 42. |
| Otherwise valid schema-1 config with that marker as top-level `artifact_source_policy` | Both contain the marker; unknown variant, line 1 column 68. |
| Otherwise valid config with that marker as model ID and zero resident bytes | Both contain the marker in the semantic reservation error. |

All three observed source chains were empty. The probe's separate serde-error downcast crossed independently selected cached crate variants and is not classification evidence. The probe proves formatting disclosure at Config::read; actual daemon-stderr propagation is source-derived, not a daemon run. Untagged nested model errors can hide their inner errors, so do not generalize every malformed field into an observed disclosure.

Pre-fix rewrite source anchors at review: `config.rs:355–367,421–435`, `supervisor.rs:1689,1704–1707`, and `main.rs:10,103–104`. Serde-core 1.0.229 `de/mod.rs:213,252–262,410` explains the raw invalid-type/unknown-variant formatting. Existing API `extract.rs` uses static invalid-request/query messages. Local `lemonade` JSON input paths at `bin/lemonade.rs:705–759` also replace parse details with static schema messages. Its selected-credential output filter is not a general private-metadata redactor. Existing CLI process coverage checks credential filtering and malformed responses, not persisted-config marker disclosure.

The selected fix is a concrete typed configuration-load boundary. Preserve required-file absence, ordinary I/O and typed root-unavailability distinctions. Map malformed/schema-invalid/semantically-invalid configuration into static recovery codes with optional safe stage, serde category, line and column. Native optional-document absence remains an explicit Absent result; it must not turn damaged present storage into defaults. Use the native `model_settings_recovery_required` only for the corresponding native model-document owner rather than pretending rewrite Config2 is that format.

For parser/validation refusal, discard raw serde/validation strings and their source chain. Store only safe typed fields; implement both Display and Debug without input values. Do not derive Debug over a hidden raw error. Semantic validation messages can contain model IDs, so scrubbing parser errors alone is insufficient. Keep API status/code/revision behavior and CLI schema/help guidance; do not disclose unexpected field names, enum values, model IDs or document contents. `lemond` also currently echoes an unknown command-line token; a static unknown-argument/help message avoids that separate direct-argv echo without hiding useful supported flag names.

The library fix now adds pure native tests for these three markers through public read and anchored read, checking Display, Debug and every source-chain item. Require static recovery classification and safe parse locations, with unchanged files. Add actual `lemond` startup cases only with authorized fresh binaries: nonzero exit, no listening/worker side effects, and marker absent from stdout, stderr and application logs. Preserve missing/I/O/root error distinctions, and verify API and CLI local-file parser rejection remains structured without values. The rewrite load fix is implemented in `config.rs`: public `ConfigRecoveryError` carries only static code/stage/category and optional line/column; custom Display and Debug omit input values and source chains. Existing read signatures remain unchanged, with ordinary missing/I/O/root errors preserved. Four new test groups cover the six marker/read-path combinations, safe syntax/EOF coordinates, missing/structural I/O, and root replacement. `cargo test -p juiced-service --lib config::tests --features test-fixture --offline` passed 22 tests; scoped service library/tests Clippy with warnings denied and formatting checks passed. Serialized-size tests remain green. No supervisor/main integration was required. Actual daemon startup acceptance remains the process-test owner's next check after fresh binaries; no native-format import or recovery qualification follows from this fix.

## Implementation boundary update

Root source-reader and private metadata primitives now exist in service/src/storage.rs and storage/metadata.rs; source bytes are shared with native_settings.rs through Arc<Vec<u8>> without another full-buffer copy. SourceCohort reads all six fixed roles plus explicitly included aliases, rejects modern/migration-marker source generations, preserves absence/bytes/source permission metadata, checks64MiB aggregate, and revalidates retained observations under cancellation/30scooperative bounds. This is inspection, not activation or proof of copied-backup consistency.

The selected private claim predicate is lifetime-bound: all LiveRoots checks include private root/control ownership, permissions and anchored control identity. Generic live leases cannot be silently upgraded to private leases. Blob creation is anchored, exclusive0600 from firstopen, bounded and scoped before each64KiB write; failed or uncertain partial files remain. Immutable descriptors use fixed roles/validated IDs and no independent native head. The generation preparer and Config3 activation are integrated. The explicit offline copied-cohort CLI is implemented; no import HTTP endpoint is published.

## Integrated selected subset

Config3 activates one fixed original/current descriptor. Profile2 retains its exact generation and derives options through the native settings resolver. The private owner uses a semantic store lock, intent-before-payload writes, retained exact handles, strict active hashes and conservative failed-candidate charges. Limits are256MiB physical,256files and32intents. No pruning, complete backup, helper replay or Windows private-store execution is qualified. Current executable bindings select registered user.* CPU llama.cpp chat GGUF with one slot and explicit managed artifact/runtime bundle. Context edit/clear is the only editable native option.

Actual-daemon import/restart/cache/options/CAS and related process checks passed67cases before the public-alias change. The strengthened real20-file package test passed one native pipeline, then exposed a private response.model path. The current alias binding and exact public-name/SSE checks await the fresh combined gate. Native library398/3ignored passed after the separate power-probe correction. The release ledger owns current acceptance and next action.
