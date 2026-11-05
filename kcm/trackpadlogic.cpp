// SPDX-License-Identifier: MIT

#include "trackpadlogic.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonParseError>
#include <QRegularExpression>
#include <QSaveFile>
#include <QStandardPaths>

#include <cmath>

using namespace Qt::StringLiterals;

// ---------------------------------------------------------------- settings

QString TrackpadLogic::momentumKey()
{
    return u"momentum"_s;
}

QString TrackpadLogic::dragKey()
{
    return u"threeFingerDrag"_s;
}

const QList<double> &TrackpadLogic::pointerPositions()
{
    static const QList<double> positions{0.0, 0.125, 0.5, 0.6875, 0.875, 1.0, 1.5, 2.0, 2.5, 3.0};
    return positions;
}

const QList<double> &TrackpadLogic::scrollPositions()
{
    static const QList<double> positions{0.0, 0.0735, 0.1265, 0.1838, 0.3125, 0.4412, 0.5882, 1.0};
    return positions;
}

int TrackpadLogic::nearestPosition(double value, const QList<double> &positions)
{
    int best = 0;
    for (int i = 1; i < positions.size(); ++i) {
        // strictly closer only: a tie keeps the lower position
        if (std::abs(positions[i] - value) < std::abs(positions[best] - value)) {
            best = i;
        }
    }
    return best;
}

double TrackpadLogic::positionValue(int position, const QList<double> &positions)
{
    if (positions.isEmpty()) {
        return 0.0;
    }
    return positions[qBound(0, position, int(positions.size()) - 1)];
}

QString TrackpadLogic::formatNumber(double v)
{
    QString s = QString::number(v, 'f', 6);
    if (s.contains(u'.')) {
        while (s.endsWith(u'0')) {
            s.chop(1);
        }
        if (s.endsWith(u'.')) {
            s.chop(1);
        }
    }
    if (s.isEmpty() || s == u"-0"_s) {
        return u"0"_s;
    }
    return s;
}

// ---------------------------------------------------------------- paths

static QString homeDir(const QProcessEnvironment &env)
{
    const QString home = env.value(u"HOME"_s);
    return home.isEmpty() ? QDir::homePath() : home;
}

QString TrackpadLogic::configDir(const QProcessEnvironment &env)
{
    const QString override = env.value(u"MAC_TRACKPAD_CONFIG_DIR"_s);
    if (!override.isEmpty()) {
        return override;
    }
    QString base = env.value(u"XDG_CONFIG_HOME"_s);
    if (base.isEmpty()) {
        base = QDir(homeDir(env)).filePath(u".config"_s);
    }
    return QDir(base).filePath(u"mac-trackpad"_s);
}

TrackpadLogic::AccelEnv TrackpadLogic::accelEnv(const QProcessEnvironment &env)
{
    AccelEnv out{env, QString()};
    const QString override = env.value(u"MAC_TRACKPAD_CONFIG_DIR"_s);
    if (!override.isEmpty()) {
        const QFileInfo dir(QDir::cleanPath(QFileInfo(override).absoluteFilePath()));
        if (dir.fileName() != u"mac-trackpad"_s) {
            out.error = u"MAC_TRACKPAD_CONFIG_DIR must end in /mac-trackpad for mac-trackpad-accel to write accel.conf there"_s;
            return out;
        }
        out.env.insert(u"XDG_CONFIG_HOME"_s, dir.path());
    }
    return out;
}

QString TrackpadLogic::findAccelTool(const QProcessEnvironment &env)
{
    const QString override = env.value(u"MAC_TRACKPAD_ACCEL"_s);
    if (!override.isEmpty()) {
        return override;
    }
    const QString local = QDir(homeDir(env)).filePath(u".local/bin/mac-trackpad-accel"_s);
    if (QFileInfo(local).isFile()) {
        return local;
    }
    const QStringList path = env.value(u"PATH"_s).split(u':', Qt::SkipEmptyParts);
    const QString found = QStandardPaths::findExecutable(u"mac-trackpad-accel"_s, path);
    return found.isEmpty() ? local : found;
}

