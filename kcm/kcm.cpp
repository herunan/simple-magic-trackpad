// SPDX-License-Identifier: MIT

#include "kcm.h"

#include <KLocalizedString>
#include <KPluginFactory>

#include <QCoreApplication>
#include <QFileInfo>
#include <QProcess>
#include <QQuickItem>
#include <QQuickWindow>

using namespace Qt::StringLiterals;

K_PLUGIN_CLASS_WITH_JSON(AppleMagicTrackpadKcm, "kcm_apple_magic_trackpad.json")

AppleMagicTrackpadKcm::AppleMagicTrackpadKcm(QObject *parent, const KPluginMetaData &data)
    : KQuickConfigModule(parent, data)
{
    // changes apply live, like macOS: no Apply/Reset/Defaults
    setButtons(NoAdditionalButton);

    const QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
    const QString dir = TrackpadLogic::configDir(env);
    m_daemonPath = dir + u"/daemon.json"_s;
    m_accelPath = dir + u"/accel.conf"_s;
    m_defaults = TrackpadLogic::accelDefaults();

    // stored values are kept as they are; nothing is written until the user
    // moves a control
    QStringList problems;
    const TrackpadLogic::DaemonConfig daemon = TrackpadLogic::readDaemonJson(m_daemonPath);
    if (daemon.error.isEmpty()) {
        m_momentum = TrackpadLogic::switchEnabled(daemon.data, TrackpadLogic::momentumKey(), TrackpadLogic::momentumDefault);
        m_drag = TrackpadLogic::switchEnabled(daemon.data, TrackpadLogic::dragKey(), TrackpadLogic::dragDefault);
    } else {
        problems << i18n("Can't read %1", daemon.error);
    }
    std::optional<TrackpadLogic::Values> conf;
    const TrackpadLogic::AccelConf accel = TrackpadLogic::readAccelConf(m_accelPath);
    if (!accel.error.isEmpty()) {
        problems << i18n("Can't read %1: %2", m_accelPath, accel.error);
    } else if (!accel.exists) {
        problems << i18n("%1 not found; showing Mac defaults", m_accelPath);
    } else {
        conf = accel.values;
    }
    const TrackpadLogic::Values values = TrackpadLogic::accelValues(conf, m_defaults);
    m_pointer = values.value(u"pointerSetting"_s);
    m_scroll = values.value(u"scrollSetting"_s);
    m_error = problems.join(u"; "_s);

    for (QTimer *t : {&m_momentumTimer, &m_dragTimer}) {
        t->setSingleShot(true);
        t->setInterval(300);
    }
    m_curveTimer.setSingleShot(true);
    m_curveTimer.setInterval(500);
    connect(&m_momentumTimer, &QTimer::timeout, this, [this] {
        writeSwitch(TrackpadLogic::momentumKey(), m_momentum);
    });
    connect(&m_dragTimer, &QTimer::timeout, this, [this] {
        writeSwitch(TrackpadLogic::dragKey(), m_drag);
    });
    connect(&m_curveTimer, &QTimer::timeout, this, &AppleMagicTrackpadKcm::applyCurve);

    // pending changes land when the page is left or the host quits
    connect(this, &KQuickConfigModule::mainUiReady, this, &AppleMagicTrackpadKcm::watchUi);
    if (QCoreApplication::instance()) {
        connect(QCoreApplication::instance(), &QCoreApplication::aboutToQuit, this, &AppleMagicTrackpadKcm::finalFlush);
    }
}

AppleMagicTrackpadKcm::~AppleMagicTrackpadKcm()
{
    // the base class tears the QML down after this; don't get called back
    if (m_ui) {
        disconnect(m_ui, nullptr, this, nullptr);
    }
    if (m_window) {
        disconnect(m_window, nullptr, this, nullptr);
    }
    finalFlush();
}

// ---------------------------------------------------------------- properties

int AppleMagicTrackpadKcm::pointerPosition() const
{
    return TrackpadLogic::nearestPosition(m_pointer, TrackpadLogic::pointerPositions());
}

int AppleMagicTrackpadKcm::pointerMaxPosition() const
{
    return int(TrackpadLogic::pointerPositions().size()) - 1;
}

int AppleMagicTrackpadKcm::scrollPosition() const
{
    return TrackpadLogic::nearestPosition(m_scroll, TrackpadLogic::scrollPositions());
}

int AppleMagicTrackpadKcm::scrollMaxPosition() const
{
    return int(TrackpadLogic::scrollPositions().size()) - 1;
}

bool AppleMagicTrackpadKcm::momentum() const
{
    return m_momentum;
}

