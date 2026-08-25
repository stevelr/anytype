# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## 0.5.3

Highlights (test & CI only):

- add github attestation to release assets (.tar.xz and .zip files)
- fix a Windows test race condition (tokio thread releasing dir handle)
- add Security and vulnerability reporting policy to repo
- remove Arch linux build from CI matrix (redundant because we build single musl-static binary for all linux platforms)

### Changed

- Build Step 1 CLI executables with the distribution profile while keeping
  Rust test harnesses on their test-specific profile.
- Preserve attested release-candidate archives from main-branch verification
  for promotion by a later release tag instead of rebuilding those binaries.
- Avoid rebuilding release candidates during manual release qualification.
- Wait up to 90 minutes for required checks when a release tag reaches GitHub
  before its main-commit workflows finish.
- Attest every final GitHub Release asset with tag-scoped build provenance,
  publish the macOS notarization record, and audit published checksums,
  attestations, Developer ID signatures, and notarization evidence daily.
- Keep live and release workflow fixtures focused on trust boundaries: an exact
  trusted-event allowlist, read-only default permissions, commit-pinned
  actions, non-persistent checkout credentials, attestation coverage, and a
  GitHub-hosted macOS verifier. Action revisions and macOS image upgrades no
  longer require fixture-only edits.
- Use native SHA-256 tooling on Linux and macOS when auditing release assets,
  while preserving checksum verification on both platforms.
- Give mocked signing and audit fixtures private temporary storage, and keep
  their synthetic notarization and finalization output out of CI logs.
- Remove the redundant Arch Linux source-test lane; Linux release builds
  already verify that the distributed musl binary is static and starts.

## 0.5.2

### Changed

- Let `anyr mcp` start with desktop HTTP or a temporarily unavailable headless
  gRPC backend. gRPC-only tools now check the backend on demand and return
  actionable setup, availability, or authentication errors without removing
  saved credentials.
- Add an `anytype-setup` Agent Skill for installing `anyr`, selecting desktop
  or headless operation, authenticating, and recovering unavailable backends.

## 0.5.1

Highlights:

- Unified binary `anyr` combines cli, backup/restore, editor hooks, archive inspection, and MCP server.
- New MCP server and agent skills
- Simpler setup and authentication with headless cli server with `anyr init-cli`.
- Uses HTTP/REST backend and, if available, uses gRPC backend for additional file, chat, and block-level capabilities.

Links:

