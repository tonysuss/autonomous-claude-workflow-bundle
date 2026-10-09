"""Runs a command in a pseudo-terminal for at most SECONDS, answering the terminal queries a TUI
sends at startup, optionally typing input after a delay. Writes the raw screen to OUT. Stops
early once the file named by --until exists.

usage: pty_drive.py SECONDS OUT [--until PATH] [--type DELAY TEXT]... -- command args...
"""
import fcntl, os, pty, select, signal, struct, sys, termios, time

argv = sys.argv[1:]
secs = float(argv.pop(0))
out_path = argv.pop(0)
until = None
typed = []
while argv and argv[0] in ("--type", "--until"):
    if argv[0] == "--until":
        until = argv[1]
        argv = argv[2:]
    else:
        typed.append((float(argv[1]), argv[2]))
        argv = argv[3:]
assert argv.pop(0) == "--"

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.execvp(argv[0], argv)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 160, 0, 0))
start = time.time()
pending = sorted(typed)
with open(out_path, "wb") as out:
    while time.time() - start < secs and not (until and os.path.exists(until)):
        r, _, _ = select.select([fd], [], [], 0.2)
        if fd in r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            out.write(data)
            out.flush()
            # Answer the common startup queries so the TUI does not wait on them.
            if b"\x1b[6n" in data:
                os.write(fd, b"\x1b[1;1R")
            if b"\x1b[c" in data or b"\x1b[0c" in data:
                os.write(fd, b"\x1b[?62;22c")
            if b"\x1b]11;?" in data:
                os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
            if b"\x1b]10;?" in data:
                os.write(fd, b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\")
        while pending and time.time() - start >= pending[0][0]:
            _, text = pending.pop(0)
            os.write(fd, text.encode().decode("unicode_escape").encode())
    try:
        os.kill(pid, signal.SIGTERM)
        time.sleep(0.5)
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
print("done after", round(time.time() - start, 1), "s")