bool AppleMagicTrackpadKcm::threeFingerDrag() const
{
    return m_drag;
}

QString AppleMagicTrackpadKcm::errorText() const
{
    return m_error;
}

// ---------------------------------------------------------------- setters

void AppleMagicTrackpadKcm::setPointerPosition(int position)
{
    const double v = TrackpadLogic::positionValue(position, TrackpadLogic::pointerPositions());
    if (m_pointer == v) {
        return;
    }
    m_pointer = v;
    Q_EMIT valuesChanged();
    m_curveTimer.start();
}

void AppleMagicTrackpadKcm::setScrollPosition(int position)
{
    const double v = TrackpadLogic::positionValue(position, TrackpadLogic::scrollPositions());
    if (m_scroll == v) {
        return;
    }
    m_scroll = v;
    Q_EMIT valuesChanged();
    m_curveTimer.start();
}

void AppleMagicTrackpadKcm::setMomentum(bool enabled)
{
    if (m_momentum == enabled) {
        return;
    }
    m_momentum = enabled;
    Q_EMIT valuesChanged();
    m_momentumTimer.start();
}

void AppleMagicTrackpadKcm::setThreeFingerDrag(bool enabled)
{
    if (m_drag == enabled) {
        return;
    }
    m_drag = enabled;
    Q_EMIT valuesChanged();
    m_dragTimer.start();
}

// ---------------------------------------------------------------- internals

void AppleMagicTrackpadKcm::setError(const QString &text)
{
    if (m_error == text) {
        return;
    }
    m_error = text;
    Q_EMIT errorTextChanged();
}

void AppleMagicTrackpadKcm::writeSwitch(const QString &key, bool enabled)
{
    QString error;
    if (!TrackpadLogic::writeBoolKey(m_daemonPath, key, enabled, &error)) {
        setError(i18n("Couldn't write %1", error));
        return;
    }
    setError(QString());
}

bool AppleMagicTrackpadKcm::setArgs(QStringList *args, QString *error) const
{
    // the two speeds shown, plus the hidden values (gains, resolution, ...)
    // as they are in accel.conf right now
    const TrackpadLogic::AccelConf accel = TrackpadLogic::readAccelConf(m_accelPath);
    if (!accel.error.isEmpty()) {
        *error = i18n("Couldn't read %1: %2", m_accelPath, accel.error);
        return false;
    }
    std::optional<TrackpadLogic::Values> conf;
    if (accel.exists) {
        conf = accel.values;
    }
    TrackpadLogic::Values values = TrackpadLogic::accelValues(conf, m_defaults);
    values[u"pointerSetting"_s] = m_pointer;
    values[u"scrollSetting"_s] = m_scroll;
    *args = QStringList{u"set"_s} + TrackpadLogic::buildSetArgs(values);
    return true;
}

void AppleMagicTrackpadKcm::applyCurve()
{
    if (m_proc) {
        m_pendingCurve = true; // run again once the current one ends
        return;
    }
    const QProcessEnvironment sys = QProcessEnvironment::systemEnvironment();
    const TrackpadLogic::AccelEnv env = TrackpadLogic::accelEnv(sys);
    if (!env.error.isEmpty()) {
        setError(env.error);
        return;
    }
    const QString tool = TrackpadLogic::findAccelTool(sys);
    const QFileInfo toolInfo(tool);
    if (!toolInfo.isFile() || !toolInfo.isExecutable()) {
        setError(i18n("mac-trackpad-accel not found (%1); run accel/install.sh", tool));
        return;
    }
    QStringList args;
    QString error;
    if (!setArgs(&args, &error)) {
        setError(error);
        return;
    }

    auto *proc = new QProcess(this);
    proc->setProcessEnvironment(env.env);
    connect(proc, &QProcess::finished, this, [this, proc](int code, QProcess::ExitStatus status) {
        curveDone(proc, status == QProcess::NormalExit ? code : -1);
    });
    connect(proc, &QProcess::errorOccurred, this, [this, proc](QProcess::ProcessError e) {
        if (e == QProcess::FailedToStart) {
            setError(i18n("Couldn't start mac-trackpad-accel: %1", proc->errorString()));
            finishProc(proc);
        }
    });
    m_proc = proc;
    proc->start(tool, args);
}

