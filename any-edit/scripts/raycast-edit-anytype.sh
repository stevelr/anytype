#!/usr/bin/env bash

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Edit Anytype Object
# @raycast.mode silent

# Optional parameters:
# @raycast.icon 📝
# @raycast.packageName Anytype

# Documentation:
# @raycast.description Edit current Anytype page using external editor
# @raycast.author stevelr

# Configuration - edit these to match your setup
# Path to any-edit program
ANY_EDIT="$HOME/.local/bin/any-edit"
# EDITOR program - use absolute path
EDITOR="/opt/homebrew/bin/hx"
# Terminal wrapper for editor
export EDITOR_COMMAND="/Applications/Alacritty.app/Contents/MacOS/alacritty -e $EDITOR"

set -euo pipefail

# Function to show notification
notify() {
  osascript -e "display notification \"$1\" with title \"Anytype Edit\""
}

# Check if any-edit exists
if [[ ! -x "$ANY_EDIT" ]]; then
  echo "ERROR: any-edit not found at $ANY_EDIT"
  exit 1
fi

notify "Opening Anytype editor..."
if "$ANY_EDIT" edit --current 2>&1; then
  notify "Changes saved to Anytype"
else
  notify "Failed to edit Anytype object"
  exit 1
fi
