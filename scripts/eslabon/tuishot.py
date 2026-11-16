"""Drive a TUI in a pty and dump the emulated screen. Usage: tuishot.py COLS ROWS SCRIPT_JSON -- cmd..."""
import os, pty, sys, time, json, select, signal
import pyte

cols, rows = int(sys.argv[1]), int(sys.argv[2])
steps = json.loads(sys.argv[3])  # [["wait", s] | ["send", text] | ["shot", label] | ["click_text", text]]
cmd = sys.argv[sys.argv.index("--") + 1:]

pid, fd = pty.fork()
if pid == 0:
    os.environ["COLUMNS"], os.environ["LINES"] = str(cols), str(rows)
    os.execvp(cmd[0], cmd)

import fcntl, termios, struct
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

class Screen(pyte.Screen):
    def write_process_input(self, data):
        os.write(fd, data.encode())

screen = Screen(cols, rows)
stream = pyte.ByteStream(screen)

def pump(secs):
    end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            stream.feed(data)

for step in steps:
    if step[0] == "wait":
        pump(step[1])
    elif step[0] == "send":
        os.write(fd, step[1].encode())
        pump(0.3)
    elif step[0] == "click_text":
        # Left-click the first screen line containing the text (SGR mouse, 1-based).
        for row, line in enumerate(screen.display):
            col = line.find(step[1])
            if col >= 0:
                press = f"[<0;{col + 1};{row + 1}M"
                os.write(fd, press.encode())
                os.write(fd, press[:-1].encode() + b"m")
                pump(0.5)
                break
        else:
            print(f"!! click_text: '{step[1]}' not on screen")
    elif step[0] == "shot":
        print(f"===== {step[1]} =====")
        for line in screen.display:
            print(line.rstrip())
os.kill(pid, signal.SIGTERM)