// ---------------------------------------------------------------- accel.conf

TrackpadLogic::Values TrackpadLogic::parseHeader(const QString &text)
{
    static const QString num = uR"(([-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?))"_s;
    static const QList<std::pair<QString, QRegularExpression>> fields{
        {u"pointerSetting"_s, QRegularExpression(uR"(\bpointer setting )"_s + num)},
        {u"scrollSetting"_s, QRegularExpression(uR"(\bscroll setting )"_s + num)},
        {u"res"_s, QRegularExpression(uR"(\bres )"_s + num + u" units/mm"_s)},
        {u"pointerGain"_s, QRegularExpression(uR"(\bpointer gain )"_s + num)},
        {u"scrollGain"_s, QRegularExpression(uR"(\bscroll gain )"_s + num)},
        {u"kwinScrollFactor"_s, QRegularExpression(uR"(\bkwin scroll factor )"_s + num)},
    };

    QStringList comments;
    Values out;
    static const QRegularExpression lineBreak(u"\r\n|\r|\n"_s);
    const QStringList lines = text.split(lineBreak);
    for (const QString &line : lines) {
        const QString s = line.trimmed();
        if (s.startsWith(u'#')) {
            qsizetype k = 0;
            while (k < s.size() && s[k] == u'#') {
                ++k;
            }
            comments.append(s.mid(k).trimmed());
        } else if (s.startsWith(u"motion_step="_s)) {
            bool ok = false;
            const double step = s.mid(s.indexOf(u'=') + 1).trimmed().toDouble(&ok);
            if (ok) {
                out[u"step"_s] = step;
            }
        }
    }
    const QString joined = comments.join(u' ');
    if (!joined.contains(u"generated by mac_curves.py"_s)) {
        return out;
    }
    for (const auto &[key, re] : fields) {
        const QRegularExpressionMatch m = re.match(joined);
        if (m.hasMatch()) {
            bool ok = false;
            const double v = m.captured(1).toDouble(&ok);
            if (ok) {
                out[key] = v;
            }
        }
    }
    return out;
}

TrackpadLogic::AccelConf TrackpadLogic::readAccelConf(const QString &path)
{
    AccelConf out;
    QFile f(path);
    if (!f.exists()) {
        return out;
    }
    out.exists = true;
    if (!f.open(QIODevice::ReadOnly)) {
        out.error = f.errorString();
        return out;
    }
    out.values = parseHeader(QString::fromUtf8(f.readAll()));
    return out;
}

TrackpadLogic::Values TrackpadLogic::accelDefaults()
{
    return {
        {u"pointerSetting"_s, 1.5},
        {u"pointerGain"_s, 1.0},
        {u"scrollSetting"_s, 0.3125},
        {u"scrollGain"_s, 1.0},
        {u"kwinScrollFactor"_s, 1.4},
        {u"res"_s, 47.0},
        {u"step"_s, 0.25},
    };
}

TrackpadLogic::Values TrackpadLogic::accelValues(const std::optional<Values> &conf, const Values &defaults)
{
    Values out = defaults;
    if (conf) {
        for (auto it = conf->cbegin(); it != conf->cend(); ++it) {
            if (defaults.contains(it.key())) {
                out[it.key()] = it.value();
            }
        }
    }
    return out;
}

QStringList TrackpadLogic::buildSetArgs(const Values &values)
{
    static const QList<std::pair<QString, QString>> options{
        {u"pointerSetting"_s, u"--pointer-setting"_s},
        {u"pointerGain"_s, u"--pointer-gain"_s},
        {u"scrollSetting"_s, u"--scroll-setting"_s},
        {u"scrollGain"_s, u"--scroll-gain"_s},
        {u"kwinScrollFactor"_s, u"--kwin-scroll-factor"_s},
        {u"res"_s, u"--res"_s},
        {u"step"_s, u"--step"_s},
    };
    QStringList args;
    for (const auto &[key, option] : options) {
        args << option << formatNumber(values.value(key));
    }
    return args;
}

