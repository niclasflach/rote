"""Example Rote plugin: a line-number gutter, toggleable between absolute
and relative numbering.

Demonstrates `rote.register_gutter`, the hook a plugin uses to draw a left
gutter column — line numbers here, but the same hook could show diagnostics,
git blame, or breakpoint signs instead. Also demonstrates `rote.bind_key`
holding state a callback closes over, rather than just firing a one-shot
action.

Rote calls the registered function once per visible line on every
relayout, not once per frame, so it's fine to do real work in here.
"""

import rote

_relative = False


def gutter(line, wrapped, current_line):
    # `wrapped` is always False today — Rote doesn't soft-wrap the body
    # text yet, so every row is a line's first (and only) row.
    marker = ">" if line == current_line else " "
    if _relative and line != current_line:
        number = abs(line - current_line)
    else:
        number = line
    return f"{marker}{number:>4} "


def toggle_relative():
    global _relative
    _relative = not _relative


rote.register_gutter(gutter)
# Rebind this to whatever you'd rather use — it's this one line.
rote.bind_key("ctrl+shift+l", toggle_relative)
