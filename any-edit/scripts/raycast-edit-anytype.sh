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
# Path to anyr program
ANYR="$HOME/.local/bin/anyr"
# EDITOR program - use absolute path
EDITOR="/opt/homebrew/bin/hx"
# Terminal wrapper for editor
export EDITOR_COMMAND="/Applications/Alacritty.app/Contents/MacOS/alacritty -e $EDITOR"

set -euo pipefail

# Function to show notification
notify() {
  osascript -e "display notification \"$1\" with title \"Anytype Edit\""
}

# Check if anyr exists
if [[ ! -x "$ANYR" ]]; then
  echo "ERROR: anyr not found at $ANYR"
  exit 1
fi

notify "Opening Anytype editor..."
if "$ANYR" md edit-current 2>&1; then
  notify "Changes saved to Anytype"
else
  notify "Failed to edit Anytype object"
  exit 1
fi
