# Raycast setup and diagnostics

This guide covers the Raycast script for editing the current Anytype page with `any-edit`.

## Setup

### 1) Authenticate

```bash
any-edit auth login
any-edit auth status
```

### 2) Configure the script

Edit `scripts/raycast-edit-anytype.sh`:

```bash
ANY_EDIT="/path/to/any-edit"
EDITOR="/opt/homebrew/bin/hx"
EDITOR_COMMAND="/Applications/Alacritty.app/Contents/MacOS/alacritty -e $EDITOR"
```

Notes:

- `EDITOR_COMMAND` is split on spaces. Use `\ ` to escape spaces inside a path.
- If `EDITOR_COMMAND` is unset, `any-edit` falls back to `EDITOR` and passes the file path as the only arg.
- Use absolute paths for all commands (EDITOR, etc.)

If your editor is terminal-based (hx, vim):

- You need a gui terminal program to wrap the editor. Alacritty is recommended (install via homebrew). Terminal and WezTerm are harder to get working for this because they persist the app process after a terminal window closes. Alacritty (and possibly Kitty) exit immediately.

If your editor is vscode/Visual Studio Code, it should work as follows (untested):

- Recommended: `EDITOR_COMMAND="code --wait"` (requires 'code' in PATH)
- Alternate: `EDITOR_COMMAND="open -W -a /Applications/Visual\\ Studio\\ Code.app"`

### 3) Add the script to Raycast

1. Raycast -> Settings
2. Extensions -> Script Commands
3. Add Directories
4. Select the `scripts` folder

### 4) Assign a hotkey

1. Search for "Edit Anytype Object"
2. Press `cmd+k`
3. Configure Command -> set your hotkey

### 5) Grant Accessibility permissions (macOS)

- Add permission for Raycast and your terminal program to use keystroke to copy the current Anytype URL.

1. System Settings -> Privacy & Security -> Accessibility
2. Enable Raycast
3. Click `+`
4. Add the `any-edit` binary path you run
5. Enable the toggle

If you rebuild `any-edit`, you may need to remove and re-add it.

## Diagnostics

### Verify `any-edit` works

```bash
/path/to/any-edit auth status
/path/to/any-edit copy-link
```

The 'copy-link' command inserts the "Copy Deep link" keys into the Anytype desktop app to get the url of the current document, then reads it from the clipboard. It should display a url beginning with "https://object.any.coop/bafy...". The first time this runs, you'll likely be prompted to allow your terminal program to control applications. This is needed to send keystrokes to an application. Click Allow, or enable the terminal app in Settings -> Privacy and Security -> Accessibility.

If `copy-link` returns a URL, the accessibility settings are working.

### Verify the editor command

```bash
EDITOR="/opt/homebrew/bin/hx"
EDITOR_COMMAND="/Applications/Alacritty.app/Contents/MacOS/alacritty -e $EDITOR" \
  /path/to/any-edit edit SPACE_ID OBJECT_ID
```

If the editor opens and closes, `EDITOR_COMMAND` is valid.

### Common issues

- "Failed to get URL" or clipboard errors
  - Make sure Anytype is open and in front of other apps.
  - Check Accessibility permissions for Raycast and the terminal program.

- "EDITOR_COMMAND is empty" or editor does not open
  - Check the `EDITOR_COMMAND` line in `scripts/raycast-edit-anytype.sh`
  - Use escaped spaces for paths, e.g. `"/Applications/My\ App/app -e /opt/homebrew/bin/hx"`

- "any-edit not found"
  - Verify the `ANY_EDIT` path in `scripts/raycast-edit-anytype.sh` is correct and uses an absolute path that doesn't depend on environment.
  - Rebuild with `cargo build --release`
