"""mac-trackpad-accel's Hyprland branch, run for real against a fake hyprctl.

Everything lives in a temp dir: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME,
XDG_DATA_HOME and OMARCHY_PATH point into it and PATH starts with a fake
hyprctl (and a fake busctl for the KDE case) that log their arguments. The
real ~/.config/mac-trackpad and ~/.config/hypr are never touched.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ACCEL = os.path.join(REPO, "accel")
TOOL = os.path.join(ACCEL, "mac-trackpad-accel")
sys.path.insert(0, ACCEL)
import mac_hyprland  # noqa: E402

DEVICES = {
    "mice": [
        {"address": "0x1", "name": "apple-inc.-magic-trackpad", "sysname": "event5"},
        # the mac-trackpad daemon's clone: same name, so Hyprland adds -1
        {"address": "0x2", "name": "apple-inc.-magic-trackpad-1", "sysname": "event21"},
        {"address": "0x3", "name": "virtual-trackpad-(created-by-mac-trackpad)", "sysname": "event22"},
        {"address": "0x4", "name": "logitech-usb-receiver", "sysname": "event7"},
    ],
    "keyboards": [],
}

FAKE_HYPRCTL = r"""#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LOG"
[ "$1" = "-j" ] && shift && json=1
case "$1" in
    status) [ -n "$FAKE_PROVIDER" ] && printf '{"configProvider": "%s", "backend": "drm"}\n' "$FAKE_PROVIDER" || echo "unknown request" ;;
    version) echo "Hyprland 0.57.1" ;;
    devices) cat "$FAKE_DEVICES" ;;
    eval|keyword) echo "${FAKE_REPLY:-ok}" ;;
    *) echo "unknown request" ;;
esac
"""

FAKE_BUSCTL = r"""#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LOG"
case "$*" in
    *"tree org.kde.KWin --list"*) printf '/org/kde/KWin/InputDevice/event5\n/org/kde/KWin/InputDevice/event7\n' ;;
    *"get-property"*event5*) echo 's "Apple Inc. Magic Trackpad"' ;;
    *"get-property"*) echo 's "Logitech USB Receiver"' ;;
