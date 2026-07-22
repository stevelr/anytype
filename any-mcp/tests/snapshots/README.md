# Catalog snapshots

These reviewed fixtures lock the complete compact/standard and
read-write/read-only `tools/list` catalogs, including every description, input
schema, output schema, and annotation. `optional-toolsets.snap` separately
locks representative read-write/read-only composition for two complete
test-only registries, including their tools, resources, templates, and common
status contract. The ordinary test suite only compares against these fixtures;
it never accepts changes automatically.

The four catalog fixtures use the `.snap` extension (JSON content) because the
tests compare them byte-for-byte against `serde_json::to_string_pretty` output;
a `.json` extension would let repo-wide auto-formatting (`gate fmt`/dprint)
rewrite them and break that exact comparison. Never rename them back to `.json`
and never hand-format them — only the regeneration test below may write them.

After intentionally changing a tool contract, regenerate all four fixtures with:

```console
cargo test -p any-mcp server::tests::write_catalog_snapshots -- --ignored --exact
```

Review the complete fixture diff, confirm the recursive schema-bound and exact
annotation tests pass, then run the ordinary `any-mcp` test suite. Never make
snapshot acceptance conditional on an environment variable in CI.

After intentionally changing the optional registry foundation, regenerate its
catalog and token fixtures with:

```console
cargo test -p any-mcp server::optional_registry::write_optional_snapshots -- --ignored --exact
```

Review both complete diffs before accepting them. The registries are test-only;
production accepts no optional selector until a complete domain registry lands.

## Token budget

`token-budget.json` is the reviewed token-count baseline for the current
production catalogs and `result-representatives.json`. The test constructs the
all four profile/read-only catalogs, recursively sorts JSON object keys while
preserving array order, serializes compact UTF-8 JSON with `serde_json`, and
counts tokens with `o200k_base` from `tiktoken-rs` 0.12.0. This fixed pipeline
is identical on Linux, macOS, and Windows; it does not use a host-installed
Python package, network service, byte/token estimate, or platform newline.

The internal compatibility-policy floor is a 200,000-token model context. The
complete default compact `tools/list` result is 9,658 tokens, leaving 342
tokens below the strict 10,000-token ceiling (5% of that support floor).
Compact read-only is 8,369 tokens; explicit standard and standard read-only are
36,135 and 28,880. The schema-valid representative `object_search` and
`object_get` results are 421 and 316 tokens. The compact catalog's 2% growth
boundary is 9,852 tokens and retains 148 tokens below the ceiling.
Flat list filters add 13,226 tokens to both standard catalogs over the preceding
22,909/15,654 baselines: 57.73% read-write and 84.49% read-only. The resulting
catalogs consume 18.068% and 14.440% of the 200,000-token floor. This material
growth is accepted because each independently valid MCP tool input schema must
embed the exhaustive closed shared-leaf union; MCP has no cross-tool schema
registry. The compact catalogs contain none of these standard-only tools and
retain a concurrent 11-token reduction rather than stale allowance.
The earlier 14-tool baseline was 22,496 tokens; a rejected global annotation
and definition-name compaction reached about 14,612 tokens, still exceeded the
ceiling, and weakened every tool instead of selecting a coherent workflow.

Every count is compared exactly with the reviewed baseline, so any increase or
decrease fails until a reviewer inspects the complete profile catalog/result fixture
diff and updates `token-budget.json` deliberately. Growth of 2% or more (194
tokens at the compact baseline) is material and its rationale must be recorded
in the change review. After intentional contract changes, regenerate the
catalogs with the command above, review both catalog diffs and the
representative results, then run:

```console
cargo test -p any-mcp server::tests::profile_catalogs_and_representative_results_match_reviewed_token_budget -- --exact
```

The failure prints the exact current counts. Update the baseline only after
confirming the 5% ceiling and the extra 2% material-growth boundary both retain
headroom. Reductions should lower the baseline rather than preserve stale
allowance.

`optional-toolsets-token-budget.json` uses the same canonical serialization and
`o200k_base` pipeline. The common status contract is 260 tokens under its
500-token ceiling. Against the unchanged 9,658-token compact Phase 1 catalog,
the representative alpha, beta, gamma, and all-enabled test catalogs are
10,157, 10,032, 10,039, and 10,400 tokens; alpha read-only is 8,743 tokens and
mutation-only gamma read-only is 8,625. Each registry owns an explicit
incremental ceiling, and the all-enabled assertion composes those ceilings with
the one common-status allowance rather than assigning that cost to every
registry. The fixture also locks the canonical selected sets, base catalog
SHA-256, each optional tool's standalone contribution, and the 29-token maximum
representative result for all three test domains.
