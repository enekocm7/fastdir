#!/bin/sh
# Flatpak launcher for the FastDir GUI.
#
# Inside the sandbox XDG_DATA_HOME is the app's private data directory
# (~/.var/app/io.github.enekocm7.FastDir/data), so "Move to Trash" would hide
# files in a trash no file manager shows. Point it at the desktop's data
# directory instead (the app has --filesystem=host); Flatpak passes the host's
# own value as HOST_XDG_DATA_HOME when one is set.
XDG_DATA_HOME="${HOST_XDG_DATA_HOME:-$HOME/.local/share}"
export XDG_DATA_HOME
exec /app/libexec/fastdir "$@"
