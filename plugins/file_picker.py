"""Example Rote plugin: a file-open picker bound to Ctrl+P.

Demonstrates `rote.open_picker` (an in-editor overlay list — type to
filter, Up/Down to move, Enter to pick, Esc to cancel) together with
`rote.open` (load a file into a new buffer) and `rote.bind_key`. Listing
files needs no Rote API at all: plugins run in a full CPython, so the
standard library's `os` is right there.
"""

import os
import rote

# Directories not worth listing files from — hidden dirs and Rust's build
# output, which is huge and never what you want to open.
_SKIP_DIRS = {"target", ".git"}


def list_files(root="."):
    files = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in _SKIP_DIRS and not d.startswith(".")]
        for name in filenames:
            files.append(os.path.normpath(os.path.join(dirpath, name)))
    return sorted(files)


def open_file_picker():
    rote.open_picker(list_files(), rote.open)


rote.bind_key("ctrl+p", open_file_picker)
