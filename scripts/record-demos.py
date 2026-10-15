#!/usr/bin/env python3
"""Record the GIF+MP4 demos in docs/demos/ with real execution.

Every command shown runs for real under a pty; the bytes rendered are the
bytes the process wrote, replayed with their true arrival timing (idle gaps
over 1s are compressed so the demos stay tight). Only the keystroke typing
of the (fixed, shown) command lines is simulated, exactly like a VHS tape.

Needs: cargo, python3, pillow, imageio-ffmpeg (for its static ffmpeg).
Run from the repo root:  python3 scripts/record-demos.py [--verify-only]
"""

import codecs
import json
import os
import pty
import re
import select
import shutil
import subprocess
import sys
import tempfile
import time

FPS = 10
FRAME_DT = 1.0 / FPS
COLS = 96
ROWS = 26
TYPE_DT = 0.025
MAX_GAP = 1.0
END_HOLD = 2.2
MIN_DUR, MAX_DUR = 10.0, 15.0
MOCK_PORT = 18087

BG = (13, 17, 23)
FG = (230, 237, 243)
GREEN = (63, 185, 80)
BAR = (22, 27, 34)
DIM = (139, 148, 158)

ANSI = re.compile(
    r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b[()][0-9A-B]|\x1b[=>MEHc7-9]|\x0e|\x0f"
)


def strip_ansi(text):
    return ANSI.sub("", text)


