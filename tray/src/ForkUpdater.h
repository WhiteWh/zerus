#pragma once
#include <QByteArray>
#include <QCryptographicHash>
#include <QObject>
#include <QPointer>
#include <QString>
#include <QUrl>

class QFile;
class QNetworkAccessManager;
class QNetworkReply;
class QJsonObject;
class QProcess;

// WhiteWh/zerus only; upstream Zerus has no such updater. Portable Windows
// builds install the fork's published zip. A per-user Linux source installation
// builds the exact published commit in a clean clone, then uses the maintained
// installers. Both keep native agents and the session host running; the Linux
// GUI keeps its ordinary rollback copy (docs/fork-updates.md).
class ForkUpdater : public QObject {
    Q_OBJECT
public:
    // Where the running build lives and where its updates go.
    struct Layout {
        // The folder of the running build, links resolved.
        QString install;
        // Versioned root: <root>\current is a junction to the running build, and
        // the builds, the staged update and the log all stay inside <root>.
        bool versioned = false;
        QString root, link;
        // Another build is current in this versioned root: this one stays put.
        bool superseded = false;
        // Where a newer build is downloaded and unpacked.
        QString stage;
    };
    static Layout layoutFor(const QString &executable);
    static bool linuxSourceInstall(const QString &executable, const QString &home);
    static ForkUpdater *instance();
    // The fork workflow's run number, or 0 for a build made elsewhere.
    static qint64 installedBuild();
    // #511 N10: what the feed's build is to the running one. The running
    // commit is never offered again; a numbered build takes a higher number;
    // an unnumbered build (made locally or experimental) has no order, so the
    // feed's build can only replace it, once the owner confirms.
    enum class Offer { Current, Newer, Replacement };
    static Offer offerFor(qint64 installed, const QString &runningCommit, qint64 build, const QString &commit);
    // On the preparation thread: source_commit in build-info.json, or empty.
    static QString sourceCommit(const QString &folder);
    // Built with a feed and running from a supported installation.
    bool enabled() const { return m_enabled; }
    // Portable Windows and per-user Linux source installations can check by hand.
    bool manualAvailable() const { return m_portable || m_linux; }
    bool installing() const { return m_installing; }
    // Check the configured or latest WhiteWh feed now.
    void checkNow();
    // A build the running unnumbered build can only be replaced with.
    qint64 offeredBuild() const { return m_offeredBuild; }
    QString offeredCommit() const { return m_offeredBuild > 0 ? m_commit : QString(); }
    QString runningCommit() const { return m_runningCommit; }
    // The owner confirmed the replacement: it downloads and waits for Restart.
    void confirmReplacement();
    // A verified newer build waits next to the installation.
    bool ready() const { return m_readyBuild > 0; }
    qint64 readyBuild() const { return m_readyBuild; }
    QString status() const { return m_status; }
    void check();
    // Windows swaps folders after Zerus quits. Linux runs the maintained
    // installers, which save drafts and restart only the GUI at replacement time.
    bool startInstall();
signals:
    void changed();
    void buildReady(qint64 build);
    void installStarted();

private:
    explicit ForkUpdater(QObject *parent);
    void fetch();
    void feedFinished(QNetworkReply *reply);
    void applyFeed(const QJsonObject &feed);
    void download();
    void downloadFinished(QNetworkReply *reply);
    void extracted();
    void sourcePrepared();
    void applyExtracted(bool complete, const QString &output, const QJsonObject &info);
    void acceptPrepared();
    void finish(const QString &status);
    QString unpacked() const;
    QString readyStatus() const;

    bool m_enabled = false, m_portable = false, m_linux = false, m_busy = false, m_installing = false;
    Layout m_layout;
    QString m_install, m_stage, m_status;
    // Where a download goes: the stage, or beside it while a ready build waits
    // there, so that a newer build replaces it only once verified.
    QString m_download;
    QNetworkAccessManager *m_network = nullptr;
    QPointer<QFile> m_file;
    QPointer<QProcess> m_extract;
    QCryptographicHash m_hash{QCryptographicHash::Sha256};
    qint64 m_received = 0, m_readyBuild = 0;
    // The build the feed offers.
    qint64 m_build = 0, m_size = 0;
    QString m_commit, m_readyCommit;
    QUrl m_url;
    // The running build's commit, and the replacement offered.
    QString m_runningCommit;
    qint64 m_offeredBuild = 0;
    QByteArray m_sha256;
};
