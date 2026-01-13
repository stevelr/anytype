# any-edit - Edit Anytype document in external editor

`any-edit` exports an Anytype document (page, note, task, or other object type) to a markdown file, opens the file in an editor, then, after the editor quits, the updated document is imported into Anytype. Using a Raycast extension ([script](./scripts/raycast-edit-anytype.sh) included), a hotkey triggers "edit this page in external editor".

## Platform compatibility

Tested only on macos.

The Raycast extension and hotkey to query the desktop app for the current page only work on macos. The other operations: exporting anytype object to markdown, and updating an object from a markdown file, should work on other platforms. However, if you just need a general export/import tool for anytype objects, check out [anyr](https://github.com/stevelr/anytype/tree/main/anytype-cli).

## Quick setup

**Install**

```sh
cargo install any-edit
```

**Authenticate**

Ensure anytype desktop app is running on the current machine.

```sh
any-edit auth login

# check authentication status
any-edit auth status
```

## Commands

```sh
# View commands and options
any-edit --help

# Export a page (or other object type) with markdown
any-edit get SPACE_ID OBJECT_ID -o page.md

# Import markdown file with changes. The file header containing space_id and markdown_id must be present.
# If the document name or markdown body changed, the changes are sent to Anytype.
any-edit update -i page.md

# Export a document, open it in editor, wait for editor to close, then import changes
any-edit edit SPACE_ID OBJECT_ID

# Ask Anytype desktop for the current document id, export, open in editor, and import changes
any-edit edit --current

# Get "Deep-link" url to current document
any-edit copy-link
```

## Accessibility Permissions

`any-edit` needs permission to send keystrokes to the Anytype desktop application. You may see a system prompt that "PROGRAM would like to control this computer using accessibility features". Depending on how it is invoked, "PROGRAM" may be any-edit, Raycast, or your terminal program (such as WezTerm or Terminal). Permissions can be enabled in System Settings -> Privacy and Security -> Accessibility.

## Raycast setup and troubleshooting

See `scripts/README.md` for Raycast setup, editor configuration, and diagnostics.
