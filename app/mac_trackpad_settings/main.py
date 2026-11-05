"""Entry point: QApplication + QML engine + Backend."""
import os
import sys


def main():
    from PyQt6.QtCore import QUrl
    from PyQt6.QtGui import QIcon
    from PyQt6.QtQml import QQmlApplicationEngine
    from PyQt6.QtWidgets import QApplication

    from . import core
    from .backend import Backend

    app = QApplication(sys.argv)
    app.setApplicationName("mac-trackpad-settings")
    app.setApplicationDisplayName("Apple Magic Trackpad")
    app.setOrganizationName("mac-trackpad")
    app.setDesktopFileName("mac-trackpad-settings")
    if QIcon.hasThemeIcon("input-touchpad"):
        app.setWindowIcon(QIcon.fromTheme("input-touchpad"))

    # On KDE the Qt Quick style is left to the desktop theme. Elsewhere use
    # Fusion, which looks native and follows dark mode. PyQt6 has no
    # QQuickStyle binding (there is no PyQt6.QtQuickControls2), so this is
    # done the documented way, with the environment variable, before the
    # engine exists. A style the user already chose is kept.
    if not core.is_kde():
        os.environ.setdefault("QT_QUICK_CONTROLS_STYLE", "Fusion")

    backend = Backend()
    engine = QQmlApplicationEngine()
    engine.rootContext().setContextProperty("backend", backend)
    qml = os.path.join(os.path.dirname(os.path.realpath(__file__)), "qml", "Main.qml")
    engine.load(QUrl.fromLocalFile(qml))
    if not engine.rootObjects():
        print("mac-trackpad-settings: failed to load QML", file=sys.stderr)
        return 1
    app.aboutToQuit.connect(backend.flush)
    rc = app.exec()
    del engine  # before backend: QML bindings still reference it
    return rc
