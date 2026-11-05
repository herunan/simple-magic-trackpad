#!/usr/bin/env python3
"""Turn the Mac's trackpad acceleration curves into libinput custom-profile points.

macOS describes each curve with a handful of parameters (see mac-params.json).
The curve maps finger speed to cursor speed in three pieces:

  1. a polynomial  y = (q*x)^4 + (c*x)^3 + (p*x)^2 + l*x   up to tangent_linear
  2. the straight line tangent to it, up to tangent_root
  3. a square-root falloff y = sqrt(a*x + b) that joins the line smoothly

x is finger speed in inches/s, y is cursor speed in inches/s on a nominal
96 dpi screen. That reading comes from WebKit's re-implementation of the same
curves (Source/WebKit/Shared/ScrollingAccelerationCurve.cpp, BSD-2, see
NOTICE): it divides device speed by resolution/67 and multiplies the result by
96/67, i.e. counts-per-67Hz-frame in, 96 dpi pixels-per-frame out.

libinput's custom profile wants, for touchpads, input speed in device units
per ms (x-axis resolution) and output speed in logical pixels per ms, sampled
at a fixed step. That's what this prints, in the format the KWin shim reads.

Usage: mac_curves.py [--res 47] [--pointer-gain 1.0] [--scroll-gain 1.0]
                     [--kwin-scroll-factor 1.4] > ~/.config/mac-trackpad/accel.conf
"""
import argparse
import json
import math
import os

FIXED = 65536.0
NPOINTS = 64  # libinput's maximum (LIBINPUT_ACCEL_NPOINTS_MAX)
MM_PER_INCH = 25.4
SCREEN_DPI = 96.0


class Curve:
    def __init__(self, p):
        self.l = p["linear"] / FIXED
        self.p = p["parabolic"] / FIXED
        self.c = p["cubic"] / FIXED
        self.q = p["quartic"] / FIXED
        self.t1 = p["tangent_linear"] / FIXED
        self.t2 = p["tangent_root"] / FIXED

        # straight line tangent to the polynomial at t1
        y1 = self._poly(self.t1)
        self.slope = self._dpoly(self.t1)
        self.icept = y1 - self.slope * self.t1
        # sqrt(a*x + b) matching value and slope of the line at t2
        y2 = self.slope * self.t2 + self.icept
        self.fa = 2 * y2 * self.slope
        self.fb = y2 * y2 - self.fa * self.t2

    def _poly(self, x):
        return (self.q * x) ** 4 + (self.c * x) ** 3 + (self.p * x) ** 2 + self.l * x

    def _dpoly(self, x):
        return (4 * x**3 * self.q**4 + 3 * x**2 * self.c**3 + 2 * x * self.p**2 + self.l)

    def __call__(self, x):
        if x <= self.t1:
            return self._poly(x)
        if x <= self.t2:
            return self.slope * x + self.icept
        return math.sqrt(self.fa * x + self.fb)


def curve_for(table, setting):
    """The curve for a slider value; linear blend of the two nearest, like macOS."""
    idx = setting * FIXED
    rows = sorted(table, key=lambda r: r["index"])
    lo = max((r for r in rows if r["index"] <= idx), key=lambda r: r["index"], default=rows[0])
    hi = min((r for r in rows if r["index"] >= idx), key=lambda r: r["index"], default=rows[-1])
    if lo is hi or hi["index"] == lo["index"]:
        return Curve(lo)
    t = (idx - lo["index"]) / (hi["index"] - lo["index"])
    keys = ("linear", "parabolic", "cubic", "quartic", "tangent_linear", "tangent_root")
    return Curve({k: lo[k] + (hi[k] - lo[k]) * t for k in keys})


def points(curve, res, step, gain):
    """Output px/ms for input speeds 0, step, 2*step, ... in device units/ms."""
    out = []
    for i in range(NPOINTS):
        units_per_ms = i * step
        inch_per_s = units_per_ms * 1000.0 / res / MM_PER_INCH
        px_per_ms = curve(inch_per_s) * SCREEN_DPI / 1000.0 * gain
        out.append(min(px_per_ms, 10000.0))  # LIBINPUT_ACCEL_POINT_MAX_VALUE
    return out


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--params", default=os.path.join(here, "mac-params.json"))
    ap.add_argument("--res", type=float, default=47.0,
                    help="touchpad x resolution, units/mm (Magic Trackpad 2: 47)")
    ap.add_argument("--pointer-setting", type=float, help="macOS tracking speed (0-3)")
    ap.add_argument("--scroll-setting", type=float, help="macOS scroll acceleration")
    ap.add_argument("--pointer-gain", type=float, default=1.0, help="overall pointer multiplier")
    ap.add_argument("--scroll-gain", type=float, default=1.0, help="overall scroll multiplier")
    ap.add_argument("--kwin-scroll-factor", type=float, default=1.4,
                    help="KWin's own scroll speed for the trackpad; divided out so the "
                         "Mac curve is what you get at the current KWin setting")
    ap.add_argument("--step", type=float, default=0.25, help="units/ms between points")
    a = ap.parse_args()

    with open(a.params) as f:
        params = json.load(f)
    psetting = a.pointer_setting if a.pointer_setting is not None else params["pointer_setting"]
    ssetting = a.scroll_setting if a.scroll_setting is not None else params["scroll_setting"]

    motion = points(curve_for(params["pointer"], psetting), a.res, a.step, a.pointer_gain)
    scroll = points(curve_for(params["scroll"], ssetting), a.res, a.step,
                    a.scroll_gain / a.kwin_scroll_factor)

    fmt = lambda pts: " ".join(f"{p:.4f}" for p in pts)
    print(f"# generated by mac_curves.py: pointer setting {psetting}, scroll setting {ssetting},")
    print(f"# res {a.res} units/mm, pointer gain {a.pointer_gain}, scroll gain {a.scroll_gain},")
    print(f"# kwin scroll factor {a.kwin_scroll_factor}")
    print(f"motion_step={a.step}")
    print(f"motion_points={fmt(motion)}")
    print(f"scroll_step={a.step}")
    print(f"scroll_points={fmt(scroll)}")


if __name__ == "__main__":
    main()
