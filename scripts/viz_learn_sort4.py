#!/usr/bin/env python3
"""Render a GIF of learn_sort4.gym LEARNING to sort.

The 5 comparators of a 4-wire sorting network each start with a random (mostly
wrong) DIRECTION. Trained on (unsorted, sorted) pairs, every direction swings
positive and the network becomes an ascending sorter. This script traces the
five directions and the loss at each GRAD step, then draws:

  * the sorting-network wiring, each comparator colored by its learned sign,
  * the five direction values as diverging bars,
  * the training-loss curve,
  * a live sample array flowing through the current (soft) network.

Pure Python + Pillow (no numpy / matplotlib). Run from the repo root:

    python3 scripts/viz_learn_sort4.py
"""
from __future__ import annotations

import math
import os
import random
import sys

from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))
from gymbo.autodiff import V          # noqa: E402
from gymbo.parser import parse        # noqa: E402

HERE = os.path.dirname(__file__)
SRC = os.path.join(HERE, "..", "examples", "learn_sort4.gym")
OUT = os.path.join(HERE, "..", "docs", "learn_sort4.gif")

PAIRS = [(0, 1), (2, 3), (0, 2), (1, 3), (1, 2)]   # the 5 comparators, in order
DNAMES = ["d1", "d2", "d3", "d4", "d5"]
SAMPLE = [3.0, 1.0, 4.0, 2.0]                       # the live demo array


# --------------------------------------------------------------------------- #
# 1. Training data: 400 steps of  x0 x1 x2 x3  s0 s1 s2 s3  (s = sorted x)
# --------------------------------------------------------------------------- #
def make_training(steps=400, seed=7):
    rng = random.Random(seed)
    data = []
    for _ in range(steps):
        xs = [rng.randint(0, 9) for _ in range(4)]
        data += xs + sorted(xs)
    return data


# --------------------------------------------------------------------------- #
# 2. Traced training run — a faithful mirror of vm.run_full, but recording the
#    five directions after every GRAD update (vm.run_full only exposes the
#    final params + loss history, and we want the whole trajectory).
# --------------------------------------------------------------------------- #
def trace(source, inp):
    prog = parse(source)
    imms = {s: V(v) for s, v in prog.imm_init.items()}
    M, out, loss_hist, snaps = {}, [], [], []
    r0, L = V(0.0), V(0.0)
    ip = pc = steps = 0
    code = prog.code
    params = list(range(5))            # d1..d5 are slots 0..4

    def mem(a):
        return M.get(a, V(0.0))

    def val(operand):
        k, v = operand
        if k == "num":
            return V(v)
        if k == "param":
            return imms[v]
        return mem(v)

    pc = prog.entry
    while pc < len(code) and steps < 10_000_000:
        ins = code[pc]
        pc += 1
        steps += 1
        op = ins.op
        if op == "LOAD":
            r0 = val(ins.operand)
        elif op == "ADD":
            r0 = r0 + val(ins.operand)
        elif op == "SUB":
            r0 = r0 - val(ins.operand)
        elif op == "MUL":
            r0 = r0 * val(ins.operand)
        elif op == "ST":
            M[ins.addr] = r0
        elif op == "SQ":
            r0 = r0 * r0
        elif op == "SIGMOID":
            r0 = r0.sigmoid()
        elif op == "LOSS":
            L = L + r0
        elif op == "GRAD":
            for s in prog.groups.get(ins.group, ()):
                imms[s].grad = 0.0
            L.backward()
            for s in prog.groups.get(ins.group, ()):
                imms[s].data -= ins.eta * imms[s].grad
            loss_hist.append(L.data)
            snaps.append([imms[s].data for s in params])
            L = V(0.0)
            r0 = V(r0.data)
            M = {a: V(v.data) for a, v in M.items()}
        elif op == "JMP":
            pc = ins.label
        elif op == "JZ":
            if round(r0.data) == 0:
                pc = ins.label
        elif op == "OUT":
            out.append(r0.data)
        elif op == "IN":
            r0 = V(inp[ip]) if ip < len(inp) else V(0.0)
            ip += 1
        elif op == "HALT":
            break
    return loss_hist, snaps


def sigmoid(x):
    if x >= 0:
        return 1.0 / (1.0 + math.exp(-x))
    e = math.exp(x)
    return e / (1.0 + e)


def soft_sort(x, d):
    """Forward pass of the soft network with the given directions."""
    w = list(x)
    for (i, j), di in zip(PAIRS, d):
        g = sigmoid(di * (w[i] - w[j]))
        lo = w[i] + g * (w[j] - w[i])
        hi = w[i] + w[j] - lo
        w[i], w[j] = lo, hi
    return w


