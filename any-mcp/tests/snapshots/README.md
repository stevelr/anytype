# Catalog snapshots

These reviewed fixtures lock the complete normal and read-only `tools/list`
catalogs, including every description, input schema, output schema, and
annotation. The ordinary test suite only compares against them; it never
accepts changes automatically.

After intentionally changing a tool contract, regenerate both fixtures with:

```console
cargo test -p any-mcp server::tests::write_catalog_snapshots -- --ignored --exact
```

Review the complete fixture diff, confirm the recursive schema-bound and exact
annotation tests pass, then run the ordinary `any-mcp` test suite. Never make
snapshot acceptance conditional on an environment variable in CI.
