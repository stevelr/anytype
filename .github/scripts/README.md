# Release scripts

## Skills package validation

`validate_skills_package.py` checks the unpacked Anytype Toolbox Skills plugin
or a release ZIP without network access. It verifies both host manifests, Agent
Skills frontmatter, local references, changelog and package versions, required
files, public-path hygiene, credential patterns, and archive entry safety.
`test_skills_package.py` exercises the directory and archive rules with offline
fixtures. The common CI prerequisite check runs both commands.

`validate-skills-hosts.sh` adds the installed `skills-ref`, Codex, and Claude
Code validators when each is available. The offline validator remains the
release gate because host installations are not part of the repository
toolchain.

```sh
python3 .github/scripts/validate_skills_package.py skills
python3 .github/scripts/test_skills_package.py
.github/scripts/validate-skills-hosts.sh skills
```

The validator accepts `--expected-version VERSION` for either an unpacked
plugin or a ZIP. Release and prerelease versions use the same semantic-version
comparison.

`prepare_skills_release.py` accepts only
`anytype-toolbox-skills-vVERSION` tags, checks that `VERSION` matches both
plugin manifests and the first released changelog section, and writes
reproducible ZIP and tar.gz archives, SHA-256 checksums, and release notes for
that section. `validate-skills-release-ref.sh` separately proves that the tag
already exists on `origin`, resolves to the checked-out commit, and is reachable
from `origin/main`.

```sh
python3 .github/scripts/prepare_skills_release.py prepare \
  anytype-toolbox-skills-v0.1.0 --output skills-release
```

The `Release skills plugin` workflow runs only for skills tags. Its packaging
job is read-only; only the final publishing job receives `contents: write`.
It publishes the three generated assets with the selected changelog section as
notes and marks the release as non-latest so it cannot replace an `anyr`
release in the repository-wide latest-release slot.

`validate_skills_marketplaces.py` checks the repository's Codex and Claude
marketplace catalogs without installing them. It confines both entries to the
`skills/` plugin root and compares their identity, publisher, and version
metadata with the host manifests. `test_skills_marketplaces.py` covers path
escape and metadata-drift failures.

`test-skills-installation-e2e.sh` exercises the networked installation
boundary without changing the operator's real home directory. A Bubblewrap
sandbox mounts the checkout read-only and gives `npx skills` 1.5.23, Codex, and
Claude disposable configuration and cache directories. A loopback server
exercises the skills CLI's archive-download path, and a transient user scope
contains its process tree. The test proves individual, combined, project,
global, release-ZIP, marketplace, fixture-upgrade, and clean removal paths:

```sh
.github/scripts/test-skills-installation-e2e.sh
```

The test requires Linux, Bubblewrap, network access to npm, and the current
`npx`, `codex`, and `claude` commands. It does not use Anytype credentials or a
running Anytype service. On 2026-08-21 it passed with skills CLI 1.5.23, Codex
0.149.0, and Claude Code 2.1.238. Both hosts discovered `anyr`, `any-mcp`, and
`anytype-setup` from the same staged plugin; install, `0.1.1` to `0.1.2`
fixture upgrade, and removal all passed.

The trigger metadata was also reviewed against this prompt matrix:

| Prompt shape                                           | Expected skill |
| ------------------------------------------------------ | -------------- |
| Use `anyr` to list Anytype pages from the command line | `anyr`         |
| Search Anytype through the configured MCP connection   | `any-mcp`      |
| Install `anyr` and connect it to the desktop app        | `anytype-setup` |
| Diagnose a missing MCP tool, then use a CLI fallback   | both           |
| Refactor an unrelated Rust parser                      | neither        |

The `anyr` skill delegates installation, authentication, endpoint changes, and
backend recovery to `anytype-setup`. The `any-mcp` prerequisite section tells
the agent to report an absent MCP connection or optional toolset instead of
inventing a tool or starting a second server. The setup skill distinguishes
desktop HTTP from paired headless backends and preserves saved credentials.

The release workflow builds all platform archives and exports the Nix-built
macOS binary as a signing input. It does not publish a GitHub Release. The
macOS signing command verifies that input against its workflow run, applies a
Developer ID signature from the local keychain, submits it to Apple's notary
service, uploads the signed handoff to a draft release, and dispatches the
finalization workflow. The finalizer verifies the signature and notarization,
rebuilds the macOS archive and global checksums, validates the installers, and
publishes the draft.

## One-time macOS setup

The keychain identity must be a `Developer ID Application` certificate with its
private key. Find the identity hash and confirm the certificate class:

```sh
security find-identity -v -p codesigning
security find-certificate -a -c "Developer ID Application" -p \
  | openssl x509 -noout -subject -issuer
```

Store notarization credentials in the macOS keychain. An app-specific password
works with an Apple ID account:

```sh
xcrun notarytool store-credentials anyr-notary \
  --apple-id APPLE_ID \
  --team-id TEAM_ID \
  --password APP_SPECIFIC_PASSWORD
```

Set the expected Team ID as a repository variable. It is public verification
metadata, not a signing secret:

```sh
gh variable set MACOS_DEVELOPER_TEAM_ID \
  --repo stevelr/anytype \
  --body TEAM_ID
```

## Sign and finalize a release

Push a supported release tag only after release qualification succeeds. Wait
for its `Release artifacts` run to finish, then note the run ID:

```sh
gh run list \
  --repo stevelr/anytype \
  --workflow release.yml \
  --event push \
  --limit 10
```

Run the signing command on the Mac containing the Developer ID identity:

```sh
.github/scripts/sign-macos-release.sh \
  --run-id RUN_ID \
  --identity CERTIFICATE_SHA1 \
  --notary-profile anyr-notary
```

Use `--keychain PATH` when the identity is outside the default user keychain.
The command refuses failed or non-tag workflow runs, a mismatched tag or hash,
a non-Developer ID signature, a Team ID mismatch, and rejected notarization.
It creates or updates only a draft release; `Finalize signed release` publishes
it after all regenerated artifacts pass validation.

The notarization submission and GitHub upload require network access. The
private key and the stored Apple credentials never leave the local keychain.