# --------------------------------------------------------------------------- #
# 3. Drawing
# --------------------------------------------------------------------------- #
W, H = 980, 600
BG = (13, 17, 23)
PANEL = (22, 27, 34)
EDGE = (48, 54, 61)
INK = (201, 209, 217)
MUTE = (139, 148, 158)
GREEN = (63, 185, 80)
RED = (248, 81, 73)
BLUE = (88, 166, 255)
GOLD = (210, 168, 60)


def _font(size, bold=False):
    names = (["DejaVuSans-Bold.ttf"] if bold else []) + ["DejaVuSans.ttf"]
    for n in names:
        try:
            return ImageFont.truetype(n, size)
        except OSError:
            continue
    return ImageFont.load_default()


F_TITLE = _font(24, bold=True)
F_H = _font(15, bold=True)
F = _font(13)
F_SM = _font(11)
F_MONO = _font(14, bold=True)


def lerp(a, b, t):
    return tuple(int(a[k] + (b[k] - a[k]) * t) for k in range(3))


def dcolor(d, alpha=1.0):
    base = GREEN if d >= 0 else RED
    return lerp(PANEL, base, alpha)


def panel(dr, x, y, w, h, title):
    dr.rounded_rectangle([x, y, x + w, y + h], 10, fill=PANEL, outline=EDGE)
    dr.text((x + 14, y + 10), title, font=F_H, fill=INK)


def text_center(dr, cx, y, s, font, fill):
    l, t, r, b = dr.textbbox((0, 0), s, font=font)
    dr.text((cx - (r - l) / 2, y), s, font=font, fill=fill)


def draw_network(dr, x, y, w, h, d):
    panel(dr, x, y, w, h, "Sorting network — comparator directions")
    ax0, ax1 = x + 55, x + w - 30
    wy = [y + 58 + i * 34 for i in range(4)]
    for i in range(4):
        dr.line([ax0, wy[i], ax1, wy[i]], fill=EDGE, width=2)
        dr.text((x + 20, wy[i] - 8), f"w{i}", font=F_SM, fill=MUTE)
    # comparator columns: (0,1)(2,3) | (0,2)(1,3) | (1,2)
    cols = [0, 0, 1, 1, 2]
    span = (ax1 - ax0)
    cxs = [ax0 + span * (0.18 + 0.30 * c) for c in cols]
    # nudge parallel comparators in the same column apart horizontally
    cxs[0] -= 14; cxs[1] += 14
    cxs[2] -= 14; cxs[3] += 14
    for k, ((i, j), di) in enumerate(zip(PAIRS, d)):
        cx = cxs[k]
        conf = min(1.0, abs(di) / 1.2)
        col = dcolor(di, 0.35 + 0.65 * conf)
        lw = 2 + int(4 * conf)
        y0, y1 = wy[i], wy[j]
        dr.line([cx, y0, cx, y1], fill=col, width=lw)
        for yy in (y0, y1):
            dr.ellipse([cx - 5, yy - 5, cx + 5, yy + 5], fill=col)
        text_center(dr, cx, min(y0, y1) - 20, f"{di:+.2f}", F_SM, col)
        text_center(dr, cx, max(y0, y1) + 8, DNAMES[k], F_SM, MUTE)
    # legend
    ly = y + h - 22
    dr.ellipse([x + 16, ly, x + 26, ly + 10], fill=GREEN)
    dr.text((x + 32, ly - 2), "ascending (d>0)", font=F_SM, fill=MUTE)
    dr.ellipse([x + 168, ly, x + 178, ly + 10], fill=RED)
    dr.text((x + 184, ly - 2), "descending (d<0)", font=F_SM, fill=MUTE)


def draw_bars(dr, x, y, w, h, d, dmax):
    panel(dr, x, y, w, h, "Learned directions")
    base = y + h - 34
    top = y + 40
    half = (base - top) / 2
    mid = top + half
    dr.line([x + 14, mid, x + w - 14, mid], fill=EDGE, width=1)
    n = len(d)
    slot = (w - 40) / n
    bw = slot * 0.5
    for k, di in enumerate(d):
        cx = x + 20 + slot * (k + 0.5)
        frac = max(-1.0, min(1.0, di / dmax))
        bh = half * abs(frac)
        col = GREEN if di >= 0 else RED
        if di >= 0:
            dr.rectangle([cx - bw / 2, mid - bh, cx + bw / 2, mid], fill=col)
        else:
            dr.rectangle([cx - bw / 2, mid, cx + bw / 2, mid + bh], fill=col)
        text_center(dr, cx, base + 4, DNAMES[k], F_SM, MUTE)
        yy = (mid - bh - 16) if di >= 0 else (mid + bh + 4)
        text_center(dr, cx, yy, f"{di:+.1f}", F_SM, col)


