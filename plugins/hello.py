"""Example Rote plugin.

Drop .py files like this one into ~/.config/rote/plugins (or ./plugins next
to the binary while developing) and Rote loads them on startup. Each file
gets `import rote` for free — no packaging step.
"""

import rote


def hello():
    rote.insert("\n[hello.py] Ctrl+Shift+H ran a command a Python plugin registered.\n")


# register_command names it, for anything that invokes commands by name
# (a future command palette); bind_key wires it straight to a chord — Rote
# has no hardcoded knowledge of "hello" or Ctrl+Shift+H at all.
rote.register_command("hello", hello)
rote.bind_key("ctrl+shift+h", hello)
