# anyr

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anyr-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anyr-v&expanded=true)
[![crates.io](https://img.shields.io/crates/v/anyr.svg)](https://crates.io/crates/anyr)

**[Documentation](https://docs.anytype-toolbox.org) ·
[Quick reference](https://docs.anytype-toolbox.org/cli/quick-reference/) ·
[Markdown export](https://docs.anytype-toolbox.org/guides/export-markdown/)**

List, search, and manipulate anytype objects from the command-line

Homepage: https://github.com/stevelr/anytype

```sh
# show alphabetically sorted commands and global options
anyr --help

# check authentication status; reports HTTP and gRPC credentials
# separately (present/missing) plus a live ping for each set
anyr auth status
# authenticate with desktop and http endpoint
anyr auth login
# initialize HTTP and gRPC credentials from a running headless CLI
anyr init-cli
# save a directly sourceable environment file for an agent or service
ANYTYPE_KEYSTORE=env anyr init-cli --save-env ./anytype.env
source ./anytype.env
# optionally join a space after initialization
anyr init-cli --join "$INVITE_LINK"

# List spaces your user is authorized to access
anyr space list -t     # output as table (-t/--table)

# Create a chat space
anyr space create "Team Chat" --chat

# Create and inspect invitations (Anytype CLI server and gRPC credentials required)
anyr space invite create "Work" --writer
anyr space invite show "Work"
anyr space invite revoke "Work"

# Enable or disable public space sharing
anyr space enable-sharing "Work"
anyr space disable-sharing "Work"

# Permanently delete a space after an archive and exact-name confirmation
anyr space delete "Old Work"
# Automation must choose the archive policy and confirmation explicitly
anyr space delete "Old Work" --archive ./old-work.zip --confirm

# Count or delete archived objects in a space
anyr space count-archived "Work"
anyr space delete-archived "Work" --confirm

# List Pages in space "Work"
anyr object list "Work" --type page -t

# List files (Anytype CLI server and gRPC credentials required)
anyr file list "Personal" -t

# Download/upload file bytes
# download fetches bytes over REST; use `--dir DIR` or `--file PATH` for the destination
anyr file download "Personal" <FILE_OBJECT_ID> --dir /tmp
anyr file upload "Personal" -f ./path/to/file.png

# Create a chat in a regular space
anyr chat create "Work" "Ops"
# Get messages (Anytype CLI server and gRPC credentials required)
anyr chat messages list "Work" "Ops" -t
# Post a message over REST by using the exact chat ID
anyr chat messages send "Work" <CHAT_ID> --text "hello world?"

# Discover or attach a derived discussion (HTTP plus CLI/gRPC required)
anyr object discussion get "Work" OBJECT_ID
anyr object discussion attach "Work" OBJECT_ID

# Inspect a page body (Anytype CLI server and gRPC credentials required)
anyr body list "Work" OBJECT_ID -t
anyr body show "Work" OBJECT_ID BLOCK_ID --pretty

# Markdown editing (the former any-edit commands)
anyr md get "Work" OBJECT_ID -o page.md
anyr md update -i page.md
anyr md edit "Work" OBJECT_ID

# Backup and archive workflows; create and restore require CLI/gRPC
anyr backup create --space "Work" --dir ./backups
anyr backup list ./backups/ARCHIVE.zip
anyr backup restore ./backups/ARCHIVE.zip --space "Work"

# MCP server and maintenance commands (the former any-mcp commands)
anyr mcp
anyr mcp init
anyr mcp check
```

The global output contract applies to every anyr command: compact JSON by
default, `--pretty` for indented JSON, `--table` for human-readable output,
`--quiet` to suppress the result document, and `-o FILE` to route it to a file
instead of stdout. Progress spinners and diagnostics always stay on stderr, so
stdout holds only the result document. Every invocation rejects more than one
of `--json`, `--pretty`, `--table`, and `--quiet`, as well as `--quiet` together
with `-o FILE`. Backup commands additionally reject output flags on the
interactive `anyr backup inspect` and result paths that alias a command input
or generated artifact.

The consolidated binary owns shared endpoint, keystore, output, and
verbosity options. Use `-v`, `-vv`, or `RUST_LOG` for diagnostics; diagnostics
are always written to stderr, so stdout carries only the command's result
document and stays parseable by `jq` and other tools. ANSI styling follows
stderr's terminal-ness, so redirected diagnostics contain no escape
sequences. `anyr -V`
or `anyr --version` reports the anyr Cargo binary version. Version reporting
is intentionally top-level; `anyr mcp --version` is rejected with guidance to
use `anyr --version`. The archive inspector is included in the default anyr
build.

## Command map

Most commands use the local HTTP API. Entries marked **CLI + gRPC** require a
running Anytype CLI server, its gRPC endpoint, and gRPC credentials in the
selected keystore. `anyr init-cli` provisions both HTTP and gRPC credentials
from that server, so it does not require them to be stored first.

- `auth`: `login`, `logout`, `status`, `set-http`, `set-grpc`, and
  `find-grpc`.
- `backup`: `create` and `export` are **CLI + gRPC**. `restore` and `import`
  are **CLI + gRPC** unless `--dry-run` is used. `list`, `manifest`, `diff`,
  `extract`, and `inspect` read local archives.
- `body`: `list`, `show`, `create`, `update`, `delete`, and `move` are
  **CLI + gRPC**.
- `chat`: transport depends on the operation and `--transport`; see
  [Chat transport](#chat-transport).
- `completions`: generates Bash, Fish, PowerShell, or Zsh completions locally.
- `file`: `list`, `search`, `get`, `preload`, and `discard-preload` are
  **CLI + gRPC**. `download`, `metadata`, `update`, and `delete` use HTTP.
  `upload` uses HTTP unless one of the gRPC-only upload options listed below
  is present.
- `init-cli`: initializes credentials from a running Anytype CLI server and
  optionally joins a space or writes a sourceable environment file.
- `list`: `objects`, `views`, `add`, and `remove` use HTTP list and collection
  endpoints.
- `mcp`: runs the embedded server or its `init` and `check` maintenance
  aliases. Required transports depend on the configured MCP catalog.
- `md`: `get`, `update`, and `edit` use the HTTP object API.
- `member`: `list` and `get` use HTTP.
- `object`: `list`, `get`, `link`, `create`, `update`, and `delete` use HTTP.
  `discussion get|attach` require both HTTP and **CLI + gRPC**.
- `property`: `list`, `get`, `create`, `update`, and `delete` use HTTP.
- `search`: searches globally or within one space over HTTP.
- `space`: `list`, `get`, regular `create`, and `update` use HTTP. `create
  --chat`, `count-archived`, `delete-archived`, `delete`, `invite`,
  `enable-sharing`, and `disable-sharing` are **CLI + gRPC**.
- `tag`: `list`, `get`, `create`, `update`, and `delete` use HTTP.
- `template`: `list` and `get` use HTTP.
- `type`: `list`, `get`, `create`, `update`, and `delete` use HTTP. The
  `type update --add-property` read phase additionally requires **CLI + gRPC**.
- `view`: `objects` uses HTTP.

### Attached discussions and typed body blocks

These commands require HTTP access plus a running Anytype CLI server and gRPC
credentials.

`anyr object discussion get SPACE OBJECT_ID` returns either an `absent` state
or the verified derived discussion ID for one exact page or note. `attach`
performs the same verification and creates the discussion when absent; repeated
calls return the same attachment. These operations are separate from ordinary
space chats, though the resulting discussion ID can be passed to the gRPC chat
message commands.

`anyr body list SPACE OBJECT_ID` returns a bounded page of blocks in exact
depth-first document order. Every item includes `order`, `depth`, `parent_id`,
`sibling_index`, its exact block ID, ordered children, restrictions,
presentation, and typed content. `body show` selects one exact block ID.

Create and update accept a closed JSON document as a literal, `@FILE`, `@-`, or
`-` for stdin. For example:

```sh
anyr body create "Work" OBJECT_ID ROOT_BLOCK_ID last-child --block \
  '{"content":{"kind":"callout","text":"Check this","icon":{"type":"emoji","content":"💡"}},"background_color":"grey"}'

anyr body update "Work" OBJECT_ID BLOCK_ID --change \
  '{"kind":"text","text":"Checked","marks":[{"range":{"start":0,"end":7},"kind":{"type":"bold"}}]}'

anyr body move "Work" OBJECT_ID BLOCK_ID TARGET_BLOCK_ID before
anyr body delete "Work" OBJECT_ID BLOCK_ID \
  --expected-subtree-blocks 1 --confirm
```

Create supports paragraphs, headings, bulleted and numbered items, checkboxes,
toggles, callouts, quotes, code blocks, dividers, unfetched bookmarks, link and
relation cards, bounded tables, LaTeX/Mermaid/YouTube embeds, and tables of
contents. Update supports text and marks, text style, checkbox state, text
color, callout icon, embed content, divider style, link appearance, alignment,
and background. The API validates each JSON value and verifies every mutation
with a fresh body read. Delete additionally compares the current subtree size
with `--expected-subtree-blocks` before dispatch.

`anyr space delete` defaults to an interactive, fail-closed flow. It first
offers to write a complete space backup in the current directory, then requires
the exact confirmation string `delete:SPACE_NAME`. An unrecognized archive
choice or confirmation cancels deletion.

For deterministic automation, `--archive PATH` writes the complete pre-delete
backup to exactly `PATH`, refuses to overwrite any existing file or symlink, and
bypasses the archive-choice prompt. `--skip-archive` explicitly declines the
backup; the two archive-policy flags conflict. `--confirm` bypasses the final
exact-name prompt, so a fully non-interactive invocation must state both its
archive policy and `--confirm`. Backup creation, local archive validation, or
destination installation must complete before deletion is attempted. The exact
selected archive is reported on stderr and can be checked with
`anyr backup list PATH --files`.

## Common options

These options apply to most commands.

<small>
<table>
  <tbody>
  <tr>
    <td><b>Category</b></td>
    <td><b>Args</b></td>
    <td><b>Description</b></td>
    <td><b>Environment default</b></td>
  </tr>
    <tr>
      <td></td>
      <td><code>-h</code>, <code>--help</code></td>
      <td>show context-specific help</td>
      <td></td>
    </tr>
    <tr>
      <td rowspan="2">Server endpoints</td>
      <td><code>--url URL</code></td>
      <td>HTTP endpoint. Default: <code>http://127.0.0.1:31012</code> (headless cli) when gRPC credentials are stored in the keystore, else <code>http://127.0.0.1:31009</code> (desktop app)</td>
      <td>ANYTYPE_URL</td>
    </tr>
    <tr>
      <td><code>--grpc URL</code></td>
      <td>Anytype CLI server gRPC endpoint. Default: <code>http://127.0.0.1:31010</code></td>
      <td>ANYTYPE_GRPC_ENDPOINT</td>
    </tr>
    <tr>
      <td rowspan="2">Key storage</td>
      <td><code>--keystore SPEC</code></td>
      <td>keystore spec, e.g., "file"</td>
      <td>ANYTYPE_KEYSTORE</td>
    </tr>
    <tr>
      <td><code>--keystore-service SVC</code></td>
      <td>service name, usually the app name</td>
      <td>ANYTYPE_KEYSTORE_SERVICE</td>
    </tr>
    <tr>
      <td rowspan="4">Output formatting</td>
      <td><code>--json</code></td>
      <td>json formatted output (the default)</td>
      <td></td>
    </tr>
    <tr>
      <td><code>--pretty</code></td>
      <td>json pretty-printed output</td>
      <td></td>
    </tr>
    <tr>
      <td><code>-t</code>, <code>--table</code></td>
      <td>table format</td>
      <td></td>
    </tr>
    <tr>
      <td><code>--date-format</code></td>
      <td>format for date columns (<em>strftime</em>)<br/>Default "%Y-%m-%d %H:%M:%S"</td>
      <td>ANYTYPE_DATE_FORMAT</td>
    </tr>
    <tr>
      <td rowspan="4">Search and list filters</td>
      <td><code>--filter KEY=VALUE</code></td>
      <td>apply filter condition(s)</td>
      <td></td>
    </tr>
    <tr>
      <td><code>--type TYPE</code></td>
      <td>apply type constraint(s)</td>
      <td></td>
    </tr>
    <tr>
      <td><code>--sort KEY</code></td>
      <td>sort on key</td>
      <td></td>
    </tr>
    <tr>
      <td><code>--desc</code></td>
      <td>sort descending</td>
      <td></td>
    </tr>
  </tbody>
</table>
</small>

## Examples

**List objects in a space**

```sh
# List <ENTITY> in a space. (entities: object, member, property, template)
# anyr <ENTITY> list <SPACE_ID_OR_NAME>

# list objects in space 'Personal'
anyr object list "Personal" -t

# list types in space 'Personal'
anyr type list "Personal" -t
```

**Search in space**

```sh
# search space "Work" for tasks containing the text "customer"
anyr search --space "Work" --type task --text customer -t
```

**Archived object cleanup**

These commands require a running Anytype CLI server and gRPC credentials.

```sh
space="Work"
anyr space count-archived "$space"
anyr space delete-archived "$space" --confirm
```

**List tasks in space**

```sh
space="Work" # specify a space by name or ID
for task in $(anyr search --type task --space "$space" --all | jq -r '.[].id'); do
  data=$(anyr object get "$space" "$task")
  status=$(jq -r '.properties[] | select (.key=="status") .select.name' <<< "$data")
  name=$(jq -r '.name' <<< "$data")
  # get created_date as YYYY-MM-DD
  created_date=$(jq -r '.properties[] | select (.key=="created_date") .date' <<< "$data" | sed 's/T.*$//')
  # generate formatted table with date, status, and name
  printf '%10s %-12s %s\n' "$created_date" "$status" "$name"
done
```

**Find files**

File list, search, and get require a running Anytype CLI server and gRPC
credentials.

```sh
# list images in space Personal, larger than 1MB with a name containing "report"
anyr file list "Personal" --file-type image --size-gte 1048576 --name-contains report -t

# list pdf or docx files in space Personal
anyr file list "Personal" --ext-in pdf,docx -t

# search files, sorted by a property (ascending by default; add --desc)
anyr file search "Personal" --text report --sort name -t
anyr file search "Personal" --sort last_modified_date --desc -t
```

**Upload files (unified builder picks REST or gRPC automatically)**

A plain path or stdin upload uses REST; `--url`, `--file-type`, `--style`,
`--details`, or a creation-context option selects gRPC and requires a running
Anytype CLI server plus gRPC credentials.

```sh
# REST: a plain path (optionally with an explicit --mime)
anyr file upload "Personal" -f ./report.pdf --mime application/pdf

# REST: read bytes from stdin (requires --name)
cat ./report.pdf | anyr file upload "Personal" --stdin --name report.pdf

# gRPC: fetch a remote URL with rich placement/details
anyr file upload "Personal" --url https://example.com/logo.png \
  --file-type image --style embed --details '{"source":"web"}' \
  --created-in-context <OBJECT_ID> --created-in-context-ref <BLOCK_ID>
```

`--http` on upload is a deprecated no-op; it errors when combined with a
gRPC-only option instead of silently dropping it.

**Preload a file for later placement**

Preload and discard-preload require a running Anytype CLI server and gRPC
credentials.

```sh
# preload returns a preload file id; source is either --file or --url
anyr file preload "Personal" -f ./draft.png --file-type image \
  --created-in-context <OBJECT_ID>
# preload a file fetched from a remote URL
anyr file preload "Personal" --url https://example.com/logo.png --file-type image
# discard a preload you no longer need
anyr file discard-preload "Personal" <PRELOAD_FILE_ID>
```

**Download with REST options and inspect metadata (HEAD)**

`anyr file download SPACE FILE_ID` fetches the bytes over REST in the anyr
process and emits `status`, `written`, `path`, `bytes`, and the HTTP `metadata`
fields as JSON. A `304 Not Modified` or a failed precondition leaves the
destination file untouched. REST is the only download path.

```sh
# REST download of a 128px image variant, only if the cache validator changed
anyr file download "Personal" <FILE_OBJECT_ID> \
  --dir /tmp --width 128 --if-none-match '"prev-etag"'

# ranged REST download
anyr file download "Personal" <FILE_OBJECT_ID> \
  --file /tmp/part --range bytes=0-499

# metadata only (HEAD): status + headers, no body written
anyr file metadata "Personal" <FILE_OBJECT_ID> --width 128
```

Verified against `anytype-cli` 0.3.6 (API `2025-11-08`): ranged downloads
(206), failed preconditions (412), and unsatisfiable ranges (416) all behave as
documented, but the server sends no `ETag` or `Last-Modified`, so
`--if-none-match` and `--if-modified-since` never produce a `304`.

**List items in query or collection**

```sh
# list queries in space. "$space" can be id ("bafy...") or name ("Projects")
anyr search --type set --space "$space" -t
# list collections in the space
anyr search --type collection --space "$space" -t
# from above, get id of query or collection of interest, then
# list items in query or collection, in view "All"
anyr view objects --view All "$space" "$query_or_collection_id" -t
```

**Get objects from a collection list or grid view**

```sh
# show names of all tasks in space "Work", using view 'All'
anyr view objects --view All "Work" Task -t

# show columns: Name, Created By, and Status (note: column names are specified by property_key)
anyr view objects --view All "Work" Task --columns name,creator,status

# get tasks from view ByProject in json, with all properties
anyr view objects --view ByProject "Work" Task --json
```

**List objects for a view (list/collection)**

```sh
# --view is required; it identifies which view's objects to return
anyr list objects "Work" $collection_id --view All -t
```

**Update a property**

```sh
# rename a property
anyr property update "Work" Status --name "Task Status"

# change only the key; the current name is reused automatically
anyr property update "Work" Status --key task_status
# (an update with neither --name nor --key is rejected)
```

**Update a type's property list**

```sh
# merge a property into the exact non-featured list (HTTP plus CLI/gRPC required)
anyr type update "Work" Task --add-property Status

# replace the complete non-featured property list (KEY:FORMAT:NAME, repeatable)
anyr type update "Work" Task --set-property status:select:Status --set-property due:date:Due

# remove all non-featured recommended properties
anyr type update "Work" Task --clear-properties
# (--add-property, --set-property, and --clear-properties are mutually exclusive)
```

`--add-property` reads Anytype's source-backed featured and recommended
property lists, then resubmits only the non-featured recommended list plus the
requested additions. Repeated keys are de-duplicated deterministically, keeping
the existing property first. The REST definition and gRPC source lists are not
an atomic snapshot, so a concurrent type edit can make the command fail closed;
retry the complete command after the other edit settles.

If you have a list or grid formatted view, you can use `view objects` to list the view items by specifying the space name, list, and view.

- Results are filtered and sorted by the criteria in the view.
- View can be specified by the view id or view name.
- The --json and --pretty format outputs include all properties of the objects.

Table listing features for `view objects`:

- Table listing defaults to the name column only. Specify columns in table
  output with `--columns` and a comma-separated list of property keys. Example:
  `--columns name,creator,created_date,status`.
- Format dates with strftime format: `--date-format` or `ANYTYPE_DATE_FORMAT`, defaults to `%Y-%m-%d %H:%M:%S`.
- Members names are displayed instead of member id.

**Chat transport**

Chat commands accept `anyr chat --transport auto|rest|grpc <command>` (default
`auto`). This is scoped to chat operations and is separate from the root
`--grpc URL` endpoint option. It selects which backend each operation runs over.

- `auto` resolves each operation to its policy backend: REST for single-space
  `list` and `create`, plain message `send` and `edit`, message `search` and
  `react`, `read`/`read-reactions`/`read-all` in one space, and a single-chat
  `listen` (REST SSE); gRPC for cross-space list, chat text search, rich
  chat-object `get`, message `list`/`get`/`delete`, `unread`, structured
  `--blocks-json` sends and edits, and multi-chat, `--previews`, or `--buffer`
  `listen`.
- `rest` rejects gRPC-only operations and options with an actionable error (for
  example `anyr chat --transport rest get ...` explains that rich chat-object
  lookup requires gRPC, message `list`/`get`/`delete` report their gRPC
  requirement, and `--blocks-json` cannot be combined with it).
- `grpc` selects the gRPC backend, which carries the full-fidelity 0.4 message
  reply shape. It requires a running Anytype CLI server and gRPC credentials.
  REST-only operations such as message `search` reject it.

The resolved backend is reported only in verbose diagnostics (`-v`), never
injected into the JSON payload. Because REST replies intentionally contain fewer
fields (no `ChatState` or structured blocks), pick `--transport grpc` when a
script needs the full reply shape.

A chat name still needs a running Anytype CLI server and gRPC credentials for
name resolution. Pass an exact chat ID when a REST-backed command must remain
HTTP-only.

**Chat listing, creation, and messages**

- `anyr chat list --space SPACE [--filter FILTER]...` applies property filters to
  a single-space REST listing. `--filter` requires `--space` and is rejected
  alongside `--text` (text search uses the gRPC discovery API).
- `anyr chat list ... --all` and `anyr chat messages search ... --all` exhaust
  the server's pages. Pagination that claims another page without usable
  progress fails closed. All `--all` requests share one 30-minute workflow
  deadline, including page requests and retry waits. Set
  `ANYR_WORKFLOW_TIMEOUT_SECS` to canonical decimal seconds up to 3600, or to
  exactly `0` to disable only this aggregate boundary.
- `anyr chat create SPACE NAME [--icon-emoji EMOJI | --icon-file FILE]` attaches
  an icon; the two icon options are mutually exclusive. With an icon under REST
  the dedicated chat builder is used; otherwise the generic object create is.
- `anyr chat messages send ... [--reply-to MESSAGE] [--blocks-json JSON_OR_@FILE]`
  and `anyr chat messages edit ... [--attachment TYPE:TARGET]... [--blocks-json
  JSON_OR_@FILE]`. `--reply-to` works over both REST and gRPC sends. On `edit`,
  the supplied `--attachment` values are the complete replacement list.
  `--blocks-json` takes a JSON array of `MessageBlock` values, selects gRPC, and
  is mutually exclusive with `--transport rest`.
- `anyr chat messages search SPACE CHAT QUERY [pagination]` is REST-only and
  preserves the server's search-result envelope.
- `anyr chat messages react SPACE CHAT MESSAGE EMOJI` toggles a reaction (REST
  under `auto`; gRPC additionally reports the resulting on/off state).

**Chat read state and streams**

- `anyr chat read SPACE CHAT` maps to the space-scoped REST read builder under
  REST; `anyr chat read-reactions SPACE CHAT [--order-id ORDER]` and `anyr chat
  read-all SPACE CHAT` map to their dedicated REST operations. `anyr chat unread`
  stays gRPC-only.
- `anyr chat listen --chat CHAT --space SPACE [--initial-limit N] [--heartbeat
  SECONDS]` streams one chat over REST SSE with initial-message replay and
  heartbeat controls. `anyr chat listen --chat CHAT... [--previews] [--buffer N]
  [--include-history N] [--after ORDER]` uses the reconnecting gRPC listener,
  which remains the choice for multiple chats, cross-chat previews, and catch-up
  watermarks.

**Chat order ids**

Chat message order ids are converted to lowercase hex before display in table-format output, to make them easier to read and type, while preserving lexicographic order. Any argument that accepts an order id also accepts the hex form. Example: the order id `!!@,` is displayed as `2121402c`, and you can pass `2121402c` back to commands that accept an order id.

## Install

Release binaries are on [github](https://github.com/stevelr/anytype/tags)

**macOS (arm64)**

```sh
brew install stevelr/tap/anyr
```

The Homebrew formula and release archive install the portable binary built by
the repository flake, signed with an Apple Developer ID certificate, and
notarized by Apple. Its system-library install names work without Nix.

**Linux (arm64/x86_64)**

Download the archive for your architecture and its checksum from the
[releases page](https://github.com/stevelr/anytype/releases). Verify the
download against the published checksum, then extract `anyr` to a directory on
`PATH`. Linux release archives contain the fully static musl binary built by
the repository flake.

GitHub CLI can also verify that the finalization workflow attested the exact
archive from the selected release tag:

```sh
gh attestation verify ARCHIVE \
  --repo stevelr/anytype \
  --signer-workflow stevelr/anytype/.github/workflows/finalize-release.yml \
  --source-ref refs/tags/RELEASE_TAG \
  --deny-self-hosted-runners
```

**Windows Powershell**

Download the Windows archive and its checksum from the
[releases page](https://github.com/stevelr/anytype/releases). Verify the
download against the published checksum, then place `anyr.exe` in a directory
on `PATH`.

**Cargo**

```sh
cargo install anyr
```

### Shell completions

Generate and load completions for your current shell session:

```sh
# Bash
source <(anyr completions bash)

# Zsh, after compinit
source <(anyr completions zsh)

# Fish
anyr completions fish | source

# PowerShell
anyr completions powershell | Out-String | Invoke-Expression
```

Add the command for your shell to its startup file to load completions in future
sessions.

## Build from source

**Cargo**

Requirements:

- protoc (from the protobuf package) in your PATH. On macos, `brew install protobuf`
- libgit2 in your library path.

```sh
cargo install --path anyr
```

**Nix**

```sh
nix build
```

## Configure

Configuration can be set with command-line parameters or environment variables.

- **Url** Override with `--url` or the environment variable `ANYTYPE_URL`. When
  neither is set, `anyr` picks a default: `init-cli` always targets the headless
  cli server `http://127.0.0.1:31012`; every other command targets the headless
  server when gRPC credentials are already stored in the selected keystore
  (meaning the Anytype CLI is in use), and otherwise the desktop app
  `http://127.0.0.1:31009`.

- **Key Storage** The default key storage method should work on most platforms. Options for overriding the defaults are described below in [Key storage](#key-storage).

```sh
# use headless server and custom key path
anyr --url "http://127.0.0.1:31012" --keystore "file:path=$HOME/.config/anytype/apikeys.db" ARGS ...

# custom endpoint url and key path in environment
export ANYTYPE_URL=http://127.0.0.1:31012
export ANYTYPE_KEYSTORE="file:path=$HOME/.config/anytype/apikeys.db"
anyr ARGS ...
```

### Generating and saving credentials

- **Desktop**: If the Anytype desktop app is running, type `anyr auth login` and the app will display a 4-digit code. Enter the code into the anyr prompt, and a key is generated and stored in the KeyStore.

- **Headless server**: Start the server, then run `anyr init-cli`. If the
  default Anytype CLI config at `~/.anytype/config.json` exists, the command
  reuses its validated `accountId` and `accountKey`, creates a fresh HTTP
  token, and stores both credential families directly in the selected `anyr`
  keystore without displaying either credential. It derives gRPC sessions
  from the account key instead of retaining the config's session token. This
  preserves the server's existing account and spaces. An unreadable,
  malformed, or incomplete config stops initialization; only a missing config
  permits `anytype auth create` to create a new account. If the config exists
  but has no `accountKey` (for example on macOS or desktop Linux, where the
  Anytype CLI keeps the key in the OS keychain), the error explains the two
  ways forward: enter the key with `anyr auth set-grpc --account-key` (or
  `--bip39`), or run `anyr init-cli --force` to ignore the existing config and
  create a new account. Set
  `ANYTYPE_CLI_BIN` to an alternate executable path when `anytype` is not on
  `PATH`. A new account is named `bot_<timestamp>`; set `ANY_USER` to choose
  it explicitly. To join a space during setup, use `anyr init-cli --join
  "$INVITE_LINK"`.

  Unless overridden globally with `--url` / `--grpc` or
  `ANYTYPE_URL` / `ANYTYPE_GRPC_ENDPOINT`, `init-cli` uses the headless
  endpoints `http://127.0.0.1:31012` and `http://127.0.0.1:31010`. Those
  effective endpoints are also passed to every Anytype CLI subprocess. After
  storing the pair, `init-cli` verifies authenticated HTTP and gRPC access
  before reporting success. A later verification or join failure leaves the
  newly initialized credentials stored so the operator can retry without
  losing them.

  `--save-env FILE` additionally writes the effective endpoints, keystore
  service, HTTP token, gRPC account key, and
  `ANYTYPE_TEST_SPACE_PREFIX=xtest` as quoted POSIX shell `export` assignments.
  The file can be sourced directly without `set -a`. The `xtest` prefix
  authorizes disposable integration tests to create and remove spaces whose
  names begin with that value. `FILE` must be a filesystem path; `-` does not
  select stdout because stdout remains the command result channel. The file is
  created with mode `0600` on Unix and the command refuses to replace an
  existing destination. It contains credentials in plaintext; keep it outside
  shared directories and configuration repositories. If file creation fails,
  the initialized credentials may already be in the selected keystore.
  Credential values remain absent from normal command output and errors.
  Subprocess failures identify the Anytype CLI operation; child output remains
  withheld, while platform-specific exit-status text is diagnostic detail.
  Credential-bearing output is captured under a fixed byte limit.

  Initialization shares one 120-second deadline across account setup, token
  creation, verification, and an optional join. Each owned child process has a
  30-second safety limit that includes output collection and exit waiting. A
  timeout terminates and reaps the direct child. On Unix it also signals
  descendants that remain in the child's owned process group; descendants that
  deliberately call `setsid` or `setpgid` after spawn are outside that boundary.
  On Windows the child starts suspended and is assigned to a kill-on-close Job
  Object before its code runs, so its descendants remain in the owned job. A
  dispatched operation has an indeterminate server outcome. Set
  `ANYR_INIT_CLI_TIMEOUT_SECS` to canonical
  decimal seconds from 1 through 600. This deadline cannot be disabled.

The global `--keystore` and `--keystore-service` options (or their environment
variables) select where `init-cli` stores credentials. See the
[keystore reference](https://docs.anytype-toolbox.org/reference/keystores/).
Because the keystore API cannot atomically replace both credential families,
`init-cli` snapshots both prior credential objects, replaces gRPC first, then
writes HTTP. If either write fails, it makes independent best-effort attempts
to restore both snapshots and reports any failed restoration without displaying
credential values.

## Logging

Debug logging

```sh
RUST_LOG=debug anyr space list -t
```

Log HTTP requests and responses:

```sh
RUST_LOG=warn,anytype::http_json=trace anyr space list -t
```

## Testing

Python CLI tests expect the same environment variables as the API tests:

- `ANYTYPE_TEST_URL` (or `ANYTYPE_URL`)
- `ANYTYPE_TEST_KEY_FILE` (or `ANYTYPE_KEY_FILE`)
- `ANYTYPE_TEST_SPACE_PREFIX`
- `ANYBACK_HEADLESS_REDACTED_LOG_FILE` (absolute reviewed redacted server log,
  required by destructive space-deletion cases)

```sh
source .test-env
python tests/cli_commands.py
```

Run the live suite as one process; Python `unittest` executes its cases serially,
and it must not overlap another mutation suite. The guarded space-deletion cases
first require healthy HTTP and gRPC pings, then cover prompted cancellation and
exact-name confirmation, non-interactive backup-before-delete to an exact path,
archive validation through `anyr backup list`, and backup-failure preservation.

The real-operation case uses the shared disposable-space guard for its uniquely
named, prefix-owned space, so setup and assertion failures still enter cleanup.
Mutation cases do not consider cleanup complete until `space get` returns the
explicit not-found outcome. Transport and server failures fail cleanup instead
of being treated as proof of deletion, and diagnostic lines from `RUST_LOG` may
precede the not-found message.

The protected `anyr-anyback-live` workflow installs `anyr` and serializes three
required subgates: the exact ignored type-property test, the manifest-pinned
Python CLI suite, and one exact backup create/restore smoke test. Each subgate
rejects skips and zero-test collection. The workflow requires authenticated HTTP
and gRPC pings, disposable-process admission, a unique safe space prefix, and a
reviewed redacted server log. Its smoke test creates and removes its own source
and destination spaces. The workflow is started manually while CI qualification
is in progress. Ordinary offline workspace tests do not run these server-backed
targets.

## License

Apache License, Version 2.0
