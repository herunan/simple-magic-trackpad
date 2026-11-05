#!/usr/bin/env python3
"""Apply accel.conf's Mac curves through Hyprland's own per-device config.

mac-trackpad-accel runs this on Hyprland, where no shim is needed: Hyprland
passes `accel_profile = custom <step> <points>` and `scroll_points = <step>
<points>` of a device straight to libinput's custom profile.

  mac_hyprland.py apply ACCEL_CONF    write the Hyprland file, apply it live
  mac_hyprland.py status ACCEL_CONF   show the file, config type and devices

Which file, by the config Hyprland runs (`hyprctl -j status`, configProvider;
without a running Hyprland, hyprland.lua beats hyprland.conf as in Hyprland):

- Lua with Omarchy (Quattro): mac-trackpad-accel.lua in
  ${XDG_STATE_HOME:-~/.local/state}/omarchy/toggles/hypr/, which Omarchy's
  default/hypr/toggles.lua loads on every config (re)load. Applied live with
  `hyprctl eval`, the way omarchy-toggle-input-device does.
- Lua without Omarchy: ${XDG_CONFIG_HOME:-~/.config}/hypr/mac-trackpad.lua,
  plus the dofile() line to add to hyprland.lua (printed, never added).
- hyprlang (Hyprland 0.56 and older): ${XDG_CONFIG_HOME:-~/.config}/hypr/
  mac-trackpad.conf, plus the source line to add to hyprland.conf (printed,
  never added), applied live with `hyprctl keyword device[NAME]:...`.

Device names come from `hyprctl -j devices`: every pointer whose name has
"magic-trackpad" in it. The mac-trackpad daemon's clone has the real pad's
name, and Hyprland gives a second device of the same name a "-1" suffix
(then "-2", ...), in the order the devices appear. So each name found gets
its -1..-3 variants too, which keeps the curve on whichever of the two ends
up with which name after a re-login or a Bluetooth reconnect. Names are only
data: in Lua every byte outside [A-Za-z0-9._-] is written as a \\ddd escape,
and hyprlang gets only names without characters it would parse.

Scrolling: the curve's output is meant to be used as is (mac-trackpad-accel
passes --kwin-scroll-factor 1 on Hyprland), so each device also gets
scroll_factor = 1.0, replacing Omarchy's touchpad scroll_factor of 0.4 for
the trackpad only. Window rules that set scroll_touchpad (Omarchy has them
for terminals) still win over it in those windows.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

LUA_FILE = "mac-trackpad-accel.lua"
CONF_FILE = "mac-trackpad.conf"
PLAIN_LUA_FILE = "mac-trackpad.lua"
MATCH = "magic-trackpad"
# Hyprland's name when nothing is found: "Apple Inc. Magic Trackpad"
# lowercased with spaces as dashes.
FALLBACK_NAME = "apple-inc.-magic-trackpad"
DUPLICATES = 3
NPOINTS_MAX = 64  # libinput's LIBINPUT_ACCEL_NPOINTS_MAX
_NUMBER = re.compile(r"[0-9]+(?:\.[0-9]+)?(?:[eE][-+]?[0-9]+)?")
# What hyprlang may see in `name = ...` and in `device[...]:`: no blanks,
# comment, variable, brackets, braces, separators or escapes.
_HYPRLANG_NAME = re.compile(r"[^\s#$\[\]{}=;,\\\"']+")


class Error(Exception):
    pass


# ---------------------------------------------------------------- accel.conf


def read_curves(path):
    """(motion_step, motion_points, scroll_step, scroll_points) from accel.conf
    as number strings, checked to be plain non-negative numbers (they end up
    inside Lua and hyprlang text)."""
    try:
        with open(path) as f:
            text = f.read()
    except OSError as e:
        raise Error(f"can't read {path}: {e.strerror or e}") from None
    values = {}
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("#") or "=" not in line:
            continue
        key, val = line.split("=", 1)
        values[key.strip()] = val.strip()
    out = []
    for key in ("motion_step", "motion_points", "scroll_step", "scroll_points"):
        if key not in values:
            raise Error(f"{path}: no {key}=")
        nums = values[key].split()
        if not nums or not all(_NUMBER.fullmatch(n) for n in nums):
            raise Error(f"{path}: {key} is not a list of plain numbers")
        if key.endswith("_step"):
            if len(nums) != 1 or float(nums[0]) <= 0:
                raise Error(f"{path}: {key} must be one number above 0")
            out.append(nums[0])
        else:
            if not 2 <= len(nums) <= NPOINTS_MAX:
                raise Error(f"{path}: {key} must have 2 to {NPOINTS_MAX} points")
            out.append(" ".join(nums))
    return tuple(out)


def profile_values(curves):
    """The accel_profile and scroll_points strings Hyprland expects."""
    motion_step, motion, scroll_step, scroll = curves
    return f"custom {motion_step} {motion}", f"{scroll_step} {scroll}"


# ---------------------------------------------------------------- devices


def trackpad_names(devices):
    """(names to configure, names actually found) from the parsed
    `hyprctl -j devices` output (None if there is none)."""
    found = []
    mice = devices.get("mice") if isinstance(devices, dict) else None
    for m in mice if isinstance(mice, list) else []:
        name = m.get("name") if isinstance(m, dict) else None
        if isinstance(name, str) and MATCH in name.lower() and name not in found:
            found.append(name)
    bases = []
    for name in found:
        m = re.fullmatch(r"(.+)-([0-9]+)", name)
        # "x-1" is Hyprland's second "x" only if "x" itself is there too;
        # otherwise the digits are part of the device's own name
        base = m.group(1) if m and m.group(1) in found else name
        if base not in bases:
            bases.append(base)
    names = []
    for base in bases or [FALLBACK_NAME]:
        for name in [base] + [f"{base}-{i}" for i in range(1, DUPLICATES + 1)]:
            if name not in names:
                names.append(name)
    for name in found:
        if name not in names:
            names.append(name)
    return names, found


def lua_string(s):
    """s as a Lua string literal that can only ever be data: every byte that
    isn't a letter, digit, '.', '_' or '-' becomes a 3-digit decimal escape."""
    out = []
    for b in s.encode("utf-8"):
        c = chr(b)
        out.append(c if (c.isascii() and (c.isalnum() or c in "._-")) else "\\%03d" % b)
    return '"' + "".join(out) + '"'


