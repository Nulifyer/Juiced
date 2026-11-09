# Credits and licenses

Juiced is maintained as an independent project at
[github.com/Nulifyer/Juiced](https://github.com/Nulifyer/Juiced).

## Source provenance

The native model-serving core is derived from
[Lemonade](https://github.com/lemonade-sdk/lemonade) under Apache 2.0.
The repository retains its [license](https://github.com/Nulifyer/Juiced/blob/main/LICENSE),
[inherited component notices](https://github.com/Nulifyer/Juiced/blob/main/LICENSES/inherited-components.txt),
and source notices.
The console includes presentation components derived from T3 Code under MIT.
Their [license](https://github.com/Nulifyer/Juiced/blob/main/juiced/ui/LICENSE.t3code) and
[source inventory](https://github.com/Nulifyer/Juiced/blob/main/juiced/ui/T3-SOURCES.json) remain in the UI source.

Juiced has its own console, runtime-management behavior, deployment image, and
release process. Upstream packages and services use their own branding and
support channels.

## Runtime and model licenses

Inference engines and optional runtime packages have their own licenses.
The Juiced image retains build and runtime notices under `/opt/juiced/licenses`.
The browser build includes dependency notices in `THIRD_PARTY_NOTICES.txt` and
its `licenses/` assets. The Inter font retains its OFL notice.

Model weights and publisher artifacts have separate terms. Review the model
publisher's license before downloading, serving, or redistributing weights.
Juiced's source license does not replace those terms.

## Compatibility names

The native executable names `lemond` and `lemonade`, `LEMONADE_*` environment
variables, and some MCP tool names retain their original spelling. They are
technical compatibility identifiers. Use the exact names shown in the guides.

Retired upstream website pages, blog posts, and demos remain in Git history.
The working tree contains Juiced's current guides and required source notices.