// ---------------------------------------------------------------- daemon.json

TrackpadLogic::DaemonConfig TrackpadLogic::readDaemonJson(const QString &path)
{
    DaemonConfig out;
    QFile f(path);
    if (!f.exists()) {
        return out;
    }
    if (!f.open(QIODevice::ReadOnly)) {
        out.error = u"%1: %2"_s.arg(path, f.errorString());
        return out;
    }
    const QByteArray text = f.readAll();
    if (text.trimmed().isEmpty()) {
        return out;
    }
    QJsonParseError pe;
    const QJsonDocument doc = QJsonDocument::fromJson(text, &pe);
    if (pe.error != QJsonParseError::NoError) {
        out.error = u"%1: invalid JSON at offset %2: %3"_s.arg(path).arg(pe.offset).arg(pe.errorString());
    } else if (!doc.isObject()) {
        out.error = u"%1: top level is not a JSON object"_s.arg(path);
    } else {
        out.data = doc.object();
    }
    return out;
}

bool TrackpadLogic::switchEnabled(const QJsonObject &data, const QString &key, bool def)
{
    const QJsonValue v = data.value(key);
    return v.isBool() ? v.toBool() : def;
}

namespace
{
bool isJsonSpace(char c)
{
    return c == ' ' || c == '\t' || c == '\n' || c == '\r';
}

qsizetype skipSpace(const QByteArray &t, qsizetype i)
{
    while (i < t.size() && isJsonSpace(t[i])) {
        ++i;
    }
    return i;
}

// i at an opening quote; returns the index just past the closing quote
qsizetype skipString(const QByteArray &t, qsizetype i)
{
    for (++i; i < t.size(); ++i) {
        if (t[i] == '\\') {
            ++i;
        } else if (t[i] == '"') {
            return i + 1;
        }
    }
    return t.size();
}

// i at the start of a value; returns the index just past it
qsizetype skipValue(const QByteArray &t, qsizetype i)
{
    if (i >= t.size()) {
        return i;
    }
    if (t[i] == '"') {
        return skipString(t, i);
    }
    if (t[i] == '{' || t[i] == '[') {
        int depth = 0;
        while (i < t.size()) {
            const char c = t[i];
            if (c == '"') {
                i = skipString(t, i);
                continue;
            }
            if (c == '{' || c == '[') {
                ++depth;
            } else if ((c == '}' || c == ']') && --depth == 0) {
                return i + 1;
            }
            ++i;
        }
        return t.size();
    }
    while (i < t.size() && !isJsonSpace(t[i]) && t[i] != ',' && t[i] != '}' && t[i] != ']') {
        ++i;
    }
    return i;
}

QString decodeJsonString(const QByteArray &quoted)
{
    return QJsonDocument::fromJson(QByteArray('[' + quoted + ']')).array().at(0).toString();
}

QByteArray encodeJsonString(const QString &s)
{
    const QByteArray a = QJsonDocument(QJsonArray{s}).toJson(QJsonDocument::Compact);
    return a.mid(1, a.size() - 2);
}

struct Member {
    qsizetype keyStart;
    QString key;
    qsizetype valueStart;
    qsizetype valueEnd;
};
} // namespace