def hyprlang_names(names):
    """(usable, skipped) for hyprlang."""
    ok = [n for n in names if _HYPRLANG_NAME.fullmatch(n) and n.isprintable()]
    return ok, [n for n in names if n not in ok]


# ---------------------------------------------------------------- output


def lua_code(names, motion, scroll, one_line=False):
    """The hl.device() calls, as a file or as one line for hyprctl eval."""
    names_lua = ", ".join(lua_string(n) for n in names)
    body = [
        f'local motion = "{motion}"',
        f'local scroll = "{scroll}"',
        f"for _, name in ipairs({{ {names_lua} }}) do",
        "  hl.device({ name = name, accel_profile = motion, scroll_points = scroll, scroll_factor = 1.0 })",
        "end",
    ]
    if one_line:
        return " ".join(line.strip() for line in body)
    return "\n".join(body) + "\n"


def lua_file(names, motion, scroll, source):
    return (
        f"-- Generated by mac-trackpad-accel from {source}; edits are overwritten.\n"
        "-- The Mac pointer and scroll curves for the Magic Trackpad (and the\n"
        "-- mac-trackpad daemon's clone of it, which has the same name plus a\n"
        "-- Hyprland -N suffix). Delete this file to go back to Hyprland's own.\n"
        + lua_code(names, motion, scroll)
    )


def hyprlang_file(names, motion, scroll, source):
    out = [
        f"# Generated by mac-trackpad-accel from {source}; edits are overwritten.",
        "# The Mac pointer and scroll curves for the Magic Trackpad (and the",
        "# mac-trackpad daemon's clone of it, which has the same name plus a",
        "# Hyprland -N suffix). Remove the source line for it to go back.",
    ]
    for name in names:
        out += [
            "device {",
            f"    name = {name}",
            f"    accel_profile = {motion}",
            f"    scroll_points = {scroll}",
            "    scroll_factor = 1.0",
            "}",
        ]
    return "\n".join(out) + "\n"


def write_atomic(path, text):
    d = os.path.dirname(path)
    os.makedirs(d, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix="." + os.path.basename(path) + ".", suffix=".tmp", dir=d)
    try:
        with os.fdopen(fd, "w") as f:
            f.write(text)
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise


# ---------------------------------------------------------------- hyprctl