class Term:
    """A minimal terminal grid: printable chars, newline/CR/BS/TAB, scroll."""

    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.grid = [[" "] * cols for _ in range(rows)]
        self.attr = [[0] * cols for _ in range(rows)]
        self.x = self.y = 0

    def newline(self):
        self.x = 0
        self.y += 1
        if self.y >= self.rows:
            self.grid.pop(0)
            self.attr.pop(0)
            self.grid.append([" "] * self.cols)
            self.attr.append([0] * self.cols)
            self.y = self.rows - 1

    def put(self, char, attr):
        if char == "\n":
            self.newline()
        elif char == "\r":
            self.x = 0
        elif char == "\b":
            self.x = max(0, self.x - 1)
        elif char == "\t":
            self.x = min(self.cols - 1, (self.x + 8) // 8 * 8)
        elif char.isprintable() or char == " ":
            self.grid[self.y][self.x] = char
            self.attr[self.y][self.x] = attr
            self.x += 1
            if self.x >= self.cols:
                self.newline()
        # Other controls are dropped: they carry no visible content.

    def write(self, text, attr=0):
        for char in text:
            self.put(char, attr)


def run_pty(cmd, cwd, env, timeout=60):
    """Run cmd under a pty; return [(seconds_after_start, text)] in arrival order."""
    chunks = []
    start = time.monotonic()
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execvpe("bash", ["bash", "-c", cmd], env)
    try:
        while True:
            if time.monotonic() - start > timeout:
                raise RuntimeError(f"timed out: {cmd}")
            ready, _, _ = select.select([fd], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            text = decoder.decode(data)
            if text:
                chunks.append((time.monotonic() - start, strip_ansi(text)))
    finally:
        try:
            os.close(fd)
        except OSError:
            pass
        _, status = os.waitpid(pid, 0)
    tail = decoder.decode(b"", final=True)
    if tail:
        chunks.append((time.monotonic() - start, strip_ansi(tail)))
    return chunks, status


def build_timeline(commands, cwd, env):
    """Execute every command for real; return [(time, text, attr)] plus total length."""
    events = []
    now = 0.6
    for cmd in commands:
        prompt = "$ "
        events.append((now, prompt, 1))
        for i, char in enumerate(cmd):
            events.append((now + i * TYPE_DT, char, 0))
        enter = now + len(cmd) * TYPE_DT + 0.25
        events.append((enter, "\n", 0))
        chunks, status = run_pty(cmd, cwd, env)
        print(f"  $ {cmd}  (exit {os.waitstatus_to_exitcode(status)})")
        acc = enter + 0.15
        prev = 0.0
        for offset, text in chunks:
            acc += min(max(0.0, offset - prev), MAX_GAP)
            prev = offset
            events.append((acc, text, 0))
        now = acc + 0.9
    total = now + END_HOLD
    if total < MIN_DUR:
        total = MIN_DUR
    if total > MAX_DUR:
        raise RuntimeError(f"demo runs {total:.1f}s, over the {MAX_DUR:.0f}s cap")
    return events, total


def render_frames(events, total, title, font_path):
    """Replay the event timeline into PIL frames at FPS."""
    from PIL import Image, ImageDraw, ImageFont

    font = ImageFont.truetype(font_path, 15)
    cell_w = int(font.getlength("M"))
    ascent, descent = font.getmetrics()
    cell_h = ascent + descent + 4
    pad = 14
    bar_h = 30
    width = COLS * cell_w + pad * 2
    height = bar_h + ROWS * cell_h + pad
    width += width % 2
    height += height % 2

    term = Term(COLS, ROWS)
    events = sorted(events)
    cursor = 0
    frames = []
    n_frames = int(round(total * FPS))
    for frame in range(n_frames):
        tick = frame * FRAME_DT
        while cursor < len(events) and events[cursor][0] <= tick:
            _, text, attr = events[cursor]
            term.write(text, attr)
            cursor += 1
        img = Image.new("RGB", (width, height), BG)
        draw = ImageDraw.Draw(img)
        draw.rectangle([0, 0, width, bar_h], fill=BAR)
        for i, color in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
            draw.ellipse([pad + i * 20, 9, pad + i * 20 + 12, 21], fill=color)
        draw.text((pad + 70, 7), title, font=font, fill=DIM)
        base_y = bar_h
        for row in range(ROWS):
            line = "".join(term.grid[row]).rstrip()
            if not line:
                continue
            y = base_y + row * cell_h
            draw.text((pad, y), line, font=font, fill=FG)
            # Repaint prompt spans green.
            col = 0
            while col < len(line):
                if term.attr[row][col] == 1:
                    start = col
                    while col < len(line) and term.attr[row][col] == 1:
                        col += 1
                    x = pad + int(font.getlength(line[:start]))
                    draw.text((x, y), line[start:col], font=font, fill=GREEN)
                else:
                    col += 1
        # Blinking block cursor at the live position.
        if (frame // 5) % 2 == 0:
            cx = pad + term.x * cell_w
            cy = base_y + term.y * cell_h
            draw.rectangle([cx, cy, cx + cell_w - 1, cy + cell_h - 4], fill=FG)
        frames.append(img)
    return frames


def save_gif(frames, path):
    head, rest = frames[0], frames[1:]
    head.save(
        path,
        save_all=True,
        append_images=rest,
        duration=int(1000 / FPS),
        loop=0,
        optimize=True,
    )


def save_mp4(frames, path, ffmpeg):
    width, height = frames[0].size
    cmd = [
        ffmpeg, "-y", "-v", "error",
        "-f", "rawvideo", "-pix_fmt", "rgb24",
        "-s", f"{width}x{height}", "-r", str(FPS),
        "-i", "-",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20",
        "-movflags", "+faststart",
        path,
    ]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    for img in frames:
        proc.stdin.write(img.tobytes())
    proc.stdin.close()
    if proc.wait() != 0:
        raise RuntimeError(f"ffmpeg mp4 encode failed for {path}")


def mp4_frames(path, ffmpeg, width, height):
    """Decode every mp4 frame to raw RGB."""
    cmd = [
        ffmpeg, "-v", "error",
        "-i", path,
        "-f", "rawvideo", "-pix_fmt", "rgb24",
        "-",
    ]
    out = subprocess.run(cmd, capture_output=True, check=True).stdout
    size = width * height * 3
    if len(out) % size != 0:
        raise RuntimeError(f"{path}: short mp4 decode")
    return [out[i : i + size] for i in range(0, len(out), size)]


def check_frames(label, frames, width, height, is_rgb, duration):
    """duration, animation, and non-blank checks shared by both formats."""
    from PIL import Image

    n = len(frames)
    print(f"  {label}: {n} frames, {duration:.1f}s, {width}x{height}")
    assert MIN_DUR <= duration <= MAX_DUR, f"{label}: duration {duration:.1f}s out of range"
    if is_rgb:
        images = [Image.frombytes("RGB", (width, height), f) for f in frames]
    else:
        images = [f.convert("RGB") for f in frames]
    distinct = len({img.tobytes()[::97] for img in images})
    assert distinct >= 5, f"{label}: only {distinct} distinct frames"
    # The mid frame must read as a terminal: mostly dark, with bright text.
    mid = images[n // 2]
    pixels = mid.load()
    step = max(1, width // 160)
    lit = dark = total = 0
    for x in range(0, width, step):
        for y in range(0, height, step):
            total += 1
            brightness = sum(pixels[x, y][:3])
            if brightness > 300:
                lit += 1
            if brightness < 150:
                dark += 1
    assert lit / total > 0.01, f"{label}: mid frame has no text"
    assert dark / total > 0.3, f"{label}: mid frame is not a dark terminal"
    print(
        f"  {label}: {distinct} distinct frames, "
        f"{lit / total:.1%} text, {dark / total:.1%} dark: OK"
    )


def verify(path, ffmpeg):
    from PIL import Image

    if path.endswith(".gif"):
        img = Image.open(path)
        frames = []
        total_ms = 0
        try:
            while True:
                # The encoder merges identical consecutive frames and sums
                # their durations, so duration is the sum, not count x delay.
                total_ms += img.info.get("duration", 100)
                frames.append(img.copy())
                img.seek(img.tell() + 1)
        except EOFError:
            pass
        check_frames(path, frames, img.width, img.height, False, total_ms / 1000)
    else:
        probe = subprocess.run(
            [ffmpeg, "-hide_banner", "-i", path],
            capture_output=True, text=True,
        )
        dims = re.search(r"Video:.*?, (\d+)x(\d+)", probe.stderr)
        assert dims, f"{path}: no video stream found"
        width, height = int(dims.group(1)), int(dims.group(2))
        frames = mp4_frames(path, ffmpeg, width, height)
        check_frames(path, frames, width, height, True, len(frames) / FPS)


DEMOS = {
    "generate": {
        "title": "mcp2dekopon — generate a provider",
        "commands": [
            "mcp2dekopon gen --url http://127.0.0.1:18087/mcp --id wiki --out ./demo-out",
            "ls demo-out/wiki demo-out/wiki/broker.d",
            "python3 -m json.tool demo-out/report.json | head -24",
        ],
    },
    "offline": {
        "title": "mcp2dekopon — offline capture and replay",
        "commands": [
            "mcp2dekopon gen --url http://127.0.0.1:18087/mcp --id wiki --out ./demo-save --save-tools tools.json",
            "mcp2dekopon gen --url http://127.0.0.1:9/mcp --id wiki --out ./demo-replay --from-file tools.json",
            "ls demo-replay/wiki && python3 -c \"import json; print(json.load(open('demo-replay/report.json'))['summary'])\"",
        ],
    },
    "review": {
        "title": "mcp2dekopon — dry run and errors",
        "commands": [
            "mcp2dekopon gen --url http://127.0.0.1:18087/mcp --id wiki --out ./demo-out --dry-run",
            "mcp2dekopon gen --url http://127.0.0.1:18087/mcp --id BadId --out ./demo-bad",
        ],
    },
}


def find_font():
    for probe in [
        ["fc-list", "DejaVu Sans Mono", "file"],
        ["fc-list", "monospace", "file"],
    ]:
        try:
            out = subprocess.run(probe, capture_output=True, text=True).stdout
        except FileNotFoundError:
            continue
        for line in out.splitlines():
            path = line.split(":")[0].strip()
            if path.endswith(".ttf") and os.path.isfile(path):
                return path
    raise RuntimeError("no monospace TTF found (install DejaVu or set fc-list up)")


def find_ffmpeg():
    exe = shutil.which("ffmpeg")
    if exe:
        return exe
    try:
        import imageio_ffmpeg

        return imageio_ffmpeg.get_ffmpeg_exe()
    except ImportError:
        raise RuntimeError("need ffmpeg on PATH or imageio-ffmpeg installed")


def main():
    verify_only = "--verify-only" in sys.argv
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    try:
        import PIL  # noqa: F401
    except ImportError:
        raise RuntimeError("need pillow installed (pip install pillow imageio-ffmpeg)")
    ffmpeg = find_ffmpeg()
    font = find_font()
    out_dir = os.path.join(root, "docs", "demos")
    os.makedirs(out_dir, exist_ok=True)

    if not verify_only:
        print("building the binary...")
        subprocess.run(["cargo", "build"], cwd=root, check=True,
                       capture_output=not sys.stdout.isatty())
        print(f"starting the loopback mock on :{MOCK_PORT}...")
        mock = subprocess.Popen(
            [sys.executable, os.path.join(root, "scripts", "mock-mcp.py"), str(MOCK_PORT)],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        try:
            line = mock.stdout.readline().strip()
            if line != str(MOCK_PORT):
                raise RuntimeError(f"mock failed to bind :{MOCK_PORT}: {line}")
            work = tempfile.mkdtemp(prefix="mcp2demos-")
            env = dict(os.environ)
            env.update({
                "PATH": os.path.join(root, "target", "debug") + ":" + env.get("PATH", ""),
                "NO_COLOR": "1", "CLICOLOR": "0", "TERM": "dumb",
                "COLUMNS": str(COLS), "LINES": str(ROWS),
            })
            try:
                for name, demo in DEMOS.items():
                    print(f"recording {name}...")
                    events, total = build_timeline(demo["commands"], work, env)
                    print(f"  timeline {total:.1f}s, rendering...")
                    frames = render_frames(events, total, demo["title"], font)
                    gif = os.path.join(out_dir, f"{name}.gif")
                    mp4 = os.path.join(out_dir, f"{name}.mp4")
                    save_gif(frames, gif)
                    save_mp4(frames, mp4, ffmpeg)
                    print(f"  wrote {gif} + {mp4}")
            finally:
                shutil.rmtree(work, ignore_errors=True)
        finally:
            mock.terminate()
            mock.wait()

    print("verifying...")
    for name in DEMOS:
        verify(os.path.join(out_dir, f"{name}.gif"), ffmpeg)
        verify(os.path.join(out_dir, f"{name}.mp4"), ffmpeg)
    print("all demos verified")


if __name__ == "__main__":
    main()