- [Online documentation](https://docs.anytype-toolbox.org/)
- [anyr cli quick reference](https://docs.anytype-toolbox.org/cli/quick-reference/)
- [Rust api docs](https://crates.io/crates/anytype)
- [Binary downloads and install scripts](https://github.com/stevelr/anytype/releases)
- [Source](https://github.com/stevelr/anytype) (Apache 2.0 License)
- [Agent skills](https://github.com/stevelr/anytype/tree/main/skills)

### Changes

0.5.0..0.5.1 patch release:

- Fixes unit tests and github workflows. No functionality changes.

## 0.5.0

### Added

- Add `anyr object discussion get|attach` for verified discovery and
  idempotent creation of discussions derived from exact page and note IDs,
  with JSON and table output.
- Add bounded `anyr body list|show|create|update|delete|move` commands over the
  typed `anytype-api` block surface. Reads expose exact document order and
  structural position; closed JSON constructors and changes cover rich text,
  callouts, links, tables, embeds, and presentation fields; mutations return
  fresh verification evidence.
- `anyr init-cli --force` ignores an existing Anytype CLI config
  (`~/.anytype/config.json`) and creates a new account instead of reusing it.
- When `init-cli` finds a CLI config without `accountKey` (the Anytype CLI
  keeps the key in the OS keychain on macOS and desktop Linux), the error now
  says so and points at `anyr auth set-grpc --account-key` / `--bip39` or
  `anyr init-cli --force`.
- Default HTTP endpoint selection: with no `--url` / `ANYTYPE_URL`, `init-cli`
  targets the headless server (`http://127.0.0.1:31012`); other commands target
  the headless server when gRPC credentials are stored in the selected keystore
  and otherwise the desktop app (`http://127.0.0.1:31009`).

### Changed

- Expand the documentation site with backup, MCP, connection, and keystore
  guides. The README, quick reference, embedded help, and `anyr` skill now use
  current command syntax and identify commands or options that require a
  running Anytype CLI server and gRPC credentials.
- Build the Linux and macOS release archives from the repository's pinned Nix
  flake outputs. Linux packages use the fully static musl binary; macOS
  packages use the portable flake binary. The macOS binary is signed and
  notarized from a maintainer's local keychain before cargo-dist regenerates
  checksums, installers, and Homebrew formulae from all five platform archives.
- Credentials are stored as one keystore entry per service instead of four
  (see the `anytype` crate changelog), so OS keychains prompt once per
  application. Existing keystores migrate automatically on first use.

- Add the Anytype Toolbox Zola documentation site, with task-oriented `anyr`
  installation, quick-reference, Markdown export, and Rust-library guides.
- Publish cargo-dist archives, checksums, shell and PowerShell installers, and
  Homebrew formulae to a GitHub Release when a supported version tag points at
  `main`. Prerelease tags create GitHub prereleases without updating the
  Homebrew tap; manual and weekly runs remain build-only.
- Run the five-platform smoke checks on pull requests and pushes to `main`, and
  run the installed anyr/anyback live gate on pushes to `main` and nightly.
- Add `anyr completions` script generation for Bash, Fish, PowerShell, and Zsh.
  Generation does not require server credentials.
- Add Streamable HTTP conformance across the shipped `anyr mcp` command
  boundary: a portable test spawns the real binary in `streamable-http` mode
  with a private static-token file and a bounded scripted Anytype upstream,
  waits for the loopback listener, and drives authentication, initialize and
  initialized, `tools/list` over SSE, the standalone GET stream, session
  DELETE, and the stateless preview JSON sentinel, then requires a graceful
  exit on Unix, empty stdout, fixed transport diagnostics, and no disclosure
  of tokens, the session ID, or bodies on stderr.
- Add portable release binaries and a tier-1 smoke workflow. The build
  workflow gains `static-{x86_64,aarch64}` rows producing fully static
  musl `anyr` binaries (the glibc nix build's ELF interpreter points into
  /nix/store, so it runs only on Nix systems or in the OCI images), and
  the macOS rows now verify install names reference only the dyld shared
  cache. The manual `smoke` workflow runs repository checks, all-platform
  clippy, and the fast lib/bins test split in a 15-20 minute budget.
- Add manual cross-platform build and release-artifact workflows for Linux
  x86_64/arm64, macOS arm64, and Windows x86_64/arm64. Release qualification
  generates shell and PowerShell installers plus Homebrew formulae, validates
  the POSIX artifacts on Linux and macOS, and cannot publish releases while
  the matrix is being debugged.
- Add `anyr init-cli --save-env FILE` to save initialized HTTP and gRPC
  credentials, effective endpoints, the keystore service, and the `xtest`
  disposable-space prefix as a directly sourceable POSIX shell environment
  file. The command uses owner-only Unix permissions, refuses to overwrite an
  existing destination, and keeps credentials out of normal stdout and errors.

### Changed

- Bound aggregate `--all` operations with one 30-minute workflow deadline and
  `init-cli` with one 120-second deadline across its subprocess, verification,
  and join phases. Timeout overrides use strict finite ranges; child failures
  terminate and reap the direct child plus its owned Windows Job or Unix process
  group, and report dispatched mutations as indeterminate. Unix descendants can
  escape that boundary with `setsid` or `setpgid`. Cleanup that exceeds its bound
  transfers ownership to a durable OS-thread reaper rather than abandoning the
  child when the command runtime shuts down.
- Sort subcommand lists alphabetically throughout the `anyr` help tree.
- Let disposable headless live workflows select the Anytype CLI through
  `ANYTYPE_CLI_BIN`, defaulting to `anytype` on `PATH`.
- Make `anyr init-cli` reuse the account ID and account key from the default
  Anytype CLI config when it exists. The command derives new sessions from the
  account key and creates only a fresh HTTP token, preserving the existing
  account and spaces. Missing configs retain first-run account creation;
  unreadable, malformed, or incomplete configs fail closed.
- Render the new anytype-api HTTP deadline and indeterminate-mutation errors
  through their typed, secret-safe display output.
- The required anyr live gates (Python CLI suite and type-property
  preservation) now run on a GitHub-hosted runner against a disposable
  namespace-isolated headless server provisioned with `anyr init-cli
  --save-env`, replacing the retired self-hosted `anytype-headless` runner.

### Fixed

- Supply the macOS signing workflow test with its own SHA-256 implementation,
  keeping Arch Linux prerequisite checks independent of Perl's `shasum`.
- Reconcile a newly created disposable test space from a complete inventory
  when the create response omits its ID, so the cleanup guard still owns and
  deletes the exact space after a later assertion failure.
- Reject conflicting global output requests for every command: any pair of
  `--json`, `--pretty`, `--table`, and `--quiet`, or `--quiet` with
  `-o`/`--output`, now fails before dispatch.
- Keep chat transport and pagination truthful: message list, get, and delete
  are gRPC-only and reject `--transport rest`, while `--all` chat listings and
  message searches exhaust checked server pages.
- Run the real-operations live case inside the shared cleanup-owned disposable
  space guard so failures during setup or assertions cannot strand its space.
- Make the `init-cli` child-failure regression assert its stable operation and
  redaction contract instead of requiring platform-specific exit-status text.
- Make the oversized `init-cli` output regression emit its limit-crossing
  payload deterministically under loaded CI runners.
- Validate cargo-dist's generated Homebrew formula in formula mode, applying
  Homebrew's formula rules without the library-only Sorbet sigil checks.
- Accept unnamed ambient spaces in the Python CLI harness's space
  inventory: a fresh account's default space has an empty name, and strict
  naming applies only to the prefix-owned spaces the tests create.
- Link the aarch64 static musl binary with inline atomics
  (`-mno-outline-atomics`): vendored libdbus C referenced GCC outline
  helpers the static link does not provide. The macOS binaries rewrite
  nix's libiconv install name to the system library and re-sign, so every
  install name resolves from the dyld shared cache.
- Compile the scripted-CLI test helpers that only the Unix `init-cli` tests
  exercise on Unix alone, keeping the Windows clippy gate clean.
- Require the protected live gate to run the ignored type-property preservation
  test exactly and reject skipped Python CLI coverage. The CLI suite now fails
  missing live prerequisites in required mode and creates its own real-operation
  space instead of selecting an ambient prefix match.
- Harden space deletion with deterministic no-overwrite pre-delete archives,
  explicit automation controls, and fail-closed archive validation.
- Make live CLI cleanup require Anytype's explicit not-found response before a
  disposable space is considered deleted, and surface transport or server
  failures instead of treating them as proof of deletion.

---

## [Unreleased - 260806]

### Added

- Server-backed `file` command coverage in the Python CLI suite: upload
  backend selection, get/list, `search --sort`/`--desc`, metadata `HEAD`,
  full, ranged, `416`, and `412` downloads, a preload round trip, and bin and
  permanent delete, each running on a per-test disposable space with a bounded
  timeout on every CLI invocation. The README now records that
  `--if-none-match`/`--if-modified-since` cannot produce a `304` against
  `anytype-cli` 0.3.6, which sends no cache validators.
- Better error messages for cli errors, with user-friendly hints.
- Consolidated the former `any-edit`, `anyback`, and `any-mcp` command
  surfaces under `anyr md`, `anyr backup`, and `anyr mcp`. Shared endpoint,
  keystore, output, and `-v`/`-vv` verbosity options are inherited from anyr.
- `anyr backup` exposes create, restore, list, manifest, diff, extract, export,
  import, and inspect workflows. The inspector is now included by default.
- Version reporting is consolidated at `anyr -V`/`--version`; the former
  nested MCP version command is rejected with guidance to use the top-level
  command.
- `anyr space create NAME --chat` creates a chat space through the gRPC
  workspace API.
- Added `anyr space invite show|create|revoke` for active member and guest
  invitations, plus `anyr space enable-sharing` and
  `anyr space disable-sharing`.
- Added guarded `anyr space delete SPACE`, which offers a backup in the
  current directory and requires the exact `delete:SPACE_NAME` confirmation
  before deletion.

### Fixed

- `anyr backup` now honors compact, pretty, table, quiet, and output-file
  contracts while keeping progress and diagnostics on stderr.
- Reject `anyr backup -o FILE` when the result path aliases an input archive,
  object-list file, restore report, created archive, or extracted output; result
  files are now replaced only after the command has produced its document.
- Write `-v`/`-vv`/`RUST_LOG` tracing diagnostics to stderr instead of stdout,
  so stdout carries only the command's result document and machine-readable
  output such as `--json` backup results stays parseable. ANSI styling now
  follows stderr's terminal-ness, so redirected diagnostics contain no escape
  sequences.
- Select `anyr` as the default package for Cargo commands launched from the
  virtual workspace root, so `cargo run -- -h` starts the user-facing CLI
  without requiring `-p anyr`.
- Remove the CLI integration test's legacy pre-existing space-ID input. It now
  uses `ANYTYPE_TEST_SPACE_PREFIX` and requires an unambiguous matching space
  before running mutation coverage.
- Add protected installed-binary coverage for `anyr backup create` and `restore`
  that verifies restored content and cleanup of both disposable spaces.

### Added

- `anyr init-cli [--join INVITE_LINK]` initializes the selected keystore from
  a running headless Anytype CLI. It invokes the executable selected by
  `ANYTYPE_CLI_BIN` (default `anytype`), stores both HTTP and gRPC credentials
  without displaying them, verifies both transports, and can optionally join a
  space afterward. It honors `ANY_USER`, defaults to the headless HTTP/gRPC
  endpoints when no global or environment overrides are present, propagates
  the effective endpoints to child commands, and makes an explicit best-effort
  rollback to both prior credential objects if a paired keystore write fails.
- `anyr chat --transport auto|rest|grpc` selects the transport policy for chat
  operations (default `auto`). `rest` rejects operations that only gRPC can
  serve (for example cross-space list, chat text search, rich `get`, `unread`,
  structured `--blocks-json` send/edit, and multi-chat `listen`) with an
  actionable error, and `grpc` rejects the REST-only `messages search`; the
  resolved policy backend is reported in verbose diagnostics (`-v`). Under
  `auto`, REST-capable operations (single-space `list`, `create`, `messages
  list|get|send|edit|delete|search|react`, `read`/`read-reactions`/`read-all`,
  and single-`--chat` `listen` with `--space`) route through the REST
  `SpaceChatsClient`; everything else falls back to gRPC.
- `anyr chat list --filter KEY=VALUE` applies property filters to a space-scoped
  REST listing (no `--text`).
- `anyr chat create` gained `--icon-emoji` / `--icon-file` (mutually exclusive)
  to set the new chat object's icon.
- `anyr chat messages send` gained `--reply-to MESSAGE` (reply to an existing
  message by id or order id) and `--blocks-json JSON` (structured message blocks
  as a JSON array via `@file`, `@-`, or `-`; routes through gRPC). `messages
  edit` likewise gained `--blocks-json`.
- `anyr chat messages search SPACE CHAT QUERY` runs a REST-only full-text search
  over a chat's messages; `anyr chat messages react SPACE CHAT MESSAGE EMOJI`
  toggles a reaction on a message.
- `anyr chat read-reactions SPACE CHAT` marks reactions read (optionally through
  an order id), and `anyr chat read-all SPACE CHAT` marks every message in a
  chat as read (both REST).
- `anyr chat listen` gained a REST SSE listener for a single `--chat` with
  `--space`: `--initial-limit N` replays the last N messages when the stream
  opens and `--heartbeat SECONDS` (1-60) sets the keep-alive interval. The
  gRPC-only options `--include-history`, `--after`, `--previews`, and `--buffer`
  (and a `--chat`-only listen without `--space`) route through the reconnecting
  gRPC listener.
- program used to generate test vectors for account key generation
- `anyr type update` property-list controls (mutually exclusive with
  `--add-property`):
  - `--set-property KEY:FORMAT:NAME` replaces the complete non-featured
    property list.
  - `--clear-properties` removes all non-featured recommended properties.
- `anyr file delete --permanent` deletes a file object permanently (skips the
  bin) instead of moving it to the bin.
- `anyr file search` sorting: `--sort PROPERTY` orders results by a property key
  (for example `name` or `last_modified_date`), and `--desc` selects descending
  order (requires `--sort`).
- `anyr file upload` gained richer sources and gRPC-only options:
  - `--url URL` uploads a remote file, `--stdin` uploads bytes read from stdin
    (requires `--name`), and `--mime` sets the MIME type for a REST upload.
  - `--file-type`, `--style`, `--details JSON_OR_@FILE`, `--created-in-context`,
    and `--created-in-context-ref` route the upload through the gRPC backend.
- `anyr file preload SPACE (--file FILE | --url URL)` preloads a file (gRPC) from
  a local path or a remote URL and returns a preload file id; `anyr file
  discard-preload SPACE FILE_ID` discards one.
- `anyr file metadata SPACE FILE` issues a REST `HEAD` request and reports the
  HTTP status plus the header metadata (etag, content-type, content-length,
  last-modified, ...) in both JSON and table output; supports `--width` and the
  conditional headers (`--if-match`, `--if-none-match`, `--if-modified-since`,
  `--if-unmodified-since`).
- `anyr file download SPACE FILE` gained REST options: `--width`, `--range`, and
  the conditional headers `--if-match`, `--if-none-match`, `--if-modified-since`,
  `--if-unmodified-since`, and `--if-range`.

### Changed

- `anyr type update --add-property` now uses Anytype's exact source-backed
  property classification and resubmits only non-featured recommended
  properties. This replaces the fixed system-key exclusion heuristic, preserves
  deterministic first-key de-duplication, and requires working HTTP and gRPC
  credentials.
- `anyr auth status` now reports HTTP and gRPC credentials separately with an
  explicit present/missing indicator per set, so it is clear which credential
  set a REST versus gRPC command needs.
- **Breaking**: `anyr list objects` now requires `--view` (view name or id); a
  missing view is rejected at parse time instead of failing client-side.
- **Breaking**: `anyr file delete` no longer accepts `--http`; the flag has been
  removed and deletion now uses the REST files client (add `--permanent` to skip
  the bin).
- **Breaking**: `anyr file download` now uses REST unconditionally and takes
  `SPACE` as a required leading positional (`anyr file download SPACE FILE`); the
  `--http` and `--space` flags have been removed, and the REST options
  (`--width`, `--range`, `--if-*`) are no longer gated behind `--http`. JSON now
  reports `{status, written, path, bytes, metadata}` (previously `{path}`), and
  table output is now `status N PATH` (previously the bare path); a
  `304`/`412`/`416` response leaves the destination file untouched and reports
  `written: false`. The legacy gRPC (anytype-heart) download command has been
  removed; REST is now the sole download path.
- `anyr file upload --http` is now a deprecated no-op (a plain upload already
  uses REST); it prints a deprecation warning and is rejected when combined with
  any gRPC-only option (`--url`, `--file-type`, `--style`, `--details`, or a
  `--created-in-context*` option), since those select the gRPC transport. The
  REST-only options `--mime` and `--stdin` are likewise rejected up front when
  combined with a gRPC-only option instead of being silently dropped.
- `anyr property update` now requires at least one of `--name` or `--key` and
  rejects a no-flag invocation before any network I/O; when `--name` is omitted
  it reuses the property's current name so a key-only update still satisfies
  the REST contract.
- Normalized spellchecker configuration formatting.
- name and id resolution (space, type, chat, view, property) moved into the
  anytype crate (`anytype::resolve`); anyr now calls the shared
  `AnytypeClient::resolve_*` methods. Behavior is unchanged, except: a type
  lookup that matches nothing now reports "not found" instead of "ambiguous",
  and not-found/ambiguous messages use the shared `AnytypeError` formats.
- chat order-id-to-message-id resolution now delegates to the shared
  `AnytypeClient::resolve_message_id` / `resolve_message_ids` resolver; the CLI
  retains only its hex order-id encode/decode helpers (no user-facing change).

## [0.4.1]

### Added

- auth updates:
  - `anyr auth set-grpc --bip39` to derive and save gRPC account credentials from a BIP39 mnemonic.
  - `anyr auth find-grpc [--program PREFIX]` to discover a local Anytype gRPC listener port.

## [0.4.0] - anyr - 2026-02-16

### Added

- new space archive commands:
  - `anyr space count-archived SPACE_NAME_OR_ID`
  - `anyr space delete-archived SPACE_NAME_OR_ID [--confirm]`
- `anyr chat create SPACE_NAME_OR_ID CHAT_NAME` to create chat objects in a space

### Changed

- **Breaking**: chat command argument order is now consistent for space-scoped chat operations:
  - `anyr chat get SPACE CHAT`
  - `anyr chat read SPACE CHAT`
  - `anyr chat unread SPACE CHAT`
  - `anyr chat messages list/get/send/edit/delete SPACE CHAT ...`

## [0.3.0] - anyr - 2026-01-28

### Added

- File commands: list/search/get/update/delete, plus `file download` and `file upload` for raw bytes.
- File list/search filters for name, type, extension, and size.
- Auth commands now support `set-http` and `set-grpc` to update credentials in the keystore.
  - Example: `anyr auth set-grpc [ --account-key | --session-token ]` to store a gRPC account key or session token.
- `--grpc` flag to override the gRPC endpoint url.
- Chat commands (gRPC): `anyr chat list/get/messages list/get/send/edit/delete/read/unread/listen`
- `anyr object link` generates web link for an object

### Changed

- protoc and libgit2 must be installed for build from source or cargo install
- Auth status now reports HTTP vs gRPC credential status with ping checks.
- file-based keystore uses sqlite (turso native rust implementation)
- Apache-2.0 license

### BREAKING

- authentication-related environment variables and flags have changed
  - `--keyfile`, `--keyfile-path`, and `--keyring` now replaced by `--keystore`.
  - omit to use platform default keystore
  - `--keystore file` to use file-based keystore in default path (~/.local/state/keystore.db)
  - `--keystore file:path=/path/to/keystore.db` to use file keystore in custom path
  - `--keystore secret-store` to use dbus secret store on linux (default kernel 'keyutils')

## [0.2.4] - anyr - 2026-01-17

### Added

- Documentation (README.md): example command for listing items in query or collection

### Changed

- Fix: 'view objects' with query views found results in table output format only. Now gives result in table or json format. Property metadata resolved before output formatting, and removed get_type call from json output path.
- removed undocumented --keyring-service arg

## [0.2.3] - anyr - 2026-01-12

### Changed

- Use rustls (native roots) for HTTP TLS to avoid OpenSSL install errors.
- Uses anytype-v0.2.8.

## [0.2.2] - anyr - 2026-01-12

### Added

- New command `anyr view objects` to list view items for grid and list views.
  - Json output includes all properties/view columns.
  - Table output defaults to name column only, and supports `--columns`/`--cols` for specific property keys

- Table display formatting improvements:
  - Column names from property names
  - Format dates with strftime format: `--date-format` or `ANYTYPE_DATE_FORMAT`, defaults to `%Y-%m-%d %H:%M:%S`.
  - For members, replace member Id with display name.

- Resolvers allow names or keys in place of ids for many cli args:
  - Resolve space id from name for any command that requires space_id. Changed arg name from `space_id` to `space`.
  - Resolve type id from type_name or type_id. Disambiguation rules:
    - if arg has '@' prefix, match type_key only. If arg begins with upper case letter, match name only.
    - type_id always works and is unambiguous.
  - Resolve view id from name. (applies to `view objects subcommand`)
  - Resolve property id from property key

- Improved README documentation on key storage, configuration, authentication, and more examples

- Improved cli help docs

### Changed

- Removed cli config file to simplify. Options can be configured by cli args or environment variables.
- Uses anytype-v0.2.7
