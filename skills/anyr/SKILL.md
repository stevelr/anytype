---
name: anyr
description: Use when reading, searching, creating, or updating Anytype documents (objects, spaces, types, properties, files, chats) from the command line with the anyr CLI - includes auth token setup for desktop and headless servers, keystore configuration, and JSON output patterns for scripting
---

# Anytype CLI (anyr)

`anyr` lists, searches, and manipulates Anytype objects over the local Anytype
HTTP/REST API. Most work — including file download/upload and everyday chat —
runs over REST; only advanced operations fall back to gRPC (see below). Source
and full README: `~/project/anytype/anyr/`.

## Mental model

- Everything lives in a **space**. Commands take a space **name or id**
  (`"Work"` or `bafy...`) interchangeably.
- A "document" is an **object**: it has a `type` (page, task, note, bookmark,
  ...), a `name`, typed `properties`, and a **markdown `body`**.
- Object content round-trips as markdown: `object get` returns it, `object
  create/update` accept `--body` / `--body-file`.
- Type keys, property keys, and tag names are per-space. Discover them with
  `anyr type list SPACE`, `anyr property list SPACE`, `anyr tag list ...`.

## Setup and auth

### Endpoints

| Env var | Default | Notes |
|---|---|---|
| `ANYTYPE_URL` | `http://127.0.0.1:31009` | desktop app. Headless cli server: `31012` |
| `ANYTYPE_GRPC_ENDPOINT` | `http://127.0.0.1:31010` | only for gRPC-only ops: advanced file upload / preload, history-rich or multi-chat `chat listen`, cross-space chat |

**Tokens are endpoint-specific**: a token minted for the desktop URL does not
work against the headless server, and vice versa.

### Keystore

Tokens are stored in a keystore selected by `ANYTYPE_KEYSTORE`
(default: OS keyring — `keyutils` on linux, `keyring` on macos, `windows` on
windows). For agent/headless use prefer one of:

```sh
# file (sqlite) keystore — persistent, no OS approval pop-ups
export ANYTYPE_KEYSTORE=file                 # default path
export ANYTYPE_KEYSTORE=file:path=$HOME/.config/anytype/apikeys.db
# optional at-rest encryption:
#   file:cipher=aegis256:hexkey=$(openssl rand -hex 32)

# env keystore — nothing persisted; token comes from the environment
export ANYTYPE_KEYSTORE=env
export ANYTYPE_KEY_HTTP_TOKEN="$TOKEN"       # http auth token
# gRPC (only if needed): ANYTYPE_KEY_ACCOUNT_KEY or ANYTYPE_KEY_SESSION_TOKEN
```

`ANYTYPE_KEYSTORE_SERVICE` namespaces entries per app; set it to `anyr` so
other tools (any-edit, the anytype crate) can share the same tokens:

```sh
export ANYTYPE_KEYSTORE_SERVICE=anyr
```

### Getting a token into the keystore

Pick one:

1. **Desktop, interactive** (needs a human at the app):
   `anyr auth login` — the app shows a 4-digit code, type it at the prompt.
2. **Headless server** (anytype-cli): generate a key, then store it:
   ```sh
   anytype auth apikey create anyr        # prints a token
   echo "$TOKEN" | anyr auth set-http     # reads token from stdin
   ```
3. **Pre-provisioned token, no persistence**: use the `env` keystore above —
   no `auth set-http` step needed.

gRPC credentials (only for the gRPC-only ops above — advanced upload/preload,
history-rich/multi-chat `listen`, cross-space chat):
`anyr auth set-grpc --config ~/.anytype/config.json` (headless server's
accountKey/sessionToken), or `--account-key` / `--token` / `--bip39` read from
stdin. See `~/project/anytype/scripts/init-cli-keys.sh` for full headless
bootstrap.

### Verify before doing anything else

```sh
anyr auth status | jq .ping
# want: {"grpc": "Ping check ok", "http": "Ping check ok"}
# http "ok" alone is enough unless you need file bytes or chat streaming
```

If ping fails: server down (restart it, wait ~30 s before use) or token
invalid/for the wrong endpoint. Repeated 500s from a headless server may mean
a corrupt server db — stop and ask the operator rather than improvising.

## Reading

```sh
anyr space list -t                             # spaces you can access
anyr object list "Work" --type page -t         # list pages (table)
anyr object get "Work" OBJECT_ID               # full object incl. markdown body (json)
anyr search --space "Work" --type Task --text customer -t   # text search
anyr search --type collection --space "Work" -t             # find collections/queries
anyr view objects --view All "Work" LIST_ID --cols name,creator,status
anyr type list "Work" -t                       # discover type keys
anyr file list "Personal" --ext-in pdf,docx -t # find files (REST)
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
body (`markdown` is null) — fetch each object by id when you need content.

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
returns the new object as JSON — capture `.id` for follow-up edits.

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

File bytes now transfer over REST — no gRPC needed for the common path.

```sh
anyr file upload "Personal" ./report.pdf                 # local file (REST)
anyr file upload "Personal" --url https://x/y.png        # remote source (gRPC)
anyr file download "Personal" FILE_ID --dir /tmp         # SPACE is a required positional (or --file PATH)
anyr file metadata "Personal" FILE_ID                    # HTTP HEAD: status + headers, no body
anyr file delete "Personal" FILE_ID                      # to bin
anyr file delete "Personal" FILE_ID --permanent          # skip the bin
anyr file search "Personal" --sort last_modified_date --desc -t
```

- Breaking (vs older skill): `file download` takes `SPACE` as a leading
  positional and always uses REST; the old `--http`/`--space` flags are gone.
  `file delete` no longer accepts `--http` (use `--permanent` to skip the bin).
- gRPC-only upload options: `--url`, `--file-type`, `--style`, `--details`,
  `--created-in-context*`. `file preload` / `discard-preload` are also gRPC.

## Chat

Chat operations pick a transport: `anyr chat --transport auto|rest|grpc <cmd>`
(default `auto`). Under `auto` the everyday operations route through REST and
fall back to gRPC only when needed.

```sh
anyr chat list --space "Work" --filter status=open -t         # space-scoped list (REST)
anyr chat messages list "Work" CHAT -t                        # REST
anyr chat messages send "Work" CHAT "hi" --reply-to MSG_ID    # REST; --blocks-json routes via gRPC
anyr chat messages search "Work" CHAT "invoice"               # REST full-text search
anyr chat messages react "Work" CHAT MSG_ID 👍                 # toggle a reaction (REST)
anyr chat read-all "Work" CHAT                                # mark whole chat read (REST)
anyr chat listen --chat CHAT --space "Work" --initial-limit 20 # REST SSE (single chat)
```

- gRPC-only chat: cross-space `list`, rich `get`, `unread`, `--blocks-json`
  send/edit, and history-rich (`--include-history`/`--after`/`--previews`/
  `--buffer`) or multi-`--chat` `listen`. Force it with `--transport grpc`;
  `--transport rest` rejects these up front with an actionable error.
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
available — never start `anyr mcp` yourself) or the `anytype` Rust crate
directly.

## Gotchas

- `anyr` shells out nothing and prompts on stdin for `auth set-*`; pipe the
  secret in (`echo "$TOKEN" | anyr auth set-http`) in non-interactive runs.
- Space arguments accept names, but names are not unique — prefer ids in
  scripts that must not touch the wrong space.
- After starting a server (desktop or headless), wait ~30 s before issuing
  commands; early requests fail spuriously.
- Test servers may run in a no-network namespace (`source .test-env-nonet` in
  the anytype repo); delete objects you create so restarts don't trigger mass
  sync.
