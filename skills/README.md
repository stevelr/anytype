# Anytype Toolbox Skills

Anytype Toolbox Skills packages two Agent Skills for Codex and Claude Code:

- `anyr` guides command-line work with Anytype objects, spaces, schema, files,
  chats, Markdown, and backups.
- `any-mcp` guides bounded workflows through an MCP connection backed by
  `anyr mcp`.

This is an independent community project and is not affiliated with or
endorsed by Anytype.

## Prerequisites

Install `anyr`, connect it to a running Anytype desktop or headless service,
and configure endpoint-specific credentials. The `any-mcp` skill also requires
an MCP host with a configured `anyr mcp` connection. Some fallback recipes use
the `anyr` executable directly; the `save-links` recipe additionally uses
Trafilatura for web-page extraction.

The package contains instructions and metadata. It does not contain Anytype
credentials or start an MCP server when installed.

Choose `anyr` when the agent can run the `anyr` executable and you want direct,
scriptable CLI operations. Choose `any-mcp` when the host already has an MCP
connection to `anyr mcp`; that skill uses the bounded MCP workflows and may
refer to `anyr` for documented fallback operations. Both require a running and
authenticated Anytype desktop or headless service. Neither skill installs or
configures those runtime prerequisites.

## Review before installation

Agent Skills are instructions that can influence an agent's tool use. Review
the selected `SKILL.md` and its local references, install from this repository
or an exact release tag, and inspect package changes before updating. Release
assets contain only this plugin tree; their `.sha256` file covers both the ZIP
and tar.gz archives. Do not put Anytype credentials in a skill directory.

## Install with the skills CLI

The interactive command discovers both skills from the monorepo and prompts
for a host, scope, and installation method:

```sh
npx skills add stevelr/anytype
```

List or select skills explicitly:

```sh
npx skills add stevelr/anytype --list
npx skills add stevelr/anytype --skill anyr
npx skills add stevelr/anytype --skill any-mcp
npx skills add stevelr/anytype --skill anyr --skill any-mcp
```

For a non-interactive global Codex install, name the host and accept the
selection explicitly:

```sh
npx skills add stevelr/anytype \
  --skill anyr --skill any-mcp \
  --agent codex --global --yes
```

To install the exact `0.1.0` release rather than the repository's current
default branch, use its immutable ZIP asset URL:

```sh
npx skills add \
  https://github.com/stevelr/anytype/releases/download/anytype-toolbox-skills-v0.1.0/anytype-toolbox-skills-v0.1.0.zip \
  --skill anyr --skill any-mcp
```

Use `npx skills update anyr any-mcp` for repository-based installations and
`npx skills remove anyr any-mcp` to remove them. An archive install stays on
the release named in its URL; install a newer versioned URL to upgrade it.

## Install as a Codex plugin

Add the repository marketplace, optionally pinned to a skills release tag,
then install the bundle:

```sh
codex plugin marketplace add stevelr/anytype \
  --ref anytype-toolbox-skills-v0.1.0
codex plugin add anytype-toolbox-skills@anytype-toolbox
```

For main-branch updates, add the marketplace with `--ref main`, run
`codex plugin marketplace upgrade anytype-toolbox`, and reinstall with
`codex plugin add anytype-toolbox-skills@anytype-toolbox`. Remove the plugin
and marketplace separately:

```sh
codex plugin remove anytype-toolbox-skills@anytype-toolbox
codex plugin marketplace remove anytype-toolbox
```

Start a new Codex thread after installation or upgrade so the new skills are
loaded.

## Install from the Claude Code marketplace

Claude Code can add the repository catalog and install the same plugin tree:

```sh
claude plugin marketplace add \
  stevelr/anytype@anytype-toolbox-skills-v0.1.0
claude plugin install anytype-toolbox-skills@anytype-toolbox
```

For a marketplace following the default branch, refresh and upgrade with:

```sh
claude plugin marketplace update anytype-toolbox
claude plugin update anytype-toolbox-skills@anytype-toolbox
```

Restart Claude Code after an upgrade. To remove both the installed plugin and
its catalog:

```sh
claude plugin uninstall anytype-toolbox-skills@anytype-toolbox
claude plugin marketplace remove anytype-toolbox
```

## Package ownership

This directory is the installable plugin root. The Codex and Claude manifests
share one package version, and [CHANGELOG.md](CHANGELOG.md) records that version
history. Each directory below `skills/` owns the operating guidance and
supporting references for one Agent Skill. The repository's crate READMEs own
the `anyr` and `any-mcp` runtime documentation.

Release packages are checked offline for matching manifest and changelog
versions, valid Agent Skills metadata and references, required files, safe
archive paths, and accidental private paths or credential material.
Tags named `anytype-toolbox-skills-vVERSION` publish reproducible ZIP and
tar.gz archives with SHA-256 checksums. Skills releases use their matching
changelog section as release notes and do not replace the repository's latest
`anyr` release.

The repository catalogs at `.agents/plugins/marketplace.json` and
`.claude-plugin/marketplace.json` both resolve to this directory. They are
discovery metadata only: installed hosts copy or cache the self-contained
plugin and do not gain access to other repository files through it.

Anytype Toolbox is licensed under the Apache License 2.0. See
[LICENSE](LICENSE).
