# anyback(1)

## NAME

anyback - backup and restore Anytype spaces and objects

## SYNOPSIS

`anyr [GLOBAL_OPTIONS] backup <COMMAND>`

`anyr backup create --space NAME_OR_ID [--objects FILE|-] [--format markdown|pb|pb-json|json] [--dir DIR | --dest PATH] [--prefix PREFIX]`

`anyr backup restore ARCHIVE --space NAME_OR_ID [--objects FILE|-] [--import-mode ignore-errors|all-or-nothing] [--log REPORT.json]`

`anyr backup export ...` (alias for `create`)

`anyr backup import ...` (alias for `restore`)

`anyr backup list ARCHIVE [--brief|--expanded|--files]`

`anyr backup manifest ARCHIVE`

`anyr backup diff ARCHIVE1 ARCHIVE2`

`anyr backup extract ARCHIVE ID OUTPUT`

`anyr backup inspect ARCHIVE [--max-cache SIZE]`

## DESCRIPTION

`anyback` is the backup and restore command set of the consolidated `anyr`
CLI; it is reached as `anyr backup <COMMAND>` and shares `anyr`'s endpoint,
keystore, and output options.

- `create` creates full-space or selective backups as `.zip` archives.
- `restore` imports an archive into an existing destination space.
- `list` shows archive summary and object IDs.
  - `--brief` prints summary only (no object IDs).
  - `--expanded` parses all snapshot files and emits per-object metadata.
  - `--files` lists files with sizes.
  - Accepts both directory archives and `.zip` archives.
- `manifest` prints the archive manifest as JSON.
- `diff` compares two archives and prints archive1-only, archive2-only, and changed objects.
- `extract` extracts one object from an archive:
  - document-like objects are written as markdown.
  - file/image objects are written as raw bytes.
- `inspect` launches an interactive TUI to browse the archive:
  - preview renders markdown from protobuf snapshots (including tables).
  - save-as (`w`) writes markdown or raw bytes.
  - `--max-cache SIZE` sets inspector preview cache budget (default `200 MiB`).

## GLOBAL OPTIONS

These are the `anyr` global options; place them before or after `backup`.

- `-u, --url URL` HTTP API endpoint (env: `ANYTYPE_URL`).
- `--grpc URL` gRPC endpoint (env: `ANYTYPE_GRPC_ENDPOINT`).
- `--keystore VALUE` keystore type/config.
- `--keystore-service NAME` keystore service name.
- `-j, --json` compact JSON result document (the default).
- `--pretty` indented JSON result document.
- `-t, --table` human-readable text summary.
- `-q, --quiet` suppress the result document; report the outcome only through the exit status.
- `-o, --output FILE` write the result document to FILE instead of stdout.
- `-v, --verbose` increase log verbosity.

## RESULT OUTPUT

Each non-interactive command writes one result document to stdout, or to the
file named by `-o FILE`. Progress indicators, warnings, and errors always go to
stderr, so stdout stays parseable in JSON modes. The destination is replaced
only when the result is ready. `inspect` renders an interactive TUI and does
not produce a result document.

`manifest` is a JSON document in every non-quiet mode; `--table` renders it
indented rather than compact.

Output combinations that cannot be honored are rejected with an error instead
of a silently chosen winner:

- more than one of `--json`, `--pretty`, `--table`, `--quiet`.
- `--quiet` together with `-o FILE`, because nothing would be written.
- any of those flags with `inspect`, which renders an interactive terminal UI.
- `-o FILE` when FILE aliases an archive, object-list input, restore report, or
  extracted output used by the command.

## OBJECT LIST INPUT

For `create` and `restore`, `--objects` accepts:

- `FILE`: path to a text file with one object ID per line.
- `-`: read object IDs from stdin.

Blank lines and lines starting with `#` are ignored.

## BACKUP OUTPUT

- `--dir DIR`: existing parent directory where a new timestamped archive is created.
- `--dest PATH`: explicit archive path to create (will not overwrite existing files).
- `--prefix PREFIX`: archive naming prefix used with `--dir` or default parent (`.`).

Backup writes `.zip` archives. Manifest metadata is written to a sidecar file `<archive>.manifest.json`.

`anyr space delete SPACE --archive PATH` uses the same archive data plane to
write a complete protobuf `.zip` to the exact non-existing path before deleting
the source space. The delete command stops before deletion if backup creation,
archive validation, or destination installation fails. Use
`anyr backup list PATH --files` to validate the selected pre-delete archive;
direct pre-delete archives may not have an anyback manifest sidecar.

## RESTORE OPTIONS

- `--import-mode ignore-errors` (default): continue importing after object errors.
- `--import-mode all-or-nothing`: stop on first error. Note: this is not transactional: previously imported objects are not rolled back.
- `--dry-run`: validate archive and destination space without importing.
- `--log FILE`: write a JSON report with per-object success/failure details.
- `--replace`: replace existing objects from archive.

Archives without manifest metadata (e.g. desktop-generated Anytype backups) are supported.

## EXTRACT

- `ARCHIVE`: archive path (directory or `.zip`).
- `ID`: object ID to extract.
- `OUTPUT`: destination file path.

## ENVIRONMENT VARIABLES

- `ANYTYPE_URL`: HTTP API endpoint (same as `--url`).
- `ANYTYPE_GRPC_ENDPOINT`: gRPC endpoint (same as `--grpc`).
- `ANYBACK_RESTORE_TRANSPORT`: set to `snapshots` to use snapshot import transport instead of path-based import.
- `ANYR_BIN`: path to the `anyr` executable used by the live backup test suites.

## EXIT STATUS

- `0`: success.
- non-zero: command failed.
