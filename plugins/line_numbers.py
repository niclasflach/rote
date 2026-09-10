"""Example Rote plugin: a line-number gutter.

Demonstrates `rote.register_gutter`, the hook a plugin uses to draw a left
gutter column — line numbers here, but the same hook could show diagnostics,
git blame, or breakpoint signs instead.

Rote calls the registered function once per line of the body text on every
relayout, not once per frame, so it's fine to do real work in here.
"""

import rote


def gutter(line, wrapped, current):
    # `wrapped` is always False today — Rote doesn't soft-wrap the body
    # text yet, so every row is a line's first (and only) row.
    marker = ">" if current else " "
    return f"{marker}{line:>4} "


rote.register_gutter(gutter)
