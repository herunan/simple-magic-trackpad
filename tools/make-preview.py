#!/usr/bin/env python3
"""Draw preview.png: the glide trail uses the daemon's real decay model.
Needs rsvg-convert. Run from the repo root."""
import subprocess

FAST, TAIL, TAIL_SPEED, STOP = 0.975, 0.91, 15.875, 1.905  # per 8 ms; mm/s
BG, PANEL, LINE, FG, DIM = "#1a1b26", "#16161e", "#3b4261", "#c0caf5", "#565f89"
BLUE, CYAN, MAGENTA, GREEN = "#7aa2f7", "#7dcfff", "#bb9af7", "#9ece6a"
MONO = "'JetBrainsMono Nerd Font','MesloLGM Nerd Font Mono','DejaVu Sans Mono',monospace"


def glide(v):
    """Distance (mm) and speed at each 8 ms tick."""
    x, out = 0.0, []
    while v > STOP:
        out.append((x, v))
        x += v * 0.008
        v *= FAST - (FAST - TAIL) * max(0.0, (TAIL_SPEED - v) / TAIL_SPEED)
    return out


def text(x, y, s, size, fill=FG, weight=400, anchor="start", spacing=0):
    return (f'<text x="{x}" y="{y}" font-family="{MONO}" font-size="{size}" fill="{fill}" '
            f'font-weight="{weight}" text-anchor="{anchor}" letter-spacing="{spacing}">{s}</text>')


def slider(x, y, w, label, ticks, pos):
    o = [text(x, y, label, 22, FG)]
    ty = y + 30
    kx = x + w * pos / (ticks - 1)
    o.append(f'<rect x="{x}" y="{ty}" width="{w}" height="4" fill="{LINE}"/>')
    o.append(f'<rect x="{x}" y="{ty}" width="{kx - x:.1f}" height="4" fill="{BLUE}"/>')
    for i in range(ticks):
        tx = x + w * i / (ticks - 1)
        o.append(f'<rect x="{tx - 1:.1f}" y="{ty + 12}" width="2" height="8" fill="{BLUE if i <= pos else LINE}"/>')
    o.append(f'<rect x="{kx - 8:.1f}" y="{ty - 10}" width="16" height="24" fill="{FG}"/>')
    return o


def switch(x, y, label):
    return [f'<rect x="{x}" y="{y - 20}" width="52" height="26" fill="{BLUE}"/>',
            f'<rect x="{x + 30}" y="{y - 16}" width="18" height="18" fill="{BG}"/>',
            text(x + 70, y, label, 22, FG)]


W, H = 1600, 1000
s = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">',
     '<defs><filter id="glow" x="-200%" y="-200%" width="500%" height="500%">'
     '<feGaussianBlur stdDeviation="14"/></filter></defs>',
     f'<rect width="{W}" height="{H}" fill="{BG}"/>']

# title
s += [text(90, 170, "simple", 84, FG, 700), text(90, 262, "magic", 84, BLUE, 700), text(90, 354, "trackpad", 84, FG, 700),
      text(90, 412, "Mac-matched glide and three-finger drag", 26, FG)]

# settings panel
px, py, pw, ph = 90, 480, 620, 420
s.append(f'<rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="{PANEL}" stroke="{BLUE}" stroke-width="2"/>')
s += slider(px + 40, py + 60, pw - 80, "Tracking speed", 10, 6)
s += slider(px + 40, py + 160, pw - 80, "Scroll speed", 8, 5)
s += switch(px + 40, py + 300, "Use inertia when scrolling")
s += switch(px + 40, py + 356, "Three finger drag")

# trackpad and the real glide
tx, ty, tw, th = 860, 90, 650, 820
s.append(f'<rect x="{tx}" y="{ty}" width="{tw}" height="{th}" rx="34" fill="{PANEL}" stroke="{LINE}" stroke-width="3"/>')
ticks = glide(420.0)
scale = (th - 230) / ticks[-1][0]
top = ty + 115
for fx in (tx + tw * 0.39, tx + tw * 0.61):
    for i, (d, v) in enumerate(ticks):
        if i % 3:
            continue
        f = v / ticks[0][1]
        s.append(f'<circle cx="{fx:.1f}" cy="{top + d * scale:.1f}" r="{3 + 11 * f:.1f}" fill="{CYAN if f > .35 else MAGENTA}" '
                 f'opacity="{0.16 + 0.8 * f ** 0.6:.2f}"/>')
    s.append(f'<circle cx="{fx:.1f}" cy="{top}" r="42" fill="{CYAN}" opacity=".55" filter="url(#glow)"/>')
    s.append(f'<circle cx="{fx:.1f}" cy="{top}" r="27" fill="{BG}" stroke="{CYAN}" stroke-width="4"/>')
s.append('</svg>')

open("preview.svg", "w").write("\n".join(s))
subprocess.run(["rsvg-convert", "-o", "preview.png", "preview.svg"], check=True)
