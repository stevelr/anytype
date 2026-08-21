---
name: anyr
description: Use when reading, searching, creating, or updating Anytype documents (objects, spaces, types, properties, files, chats) from the command line with the configured anyr CLI, including JSON output patterns for scripting
---

# Anytype CLI (anyr)

`anyr` lists, searches, and manipulates Anytype objects through the local
Anytype HTTP and gRPC APIs. Common object, schema, search, collection, Markdown,
and byte-transfer operations use REST. Rich file discovery, typed body blocks,
archived-object cleanup, space administration, backup/restore, and some chat
operations require gRPC. The complete CLI documentation is available in the
[Anytype Toolbox repository](https://github.com/stevelr/anytype/tree/main/anyr).

*Tool choice*: Use this skill for explicit CLI workflows, unavailable MCP
tools, and documented CLI-only fallbacks. Use `anytype-setup` to install
`anyr`, authenticate, select or change endpoints, or recover a backend. When
connected any-mcp tools advertise the required capability, use the any-mcp
skill instead.

## Prerequisites

The `anyr` executable must be on `PATH`, and a reachable Anytype desktop or
headless service must have valid endpoint-specific credentials. The examples
use `jq` for JSON processing. OpenSSL is optional and appears only in the
encrypted file-keystore example.

## Mental model

- Everything lives in a **space**. Commands take a space **name or id**
  (`"Work"` or `bafy...`) interchangeably.
- A "document" is an **object**: it has a `type` (page, task, note, bookmark,
  ...), a `name`, typed `properties`, and a **markdown `body`**.
- Object content round-trips as markdown: `object get` returns it, `object
  create/update` accept `--body` / `--body-file`.
- Type keys, property keys, and tag names are per-space. Discover them with
  `anyr type list SPACE`, `anyr property list SPACE`, `anyr tag list ...`.

## Connection prerequisite

For installation, authentication, endpoint selection, keystore configuration,
or recovery, use `anytype-setup`. Do not probe both transports before every
command. Run an existing configured HTTP command directly; invoke setup only
after a relevant connection or authentication failure. Before a command marked
**CLI + gRPC**, ensure the selected connection is headless. Preserve saved
credentials when a configured backend is temporarily unavailable.

## Reading

```sh
anyr space list -t                             # spaces you can access
anyr object list "Work" --type page -t         # list pages (table)
anyr object get "Work" OBJECT_ID               # full object incl. markdown body (json)
anyr search --space "Work" --type Task --text customer -t   # text search
anyr search --type collection --space "Work" -t             # find collections/queries
anyr view objects --view All "Work" LIST_ID --columns name,creator,status
anyr type list "Work" -t                       # discover type keys
anyr file list "Personal" --ext-in pdf,docx -t # CLI + gRPC
```

- Output: `--json` (default), `--pretty`, `-t/--table`.
- List/search share `--filter KEY=VALUE` (repeatable), `--sort KEY`, `--desc`,
  and pagination (`--limit/--offset`, or `--all` to collect every page).
- Search without `--space` is global across all spaces.

Extract the markdown body of a document:

```sh
anyr object get "Work" $id | jq -r .markdown
```

Only single-object `get` includes `.markdown`; list/search results omit the
body (`markdown` is null). Fetch each object by id when you need content.

## Command coverage and transport boundaries

**CLI + gRPC** means the command needs a running Anytype CLI server and gRPC
credentials in the selected keystore.

- `space list|get|create|update` use REST. `space create --chat`,
  `count-archived`, `delete-archived`, `delete`, `invite`, `enable-sharing`,
  and `disable-sharing` are **CLI + gRPC**.
- `object list|get|link|create|update|delete` and global or space-scoped
  `search` use REST. `object discussion get|attach` require REST plus
  **CLI + gRPC**.
- `body list|show|create|update|delete|move` are **CLI + gRPC**. Mutations use
  closed JSON input and verify the resulting block graph.
- `type`, `property`, `tag`, `template`, `member`, `list`, and `view` use REST.
  `type update --add-property` additionally reads gRPC property-source lists.
- `file list|search|get|preload|discard-preload` are **CLI + gRPC**. File byte
  upload, download, metadata, update, and delete use REST unless an upload
  option listed in [Files](#files) selects gRPC.
- `md get|update|edit` use REST.
- `backup create|export` are **CLI + gRPC**. `backup restore|import` require
  gRPC unless `--dry-run` is used. `list`, `manifest`, `diff`, `extract`, and
  `inspect` operate on local archives.
- `completions` is local. `mcp init` and `mcp check` maintain the embedded MCP
  configuration; `mcp` transport needs depend on the configured tool catalog.

## Creating

```sh
# a page with markdown content
anyr object create "Work" page --name "Meeting notes" --body-file notes.md

# inline body, emoji icon, properties (key=value; repeat -p or positional)
anyr object create "Work" task --name "Fix login" \
  --icon-emoji "🐛" -p status="In Progress" -p priority=High

# from a template / a bookmark
anyr object create "Work" page --name "Weekly" --template TEMPLATE_ID
anyr object create "Work" bookmark --url https://example.com
```

Type key must already exist in the space (`anyr type list SPACE`). Create
returns the new object as JSON. Capture `.id` for follow-up edits.

## Updating and deleting

```sh
anyr object update "Work" OBJECT_ID --name "New title"
anyr object update "Work" OBJECT_ID --body-file revised.md   # replaces body
anyr object update "Work" OBJECT_ID -p status=Done
anyr object update "Work" OBJECT_ID --type task              # change type
anyr object delete "Work" OBJECT_ID                          # moves to archive
```

`--body`/`--body-file` **replace** the whole markdown body. For edit-in-place
of a section: `object get` → modify the markdown locally → `object update
--body-file`.

Bulk archive cleanup:

```sh
anyr space count-archived "Work"
anyr space delete-archived "Work" --confirm
```

## Files

File bytes transfer over REST. Rich file discovery and placement use gRPC.

```sh
anyr file upload "Personal" --file ./report.pdf          # local file (REST)
anyr file upload "Personal" --url https://x/y.png        # CLI + gRPC
anyr file download "Personal" FILE_ID --dir /tmp         # SPACE is a required positional (or --file PATH)
anyr file metadata "Personal" FILE_ID                    # HTTP HEAD: status + headers, no body
anyr file delete "Personal" FILE_ID                      # to bin
anyr file delete "Personal" FILE_ID --permanent          # skip the bin
anyr file search "Personal" --sort last_modified_date --desc -t # CLI + gRPC
```

- Breaking (vs older skill): `file download` takes `SPACE` as a leading
  positional and always uses REST; the old `--http`/`--space` flags are gone.
  `file delete` no longer accepts `--http` (use `--permanent` to skip the bin).
- gRPC-only upload options: `--url`, `--file-type`, `--style`, `--details`,
  `--created-in-context`, and `--created-in-context-ref`. These options and
  `file list|search|get|preload|discard-preload` require a running Anytype CLI
  server and gRPC credentials.

## Chat

Chat operations pick a transport: `anyr chat --transport auto|rest|grpc <cmd>`
(default `auto`). Under `auto`, each operation follows the transport policy
below. Passing `--transport grpc` requires a running Anytype CLI server and
gRPC credentials.

```sh
anyr chat list --space "Work" --filter status=open -t         # space-scoped list (REST)
anyr chat messages list "Work" "$CHAT_ID" -t                  # CLI + gRPC
anyr chat messages send "Work" "$CHAT_ID" "hi" --reply-to "$MSG_ID" # REST
anyr chat messages search "Work" "$CHAT_ID" "invoice"        # REST full-text search
anyr chat messages react "Work" "$CHAT_ID" "$MSG_ID" 👍      # REST reaction toggle
anyr chat read-all "Work" "$CHAT_ID"                          # REST
anyr chat listen --chat "$CHAT_ID" --space "Work" --initial-limit 20 # REST SSE
```

- **CLI + gRPC** chat: cross-space or `--text` chat `list`, rich `get`, message
  `list|get|delete`, `unread`, `--blocks-json` send/edit, and listeners without
  `--space`, with multiple `--chat` values, or with `--include-history`,
  `--after`, `--previews`, or `--buffer`. `--transport rest` rejects these
  before network access.
- A chat name or order ID also needs gRPC resolution. Use exact chat and
  message IDs to keep an otherwise REST-backed command HTTP-only.
- `chat create` accepts `--icon-emoji` / `--icon-file`.

## Scripting patterns

Chain with `jq`; every command emits clean JSON on stdout:

```sh
space="Work"
for id in $(anyr search --type Task --space "$space" --json | jq -r '.items[].id'); do
  data=$(anyr object get "$space" "$id" --json)
  name=$(jq -r '.name' <<<"$data")
  status=$(jq -r '.properties[] | select(.key=="status") | .select.name' <<<"$data")
  printf "%-12s %s\n" "$status" "$name"
done
```

- Property values in JSON output are keyed by format:
  `.select.name` (select), `.date` (dates), `.text`, `.number`, etc.
- Dates format with `--date-format` (strftime) or `ANYTYPE_DATE_FORMAT`.
- Chat order ids display as lowercase hex in tables; the hex form is accepted
  back anywhere an order id is expected.
- Debug HTTP traffic: `RUST_LOG=warn,anytype::http_json=trace anyr ...`

## When not to use the CLI

One-shot reads/writes and small scripted loops: use `anyr`. Bulk scans or
multi-document rewrites that need state across many calls are better served by
the Anytype MCP server (the persistent `any-mcp` HTTP service, when
available; never start `anyr mcp` yourself) or the `anytype` Rust crate
directly.

## Gotchas

- `auth set-*` reads the secret from standard input. Pipe it in
  (`printf '%s\n' "$TOKEN" | anyr auth set-http`) in non-interactive runs.
  `init-cli` is the exception to the otherwise in-process command surface: it
  invokes the configured Anytype CLI executable to provision credentials.
- Space arguments accept names, but names are not unique. Prefer ids in
  scripts that must not touch the wrong space.
- After starting a server (desktop or headless), wait ~30 s before issuing
  commands; early requests fail spuriously.
