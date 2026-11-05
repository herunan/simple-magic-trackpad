import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// The settings panel: the trackpad options macOS has, and nothing else.
//
// Same rules as the Python app (app/): nothing is written when the panel
// opens or the shell starts; a stored value between two slider positions
// shows the nearest one; a switch re-reads daemon.json and writes only its
// own key (momentum, threeFingerDrag), atomically, keeping every other key;
// the sliders run `mac-trackpad-accel set ...` after a short pause.
//
// The only command this runs is ~/.local/bin/mac-trackpad-accel (installed by
// accel/install.sh). Files are read and written with Quickshell's FileView.
Panel {
  id: root
  moduleName: "io.github.herunan.simple-magic-trackpad"
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null

  // ---- paths (the same ones the daemon, accel/install.sh and the app use)
  readonly property string home: Quickshell.env("HOME")
  readonly property string configHome: Quickshell.env("XDG_CONFIG_HOME") || (home + "/.config")
  readonly property string dataHome: Quickshell.env("XDG_DATA_HOME") || (home + "/.local/share")
  readonly property string configDir: configHome + "/mac-trackpad"
  readonly property string daemonPath: configDir + "/daemon.json"
  readonly property string accelPath: configDir + "/accel.conf"
  readonly property string toolPath: home + "/.local/bin/mac-trackpad-accel"
  // daemon/install.sh puts the unit here (not under XDG_CONFIG_HOME)
  readonly property string unitPath: home + "/.config/systemd/user/mac-trackpad.service"
  readonly property string pluginDir: decodeURIComponent(String(Qt.resolvedUrl("..")).replace(/^file:\/\//, "")).replace(/\/$/, "")

  // ---- what's installed
  property bool daemonInstalled: true
  property bool daemonConfigPresent: true
  property bool toolPresent: true

  // ---- values shown
  property int pointerPosition: Model.nearestPosition(Model.ACCEL_DEFAULTS.pointerSetting, Model.POINTER_POSITIONS)
  property int scrollPosition: Model.nearestPosition(Model.ACCEL_DEFAULTS.scrollSetting, Model.SCROLL_POSITIONS)
  property bool momentum: true
  property bool threeFingerDrag: false
  property var defaults: Model.accelDefaults("")
  property var accelHeader: null

  property string status: ""
  property bool statusError: false
  property string daemonError: ""

  // ---- keyboard cursor: 0 tracking, 1 scroll, 2 inertia, 3 drag
  property int cursorRow: -1

  // Not run from this folder: the shell reloads the plugin on every file
  // written in it, and building the daemon writes thousands.
  readonly property string readmeHint: "Steps: Omarchy section of " + root.pluginDir + "/README.md."
  readonly property bool speedsEnabled: root.toolPresent
  readonly property bool switchesEnabled: root.daemonConfigPresent && root.daemonError === ""

  function open() {
    root.cursorRow = -1
    // pick up an install done since the shell started
    toolFile.reload()
    unitFile.reload()
    root.controller.show()
  }

  function close() {
    root.controller.hide()
  }

  function switchPanel(direction) {
    if (root.bar && typeof root.bar.switchPanelFrom === "function")
      return root.bar.switchPanelFrom(root.hostWidget || root, direction)
    return false
  }

  function setStatus(text, error) {
    root.status = text
    root.statusError = error === true
  }

  // ---- reading

  function readDaemon(text) {
    var parsed = Model.parseDaemon(text)
    root.daemonError = parsed.ok ? "" : root.daemonPath + ": " + parsed.error
    if (!parsed.ok) return
    // a click still waiting to be written wins over what's on disk
    if (!momentumTimer.running) root.momentum = Model.switchValue(parsed.data, Model.MOMENTUM_KEY)
    if (!dragTimer.running) root.threeFingerDrag = Model.switchValue(parsed.data, Model.DRAG_KEY)
  }

  function readAccel(text) {
    root.accelHeader = text === null ? null : Model.parseHeader(text)
    root.showAccel()
  }

  function showAccel() {
    // a slider move still waiting to be applied wins over what's on disk
    if (curveTimer.running || accelProc.running || accelProc.pending) return
    var values = Model.accelValues(root.accelHeader, root.defaults)
    root.pointerPosition = Model.nearestPosition(values.pointerSetting, Model.POINTER_POSITIONS)
    root.scrollPosition = Model.nearestPosition(values.scrollSetting, Model.SCROLL_POSITIONS)
  }

  // A fresh read of a file through a FileView that nothing displays, so
  // reading it can't reset what the panel shows.
  function freshText(view) {
    view.reload()
    view.waitForJob()
    return view.text()
  }

  // ---- writing (only after the user moves a control)

  function writeSwitch(key, value) {
    // re-read first so a key changed since (by hand, by another tool) stays
    var result = Model.setSwitch(root.freshText(daemonWriter), key, value)
    if (!result.ok) {
      root.setStatus("Couldn't write " + root.daemonPath + ": " + result.error, true)
      return
    }
    daemonWriter.setText(result.text)
  }

  function setArgs(header) {
    var values = Model.accelValues(header, root.defaults)
    values.pointerSetting = Model.positionValue(root.pointerPosition, Model.POINTER_POSITIONS)
    values.scrollSetting = Model.positionValue(root.scrollPosition, Model.SCROLL_POSITIONS)
    return Model.buildSetArgs(values)
  }

  function freshHeader() {
    return Model.parseHeader(root.freshText(accelReader))
  }

  function applyCurve() {
    if (!root.toolPresent) return
    if (accelProc.running) {
      accelProc.pending = true   // run again once this one ends
      return
    }
    // the header as it is right now, so hidden values carry over
    accelProc.command = [root.toolPath].concat(root.setArgs(root.freshHeader()))
    root.setStatus("Applying…", false)
    accelProc.running = true
  }

  function movePointer(position) {
    if (!root.speedsEnabled) return
    var p = Math.max(0, Math.min(Model.POINTER_POSITIONS.length - 1, Math.round(position)))
    if (p === root.pointerPosition) return
    root.pointerPosition = p
    curveTimer.restart()
  }

  function moveScroll(position) {
    if (!root.speedsEnabled) return
    var p = Math.max(0, Math.min(Model.SCROLL_POSITIONS.length - 1, Math.round(position)))
    if (p === root.scrollPosition) return
    root.scrollPosition = p
    curveTimer.restart()
  }

  function toggleMomentum() {
    if (!root.switchesEnabled) return
    root.momentum = !root.momentum
    momentumTimer.restart()
  }

  function toggleDrag() {
    if (!root.switchesEnabled) return
    root.threeFingerDrag = !root.threeFingerDrag
    dragTimer.restart()
  }

  function moveCursor(dx, dy) {
    if (root.cursorRow < 0) { root.cursorRow = 0; return }
    if (dy !== 0) root.cursorRow = Math.max(0, Math.min(3, root.cursorRow + dy))
    else if (root.cursorRow === 0) root.movePointer(root.pointerPosition + dx)
    else if (root.cursorRow === 1) root.moveScroll(root.scrollPosition + dx)
  }

  function activateCursor() {
    if (root.cursorRow === 2) root.toggleMomentum()
    else if (root.cursorRow === 3) root.toggleDrag()
  }

  // Unloaded (plugin update, shell restart) with a change still waiting:
  // write it now, and let the curve run detached so it still lands.
  Component.onDestruction: {
    try {
      if (momentumTimer.running) root.writeSwitch(Model.MOMENTUM_KEY, root.momentum)
      if (dragTimer.running) root.writeSwitch(Model.DRAG_KEY, root.threeFingerDrag)
      if (root.toolPresent && (curveTimer.running || accelProc.pending))
        Quickshell.execDetached([root.toolPath].concat(root.setArgs(root.freshHeader())))
    } catch (e) {
      console.warn("simple-magic-trackpad: change lost on unload:", e)
    }
  }

  Timer {
    id: momentumTimer
    interval: 300
    onTriggered: root.writeSwitch(Model.MOMENTUM_KEY, root.momentum)
  }

  Timer {
    id: dragTimer
    interval: 300
    onTriggered: root.writeSwitch(Model.DRAG_KEY, root.threeFingerDrag)
  }

  Timer {
    id: curveTimer
    interval: 500
    onTriggered: root.applyCurve()
  }

  // Shown: daemon.json as it is on disk, followed live.
  FileView {
    id: daemonFile
    path: root.daemonPath
    watchChanges: true
    printErrors: false
    onFileChanged: daemonFile.reload()
    onLoaded: {
      root.daemonConfigPresent = true
      root.readDaemon(daemonFile.text())
    }
    onLoadFailed: function(error) {
      root.daemonConfigPresent = false
      root.daemonError = error === FileViewError.FileNotFound ? "" : root.daemonPath + ": can't read it"
    }
  }

  // Written: re-read right before each write, then replaced atomically
  // (temp file + rename) so the daemon's hot reload never sees half a file.
  FileView {
    id: daemonWriter
    path: root.daemonPath
    blockLoading: true
    atomicWrites: true
    printErrors: false
    onSaved: root.setStatus("Applied", false)
    onSaveFailed: root.setStatus("Couldn't write " + root.daemonPath, true)
  }

  FileView {
    id: accelFile
    path: root.accelPath
    watchChanges: true
    printErrors: false
    onFileChanged: accelFile.reload()
    onLoaded: root.readAccel(accelFile.text())
    onLoadFailed: root.readAccel(null)
  }

  FileView {
    id: accelReader
    path: root.accelPath
    blockLoading: true
    printErrors: false
  }

  // Only to learn whether these exist; nothing in them is used.
  FileView {
    id: toolFile
    path: root.toolPath
    printErrors: false
    onLoaded: root.toolPresent = true
    onLoadFailed: root.toolPresent = false
  }

  FileView {
    id: unitFile
    path: root.unitPath
    printErrors: false
    onLoaded: root.daemonInstalled = true
    onLoadFailed: root.daemonInstalled = false
  }

  // mac-params.json: where the speeds default to without an accel.conf.
  // accel/install.sh's copy first, else the one in this plugin's checkout.
  FileView {
    id: paramsFile
    path: root.dataHome + "/mac-trackpad/mac-params.json"
    printErrors: false
    onLoaded: {
      root.defaults = Model.accelDefaults(paramsFile.text())
      root.showAccel()
    }
    onLoadFailed: {
      var own = root.pluginDir + "/accel/mac-params.json"
      if (paramsFile.path !== own) paramsFile.path = own
    }
  }

  Process {
    id: accelProc
    property bool pending: false
    stdout: StdioCollector { id: accelOut }
    stderr: StdioCollector { id: accelErr }
    onExited: function(exitCode) {
      var line = Model.lastLine(accelErr.text) || Model.lastLine(accelOut.text) || ("exit code " + exitCode)
      if (exitCode !== 0) root.setStatus("mac-trackpad-accel failed: " + line, true)
      else root.setStatus("Applied", false)
      if (accelProc.pending) {
        accelProc.pending = false
        root.applyCurve()
      }
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.hostWidget || root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(340))
    contentHeight: panel.fittedContentHeight(panelContent.implicitHeight)

    readonly property color fg: root.bar ? root.bar.foreground : Commons.Color.foreground
    readonly property color dim: Qt.darker(fg, 1.4)
    readonly property string fontName: root.bar ? root.bar.fontFamily : Style.font.family

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onMoveRequested: function(dx, dy) { root.moveCursor(dx, dy) }
      onActivateRequested: root.activateCursor()
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Column {
        id: panelContent
        width: parent.width
        spacing: Style.space(12)

        Text {
          width: parent.width
          textFormat: Text.PlainText
          text: "Trackpad"
          color: panel.fg
          font.family: panel.fontName
          font.pixelSize: Style.font.title
          font.bold: true
        }

        // What's missing, and the step that installs it.
        Text {
          width: parent.width
          visible: text !== ""
          textFormat: Text.PlainText
          wrapMode: Text.WordWrap
          color: Commons.Color.urgent
          font.family: panel.fontName
          font.pixelSize: Style.font.caption
          text: {
            var lines = []
            if (!root.daemonInstalled || !root.daemonConfigPresent)
              lines.push("The mac-trackpad daemon is not installed, so inertia and three finger drag do nothing. "
                + "Copy the plugin folder out of ~/.config/omarchy/plugins, run daemon/install.sh there, add its udev rules and enable mac-trackpad.service. "
                + root.readmeHint)
            if (!root.toolPresent)
              lines.push("Tracking and scroll speed need mac-trackpad-accel: run accel/install.sh from a copy of the plugin folder. "
                + root.readmeHint)
            if (root.daemonError !== "")
              lines.push(root.daemonError + "; the switches are off until it's fixed.")
            return lines.join("\n\n")
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(6)
          opacity: root.speedsEnabled ? 1 : 0.45

          PanelSectionHeader {
            text: "Tracking speed"
            foreground: panel.fg
            fontFamily: panel.fontName
          }

          CursorSurface {
            width: parent.width
            height: pointerSlider.implicitHeight + Style.spacing.controlGap
            hasCursor: root.cursorRow === 0
            foreground: panel.fg
            outline: true

            PanelSlider {
              id: pointerSlider
              bar: root.bar
              enabled: root.speedsEnabled
              anchors.fill: parent
              anchors.leftMargin: Style.space(6)
              anchors.rightMargin: Style.space(6)
              minimum: 0
              maximum: Model.POINTER_POSITIONS.length - 1
              step: 1
              integer: true
              tickCount: Model.POINTER_POSITIONS.length
              value: root.pointerPosition
              onMoved: function(v) { root.movePointer(v) }
            }

            HoverHandler { onHoveredChanged: if (hovered) root.cursorRow = 0 }
          }

          Item {
            width: parent.width
            implicitHeight: slowLabel.implicitHeight
            Text { id: slowLabel; text: "Slow"; color: panel.dim; font.family: panel.fontName; font.pixelSize: Style.font.caption }
            Text { anchors.right: parent.right; text: "Fast"; color: panel.dim; font.family: panel.fontName; font.pixelSize: Style.font.caption }
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(6)
          opacity: root.speedsEnabled ? 1 : 0.45

          PanelSectionHeader {
            text: "Scroll speed"
            foreground: panel.fg
            fontFamily: panel.fontName
          }

          CursorSurface {
            width: parent.width
            height: scrollSlider.implicitHeight + Style.spacing.controlGap
            hasCursor: root.cursorRow === 1
            foreground: panel.fg
            outline: true

            PanelSlider {
              id: scrollSlider
              bar: root.bar
              enabled: root.speedsEnabled
              anchors.fill: parent
              anchors.leftMargin: Style.space(6)
              anchors.rightMargin: Style.space(6)
              minimum: 0
              maximum: Model.SCROLL_POSITIONS.length - 1
              step: 1
              integer: true
              tickCount: Model.SCROLL_POSITIONS.length
              value: root.scrollPosition
              onMoved: function(v) { root.moveScroll(v) }
            }

            HoverHandler { onHoveredChanged: if (hovered) root.cursorRow = 1 }
          }

          Item {
            width: parent.width
            implicitHeight: slowLabel2.implicitHeight
            Text { id: slowLabel2; text: "Slow"; color: panel.dim; font.family: panel.fontName; font.pixelSize: Style.font.caption }
            Text { anchors.right: parent.right; text: "Fast"; color: panel.dim; font.family: panel.fontName; font.pixelSize: Style.font.caption }
          }
        }

        PanelSeparator {
          width: parent.width
          foreground: panel.fg
        }

        Toggle {
          width: parent.width
          enabled: root.switchesEnabled
          opacity: enabled ? 1 : 0.45
          label: "Use inertia when scrolling"
          foreground: panel.fg
          fontFamily: panel.fontName
          checked: root.momentum
          hasCursor: root.cursorRow === 2
          onClicked: root.toggleMomentum()
          onHovered: function(h) { if (h) root.cursorRow = 2 }
        }

        Toggle {
          width: parent.width
          enabled: root.switchesEnabled
          opacity: enabled ? 1 : 0.45
          label: "Three finger drag"
          foreground: panel.fg
          fontFamily: panel.fontName
          checked: root.threeFingerDrag
          hasCursor: root.cursorRow === 3
          onClicked: root.toggleDrag()
          onHovered: function(h) { if (h) root.cursorRow = 3 }
        }

        Text {
          width: parent.width
          visible: root.status !== ""
          textFormat: Text.PlainText
          elide: Text.ElideRight
          text: root.status
          color: root.statusError ? Commons.Color.urgent : panel.dim
          font.family: panel.fontName
          font.pixelSize: Style.font.caption
        }
      }
    }
  }
}
