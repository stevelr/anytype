# anyback(1)

## NAME

anyback - backup and restore Anytype spaces and objects

## SYNOPSIS

`anyback [GLOBAL_OPTIONS] <COMMAND>`

`anyback backup --space NAME_OR_ID [--objects FILE|-] [--format markdown|pb|pb-json|json] [--dir DIR | --dest PATH] [--prefix PREFIX]`

`anyback restore ARCHIVE --space NAME_OR_ID [--objects FILE|-] [--import-mode ignore-errors|all-or-nothing] [--log REPORT.json]`

`anyback export ...` (alias for `backup`)

`anyback import ...` (alias for `restore`)

`anyback list ARCHIVE [--brief|--expanded|--files]`

`anyback manifest ARCHIVE`

`anyback diff ARCHIVE1 ARCHIVE2`

`anyback extract ARCHIVE ID OUTPUT`

`anyback inspect ARCHIVE [--max-cache SIZE]`

`anyback auth <SUBCOMMAND>`

## DESCRIPTION

`anyback` is a CLI tool for backing up and restoring Anytype spaces.

- `backup` creates full-space or selective backups as `.zip` archives.
- `restore` imports an archive into an existing destination space.
- `list` shows archive summary and object IDs.
  - `--brief` prints summary only (no object IDs).
  - `--expanded` parses all snapshot files and emits per-object metadata.
  - `--files` lists files with sizes.
  - Accepts both directory archives and `.zip` archives.
  - Supports `--json` for machine-readable output.
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

- `-u, --url URL` HTTP API endpoint (env: `ANYTYPE_URL`).
- `--grpc URL` gRPC endpoint (env: `ANYTYPE_GRPC_ENDPOINT`).
- `--keystore VALUE` keystore type/config.
- `--keystore-service NAME` keystore service name.
- `--json` machine-readable output where applicable.
- `-v, --verbose` increase log verbosity.
- `--color auto|always|never` color output mode (default `auto`).

## OBJECT LIST INPUT

For `backup` and `restore`, `--objects` accepts:

- `FILE`: path to a text file with one object ID per line.
- `-`: read object IDs from stdin.

Blank lines and lines starting with `#` are ignored.

## BACKUP OUTPUT

- `--dir DIR`: existing parent directory where a new timestamped archive is created.
- `--dest PATH`: explicit archive path to create (will not overwrite existing files).
- `--prefix PREFIX`: archive naming prefix used with `--dir` or default parent (`.`).

Backup writes `.zip` archives. Manifest metadata is written to a sidecar file `<archive>.manifest.json`.

## RESTORE OPTIONS

- `--import-mode ignore-errors` (default): continue importing after object errors.
- `--import-mode all-or-nothing`: stop on first error. Note: this is not transactional — previously imported objects are not rolled back.
- `--dry-run`: validate archive and destination space without importing.
- `--log FILE`: write a JSON report with per-object success/failure details.
- `--replace`: replace existing objects from archive.

Archives without manifest metadata (e.g. desktop-generated Anytype backups) are supported.

## EXTRACT

- `ARCHIVE`: archive path (directory or `.zip`).
- `ID`: object ID to extract.
- `OUTPUT`: destination file path.

## ENVIRONMENT VARIABLES

- `ANYTYPE_URL` — HTTP API endpoint (same as `--url`).
- `ANYTYPE_GRPC_ENDPOINT` — gRPC endpoint (same as `--grpc`).
- `ANYBACK_RESTORE_TRANSPORT` — set to `snapshots` to use snapshot import transport instead of path-based import.

## EXIT STATUS

- `0`: success.
- non-zero: command failed.