def draw_loss(dr, x, y, w, h, hist, upto, lmax):
    panel(dr, x, y, w, h, "Training loss")
    px0, py0 = x + 40, y + 40
    px1, py1 = x + w - 18, y + h - 26
    dr.line([px0, py1, px1, py1], fill=EDGE, width=1)
    dr.line([px0, py0, px0, py1], fill=EDGE, width=1)
    n = len(hist)

    def X(i):
        return px0 + (px1 - px0) * (i / max(1, n - 1))

    def Y(v):
        return py1 - (py1 - py0) * (min(v, lmax) / lmax)

    pts = [(X(i), Y(hist[i])) for i in range(upto + 1)]
    if len(pts) > 1:
        dr.line(pts, fill=BLUE, width=2, joint="curve")
    cx, cy = pts[-1]
    dr.ellipse([cx - 4, cy - 4, cx + 4, cy + 4], fill=BLUE)
    dr.text((px0 - 34, py0 - 6), f"{lmax:.0f}", font=F_SM, fill=MUTE)
    dr.text((px0 - 22, py1 - 6), "0", font=F_SM, fill=MUTE)
    dr.text((px1 - 66, py1 + 6), f"step {upto+1}/{n}", font=F_SM, fill=MUTE)
    text_center(dr, cx, cy - 18, f"{hist[upto]:.2f}", F_SM, BLUE)


def draw_sample(dr, x, y, w, h, d):
    panel(dr, x, y, w, h, "Live sample")
    outv = soft_sort(SAMPLE, d)
    target = sorted(SAMPLE)
    dr.text((x + 14, y + 34), "in:  " + "  ".join(f"{v:.0f}" for v in SAMPLE),
            font=F_MONO, fill=MUTE)
    base = y + h - 30
    top = y + 74
    vmax = max(max(SAMPLE), 1)
    n = 4
    slot = (w - 40) / n
    bw = slot * 0.5
    ok = all(abs(o - t) < 0.5 for o, t in zip(outv, target))
    for k in range(n):
        cx = x + 20 + slot * (k + 0.5)
        bh = (base - top) * (outv[k] / vmax)
        good = abs(outv[k] - target[k]) < 0.5
        col = GREEN if good else GOLD
        dr.rectangle([cx - bw / 2, base - bh, cx + bw / 2, base], fill=col)
        text_center(dr, cx, base - bh - 16, f"{outv[k]:.1f}", F_SM, INK)
        text_center(dr, cx, base + 4, f"→{target[k]:.0f}", F_SM, MUTE)
    tag = "sorted ✓" if ok else "sorting…"
    dr.text((x + w - 92, y + 34), tag, font=F_H, fill=GREEN if ok else GOLD)


def render_frame(hist, snaps, i, dmax, lmax):
    img = Image.new("RGB", (W, H), BG)
    dr = ImageDraw.Draw(img)
    d = snaps[i]
    dr.text((28, 20), "learn_sort4.gym", font=F_TITLE, fill=INK)
    dr.text((30, 50), "a 4-wire sorting network learning which way each "
            "comparator points", font=F, fill=MUTE)
    m = 24
    top_y, top_h = 78, 300
    col_w = (W - 3 * m) / 2
    draw_network(dr, m, top_y, col_w, top_h, d)
    draw_loss(dr, m * 2 + col_w, top_y, col_w, top_h, hist, i, lmax)
    bot_y, bot_h = top_y + top_h + 20, 168
    draw_bars(dr, m, bot_y, col_w, bot_h, d, dmax)
    draw_sample(dr, m * 2 + col_w, bot_y, col_w, bot_h, d)
    return img


def main():
    src = open(SRC).read()
    inp = make_training(steps=400)
    hist, snaps = trace(src, inp)
    n = len(snaps)
    print(f"traced {n} GRAD steps; final loss={hist[-1]:.4f}, "
          f"final d={['%+.2f' % v for v in snaps[-1]]}")

    dmax = max(0.5, max(abs(v) for s in snaps for v in s))
    lmax = max(hist) * 1.02

    # subsample to a smooth ~70-frame clip; more frames early where it moves fast
    idx = sorted(set(
        [round((k / 69) ** 0.85 * (n - 1)) for k in range(70)] + [n - 1]))
    frames = [render_frame(hist, snaps, i, dmax, lmax) for i in idx]
    # per-frame timing: brisk animation, then a long hold on the solved state
    # (a single long frame instead of duplicates, which the GIF optimizer merges)
    durations = [60] * (len(frames) - 1) + [2600]

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    frames[0].save(OUT, save_all=True, append_images=frames[1:],
                   duration=durations, loop=0, optimize=True)
    print(f"wrote {OUT}  ({len(frames)} frames, {sum(durations)/1000:.1f}s)")


if __name__ == "__main__":
    main()