std::optional<QByteArray> TrackpadLogic::setBoolKey(const QByteArray &text, const QString &key, bool value, QString *error)
{
    const QByteArray literal = value ? QByteArray("true") : QByteArray("false");
    const QByteArray member = encodeJsonString(key) + ": " + literal;
    if (text.trimmed().isEmpty()) {
        return QByteArray("{\n    " + member + "\n}\n");
    }

    QJsonParseError pe;
    const QJsonDocument doc = QJsonDocument::fromJson(text, &pe);
    if (pe.error != QJsonParseError::NoError) {
        if (error) {
            *error = u"invalid JSON at offset %1: %2"_s.arg(pe.offset).arg(pe.errorString());
        }
        return std::nullopt;
    }
    if (!doc.isObject()) {
        if (error) {
            *error = u"top level is not a JSON object"_s;
        }
        return std::nullopt;
    }

    // The text is valid JSON with an object at the top: find its members.
    const qsizetype open = skipSpace(text, 0);
    qsizetype close = -1;
    QList<Member> members;
    qsizetype i = skipSpace(text, open + 1);
    while (i < text.size()) {
        if (text[i] == '}') {
            close = i;
            break;
        }
        if (text[i] == ',') {
            i = skipSpace(text, i + 1);
            continue;
        }
        Member m;
        m.keyStart = i;
        const qsizetype keyEnd = skipString(text, i);
        m.key = decodeJsonString(text.mid(i, keyEnd - i));
        i = skipSpace(text, keyEnd); // at ':'
        m.valueStart = skipSpace(text, i + 1);
        m.valueEnd = skipValue(text, m.valueStart);
        members.append(m);
        i = skipSpace(text, m.valueEnd);
    }
    if (open >= text.size() || text[open] != '{' || close < 0) {
        if (error) {
            *error = u"unexpected JSON layout"_s;
        }
        return std::nullopt;
    }

    QByteArray out = text;
    bool found = false;
    for (qsizetype k = members.size() - 1; k >= 0; --k) {
        if (members[k].key == key) {
            out.replace(members[k].valueStart, members[k].valueEnd - members[k].valueStart, literal);
            found = true;
        }
    }
    if (!found) {
        if (members.isEmpty()) {
            out.replace(open, close - open + 1, QByteArray("{\n    " + member + "\n}"));
        } else {
            // one member per line (as json.dumps(indent=4) writes it): a new
            // line with the last key's indentation; otherwise ", " inline
            const Member &last = members.last();
            const qsizetype nl = text.lastIndexOf('\n', last.keyStart - 1);
            bool ownLine = nl > open;
            const QByteArray indent = ownLine ? text.mid(nl + 1, last.keyStart - nl - 1) : QByteArray();
            for (const char c : indent) {
                ownLine = ownLine && (c == ' ' || c == '\t');
            }
            out.insert(last.valueEnd, ownLine ? QByteArray(",\n" + indent + member) : QByteArray(", " + member));
        }
    }

    // never hand out something the daemon can't read
    const QJsonDocument check = QJsonDocument::fromJson(out, &pe);
    if (pe.error != QJsonParseError::NoError || !check.isObject() || check.object().value(key) != QJsonValue(value)) {
        if (error) {
            *error = u"could not update the JSON text"_s;
        }
        return std::nullopt;
    }
    return out;
}

bool TrackpadLogic::writeBoolKey(const QString &path, const QString &key, bool value, QString *error)
{
    auto fail = [&](const QString &why) {
        if (error) {
            *error = u"%1: %2"_s.arg(path, why);
        }
        return false;
    };

    const QFileInfo info(path);
    QByteArray text;
    QFileDevice::Permissions perms = QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ReadUser | QFileDevice::WriteUser
        | QFileDevice::ReadGroup | QFileDevice::ReadOther; // 0644
    if (info.exists()) {
        QFile f(path);
        if (!f.open(QIODevice::ReadOnly)) {
            return fail(f.errorString());
        }
        text = f.readAll();
        perms = f.permissions();
    }

    QString why;
    const std::optional<QByteArray> out = setBoolKey(text, key, value, &why);
    if (!out) {
        return fail(why);
    }

    if (!QDir().mkpath(info.absolutePath())) {
        return fail(u"can't create the directory"_s);
    }
    QSaveFile save(path);
    if (!save.open(QIODevice::WriteOnly)) {
        return fail(save.errorString());
    }
    if (save.write(*out) != out->size()) {
        const QString e = save.errorString();
        save.cancelWriting();
        return fail(e);
    }
    if (!save.setPermissions(perms)) {
        const QString e = save.errorString();
        save.cancelWriting();
        return fail(e);
    }
    if (!save.commit()) {
        return fail(save.errorString());
    }
    return true;
}
