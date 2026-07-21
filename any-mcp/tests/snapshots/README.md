# Catalog snapshots

These reviewed fixtures lock the complete compact/standard and read-write/read-only `tools/list`
catalogs, including every description, input schema, output schema, and
annotation. The ordinary test suite only compares against them; it never
accepts changes automatically.

After intentionally changing a tool contract, regenerate all four fixtures with:

```console
cargo test -p any-mcp server::tests::write_catalog_snapshots -- --ignored --exact
```

Review the complete fixture diff, confirm the recursive schema-bound and exact
annotation tests pass, then run the ordinary `any-mcp` test suite. Never make
snapshot acceptance conditional on an environment variable in CI.

## Token budget

`token-budget.json` is the reviewed token-count baseline for the current
production catalogs and `result-representatives.json`. The test constructs the
all four profile/read-only catalogs, recursively sorts JSON object keys while
preserving array order, serializes compact UTF-8 JSON with `serde_json`, and
counts tokens with `o200k_base` from `tiktoken-rs` 0.12.0. This fixed pipeline
is identical on Linux, macOS, and Windows; it does not use a host-installed
Python package, network service, byte/token estimate, or platform newline.

The internal compatibility-policy floor is a 200,000-token model context. The
complete default compact `tools/list` result is 9,483 tokens, leaving 517
tokens below the strict 10,000-token ceiling (5% of that support floor).
Compact read-only is 8,194 tokens; explicit standard and standard read-only are
22,723 and 15,468. The schema-valid representative `object_search` and
`object_get` results are 421 and 316 tokens. The compact catalog's 2% growth
boundary is 9,673 tokens and retains 327 tokens below the ceiling.
The earlier 14-tool baseline was 22,496 tokens; a rejected global annotation
and definition-name compaction reached about 14,612 tokens, still exceeded the
ceiling, and weakened every tool instead of selecting a coherent workflow.

Every count is compared exactly with the reviewed baseline, so any increase or
decrease fails until a reviewer inspects the complete profile catalog/result fixture
diff and updates `token-budget.json` deliberately. Growth of 2% or more (190
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
