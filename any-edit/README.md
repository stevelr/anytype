# any-edit: Markdown editing library for anyr

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&filter=any-edit-v*&label=release)](https://github.com/stevelr/anytype/releases?q=any-edit-v&expanded=true)
[![crates.io](https://img.shields.io/crates/v/any-edit.svg)](https://crates.io/crates/any-edit)

`any-edit` is the library implementation used by the consolidated `anyr md`
commands. It exports an [Anytype](https://anytype.io) document (page, note,
task, or other object type) to a markdown file, opens the file in an editor,
waits for the editor to exit, then imports the updated document into Anytype.

A Raycast extension ([script](./scripts/) included) can be used to assign a hotkey for "edit this page in external editor".

## Commands

```sh
# Authenticate with desktop app
anyr auth login

# Check authentication status
anyr auth status

# View commands and options
anyr md --help

# Export a page (or other object type) with markdown
anyr md get SPACE_ID OBJECT_ID -o page.md

# Update document title or body if there are changes.
anyr md update -i page.md

# Round trip: Export a document, open it in editor,
# wait for editor to close, then import changes
anyr md edit SPACE_ID OBJECT_ID

# Same as edit but uses LINK obtained from
# the app menus 'Copy Link' or 'Copy Deeplink'
anyr md edit --doc "LINK"
```

**macos-only commands**

```sh
# Ask Anytype desktop for the current document,
# export it as markdown, open in editor, and import changes.
anyr md edit-current

# Get "Deeplink" url of currently viewed document
anyr md copy-link
```

## Install

Release binaries are on [github](https://github.com/stevelr/anytype/tags)

**Cargo**

```sh
cargo install -p anyr
```

## Build from source

**Cargo**
Ensure you have 'protoc' from the protobuf package in your path. On macos, 'brew install protobuf'

```sh
cargo build -p anyr
```

**Nix**

```sh
nix build
```

## Configure

### Use with desktop app

1. Ensure anytype desktop app is running on the current machine. The default http api endpoint is http://127.0.0.1:31009.

2. Enter `anyr auth login` to begin interactive authentication. The app displays a 4-digit code. Enter the code into `anyr`, and an access token is generated and stored securely in the OS keyring or key-file.

3. Type `anyr auth status` to confirm authentication status.

See `scripts/README.md` for Raycast setup, editor configuration, and diagnostics.

### Use with headless cli

1. Generate a token with the cli, `anytype auth apikey create anyr`, and store it with `anyr auth set-http`.

2. Configure the url path, either
   - set as an environment variable, for example, `export ANYTYPE_URL=http://127.0.0.1:31012`
   - or use the url parameter before the command: `anyr --url=http://127.0.0.1:31012 md get ...`

3. Check that the key is valid with `anyr auth status`

The headless cli doesn't support the copy link hotkeys so `--copy-url` or `--edit-current`, but the other commands should work.

### Platform compatibility

The Raycast extension and hotkey to query the desktop app for the current page only work on macos. The other operations: exporting anytype object to markdown, and updating an object from a markdown file, should work on other platforms. However, if you just need a general export/import tool for anytype objects, check out [anyr](https://github.com/stevelr/anytype/tree/main/anyr).

## Accessibility Permissions

`anyr md` needs permission to send keystrokes to the Anytype desktop application. You may see a system prompt that *PROGRAM* would like to control this computer using accessibility features". Depending on how it is invoked, "*PROGRAM*" may be anyr, Raycast, or your terminal program (such as WezTerm or Terminal). Permissions can be enabled in System Settings -> Privacy and Security -> Accessibility.

## License

Apache License, Version 2.0
