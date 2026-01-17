# any-edit: Edit Anytype document in external editor

`any-edit` exports an [Anytype](https://anytype.io) document (page, note, task, or other object type) to a markdown file, opens the file in an editor, waits for the editor to exit, then imports the updated document into Anytype.

A Raycast extension ([script](./scripts/) included) can be used to assign a hotkey for "edit this page in external editor".

## Commands

```sh
# Authenticate with desktop app
any-edit auth login

# Check authentication status
any-edit auth status

# View commands and options
any-edit --help

# Export a page (or other object type) with markdown
any-edit get SPACE_ID OBJECT_ID -o page.md

# Update document title or body if there are changes.
any-edit update -i page.md

# Round trip: Export a document, open it in editor,
# wait for editor to close, then import changes
any-edit edit SPACE_ID OBJECT_ID

# Same as edit but uses LINK obtained from
# the app menus 'Copy Link' or 'Copy Deeplink'
any-edit edit --doc "LINK"
```

**macos-only commands**

```sh
# Ask Anytype desktop for the current document,
# export it as markdown, open in editor, and import changes.
any-edit edit-current

# Get "Deeplink" url of currently viewed document
any-edit copy-link
```

## Install

Release binaries are on [github](https://github.com/stevelr/anytype/tags)

**Macos Homebrew**

```sh
brew install stevelr/tap/any-edit
```

**Linux (arm64/x86_64)**

```sh
curl -fsSL https://github.com/stevelr/anytype/releases/latest/download/any-edit-installer.sh | sh
```

**Windows Powershell**

```sh
irm https://github.com/stevelr/anytype/releases/latest/download/any-edit-installer.ps1 | iex
```

**Cargo**

```sh
cargo install -p any-edit
```

## Build from source

**Cargo**

```sh
cargo install -p any-edit
```

**Nix**

```sh
nix build
```

## Configure

### Use with desktop app

1. Ensure anytype desktop app is running on the current machine. The default http api endpoint is http://127.0.0.1:31009.

2. Enter `any-edit auth login` to begin interactive authentication. The app displays a 4-digit code. Enter the code into `any-edit`, and an access token is generated and stored securely in the OS keyring or key-file.

3. Type `any-edit auth status` to confirm authentication status.

See `scripts/README.md` for Raycast setup, editor configuration, and diagnostics.

### Use with headless cli

1. Generate a token with the cli, `anytype auth apikey create any-edit`, and store the token either:
   - in the default location: (linux) `~/.config/any-edit/api.key` (mac) `~/Library/Application Support/any-edit/api.key`,
   - or in custom path and pass the path to any-edit with `--keyfile-path=PATH`

2. Configure the url path, either
   - set as an environment variable, for example, `export ANYTYPE_URL=http://127.0.0.1:31012`
   - or use the url parameter: `any-edit --url=http://127.0.0.1:31012`

3. Check that the key is valid with `any-edit auth status`

The headless cli doesn't support the copy link hotkeys so `--copy-url` or `--edit-current`, but the other commands should work.

### Platform compatibility

The Raycast extension and hotkey to query the desktop app for the current page only work on macos. The other operations: exporting anytype object to markdown, and updating an object from a markdown file, should work on other platforms. However, if you just need a general export/import tool for anytype objects, check out [anyr](https://github.com/stevelr/anytype/tree/main/anyr).

## Accessibility Permissions

`any-edit` needs permission to send keystrokes to the Anytype desktop application. You may see a system prompt that _PROGRAM_ would like to control this computer using accessibility features". Depending on how it is invoked, "_PROGRAM_" may be any-edit, Raycast, or your terminal program (such as WezTerm or Terminal). Permissions can be enabled in System Settings -> Privacy and Security -> Accessibility.

```

```
