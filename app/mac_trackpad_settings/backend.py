"""Qt side: the object QML talks to. All file/format logic is in core.py."""
import os

from PyQt6.QtCore import (QObject, QProcess, QProcessEnvironment, QTimer,
                          pyqtProperty, pyqtSignal, pyqtSlot)

from . import core

APP_DIR = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))


class Backend(QObject):
    valuesChanged = pyqtSignal()
    statusChanged = pyqtSignal()

    def __init__(self, parent=None, env=None):
        super().__init__(parent)
        # The curves are applied by a KWin shim or by Hyprland's device
        # config (both through mac-trackpad-accel), so the speed sliders only
        # work on KDE and Hyprland; the switches work everywhere.
        self._speeds = core.curves_supported(env)
        self._status = ""
        self._error = False
        self._dir = core.config_dir()
        self._daemon_path = os.path.join(self._dir, "daemon.json")
        self._accel_path = os.path.join(self._dir, "accel.conf")

        # mac-params.json: where the settings default to without an accel.conf
        problems = []
        params = None
        curves = core.find_curves_script(APP_DIR) if self._speeds else None
        if curves:
            try:
                params = core.load_params(curves)
            except Exception as e:  # noqa: BLE001 - shown to the user
                problems.append(f"can't load mac-params.json next to {curves}: {e}")
        self._defaults = core.accel_defaults(params)

        # stored values are kept as they are; nothing is written until the
        # user moves a control
        self._momentum = core.MOMENTUM_SWITCH_DEFAULT
        self._drag = core.DRAG_SWITCH_DEFAULT
        try:
            daemon = core.read_json(self._daemon_path)
            self._momentum = core.momentum_enabled(daemon)
            self._drag = core.drag_enabled(daemon)
        except (OSError, ValueError) as e:
            problems.append(f"can't read {self._daemon_path}: {e}")
        conf = None
        try:
            conf = core.read_accel_conf(self._accel_path)
        except OSError as e:
            if self._speeds:
                problems.append(f"can't read {self._accel_path}: {e}")
        else:
            if conf is None and self._speeds:
                problems.append(f"{self._accel_path} not found; showing Mac defaults")
        values = core.accel_values(conf, self._defaults)
        self._pointer = values["pointerSetting"]
        self._scroll = values["scrollSetting"]

        self._mtimer = QTimer(self, singleShot=True, interval=300)
        self._mtimer.timeout.connect(self._write_momentum)
        self._dtimer = QTimer(self, singleShot=True, interval=300)
        self._dtimer.timeout.connect(self._write_drag)
        self._ctimer = QTimer(self, singleShot=True, interval=500)
        self._ctimer.timeout.connect(self._apply_curve)
        self._proc = None
        self._pending_curve = False

        if problems:
            self._set_status("; ".join(problems), True)
        else:
            self._set_status("Ready", False)

    # ---- properties -------------------------------------------------------

    @pyqtProperty(bool, constant=True)
    def speedsAvailable(self):
        """False off KDE and Hyprland: the speed sliders are disabled and nothing runs
        mac-trackpad-accel."""
        return self._speeds

    @pyqtProperty(int, notify=valuesChanged)
    def pointerPosition(self):
        """Tracking speed slider position: the nearest of the 10 macOS speeds."""
        return core.nearest_position(self._pointer)

    @pyqtProperty(int, constant=True)
    def pointerMaxPosition(self):
        return len(core.POINTER_POSITIONS) - 1

    @pyqtProperty(int, notify=valuesChanged)
    def scrollPosition(self):
        """Scroll speed slider position: the nearest of the 8 macOS speeds."""
        return core.nearest_position(self._scroll, core.SCROLL_POSITIONS)

    @pyqtProperty(int, constant=True)
    def scrollMaxPosition(self):
        return len(core.SCROLL_POSITIONS) - 1

    @pyqtProperty(bool, notify=valuesChanged)
    def momentum(self):
        return self._momentum

    @pyqtProperty(bool, notify=valuesChanged)
    def threeFingerDrag(self):
        return self._drag

    @pyqtProperty(str, notify=statusChanged)
    def status(self):
        return self._status

    @pyqtProperty(bool, notify=statusChanged)
    def statusError(self):
        return self._error

    # ---- slots ------------------------------------------------------------

    @pyqtSlot(int)
    def setPointerPosition(self, position):
        if not self._speeds:
            return
        v = core.position_value(position)
        if self._pointer == v:
            return
        self._pointer = v
        self.valuesChanged.emit()
        self._ctimer.start()

    @pyqtSlot(int)
    def setScrollPosition(self, position):
        if not self._speeds:
            return
        v = core.position_value(position, core.SCROLL_POSITIONS)
        if self._scroll == v:
            return
        self._scroll = v
        self.valuesChanged.emit()
        self._ctimer.start()

    @pyqtSlot(bool)
    def setMomentum(self, enabled):
        if self._momentum == enabled:
            return
        self._momentum = enabled
        self.valuesChanged.emit()
        self._mtimer.start()

    @pyqtSlot(bool)
    def setThreeFingerDrag(self, enabled):
        if self._drag == enabled:
            return
        self._drag = enabled
        self.valuesChanged.emit()
        self._dtimer.start()

    # ---- internals --------------------------------------------------------

    def _set_status(self, text, error):
        self._status, self._error = text, error
        self.statusChanged.emit()

    def _write_momentum(self):
        try:
            core.write_momentum(self._daemon_path, self._momentum)
        except (OSError, ValueError) as e:
            self._set_status(f"Couldn't write {self._daemon_path}: {e}", True)
            return
        self._set_status("Applied", False)

    def _write_drag(self):
        try:
            core.write_drag(self._daemon_path, self._drag)
        except (OSError, ValueError) as e:
            self._set_status(f"Couldn't write {self._daemon_path}: {e}", True)
            return
        self._set_status("Applied", False)

    def _set_args(self):
        """`set` arguments: the two speeds shown, plus the hidden values (gains,
        resolution, ...) as they are in accel.conf right now. Raises OSError."""
        values = core.accel_values(core.read_accel_conf(self._accel_path), self._defaults)
        values["pointerSetting"] = self._pointer
        values["scrollSetting"] = self._scroll
        return ["set"] + core.build_set_args(values)

    def _apply_curve(self):
        if not self._speeds:
            return
        if self._proc is not None:
            self._pending_curve = True  # run again once the current one ends
            return
        env, err = core.accel_env()
        if err:
            self._set_status(err, True)
            return
        tool = core.find_accel_tool()
        if not os.path.isfile(tool) or not os.access(tool, os.X_OK):
            self._set_status(f"mac-trackpad-accel not found ({tool}); run accel/install.sh", True)
            return
        try:
            args = self._set_args()
        except OSError as e:
            self._set_status(f"Couldn't read {self._accel_path}: {e}", True)
            return
        proc = QProcess(self)
        qenv = QProcessEnvironment()
        for k, v in env.items():
            qenv.insert(k, v)
        proc.setProcessEnvironment(qenv)
        proc.finished.connect(lambda code, st, p=proc: self._curve_done(
            p, code if st == QProcess.ExitStatus.NormalExit else -1))
        proc.errorOccurred.connect(lambda e, p=proc: self._curve_error(p, e))
        self._proc = proc
        self._set_status("Applying…", False)
        proc.start(tool, args)

    def _curve_error(self, proc, err):
        if err == QProcess.ProcessError.FailedToStart:
            self._set_status(f"Couldn't start mac-trackpad-accel: {proc.errorString()}", True)
            self._finish_proc(proc)

    def _curve_done(self, proc, code):
        out = bytes(proc.readAllStandardOutput()).decode(errors="replace").strip()
        err = bytes(proc.readAllStandardError()).decode(errors="replace").strip()
        if code != 0:
            last = (err or out).splitlines()[-1:] or [f"exit code {code}"]
            self._set_status(f"mac-trackpad-accel failed ({code}): {last[0]}", True)
        else:
            try:
                conf = core.read_accel_conf(self._accel_path)
            except OSError:
                conf = None
            if conf:
                self._set_status("Applied", False)
            else:
                self._set_status(f"mac-trackpad-accel ran but {self._accel_path} has no header", True)
        self._finish_proc(proc)

    def _finish_proc(self, proc):
        if self._proc is proc:
            self._proc = None
            proc.deleteLater()
            if self._pending_curve:
                self._pending_curve = False
                self._apply_curve()

    def flush(self):
        """Write anything still waiting on a debounce timer (on quit)."""
        if self._mtimer.isActive():
            self._mtimer.stop()
            self._write_momentum()
        if self._dtimer.isActive():
            self._dtimer.stop()
            self._write_drag()
        if self._speeds and (self._ctimer.isActive() or self._pending_curve):
            # the app is going away: run it detached so it still lands
            self._ctimer.stop()
            env, err = core.accel_env()
            tool = core.find_accel_tool()
            if err is None and os.access(tool, os.X_OK):
                try:
                    args = self._set_args()
                except OSError:
                    return
                proc = QProcess()
                qenv = QProcessEnvironment()
                for k, v in env.items():
                    qenv.insert(k, v)
                proc.setProcessEnvironment(qenv)
                proc.setProgram(tool)
                proc.setArguments(args)
                proc.startDetached()
