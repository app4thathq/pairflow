#!/bin/bash
# Double-click entry point for Pairflow.app. The real binary is pairflow-cli
# in the same directory and can also be invoked from a terminal.
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
CLI="$DIR/pairflow-cli"

ACTION="$(osascript <<'APPLESCRIPT' || true
try
  set choice to button returned of (display dialog "Pairflow shares one keyboard and mouse across computers using a 5-character code." buttons {"Quit", "Join", "Host"} default button "Host" with title "Pairflow")
  if choice is "Host" then
    return "host"
  else if choice is "Join" then
    set code to text returned of (display dialog "Enter the 5-character pairing code:" default answer "" buttons {"Cancel", "Connect"} default button "Connect" with title "Pairflow")
    return "join " & code
  else
    return "quit"
  end if
on error
  return "quit"
end try
APPLESCRIPT
)"

if [[ -z "${ACTION}" || "${ACTION}" == "quit" ]]; then
  exit 0
fi

# Codes are 5 characters from a fixed alphabet. Reject anything else before
# interpolating into the Terminal command.
if [[ "${ACTION}" == join* ]]; then
  CODE="${ACTION#join }"
  if [[ ! "${CODE}" =~ ^[A-Za-z0-9]{5}$ ]]; then
    osascript -e 'display dialog "A pairing code is 5 letters or digits." buttons {"OK"} with title "Pairflow"' >/dev/null || true
    exit 1
  fi
  CODE="$(printf '%s' "${CODE}" | tr '[:lower:]' '[:upper:]')"
  ACTION="join ${CODE}"
fi

osascript <<APPLESCRIPT
tell application "Terminal"
  activate
  do script "exec '$CLI' $ACTION"
end tell
APPLESCRIPT
