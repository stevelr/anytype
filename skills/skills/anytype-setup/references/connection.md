# Install and connect `anyr`

Check for an existing executable before changing package state:

```sh
command -v anyr
anyr --version
```

If `anyr` is absent, ask the operator to select Homebrew, an exact release
archive, or Cargo and approve the exact version. Do not select `latest`, run a
remote installer script, use a fork or mirror, or continue after verification
fails.

## Install `anyr`

### Homebrew on macOS

After approval, use the maintained tap:

```sh
brew install stevelr/tap/anyr
anyr --version
codesign --verify --strict --verbose=2 "$(command -v anyr)"
codesign --display --verbose=4 "$(command -v anyr)"
spctl --assess --type execute --verbose=2 "$(command -v anyr)"
```

The formula verifies the archive SHA-256 before installation. It installs the
same macOS binary contained in the direct-download archive; that binary is
signed with an Apple Developer ID certificate and notarized by Apple. Require
`codesign --display` to show an `Authority=Developer ID Application:` entry and
require `codesign --verify` to succeed. Treat `spctl` as a supplementary
assessment: macOS 15 may report that a valid standalone executable does not
seem to be an app, but stop on any other rejection.

### Exact release archive

Select one immutable `anyr-vVERSION` release and the archive for the detected
platform from the
[Anytype Toolbox releases](https://github.com/stevelr/anytype/releases). Download
both the archive and its adjacent `.sha256` file from that exact release. Do
not extract or execute the archive until one platform-appropriate check passes
in the directory containing both files:

```sh
# Linux
sha256sum --check ARCHIVE.sha256

# macOS
shasum -a 256 --check ARCHIVE.sha256
```

On Windows PowerShell, compare the sidecar's first field with `Get-FileHash`
and stop on a mismatch:

```powershell
$expected = ((Get-Content "ARCHIVE.sha256" -Raw) -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash "ARCHIVE" -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "anyr archive SHA-256 mismatch" }
```

The adjacent checksum detects a corrupted or substituted download only when
the checksum itself remains trustworthy; because both files are published in
the same release, it does not independently prove publisher identity. On a
platform without a native code signature, verify the GitHub build-provenance
attestation before installing or use the pinned Cargo source-build method:

```sh
gh attestation verify ARCHIVE \
  --repo stevelr/anytype \
  --signer-workflow stevelr/anytype/.github/workflows/finalize-release.yml \
  --source-ref refs/tags/RELEASE_TAG \
  --deny-self-hosted-runners
```

Stop if verification does not resolve the archive digest to the selected tag
and finalization workflow.

After extracting a macOS archive, verify the same Developer ID signature and
Apple notarization assessment used by the Homebrew installation:

```sh
codesign --verify --strict --verbose=2 ./anyr
codesign --display --verbose=4 ./anyr
spctl --assess --type execute --verbose=2 ./anyr
```

Require `codesign --display` to show an `Authority=Developer ID Application:`
entry and require `codesign --verify` to succeed. Treat `spctl` as a
supplementary assessment: macOS 15 may report that a valid standalone
executable does not seem to be an app, but stop on any other rejection.

Move the verified executable to the approved location on `PATH`, then run
`anyr --version` and require the selected version.

### Cargo

When the operator chooses a Rust source build, pin the requested release and
its lockfile:

```sh
cargo install anyr --locked --version '=VERSION'
anyr --version
```

Stop if the reported version differs from the approved version.

Choose one backend while configuring or switching a connection. Ask when that
choice is genuinely ambiguous; an ordinary operation on an already configured
MCP connection uses its selected mode without asking again.

## Limit model-visible data

Use a separate Anytype CLI account when the operator wants to keep personal,
team, or family spaces outside the agent's reach. Provision that account in an
isolated headless profile, create a purpose-built sharing space from the
desktop app, and invite the headless account only to that space. Running a
headless server with the desktop account does not reduce the spaces available
to the account.

Choose the least space role that supports the workflow. Reader permits reads;
Writer permits content changes. Owner is unnecessary for ordinary agent work.
Each role applies across the space because Anytype has no per-object permission
boundary within a space. Content moved or copied into the sharing space should
therefore be treated as available to the connected agent and model.

Keep the invitation link out of chat, logs, and skill files. After the
operator supplies it through a protected local mechanism, join during
headless initialization:

```sh
anyr init-cli --join "$INVITE_LINK"
```

Do not use `--force` in a profile that contains an account the operator wants
to preserve. A dedicated operating-system account, home directory, or
equivalent Anytype CLI profile prevents the new headless account from reusing
the desktop account's configuration.

## Desktop

Start the existing Anytype desktop application. Select an isolated desktop
profile and authenticate with the code shown by the application:

```sh
export ANYTYPE_URL=http://127.0.0.1:31009
export ANYTYPE_KEYSTORE_SERVICE=anyr-desktop
anyr auth login
anyr auth status
```

For `anyr mcp`, set `ANY_MCP_CONNECTION_MODE=desktop` in the MCP host and
omit `ANYTYPE_GRPC_ENDPOINT` from that process environment, e.g.:

```json
{
  "anytype": {
    "command": "anyr",
    "args": ["mcp"],
    "env": {
      "ANY_MCP_CONNECTION_MODE": "desktop",
      "ANYTYPE_URL": "http://127.0.0.1:31009",
      "ANYTYPE_KEYSTORE_SERVICE": "anyr-desktop"
    }
  }
}
```

Omitting this
non-secret selector from one isolated profile does not remove saved gRPC
credentials from another keystore profile.

HTTP commands can proceed when `ping.http` is successful. A missing or failed
gRPC check does not block an HTTP-only workflow. Do not add headless gRPC
credentials to this profile.

## Headless

Check for the [Anytype CLI](https://github.com/anyproto/anytype-cli) before
changing connection state:

```sh
anytype --help
```

If it is absent, show the operator the official project and stop. Do not run a
remote installer script or download and execute an upstream release asset from
this workflow. Resume after the operator installs an approved version and
`anytype --help` succeeds.

Start the operator's existing Anytype CLI account with `anytype serve`, then
use a separate headless profile:

```sh
export ANYTYPE_URL=http://127.0.0.1:31012
export ANYTYPE_GRPC_ENDPOINT=http://127.0.0.1:31010
export ANYTYPE_KEYSTORE_SERVICE=anyr-headless
anyr init-cli
anyr auth status
```

For `anyr mcp`, set `ANY_MCP_CONNECTION_MODE=headless` in the MCP host, e.g.:

```json
{
  "anytype": {
    "command": "anyr",
    "args": ["mcp"],
    "env": {
      "ANY_MCP_CONNECTION_MODE": "headless",
      "ANYTYPE_URL": "http://127.0.0.1:31012",
      "ANYTYPE_GRPC_ENDPOINT": "http://127.0.0.1:31010",
      "ANYTYPE_KEYSTORE_SERVICE": "anyr-headless"
    }
  }
}
```

When customized
endpoints are needed, provide both HTTP and gRPC endpoints for the same host;
do not combine transports from different Anytype processes.

`anyr init-cli` reuses an existing CLI account when its reusable credentials
are available. Do not pass `--force` for ordinary setup: it intentionally
ignores the existing CLI configuration and creates a new account. Use it only
after the operator explicitly requests a new account or a destructive reset.

Headless HTTP and gRPC are a pair. Commands that require gRPC proceed only
when both applicable pings succeed; HTTP-only commands need only the HTTP
result. Preserve the selected URL, gRPC endpoint, and keystore service for
later invocations, such as in the host's protected environment configuration.

## Status and bounded recovery

`anyr auth status` reports credential presence and a redacted ping for each
configured transport. Check the transport the requested command uses. Do not
use an absent gRPC credential as a reason to block an HTTP-only operation.

For a structured MCP or CLI error that says a backend is missing,
unavailable, or unauthenticated, route to this setup procedure once:

1. Confirm the intended desktop or headless profile and inspect `anyr auth
   status` without exposing its stored values.
2. For a local service, verify that the operator's existing application or
   CLI service is running. For `grpc_unavailable` in an explicitly selected
   headless profile with saved credentials, use the operator's normal service
   supervisor or start `anytype serve` once when authorized; do not spawn a
   duplicate daemon. Wait for one bounded readiness interval, then repeat only
   the applicable status check. Authenticate again only for an authentication
   error, not merely because the service was stopped.
3. For a remote service, do not restart it, guess its endpoint, or create an
   account. Report the redacted category and request its endpoint or operator
   assistance.

If the MCP host is connected to a stopped headless service and the operator
chooses the running desktop app instead, update that host's protected
environment to the isolated desktop profile, remove the gRPC endpoint selector
from that process configuration, and reconnect or restart the MCP process.
Do not copy or delete credentials while switching profiles.

Do not run `anyr auth logout`, delete a keystore, clear credential variables,
or reset account data as connection recovery. Those actions discard usable
credentials and require separate authorization. Removing an endpoint selector
from one isolated MCP process configuration is profile selection, not
credential deletion.

If an initialization or mutation command times out or is cancelled, its result
may already exist. Read or search the relevant state before retrying. Reuse an
idempotency key only for the identical request; otherwise report the
indeterminate outcome.
