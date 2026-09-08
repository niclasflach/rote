"""Example Rote plugin.

Drop .py files like this one into ~/.config/rote/plugins (or ./plugins next
to the binary while developing) and Rote loads them on startup. Each file
gets `import rote` for free — no packaging step.
"""

import rote


def hello():
    rote.insert("\n[hello.py] Ctrl+Shift+H ran a command a Python plugin registered.\n")


rote.register_command("hello", hello)
