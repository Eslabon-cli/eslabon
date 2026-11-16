# Herramientas de eslabon

`tuishot.py` — maneja el TUI en un pty y vuelca la pantalla emulada (no hace falta tmux).

```
python3 -m venv /tmp/esl-venv && /tmp/esl-venv/bin/pip install pyte
/tmp/esl-venv/bin/python scripts/eslabon/tuishot.py 140 40 \
  '[["wait", 10], ["send", "hola"], ["send", "\r"], ["wait", 60], ["shot", "tras turno"]]' \
  -- codex-rs/target/debug/codex -p local
```
