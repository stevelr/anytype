# Anytype Toolbox documentation site

This Zola site publishes the user guides and cross-project documentation for
Anytype Toolbox. Package READMEs remain the local entry points for their Rust
crates and link here for task-oriented workflows.

## Build and preview

The recipes expect Zola 0.22.1 on `PATH`, the version pinned by the deployment
workflow:

```sh
just serve
just build
just check
```

`just serve` listens on `http://127.0.0.1:21100`. The production URL is
`https://docs.anytype-toolbox.org`; use `build-at` to verify a preview URL:

```sh
just build-at https://preview.example.com
```

The output is written to the workspace's ignored `target/docs-site/` directory.
The Cloudflare Pages workflow deploys that directory to the `anytype-toolbox`
Pages project and expects the secrets
`CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` before its manual deployment
job is enabled.

The vendored EasyDocs theme is MIT-licensed. Its palette and templates derive
from the customized theme in `basil-doc`; Anytype Toolbox documentation and
site-specific build files use the repository's Apache-2.0 license.
