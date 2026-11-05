// SPDX-License-Identifier: MIT
//
// System Settings page for the Apple Magic Trackpad: tracking speed, scroll
// speed, inertia and three finger drag. Changes apply live (no Apply button):
// the switches go to daemon.json, the speeds through mac-trackpad-accel.

#pragma once

#include <KQuickConfigModule>

#include <QPointer>
#include <QTimer>

#include "trackpadlogic.h"

class QProcess;
class QQuickItem;
class QQuickWindow;

class AppleMagicTrackpadKcm : public KQuickConfigModule
{
    Q_OBJECT
    Q_PROPERTY(int pointerPosition READ pointerPosition NOTIFY valuesChanged)
    Q_PROPERTY(int pointerMaxPosition READ pointerMaxPosition CONSTANT)
    Q_PROPERTY(int scrollPosition READ scrollPosition NOTIFY valuesChanged)
    Q_PROPERTY(int scrollMaxPosition READ scrollMaxPosition CONSTANT)
    Q_PROPERTY(bool momentum READ momentum NOTIFY valuesChanged)
    Q_PROPERTY(bool threeFingerDrag READ threeFingerDrag NOTIFY valuesChanged)
    Q_PROPERTY(QString errorText READ errorText NOTIFY errorTextChanged)

public:
    AppleMagicTrackpadKcm(QObject *parent, const KPluginMetaData &data);
    ~AppleMagicTrackpadKcm() override;

    int pointerPosition() const;
    int pointerMaxPosition() const;
    int scrollPosition() const;
    int scrollMaxPosition() const;
    bool momentum() const;
    bool threeFingerDrag() const;
    QString errorText() const;

    Q_INVOKABLE void setPointerPosition(int position);
    Q_INVOKABLE void setScrollPosition(int position);
    Q_INVOKABLE void setMomentum(bool enabled);
    Q_INVOKABLE void setThreeFingerDrag(bool enabled);

    // Apply anything still waiting on a debounce timer now (page left).
    Q_INVOKABLE void flush();

Q_SIGNALS:
    void valuesChanged();
    void errorTextChanged();

private:
    void setError(const QString &text);
    void writeSwitch(const QString &key, bool enabled);
    bool setArgs(QStringList *args, QString *error) const;
    void applyCurve();
    void curveDone(QProcess *proc, int code);
    void finishProc(QProcess *proc);
    void finalFlush();
    void watchUi();
    void watchWindow(QQuickWindow *window);
    void onHidden();

    QString m_daemonPath;
    QString m_accelPath;
    TrackpadLogic::Values m_defaults;

    double m_pointer = 0;
    double m_scroll = 0;
    bool m_momentum = TrackpadLogic::momentumDefault;
    bool m_drag = TrackpadLogic::dragDefault;
    QString m_error;

    QTimer m_momentumTimer;
    QTimer m_dragTimer;
    QTimer m_curveTimer;
    QProcess *m_proc = nullptr;
    bool m_pendingCurve = false;
    bool m_finalFlushed = false;

    QPointer<QQuickItem> m_ui;
    QPointer<QQuickWindow> m_window;
};
