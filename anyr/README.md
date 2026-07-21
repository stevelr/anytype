# anyr

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=anyr-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anyr-v&expanded=true)
[![crates.io](https://img.shields.io/crates/v/anyr.svg)](https://crates.io/crates/anyr)

List, search, and manipulate anytype objects from the command-line

Homepage: https://github.com/stevelr/anytype

```sh
# show options
anyr --help

# check authentication status (HTTP + gRPC)
anyr auth status
# authenticate with desktop and http endpoint
anyr auth login

# List spaces your user is authorized to access
anyr space list -t     # output as table (-t/--table)

# Count or delete archived objects in a space
anyr space count-archived "Work"
anyr space delete-archived "Work" [ --confirm ]

# List Pages in space "Work"
anyr object list "Work" --type page -t

# List Files in a space (requires gRPC credentials)
anyr file list "Personal" -t

# Download/upload file bytes
# use `--dir DIR`  to set download dir, or `--file PATH` for destination file path
anyr file download <FILE_OBJECT_ID> --dir /tmp
anyr file upload "Personal" -f ./path/to/file.png

# Create a chat in a regular space
anyr chat create "Work" "Ops"
# Get chat messages from a chat in a space
anyr chat messages list "Work" "Ops" -t
# Post message
anyr chat messages send "Work" "Ops" --text "hello world?"
```

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
      <td>HTTP endpoint. Default: <code>http://127.0.0.1:31009</code></td>
      <td>ANYTYPE_URL</td>
    </tr>
    <tr>
      <td><code>--grpc URL</code></td>
      <td>gRPC endpoint. Default: <code>http://127.0.0.1:31010</code></td>
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
anyr search --space "Work" --type Task --text customer -t
```

**Archived object cleanup**

```sh
space="Work"
anyr space count-archived "$space"
anyr space delete-archived "$space" --confirm
```

**List tasks in space**

```sh
space="Work" # specify space using name or id
for task in `anyr search --type Task --space $space --json | jq -r '.items[] | .id`; do
  data=$(anyr object get $space $task --json)
  status=$(jq -r '.properties[] | select (.key=="status") .select.name' <<< "$data")
  name=$(jq -r '.name' <<< "$data")
  # get created_date as YYYY-MM-DD
  created_date=$(jq -r '.properties[] | select (.key=="created_date") .date' <<< "$data" | sed 's/T.*$//')
  # generate formatted table with date, status, and name
  printf "%10s %-12s %s" $created_date $status $name
done
```

**Find files**

```sh
# list images in space Personal, larger than 1MB with a name containing "report"
anyr file list "Personal" --type image --size-gte 1048576 --name-contains report -t

# list pdf or docx files in space Personal
anyr file list "Personal" --ext-in pdf,docx -t

# search files, sorted by a property (ascending by default; add --desc)
anyr file search "Personal" --text report --sort name -t
anyr file search "Personal" --sort last_modified_date --desc -t
```

**Upload files (unified builder picks REST or gRPC automatically)**

A plain path or stdin upload uses REST; `--url`, `--file-type`, `--style`,
`--details`, or a creation-context option promotes the request to gRPC.

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

**Preload a file for later placement (gRPC)**

```sh
# preload returns a preload file id; --url is not yet supported (file only)
anyr file preload "Personal" -f ./draft.png --file-type image \
  --created-in-context <OBJECT_ID>
# discard a preload you no longer need
anyr file discard-preload "Personal" <PRELOAD_FILE_ID>
```

**Download with REST options and inspect metadata (HEAD)**

The REST download (`--http --space SPACE`) emits `status`, `written`, `path`,
`bytes`, and the HTTP `metadata` fields as JSON. A `304 Not Modified` or a
failed precondition leaves the destination file untouched.

```sh
# REST download of a 128px image variant, only if the cache validator changed
anyr file download <FILE_OBJECT_ID> --http --space "Personal" \
  --dir /tmp --width 128 --if-none-match '"prev-etag"'

# ranged REST download
anyr file download <FILE_OBJECT_ID> --http --space "Personal" \
  --file /tmp/part --range bytes=0-499

# metadata only (HEAD): status + headers, no body written
anyr file metadata "Personal" <FILE_OBJECT_ID> --width 128
```

**List items in query or collection**

```sh
# list queries in space. "$space" can be id ("bafy...") or name ("Projects")
anyr search --type set --space $space -t
# list collections in the space
anyr search --type collection --space $space -t
# from above, get id of query or collection of interest, then
# list items in query or collection, in view "All"
anyr view objects --view All $space $query_or_collection_id -t
```

**Get objects from a collection list or grid view**

```sh
# show names of all tasks in space "Work", using view 'All'
anyr view objects --view All "Work" Task -t

# show columns: Name, Created By, and Status (note: column names are specified by property_key)
anyr view objects --view All "Work" Task --cols name,creator,status

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
# merge a property into the current list (read/merge)
anyr type update "Work" Task --add-property Status

# replace the complete non-featured property list (KEY:FORMAT:NAME, repeatable)
anyr type update "Work" Task --set-property status:select:Status --set-property due:date:Due

# remove all non-featured recommended properties
anyr type update "Work" Task --clear-properties
# (--add-property, --set-property, and --clear-properties are mutually exclusive)
```

If you have a list or grid formatted view, you can use `view objects` to list the view items by specifying the space name, list, and view.

- Results are filtered and sorted by the criteria in the view.
- View can be specified by the view id or view name.
- The --json and --pretty format outputs include all properties of the objects.

Table listing features for `view objects`:

- Table listing defaults to name column only. Specify columns in table output with `--cols/--columns` and a comma-separated list of property keys. Example `--cols name,creator,created_date,status`
- Format dates with strftime format: `--date-format` or `ANYTYPE_DATE_FORMAT`, defaults to `%Y-%m-%d %H:%M:%S`.
- Members names are displayed instead of member id.

**Chat transport**

Chat commands accept `anyr chat --transport auto|rest|grpc <command>` (default
`auto`). This is scoped to chat operations and is separate from the root
`--grpc URL` endpoint option. It selects the transport *policy* for each
operation: which backend the operation is intended to use.

- `auto` resolves each operation to its policy backend: REST for single-space
  `list` and `create`, plain message `list`/`get`/`send`/`edit`/`delete` and
  `read` in one space, and a single-chat `listen` (REST SSE); gRPC for
  cross-space list, chat text search, rich chat-object `get`, `unread`, and
  multi-chat `listen`.
- `rest` rejects gRPC-only operations with an actionable error (for example
  `anyr chat --transport rest get ...` explains that rich chat-object lookup
  requires gRPC).
- `grpc` selects the gRPC policy, which carries the full-fidelity 0.4 message
  reply shape.

The resolved policy backend is reported only in verbose diagnostics (`-v`),
never injected into the JSON payload. Note that the `rest` rejection guard is
the only part of the policy enforced today: the handlers currently dispatch
`create` and single-space plain `list` over REST and every other operation over
gRPC. Rerouting the REST-capable message/read/listen operations onto the REST
backend is staged for follow-up work, so `auto` and `grpc` do not yet change
which backend an operation executes over.

**Chat order ids**

Chat message order ids are converted to lowercase hex before display in table-format output, to make them easier to read and type, while preserving lexicographic order. Any argument that accepts an order id also accepts the hex form. Example: the order id `!!@,` is displayed as `2121402c`, and you can pass `2121402c` back to commands that accept an order id.

## Install

Release binaries are on [github](https://github.com/stevelr/anytype/tags)

**Macos via Homebrew**

```sh
brew install stevelr/tap/anyr
```

**Linux (arm64/x86_64)**

```sh
curl -fsSL https://github.com/stevelr/anytype/releases/latest/download/anyr-installer.sh | sh
```

**Windows Powershell**

```sh
irm https://github.com/stevelr/anytype/releases/latest/download/anyr-installer.ps1 | iex
```

**Cargo**

```sh
cargo install -p anyr
```

## Build from source

**Cargo**

Requirements:

- protoc (from the protobuf package) in your PATH. On macos, `brew install protobuf`
- libgit2 in your library path.

```sh
cargo install -p anyr
```

**Nix**

```sh
nix build
```

## Configure

Configuration can be set with command-line parameters or environment variables.

- **Url** The default url is the desktop client `http://127.0.0.1:31009`. Override with `--url` or the environment variable `ANYTYPE_URL`.

- **Key Storage** The default key storage method should work on most platforms. Options for overriding the defaults are described below in [Key storage](#key-storage).

```sh
# use headless server and custom key path
anyr --url "http://127.0.0.1:31012" --keystore "file:path=$HOME/.config/anytype/apikeys.db" ARGS ...`

# custom endpoint url and key path in environment
export ANYTYPE_URL=http://127.0.0.1:31012
export ANYTYPE_KEYSTORE="file:path=$HOME/.config/anytype/apikeys.db"
anyr ARGS ...
```

### Generating and saving credentials

- **Desktop**: If the Anytype desktop app is running, type `anyr auth login` and the app will display a 4-digit code. Enter the code into the anyr prompt, and a key is generated and stored in the KeyStore.

- **Headless server**: If you are using the headless cli server, start the server, run `anytype auth apikey create anyr` to generate and display a key, then either:
  - paste it into `anyr auth set-http` (reads from stdin), or
  - save it in a file and set the key file path as described in [Key storage](#key-storage).

See [anytype README.md](../anytype-api/README.md#keystore) for more info, and the helper script [init-cli-keys.sh](../scripts/init-cli-keys.sh) for generating and saving http and gRPC credentials.

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
- `ANYTYPE_TEST_SPACE_ID`

```sh
source .test-env
python tests/cli_commands.py
```

## License

Apache License, Version 2.0