esac
"""

# A stand-in for Hyprland's Lua API: records every hl.device() table.
LUA_HARNESS = r"""
local calls = {}
hl = { device = function(t) calls[#calls + 1] = t end }
dofile(arg[1])
for _, t in ipairs(calls) do
  local keys = {}
  for k in pairs(t) do keys[#keys + 1] = k end
  table.sort(keys)
  print(t.name .. "|" .. table.concat(keys, ",") .. "|" .. t.accel_profile:sub(1, 12) .. "|" .. t.scroll_points:sub(1, 5) .. "|" .. t.scroll_factor)
end
"""


@unittest.skipIf(shutil.which("python3") is None, "python3 not on PATH")
class HyprlandAccelTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = self.tmp.name
        self.home = os.path.join(t, "home")
        self.cfg = os.path.join(t, "config")
        self.state = os.path.join(t, "state")
        self.omarchy = os.path.join(t, "omarchy")
        self.bin = os.path.join(t, "bin")
        self.log = os.path.join(t, "calls.log")
        for d in (self.home, self.cfg, self.state, self.bin, os.path.join(self.omarchy, "default", "hypr")):
            os.makedirs(d)
        with open(os.path.join(self.omarchy, "default", "hypr", "toggles.lua"), "w") as f:
            f.write("-- stand-in for Omarchy's toggles.lua\n")
        self.devices = os.path.join(t, "devices.json")
        with open(self.devices, "w") as f:
            json.dump(DEVICES, f)
        for name, body in (("hyprctl", FAKE_HYPRCTL), ("busctl", FAKE_BUSCTL)):
            p = os.path.join(self.bin, name)
            with open(p, "w") as f:
                f.write(body)
            os.chmod(p, 0o755)
        self.env = {
            "PATH": self.bin + os.pathsep + os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": self.home,
            "XDG_CONFIG_HOME": self.cfg,
            "XDG_STATE_HOME": self.state,
            "XDG_DATA_HOME": os.path.join(t, "data"),   # empty: the tool uses the repo's scripts
            "OMARCHY_PATH": self.omarchy,
            "FAKE_LOG": self.log,
            "FAKE_DEVICES": self.devices,
            "FAKE_PROVIDER": "lua",
            "HYPRLAND_INSTANCE_SIGNATURE": "test",
            "XDG_CURRENT_DESKTOP": "Hyprland",
        }
        self.conf = os.path.join(self.cfg, "mac-trackpad", "accel.conf")
        self.lua = os.path.join(self.state, "omarchy", "toggles", "hypr", "mac-trackpad-accel.lua")
        self.hyprconf = os.path.join(self.cfg, "hypr", "mac-trackpad.conf")

    def tearDown(self):
        self.tmp.cleanup()

    def run_tool(self, *args, **env):
        e = dict(self.env)
        for k, v in env.items():
            if v is None:
                e.pop(k, None)
            else:
                e[k] = v
        return subprocess.run([TOOL, *args], env=e, capture_output=True, text=True, timeout=60)

    def calls(self):
        try:
            with open(self.log) as f:
                return f.read().splitlines()
        except FileNotFoundError:
            return []

    def read(self, path):
        with open(path) as f:
            return f.read()

    # ---- Lua (Omarchy Quattro)

    def test_set_on_lua_writes_toggle_and_evals(self):
        # the app passes KWin's 1.4 from an old accel.conf; Hyprland must get 1
        r = self.run_tool("set", "--pointer-setting", "1", "--scroll-setting", "0.4412",
                          "--kwin-scroll-factor", "1.4")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("# kwin scroll factor 1.0", self.read(self.conf))
        lua = self.read(self.lua)
        for name in ("apple-inc.-magic-trackpad", "apple-inc.-magic-trackpad-1",
                     "apple-inc.-magic-trackpad-2", "apple-inc.-magic-trackpad-3"):
            self.assertIn('"%s"' % name, lua)
        self.assertNotIn("logitech", lua)
        self.assertNotIn("virtual-trackpad", lua)
        evals = [c for c in self.calls() if c.startswith("eval ")]
        self.assertEqual(len(evals), 1)
        self.assertNotIn("\n", evals[0])
        self.assertIn("hl.device({ name = name, accel_profile = motion", evals[0])
        self.assertFalse(any(c.startswith("keyword") for c in self.calls()))
        self.assertIn("applied live", r.stdout)

    @unittest.skipIf(shutil.which("lua") is None, "lua not installed")
    def test_lua_file_runs_and_sets_every_name(self):
        self.assertEqual(self.run_tool("set").returncode, 0)
        harness = os.path.join(self.tmp.name, "harness.lua")
        with open(harness, "w") as f:
            f.write(LUA_HARNESS)
        out = subprocess.run(["lua", harness, self.lua], capture_output=True, text=True, timeout=30)
        self.assertEqual(out.returncode, 0, out.stderr)
        lines = out.stdout.splitlines()
        self.assertEqual([l.split("|")[0] for l in lines],
                         ["apple-inc.-magic-trackpad", "apple-inc.-magic-trackpad-1",
                          "apple-inc.-magic-trackpad-2", "apple-inc.-magic-trackpad-3"])
        for l in lines:
            name, keys, profile, scroll, factor = l.split("|")
            self.assertEqual(keys, "accel_profile,name,scroll_factor,scroll_points")
            self.assertEqual(profile, "custom 0.25 ")
            self.assertEqual(scroll, "0.25 ")
            self.assertEqual(float(factor), 1.0)

    @unittest.skipIf(shutil.which("lua") is None, "lua not installed")
    def test_hostile_device_name_stays_data(self):
        evil = 'magic-trackpad"}) os.execute("touch PWNED") --'
        with open(self.devices, "w") as f:
            json.dump({"mice": [{"name": evil}]}, f)
        self.assertEqual(self.run_tool("set").returncode, 0)
        harness = os.path.join(self.tmp.name, "harness.lua")
        with open(harness, "w") as f:
            f.write(LUA_HARNESS)
        out = subprocess.run(["lua", harness, self.lua], capture_output=True, text=True,
                             timeout=30, cwd=self.tmp.name)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.tmp.name, "PWNED")))
        self.assertEqual(out.stdout.splitlines()[0].split("|")[0], evil)
        # and hyprlang refuses it rather than quoting it
        self.assertEqual(mac_hyprland.hyprlang_names([evil]), ([], [evil]))

    def test_reload_without_omarchy_prints_dofile_line(self):
        shutil.rmtree(self.omarchy)
        self.run_tool("set")
        r = self.run_tool("reload")
        self.assertEqual(r.returncode, 0, r.stderr)
        path = os.path.join(self.cfg, "hypr", "mac-trackpad.lua")
        self.assertTrue(os.path.isfile(path))
        self.assertIn('dofile("%s")' % path, r.stdout)
        self.assertFalse(os.path.exists(self.lua))

    def test_eval_error_fails(self):
        self.run_tool("set")
        r = self.run_tool("reload", FAKE_REPLY="error: hl.device: unknown field 'scroll_points'")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("unknown field", r.stderr)

    # ---- hyprlang

    def test_set_on_hyprlang_writes_conf_and_uses_keyword(self):
        hypr = os.path.join(self.cfg, "hypr")
        os.makedirs(hypr)
        main = os.path.join(hypr, "hyprland.conf")
        with open(main, "w") as f:
            f.write("# the user's own config\n")
        r = self.run_tool("set", FAKE_PROVIDER="hyprlang")
        self.assertEqual(r.returncode, 0, r.stderr)
        conf = self.read(self.hyprconf)
        self.assertEqual(conf.count("device {"), 4)
        self.assertIn("    name = apple-inc.-magic-trackpad-1\n", conf)
        self.assertIn("    accel_profile = custom 0.25 ", conf)
        self.assertIn("    scroll_points = 0.25 ", conf)
        self.assertIn("    scroll_factor = 1.0\n", conf)
        self.assertIn("source = " + self.hyprconf, r.stdout)
        self.assertEqual(self.read(main), "# the user's own config\n")   # never edited
        keywords = [c for c in self.calls() if c.startswith("keyword ")]
        self.assertEqual(len(keywords), 12)
        self.assertTrue(keywords[0].startswith("keyword device[apple-inc.-magic-trackpad]:scroll_factor 1.0"))
        self.assertTrue(keywords[2].startswith("keyword device[apple-inc.-magic-trackpad]:accel_profile custom 0.25 "))
        self.assertFalse(any(c.startswith("eval") for c in self.calls()))
        self.assertFalse(os.path.exists(self.lua))

    def test_hyprlang_source_line_not_repeated_once_added(self):
        hypr = os.path.join(self.cfg, "hypr")
        os.makedirs(hypr)
        with open(os.path.join(hypr, "hyprland.conf"), "w") as f:
            f.write("source = ~/.config/hypr/mac-trackpad.conf\n")
        r = self.run_tool("set", FAKE_PROVIDER="hyprlang")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("add this line", r.stdout)

    # ---- no Hyprland to ask

    def test_no_running_hyprland_writes_file_only(self):
        r = self.run_tool("set", HYPRLAND_INSTANCE_SIGNATURE=None)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.calls(), [])
        # no hyprland.lua here, so it's taken for hyprlang, with the fallback name
        self.assertIn("name = apple-inc.-magic-trackpad\n", self.read(self.hyprconf))
        self.assertIn("next config reload", r.stdout)
        self.assertIn("no Magic Trackpad", r.stderr)

    def test_old_hyprland_without_status_uses_files(self):
        hypr = os.path.join(self.cfg, "hypr")
        os.makedirs(hypr)
        open(os.path.join(hypr, "hyprland.lua"), "w").close()
        r = self.run_tool("set", FAKE_PROVIDER="")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(os.path.isfile(self.lua))

    def test_status(self):
        self.run_tool("set")
        r = self.run_tool("status")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("config: lua", r.stdout)
        self.assertIn("present", r.stdout)
        self.assertIn("apple-inc.-magic-trackpad, apple-inc.-magic-trackpad-1", r.stdout)

    def test_hand_edited_accel_conf_is_refused(self):
        os.makedirs(os.path.dirname(self.conf))
        with open(self.conf, "w") as f:
            f.write('motion_step=0.25\nmotion_points=0 1"); os.exit(1) --\n'
                    "scroll_step=0.25\nscroll_points=0 1\n")
        r = self.run_tool("reload")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("not a list of plain numbers", r.stderr)
        self.assertFalse(os.path.exists(self.lua))

    # ---- KDE is untouched

    def test_kde_still_uses_kwin(self):
        os.makedirs(os.path.dirname(self.conf))   # the KWin path never made it
        r = self.run_tool("set", HYPRLAND_INSTANCE_SIGNATURE=None, XDG_CURRENT_DESKTOP="KDE")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("# kwin scroll factor 1.4", self.read(self.conf))
        calls = self.calls()
        self.assertIn("--user tree org.kde.KWin --list", calls)
        self.assertIn("--user set-property org.kde.KWin /org/kde/KWin/InputDevice/event5 "
                      "org.kde.KWin.InputDevice pointerAccelerationProfileAdaptive b true", calls)
        self.assertFalse(any("hyprctl" in c or c.startswith(("eval", "keyword", "-j")) for c in calls))
        self.assertFalse(os.path.exists(self.lua) or os.path.exists(self.hyprconf))


class TrackpadNamesTest(unittest.TestCase):
    def test_clone_suffix_is_folded(self):
        names, found = mac_hyprland.trackpad_names(DEVICES)
        self.assertEqual(found, ["apple-inc.-magic-trackpad", "apple-inc.-magic-trackpad-1"])
        self.assertEqual(names, ["apple-inc.-magic-trackpad", "apple-inc.-magic-trackpad-1",
                                 "apple-inc.-magic-trackpad-2", "apple-inc.-magic-trackpad-3"])

    def test_digits_in_the_real_name_are_kept(self):
        names, _ = mac_hyprland.trackpad_names({"mice": [{"name": "apple-inc.-magic-trackpad-2"}]})
        self.assertEqual(names[:2], ["apple-inc.-magic-trackpad-2", "apple-inc.-magic-trackpad-2-1"])

    def test_nothing_found_falls_back(self):
        for devices in (None, {}, {"mice": "x"}, {"mice": [{"name": "logitech"}, 3]}):
            names, found = mac_hyprland.trackpad_names(devices)
            self.assertEqual(found, [])
            self.assertEqual(names[0], "apple-inc.-magic-trackpad")

    def test_lua_string(self):
        self.assertEqual(mac_hyprland.lua_string("apple-inc.-magic-trackpad"), '"apple-inc.-magic-trackpad"')
        self.assertEqual(mac_hyprland.lua_string('a"b'), '"a\\034b"')
        self.assertEqual(mac_hyprland.lua_string("é"), '"\\195\\169"')


if __name__ == "__main__":
    unittest.main()
