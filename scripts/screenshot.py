#!/usr/bin/env python3
"""Render a gpulens run to a PNG by driving it inside tmux and painting the captured ANSI screen."""

import argparse
import re
import subprocess
import sys
import time

from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/TTF/FiraCodeNerdFontMono-Regular.ttf"
FONT_BOLD = "/usr/share/fonts/TTF/FiraCodeNerdFontMono-Bold.ttf"
DEFAULT_FG = (230, 230, 230)
DEFAULT_BG = (17, 20, 27)
ANSI16 = [
    (0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188),
    (17, 168, 205), (229, 229, 229), (102, 102, 102), (241, 76, 76), (35, 209, 139),
    (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255),
]
SGR = re.compile(r"\x1b\[([0-9;:]*)m")


def indexed(n):
    if n < 16:
        return ANSI16[n]
    if n < 232:
        n -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return (levels[n // 36], levels[(n // 6) % 6], levels[n % 6])
    v = 8 + (n - 232) * 10
    return (v, v, v)


def apply_sgr(params, state):
    codes = [int(p) if p else 0 for p in re.split(r"[;:]", params)] if params else [0]
    i = 0
    while i < len(codes):
        c = codes[i]
        if c == 0:
            state.update(fg=None, bg=None, bold=False, dim=False, reverse=False, underline=False)
        elif c == 1:
            state["bold"] = True
        elif c == 2:
            state["dim"] = True
        elif c == 4:
            state["underline"] = True
        elif c == 7:
            state["reverse"] = True
        elif c in (22,):
            state.update(bold=False, dim=False)
        elif c == 27:
            state["reverse"] = False
        elif 30 <= c <= 37:
            state["fg"] = ANSI16[c - 30]
        elif 90 <= c <= 97:
            state["fg"] = ANSI16[c - 90 + 8]
        elif 40 <= c <= 47:
            state["bg"] = ANSI16[c - 40]
        elif 100 <= c <= 107:
            state["bg"] = ANSI16[c - 100 + 8]
        elif c == 39:
            state["fg"] = None
        elif c == 49:
            state["bg"] = None
        elif c in (38, 48) and i + 1 < len(codes):
            target = "fg" if c == 38 else "bg"
            if codes[i + 1] == 2 and i + 4 < len(codes):
                state[target] = tuple(codes[i + 2 : i + 5])
                i += 4
            elif codes[i + 1] == 5 and i + 2 < len(codes):
                state[target] = indexed(codes[i + 2])
                i += 2
        i += 1


def parse(text):
    rows = []
    state = dict(fg=None, bg=None, bold=False, dim=False, reverse=False, underline=False)
    for line in text.split("\n"):
        cells, pos = [], 0
        for m in SGR.finditer(line):
            for ch in line[pos : m.start()]:
                cells.append((ch, dict(state)))
            apply_sgr(m.group(1), state)
            pos = m.end()
        for ch in line[pos:]:
            cells.append((ch, dict(state)))
        rows.append(cells)
    return rows


def render(rows, out, scale):
    size = int(16 * scale)
    font = ImageFont.truetype(FONT, size)
    bold = ImageFont.truetype(FONT_BOLD, size)
    cw = int(font.getlength("M"))
    ch = int(size * 1.35)
    width = max(len(r) for r in rows) * cw + 24
    height = len(rows) * ch + 24
    image = Image.new("RGB", (width, height), DEFAULT_BG)
    draw = ImageDraw.Draw(image)
    for y, row in enumerate(rows):
        for x, (char, st) in enumerate(row):
            fg = st["fg"] or DEFAULT_FG
            bg = st["bg"] or DEFAULT_BG
            if st["reverse"]:
                fg, bg = bg, fg
            if st["dim"]:
                fg = tuple(int(v * 0.6 + b * 0.4) for v, b in zip(fg, bg))
            px, py = 12 + x * cw, 12 + y * ch
            if bg != DEFAULT_BG:
                draw.rectangle([px, py, px + cw, py + ch], fill=bg)
            if char != " ":
                draw.text((px, py), char, font=bold if st["bold"] else font, fill=fg)
            if st["underline"]:
                draw.line([px, py + ch - 3, px + cw, py + ch - 3], fill=fg)
    image.save(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/debug/gpulens")
    ap.add_argument("--out", default="/tmp/gpulens.png")
    ap.add_argument("--cols", type=int, default=150)
    ap.add_argument("--rows", type=int, default=46)
    ap.add_argument("--wait", type=float, default=6.0)
    ap.add_argument("--keys", default="", help="space-separated tmux keys to send after the wait")
    ap.add_argument("--after", type=float, default=1.0)
    ap.add_argument("--args", default="--demo --no-ai")
    ap.add_argument("--scale", type=float, default=1.0)
    ap.add_argument("--env", default="", help="extra env assignments for the session, e.g. NO_COLOR=1")
    ap.add_argument("--text", action="store_true", help="print plain text instead of writing a PNG")
    a = ap.parse_args()
    name = f"gpulens-shot-{int(time.time())}"
    cmd = f"env {a.env} {a.bin} {a.args}"
    subprocess.run(["tmux", "new-session", "-d", "-s", name, "-x", str(a.cols), "-y", str(a.rows), cmd], check=True)
    try:
        time.sleep(a.wait)
        if a.keys:
            subprocess.run(["tmux", "send-keys", "-t", name, *a.keys.split()], check=True)
            time.sleep(a.after)
        flags = [] if a.text else ["-e"]
        cap = subprocess.run(["tmux", "capture-pane", "-t", name, "-p", *flags], capture_output=True, text=True, check=True).stdout
    finally:
        subprocess.run(["tmux", "kill-session", "-t", name], check=False)
    if a.text:
        sys.stdout.write(cap)
        return
    render(parse(cap.rstrip("\n")), a.out, a.scale)
    print(a.out)


if __name__ == "__main__":
    main()