void AppleMagicTrackpadKcm::curveDone(QProcess *proc, int code)
{
    const QString out = QString::fromUtf8(proc->readAllStandardOutput()).trimmed();
    const QString err = QString::fromUtf8(proc->readAllStandardError()).trimmed();
    if (code != 0) {
        const QString text = err.isEmpty() ? out : err;
        QString last = text.split(u'\n').constLast().trimmed();
        if (text.isEmpty()) {
            last = i18n("exit code %1", code);
        }
        setError(i18n("mac-trackpad-accel failed (%1): %2", code, last));
    } else {
        const TrackpadLogic::AccelConf accel = TrackpadLogic::readAccelConf(m_accelPath);
        if (accel.exists && accel.error.isEmpty() && !accel.values.isEmpty()) {
            setError(QString());
        } else {
            setError(i18n("mac-trackpad-accel ran but %1 has no header", m_accelPath));
        }
    }
    finishProc(proc);
}

void AppleMagicTrackpadKcm::finishProc(QProcess *proc)
{
    if (m_proc != proc) {
        return;
    }
    m_proc = nullptr;
    proc->deleteLater();
    if (m_pendingCurve) {
        m_pendingCurve = false;
        applyCurve();
    }
}

void AppleMagicTrackpadKcm::flush()
{
    if (m_finalFlushed) {
        return;
    }
    if (m_momentumTimer.isActive()) {
        m_momentumTimer.stop();
        writeSwitch(TrackpadLogic::momentumKey(), m_momentum);
    }
    if (m_dragTimer.isActive()) {
        m_dragTimer.stop();
        writeSwitch(TrackpadLogic::dragKey(), m_drag);
    }
    if (m_curveTimer.isActive()) {
        m_curveTimer.stop();
        applyCurve(); // asynchronous; queued behind a run in progress
    }
}

void AppleMagicTrackpadKcm::finalFlush()
{
    // The module (or the whole host) is going away: the switches are written
    // now, a pending speed change is run detached so it still lands, and a
    // run in progress is left to finish instead of being killed.
    if (m_finalFlushed) {
        return;
    }
    flush();
    m_finalFlushed = true;

    const bool pending = m_curveTimer.isActive() || m_pendingCurve;
    m_curveTimer.stop();
    m_pendingCurve = false;

    qint64 runningPid = 0;
    if (QProcess *proc = m_proc) {
        m_proc = nullptr;
        disconnect(proc, nullptr, this, nullptr);
        if (proc->state() == QProcess::NotRunning) {
            proc->deleteLater();
        } else {
            runningPid = proc->processId();
            // no parent: not killed with this module; reaped and freed when
            // it ends, or left running if the host exits first
            proc->setParent(nullptr);
            connect(proc, &QProcess::finished, proc, &QObject::deleteLater);
        }
    }
    if (!pending) {
        return;
    }

    const QProcessEnvironment sys = QProcessEnvironment::systemEnvironment();
    const TrackpadLogic::AccelEnv env = TrackpadLogic::accelEnv(sys);
    const QString tool = TrackpadLogic::findAccelTool(sys);
    const QFileInfo toolInfo(tool);
    QStringList args;
    QString error;
    if (!env.error.isEmpty() || !toolInfo.isFile() || !toolInfo.isExecutable() || !setArgs(&args, &error)) {
        return;
    }
    QProcess detached;
    detached.setProcessEnvironment(env.env);
    if (runningPid > 0) {
        // after the run in progress, so the newest values are the last ones
        detached.setProgram(u"/bin/sh"_s);
        detached.setArguments(QStringList{u"-c"_s,
                                          u"while kill -0 \"$1\" 2>/dev/null; do sleep 0.05; done; shift; exec \"$@\""_s,
                                          u"sh"_s,
                                          QString::number(runningPid),
                                          tool}
                              + args);
    } else {
        detached.setProgram(tool);
        detached.setArguments(args);
    }
    detached.startDetached();
}

void AppleMagicTrackpadKcm::watchUi()
{
    QQuickItem *ui = mainUi();
    if (!ui || ui == m_ui) {
        return;
    }
    m_ui = ui;
    connect(ui, &QQuickItem::visibleChanged, this, [this] {
        if (m_ui && !m_ui->isVisible()) {
            onHidden();
        }
    });
    connect(ui, &QQuickItem::windowChanged, this, &AppleMagicTrackpadKcm::watchWindow);
    watchWindow(ui->window());
}

void AppleMagicTrackpadKcm::watchWindow(QQuickWindow *window)
{
    if (m_window == window) {
        return;
    }
    if (m_window) {
        disconnect(m_window, nullptr, this, nullptr);
    }
    m_window = window;
    if (window) {
        connect(window, &QWindow::visibleChanged, this, [this](bool visible) {
            if (!visible) {
                onHidden();
            }
        });
    }
}

void AppleMagicTrackpadKcm::onHidden()
{
    flush();
}

#include "kcm.moc"