def hyprctl(*args):
    """hyprctl's output, or None when there's no Hyprland to ask."""
    if not os.environ.get("HYPRLAND_INSTANCE_SIGNATURE") or not shutil.which("hyprctl"):
        return None
    try:
        r = subprocess.run(["hyprctl", *args], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return r.stdout.strip() if r.returncode == 0 else None


def hyprctl_json(*args):
    out = hyprctl("-j", *args)
    try:
        return json.loads(out) if out else None
    except ValueError:
        return None


# ---------------------------------------------------------------- paths


def home():
    return os.environ.get("HOME") or os.path.expanduser("~")


def config_home():
    return os.environ.get("XDG_CONFIG_HOME") or os.path.join(home(), ".config")


def state_home():
    return os.environ.get("XDG_STATE_HOME") or os.path.join(home(), ".local", "state")


def omarchy_toggles():
    """Omarchy's Lua toggles directory if this is Omarchy, else None. Same
    paths as Omarchy's default/hypr/paths.lua and toggles.lua."""
    omarchy = os.environ.get("OMARCHY_PATH") or "/usr/share/omarchy"
    if os.path.isfile(os.path.join(omarchy, "default", "hypr", "toggles.lua")):
        return os.path.join(state_home(), "omarchy", "toggles", "hypr")
    return None


def config_type(status):
    provider = status.get("configProvider") if isinstance(status, dict) else None
    if provider in ("lua", "hyprlang"):
        return provider
    # No running Hyprland to ask (or one too old to say): Hyprland itself
    # takes hyprland.lua when it exists, else hyprland.conf.
    return "lua" if os.path.isfile(os.path.join(config_home(), "hypr", "hyprland.lua")) else "hyprlang"


def target(kind):
    """(path of the file to write, line the user must add or None)."""
    hypr = os.path.join(config_home(), "hypr")
    if kind == "lua":
        toggles = omarchy_toggles()
        if toggles:
            return os.path.join(toggles, LUA_FILE), None
        path = os.path.join(hypr, PLAIN_LUA_FILE)
        quoted = f'"{path}"' if re.fullmatch(r'[^"\\\n]*', path) else lua_string(path)
        return path, f"dofile({quoted})"
    path = os.path.join(hypr, CONF_FILE)
    return path, f"source = {path}"


def already_included(kind, path):
    """Whether the user's main config already mentions the file (read only)."""
    main = os.path.join(config_home(), "hypr", "hyprland.lua" if kind == "lua" else "hyprland.conf")
    try:
        with open(main, errors="replace") as f:
            return os.path.basename(path) in f.read()
    except OSError:
        return False


# ---------------------------------------------------------------- commands


def apply(accel_conf):
    motion, scroll = profile_values(read_curves(accel_conf))
    st = hyprctl_json("status")
    # Hyprland before `status` existed still answers `version`.
    live = st is not None or hyprctl("version") is not None
    kind = config_type(st)
    names, found = trackpad_names(hyprctl_json("devices"))
    if not found:
        print(f"mac-trackpad-accel: no Magic Trackpad in `hyprctl devices`; using {FALLBACK_NAME}",
              file=sys.stderr)
    path, line = target(kind)
    source = accel_conf.replace(home(), "~", 1) if accel_conf.startswith(home() + "/") else accel_conf
    # it goes into a comment line: no newline may end that comment early
    source = re.sub(r"[\x00-\x1f\x7f]", "?", source)

    if kind == "lua":
        write_atomic(path, lua_file(names, motion, scroll, source))
    else:
        names, skipped = hyprlang_names(names)
        for n in skipped:
            print(f"mac-trackpad-accel: skipping device name hyprlang can't hold: {n!r}", file=sys.stderr)
        if not names:
            raise Error("no device name usable in hyprlang")
        write_atomic(path, hyprlang_file(names, motion, scroll, source))
    print(f"wrote {path} ({kind}; devices: {', '.join(names)})")
    if line and not already_included(kind, path):
        main = "hyprland.lua" if kind == "lua" else "hyprland.conf"
        print(f"add this line to {os.path.join(config_home(), 'hypr', main)} once, so it's loaded at login:")
        print(f"  {line}")

    if not live:
        print("Hyprland isn't reachable from here; it takes effect at its next config reload")
        return 0
    failures = []
    if kind == "lua":
        reply = hyprctl("eval", lua_code(names, motion, scroll, one_line=True))
        if reply != "ok":
            failures.append(f"hyprctl eval: {reply}")
    else:
        for name in names:
            for key, value in (("scroll_factor", "1.0"), ("scroll_points", scroll), ("accel_profile", motion)):
                reply = hyprctl("keyword", f"device[{name}]:{key}", value)
                if reply != "ok":
                    failures.append(f"hyprctl keyword device[{name}]:{key}: {reply}")
    if failures:
        raise Error("; ".join(failures))
    print("applied live")
    return 0


def status(accel_conf):
    st = hyprctl_json("status")
    kind = config_type(st)
    path, line = target(kind)
    names, found = trackpad_names(hyprctl_json("devices"))
    print(f"config: {kind}" + ("" if st is not None else " (no running Hyprland asked)"))
    print(f"file: {path} ({'present' if os.path.isfile(path) else 'missing; run mac-trackpad-accel reload'})")
    if line:
        print(f"included from the main config: {'yes' if already_included(kind, path) else 'no, add: ' + line}")
    print("trackpads now: " + (", ".join(found) if found else "none found"))
    print(f"curves from: {accel_conf}" + ("" if os.path.isfile(accel_conf) else " (missing)"))
    return 0


def main(argv):
    if len(argv) != 3 or argv[1] not in ("apply", "status"):
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    try:
        return apply(argv[2]) if argv[1] == "apply" else status(argv[2])
    except Error as e:
        print(f"mac-trackpad-accel: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
