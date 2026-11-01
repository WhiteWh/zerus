#include "UpdateController.h"
#include "Settings.h"
#include "UpdatesWidget.h"
#include "WindowsUpdateTrust.h"
#include "ForkUpdater.h"
#include <QDesktopServices>
#include <QDir>
#include <QProcess>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QScopeGuard>
#include <QSettings>
#include <QSignalSpy>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTest>
#include <QTimer>
#include <QDateTime>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QRegularExpression>
#include <QSet>
#ifdef Q_OS_UNIX
#include <fcntl.h>
#include <unistd.h>
#endif
#include "ChildProcess.h"
#include "OffGuiThread.h"
#include "ProcessRunner.h"

namespace {
// <link> pointing at <target>: a junction on Windows, as a versioned fork root has.
bool makeDirectoryLink(const QString &link,const QString &target){
#ifdef Q_OS_WIN
    return QProcess::execute("cmd",{"/c","mklink","/J",QDir::toNativeSeparators(link),QDir::toNativeSeparators(target)})==0;
#else
    return QFile::link(target,link);
#endif
}
// The parsers parse JSON, which production does off the GUI thread (JsonParse.h).
DesktopUpdateFeed parsedFeed(const QByteArray &data,const QString &channel){return offGuiThread([&]{return UpdateController::parseFeed(data,channel);});}
bool isInstallerRecord(const QByteArray &data){return offGuiThread([&]{return UpdateController::installerRecord(data);});}
// Applies after every prepare step posted before it, such as a catalog restore (ProcessRunner.h).
void preparationBarrier(){QObject owner;bool done=false;ProcessRunner::post(&owner,[]{return true;},[&done](bool){done=true;});QTRY_VERIFY(done);}
#ifdef Q_OS_UNIX
// A request's pipe ProcessRunner's macOS worker is still creating: pipe() made it
// and close-on-exec follows only afterwards. High numbers keep its ends apart from
// the descriptor a listing of /dev/fd opens.
struct CreatingPipe {
    int ends[2]={-1,-1};
    CreatingPipe(){int made[2];if(::pipe(made)!=0)return;for(int i=0;i<2;++i){ends[i]=::fcntl(made[i],F_DUPFD,64);::close(made[i]);}}
    ~CreatingPipe(){for(int fd:ends)if(fd>=0)::close(fd);}
    bool valid() const{return ends[0]>=64&&ends[1]>=64;}
};
// The descriptors in a child's `ls /dev/fd`.
QList<int> descriptors(const QString &path){
    QFile file(path);QList<int> open;if(!file.open(QIODevice::ReadOnly))return open;
    for(const auto &name:file.readAll().simplified().split(' '))if(!name.isEmpty())open<<name.toInt();
    return open;
}
#endif
// The anchors GitHub gives a Markdown file: each heading's text in lower case without
// punctuation other than hyphens and underscores, spaces turned into hyphens and a
// -1, -2 suffix on repeats, plus explicit <a id> anchors. Fenced code has none.
QSet<QString> markdownAnchors(const QString &path){
    QSet<QString> anchors;QFile file(path);if(!file.open(QIODevice::ReadOnly|QIODevice::Text))return anchors;
    QHash<QString,int> seen;bool fenced=false;
    const QRegularExpression heading("\\A#{1,6}\\s+(.+?)\\s*#*\\s*\\z"),explicitAnchor("<a\\s+(?:id|name)=\"([^\"]+)\""),punctuation("[^\\w\\- ]",QRegularExpression::UseUnicodePropertiesOption);
    for(const auto &line:QString::fromUtf8(file.readAll()).split('\n')){
        if(line.trimmed().startsWith("```")){fenced=!fenced;continue;}
        if(fenced)continue;
        for(auto found=explicitAnchor.globalMatch(line);found.hasNext();)anchors<<found.next().captured(1);
        const auto match=heading.match(line);if(!match.hasMatch())continue;
        QString slug=match.captured(1).toLower().remove(punctuation).replace(' ','-');
        const int repeat=seen.value(slug);seen[slug]=repeat+1;
        anchors<<(repeat?slug+"-"+QString::number(repeat):slug);
    }
    return anchors;
}
}

// Stands in for the desktop browser: QDesktopServices hands web links here.
class UrlRecorder final : public QObject {
    Q_OBJECT
public:
    UrlRecorder(){for(const auto *scheme:{"https","http"})QDesktopServices::setUrlHandler(scheme,this,"record");}
    ~UrlRecorder() override{for(const auto *scheme:{"https","http"})QDesktopServices::unsetUrlHandler(scheme);}
    QList<QUrl> urls;
public slots:
    void record(const QUrl &url){urls<<url;}
};

class FixtureReply : public QNetworkReply {
    QByteArray bytes; qint64 position=0;
public:
    FixtureReply(){open(QIODevice::ReadOnly);setAttribute(QNetworkRequest::HttpStatusCodeAttribute,200);}
    void finish(const QByteArray &data){bytes=data;emit readyRead();setFinished(true);emit finished();}
    void abort() override{setError(QNetworkReply::OperationCanceledError,"canceled");setFinished(true);emit finished();}
    qint64 bytesAvailable() const override{return bytes.size()-position+QNetworkReply::bytesAvailable();}
protected:
    qint64 readData(char *data,qint64 max) override{qint64 count=qMin(max,bytes.size()-position);if(count==0)return -1;memcpy(data,bytes.constData()+position,count);position+=count;return count;}
};

class TestUpdates : public QObject {
    Q_OBJECT
    QJsonObject raw() const {
        return {{"schemaVersion",1},{"product","zerus"},{"platform","desktop"},{"channel","stable"},{"versionName","0.37.0"},{"releaseId",407198248},{"publishedAt","2026-10-09T12:00:00Z"},{"sourceCommit",QString(40,'a')},{"releaseUrl","https://github.com/ufna/zerus/releases/tag/v0.37.0"},
            {"packages",QJsonObject{{"zerus-ade-bin","0.37.0-1"},{"zerus-git","0.37.0.r10.gaaaaaaa-1"}}},
            {"artifacts",QJsonArray{QJsonObject{{"kind","arch-package"},{"name","zerus-ade-bin-0.37.0-1-x86_64.pkg.tar.zst"},{"url","https://zerus.dev/downloads/v0.37.0/zerus-ade-bin-0.37.0-1-x86_64.pkg.tar.zst"},{"sha256",QString(64,'b')},{"size",100},{"os","linux"},{"arch","x86_64"}}}}};
    }
    // Stable and nightly catalogs as scripts/update_feed.py publishes them (the
    // Windows zip is linked from the release page only, never listed as an artifact).
    QJsonObject stableRaw(const QString &version) const {
        const QString tag="v"+version,name="zerus-ade-bin-"+version+"-1-x86_64.pkg.tar.zst";
        auto value=raw();value["versionName"]=version;value["releaseUrl"]="https://github.com/ufna/zerus/releases/tag/"+tag;
        value["packages"]=QJsonObject{{"zerus-ade-bin",version+"-1"}};
        value["artifacts"]=QJsonArray{QJsonObject{{"kind","arch-package"},{"name",name},{"url","https://zerus.dev/downloads/"+tag+"/"+name},{"sha256",QString(64,'b')},{"size",100},{"os","linux"},{"arch","x86_64"}}};
        return value;
    }
    QJsonObject nightlyRaw(const QString &version) const {
        const QString tag="nightly-123",name="zerus-ade-nightly-bin-"+version+"-1-x86_64.pkg.tar.zst";
        auto value=raw();value["channel"]="nightly";value["versionName"]=version;value["releaseUrl"]="https://github.com/ufna/zerus/releases/tag/"+tag;
        value["packages"]=QJsonObject{{"zerus-ade-nightly-bin",version+"-1"}};
        value["artifacts"]=QJsonArray{QJsonObject{{"kind","arch-package"},{"name",name},{"url","https://zerus.dev/downloads/"+tag+"/"+name},{"sha256",QString(64,'b')},{"size",100},{"os","linux"},{"arch","x86_64"}}};
        return value;
    }
    QJsonObject devRaw() const {
        auto value=raw();value["schemaVersion"]=2;value["channel"]="dev";value["channelTag"]="dev";value["buildId"]=12345;value["buildNumber"]=50;value.remove("releaseId");value.remove("releaseUrl");value["buildUrl"]="https://github.com/ufna/zerus/actions/runs/12345";value.remove("packages");
        value["artifacts"]=QJsonArray{
            QJsonObject{{"kind","arch-package"},{"name","zerus.pkg.tar.zst"},{"url","https://zerus.dev/downloads/dev-desktop-12345/zerus.pkg.tar.zst"},{"sha256",QString(64,'b')},{"size",100},{"os","linux"},{"arch","x86_64"}},
            QJsonObject{{"kind","source"},{"name","zerus-source.tar.gz"},{"url","https://zerus.dev/downloads/dev-desktop-12345/zerus-source.tar.gz"},{"sha256",QString(64,'c')},{"size",100},{"os","any"},{"arch","source"}}};
        return value;
    }
    DesktopUpdateFeed nightlyCatalog(const QString &version) const {
        return parsedFeed(QJsonDocument(nightlyRaw(version)).toJson(),"nightly");
    }
    QJsonObject withMacos(QJsonObject value) const {
        const QString version=value["versionName"].toString();
        const QString prefix=value["channel"]=="dev"?QString("https://zerus.dev/downloads/dev-desktop-%1/").arg(value["buildId"].toInteger()):QString(value["releaseUrl"].toString()).replace("/tag/","/download/")+"/";
        const QString archive="zerus-"+version+"-macos-arm64.zip",image="zerus-"+version+"-macos-arm64.dmg";
        QJsonArray downloads;for(const auto &name:{archive,image})downloads.append(QJsonObject{{"name",name},{"url",prefix+name},{"size",100},{"sha256",QString(64,'a')}});
        value["macos"]=QJsonObject{{"asset",archive},{"minimum_system_version","14.0"},{"qt_version","6.11.2"},{"team_id","ABCDE12345"},{"notarization_id","0123abcd-4567-89ef-0123-456789abcdef"},
            {"dmg",QJsonObject{{"asset",image},{"notarization_id","1123abcd-4567-89ef-0123-456789abcdef"}}},{"downloads",downloads}};
        return value;
    }
    void tool(const QString &path,const QByteArray &script){QFile file(path);QVERIFY(file.open(QIODevice::WriteOnly));file.write(script);file.close();QVERIFY(file.setPermissions(QFile::ReadOwner|QFile::WriteOwner|QFile::ExeOwner));}
private slots:
    void forkLayoutOfAPlainFolderKeepsItsSiblingStage(){
        QTemporaryDir root;QVERIFY(root.isValid());
        const QString build=root.filePath("zerus");QVERIFY(QDir().mkpath(build));
        const auto layout=ForkUpdater::layoutFor(build+"/hgs-tray.exe");
        QVERIFY(!layout.versioned);QVERIFY(!layout.superseded);
        QCOMPARE(layout.install,QFileInfo(build).canonicalFilePath());
        QCOMPARE(layout.stage,layout.install+".update");
    }
    void forkLayoutOfAVersionedRootKeepsEverythingInside(){
        QTemporaryDir root;QVERIFY(root.isValid());
        const QString build=root.filePath("app-7");QVERIFY(QDir().mkpath(build));
        QVERIFY(makeDirectoryLink(root.filePath("current"),build));
        const QString canonicalRoot=QFileInfo(root.path()).canonicalFilePath();
        for(const QString &exe:{root.filePath("current/hgs-tray.exe"),build+"/hgs-tray.exe"}){
            const auto layout=ForkUpdater::layoutFor(exe);
            QVERIFY2(layout.versioned,qPrintable(exe));QVERIFY(!layout.superseded);
            QCOMPARE(layout.install,QFileInfo(build).canonicalFilePath());
            QCOMPARE(layout.root,canonicalRoot);
            QCOMPARE(layout.link,QDir(canonicalRoot).filePath("current"));
            QCOMPARE(layout.stage,QDir(canonicalRoot).filePath(".update"));
        }
    }
    void forkLayoutOfABuildThatIsNotCurrentLeavesTheRootAlone(){
        QTemporaryDir root;QVERIFY(root.isValid());
        QVERIFY(QDir().mkpath(root.filePath("app-7")));QVERIFY(QDir().mkpath(root.filePath("app-8")));
        QVERIFY(makeDirectoryLink(root.filePath("current"),root.filePath("app-8")));
        const auto layout=ForkUpdater::layoutFor(root.filePath("app-7/hgs-tray.exe"));
        QVERIFY(!layout.versioned);QVERIFY(layout.superseded);
    }
    void forkLinuxSourceInstallIsOnlyThePerUserLayout(){
        QTemporaryDir root;QVERIFY(root.isValid());
        const QString bin=root.filePath(".local/bin");QVERIFY(QDir().mkpath(bin));
        const QString gui=bin+"/hgs-tray";tool(gui,"");tool(bin+"/hgs","");
#ifdef Q_OS_LINUX
        QVERIFY(ForkUpdater::linuxSourceInstall(gui,root.path()));
        QVERIFY(!ForkUpdater::linuxSourceInstall(root.filePath("hgs-tray"),root.path()));
        QFile::remove(bin+"/hgs");
        QVERIFY(!ForkUpdater::linuxSourceInstall(gui,root.path()));
#else
        QVERIFY(!ForkUpdater::linuxSourceInstall(gui,root.path()));
#endif
    }
#ifdef Q_OS_LINUX
    void forkLinuxPrepareChecksTheCommitAndBuildsInTheStage(){
        QTemporaryDir root;QVERIFY(root.isValid());
        const QString bin=root.filePath("bin");QVERIFY(QDir().mkpath(bin));
        tool(bin+"/git",R"(#!/bin/bash
echo "git $*" >> "$LOG"
if [ "$1" = clone ]; then
    for target; do :; done
    mkdir -p "$target"
elif [ "$1" = -C ] && [ "$3" = rev-parse ]; then
    echo "$EXPECT"
fi
)");
        tool(bin+"/cargo",R"(#!/bin/bash
echo "cargo $*" >> "$LOG"
)");
        tool(bin+"/cmake",R"(#!/bin/bash
echo "cmake $*" >> "$LOG"
)");
        const auto path=qgetenv("PATH"),home=qgetenv("HOME"),prefix=qgetenv("CMAKE_PREFIX_PATH");
        qputenv("PATH",bin.toLocal8Bit()+":"+path);qputenv("HOME",root.path().toLocal8Bit());qunsetenv("CMAKE_PREFIX_PATH");
        QVERIFY(QDir().mkpath(root.filePath(".local/share/zerus/kf6-test")));
        const auto restore=qScopeGuard([&]{qputenv("PATH",path);qputenv("HOME",home);qputenv("CMAKE_PREFIX_PATH",prefix);});
        qputenv("LOG",root.filePath("calls").toLocal8Bit());
        const QString commit(40,'a');qputenv("EXPECT",commit.toLocal8Bit());
        const QString stage=root.filePath("stage");
        QProcess process;process.setProgram("/bin/bash");
        process.setArguments({QStringLiteral(ZERUS_LINUX_UPDATE_HELPER),"prepare",stage,commit,"7"});
        ChildProcess::inheritStdioOnly(process);process.start();QVERIFY(process.waitForFinished(10000));
        QCOMPARE(process.exitStatus(),QProcess::NormalExit);QCOMPARE(process.exitCode(),0);
        QFile calls(root.filePath("calls"));QVERIFY(calls.open(QIODevice::ReadOnly));const QByteArray first=calls.readAll();
        QVERIFY(first.contains("clone --filter=blob:none --no-checkout --branch whitewh/main https://github.com/WhiteWh/zerus.git"));
        QVERIFY(first.contains("checkout --detach "+commit.toLocal8Bit()));
        QVERIFY(first.contains("cargo build --manifest-path "+stage.toLocal8Bit()+"/source/Cargo.toml --locked --release"));
        QVERIFY(first.contains("-DBUILD_TESTING=OFF"));
        QVERIFY(first.contains("-DCMAKE_PREFIX_PATH="+root.filePath(".local/share/zerus/kf6-test").toLocal8Bit()));
        QVERIFY(first.contains("--target hgs-tray"));
        calls.close();QVERIFY(calls.open(QIODevice::WriteOnly|QIODevice::Truncate));calls.close();
        qputenv("EXPECT",QByteArray(40,'b'));
        QProcess wrong;wrong.setProgram("/bin/bash");
        wrong.setArguments({QStringLiteral(ZERUS_LINUX_UPDATE_HELPER),"prepare",root.filePath("wrong"),commit,"7"});
        ChildProcess::inheritStdioOnly(wrong);wrong.start();QVERIFY(wrong.waitForFinished(10000));
        QVERIFY(wrong.exitCode()!=0);
        QVERIFY(calls.open(QIODevice::ReadOnly));const QByteArray second=calls.readAll();QVERIFY(!second.contains("cargo build"));
    }
    void forkLinuxInstallUsesTheMaintainedInstallers(){
        QTemporaryDir root;QVERIFY(root.isValid());
        const QString bin=root.filePath("bin"),source=root.filePath("stage/source");
        QVERIFY(QDir().mkpath(bin));QVERIFY(QDir().mkpath(source+"/tray"));
        tool(bin+"/git",R"(#!/bin/bash
if [ "$1" = -C ] && [ "$3" = rev-parse ]; then echo "$EXPECT"; fi
)");
        tool(source+"/install.sh",R"(#!/bin/bash
echo "root $HGS_INSTALL_PREFIX $CARGO_TARGET_DIR $ZERUS_FORK_BUILD ${HGS_STATE_DIR:-unset}" >> "$LOG"
)");
        tool(source+"/tray/install.sh",R"(#!/bin/bash
echo "gui $HGS_TRAY_RESTART $ZERUS_DEV_BUILD_NUMBER $ZERUS_FORK_FEED_URL" >> "$LOG"
)");
        const auto path=qgetenv("PATH"),home=qgetenv("HOME"),prefix=qgetenv("HGS_INSTALL_PREFIX"),state=qgetenv("HGS_STATE_DIR");
        qputenv("PATH",bin.toLocal8Bit()+":"+path);qputenv("HOME",root.path().toLocal8Bit());
        qputenv("HGS_INSTALL_PREFIX","/elsewhere");qputenv("HGS_STATE_DIR","/elsewhere");
        qputenv("LOG",root.filePath("calls").toLocal8Bit());
        const QString commit(40,'a');qputenv("EXPECT",commit.toLocal8Bit());
        const auto restore=qScopeGuard([&]{qputenv("PATH",path);qputenv("HOME",home);qputenv("HGS_INSTALL_PREFIX",prefix);qputenv("HGS_STATE_DIR",state);});
        QProcess process;process.setProgram("/bin/bash");
        process.setArguments({QStringLiteral(ZERUS_LINUX_UPDATE_HELPER),"install",root.filePath("stage"),commit,"7"});
        ChildProcess::inheritStdioOnly(process);process.start();QVERIFY(process.waitForFinished(10000));
        QCOMPARE(process.exitStatus(),QProcess::NormalExit);QCOMPARE(process.exitCode(),0);
        QFile calls(root.filePath("calls"));QVERIFY(calls.open(QIODevice::ReadOnly));const QByteArray text=calls.readAll();
        QVERIFY(text.contains("root "+root.path().toLocal8Bit()+"/.local "+source.toLocal8Bit()+"/target 7 unset"));
        QVERIFY(text.contains("gui 1 7 https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json"));
        QFile result(root.filePath("stage/install.exit"));QVERIFY(result.open(QIODevice::ReadOnly));
        QCOMPARE(result.readAll().trimmed(),QByteArray("0"));
    }
#endif
    // #511 N10: a local or experimental build has no number (0); any release
    // used to count as newer and replaced it unasked.
    void forkFeedIsComparedWithTheRunningCommit(){
        using Offer=ForkUpdater::Offer;
        const QString running(40,'a'),other(40,'b');
        QVERIFY(ForkUpdater::offerFor(0,running,7,running)==Offer::Current);
        QVERIFY(ForkUpdater::offerFor(0,running,7,other)==Offer::Replacement);
        QVERIFY(ForkUpdater::offerFor(0,QString(),7,other)==Offer::Replacement);
        QVERIFY(ForkUpdater::offerFor(5,running,7,running)==Offer::Current);
        QVERIFY(ForkUpdater::offerFor(5,running,7,other)==Offer::Newer);
        QVERIFY(ForkUpdater::offerFor(7,running,7,other)==Offer::Current);
        QVERIFY(ForkUpdater::offerFor(9,running,7,other)==Offer::Current);
    }
    void forkRunningCommitComesFromBuildInfo(){
        QTemporaryDir root;QVERIFY(root.isValid());
        QCOMPARE(offGuiThread([&]{return ForkUpdater::sourceCommit(root.path());}),QString());
        const auto write=[&](const QByteArray &text){QFile file(root.filePath("build-info.json"));QVERIFY(file.open(QIODevice::WriteOnly|QIODevice::Truncate));file.write(text);};
        write(R"({"source_commit":"0123456789abcdef0123456789abcdef01234567"})");
        QCOMPARE(offGuiThread([&]{return ForkUpdater::sourceCommit(root.path());}),QStringLiteral("0123456789abcdef0123456789abcdef01234567"));
        write(R"({"source_commit":"0123456"})");
        QCOMPARE(offGuiThread([&]{return ForkUpdater::sourceCommit(root.path());}),QString());
        write("not json");
        QCOMPARE(offGuiThread([&]{return ForkUpdater::sourceCommit(root.path());}),QString());
    }
    void devVersionSequenceIsVisibleAndMatchesTheFeed(){
        auto value=devRaw();
        auto legacy=parsedFeed(QJsonDocument(value).toJson(),"dev");
        QVERIFY(legacy.valid);QCOMPARE(legacy.version,value["versionName"].toString()+"-dev.50");
        value["versionName"]="0.42.1-dev.50";
        auto numbered=parsedFeed(QJsonDocument(value).toJson(),"dev");
        QVERIFY(numbered.valid);QCOMPARE(numbered.version,QString("0.42.1-dev.50"));
        for(const auto &bad:{"0.42.1-dev.49","0.42.1-dev.050","0.42.1-dev.51"}){
            value["versionName"]=bad;QVERIFY(!parsedFeed(QJsonDocument(value).toJson(),"dev").valid);
        }
        const auto identity=UpdateController::buildInfo();const auto version=UpdateController::installedVersion();
        QSettings settings;const auto previous=settings.value("updates/channel");
        for(const auto &channel:{"stable","dev","nightly"}){
            settings.setValue("updates/channel",channel);
            QCOMPARE(UpdateController::buildInfo(),identity);QCOMPARE(UpdateController::installedVersion(),version);
        }
        if(previous.isValid())settings.setValue("updates/channel",previous);else settings.remove("updates/channel");
    }
    void devCatalogAndNotificationsUseBuildIdentity(){
        auto value=raw();value["schemaVersion"]=2;value["channel"]="dev";value["channelTag"]="dev";value["buildId"]=12345;value["buildNumber"]=50;value.remove("releaseId");value.remove("releaseUrl");value["buildUrl"]="https://github.com/ufna/zerus/actions/runs/12345";value.remove("packages");
        value["artifacts"]=QJsonArray{
            QJsonObject{{"kind","arch-package"},{"name","zerus.pkg.tar.zst"},{"url","https://zerus.dev/downloads/dev-desktop-12345/zerus.pkg.tar.zst"},{"sha256",QString(64,'b')},{"size",100},{"os","linux"},{"arch","x86_64"}},
            QJsonObject{{"kind","source"},{"name","zerus-source.tar.gz"},{"url","https://zerus.dev/downloads/dev-desktop-12345/zerus-source.tar.gz"},{"sha256",QString(64,'c')},{"size",100},{"os","any"},{"arch","source"}}};
        auto feed=parsedFeed(QJsonDocument(value).toJson(),"dev");QVERIFY(feed.valid);QCOMPARE(feed.buildNumber,50);QCOMPARE(feed.releaseId,12345);
        QVERIFY(!parsedFeed(QJsonDocument(value).toJson(),"stable").valid);
        QVERIFY(UpdateController::devBuildIsNewer(feed,49,QString(40,'d')));
        QVERIFY(!UpdateController::devBuildIsNewer(feed,51,QString(40,'d')));
        QVERIFY(!UpdateController::devBuildIsNewer(feed,0,QString(40,'a')));
        QVERIFY(UpdateController::devBuildIsNewer(feed,0,{}));
        QVERIFY(UpdateController::isUpdateToken("zerus-update:v2:dev:12345"));
        QVERIFY(!UpdateController::isUpdateToken("zerus-update:v1:dev:12345"));
        QVERIFY(!UpdateController::isUpdateToken("zerus-update:v2:stable:12345"));
        QCOMPARE(UpdateController::automaticInterval("dev"),15*60);QCOMPARE(UpdateController::automaticInterval("stable"),24*60*60);
        DesktopInstallation installation{"package","zerus-ade-bin","0.39.0-1",true,"yay"};QVERIFY(UpdateController::updateCommand(installation,"dev").isEmpty());
        for(QString key:{"buildId","buildNumber","channelTag","buildUrl","sourceCommit"}){auto bad=value;bad[key]="bad";QVERIFY(!parsedFeed(QJsonDocument(bad).toJson(),"dev").valid);}
        value["sourceDirty"]=false;QVERIFY(!parsedFeed(QJsonDocument(value).toJson(),"dev").valid);
    }
    void devChannelPersistsAndChecksItsIndependentFeed(){
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);QString requested;
        UpdateController controller(nullptr,dir.path()+"/missing",[&](const QNetworkRequest &request){requested=request.url().toString();auto *reply=new FixtureReply;QTimer::singleShot(0,reply,[reply]{reply->finish("{}");});return reply;});
        controller.setChannel("dev");QCOMPARE(controller.channel(),QString("dev"));controller.check();QTRY_VERIFY(!controller.busy());QCOMPARE(requested,QString("https://zerus.dev/updates/v2/desktop/dev.json"));
        UpdateController restored(nullptr,dir.path()+"/missing");QCOMPARE(restored.channel(),QString("dev"));
    }
    void initTestCase(){QCoreApplication::setOrganizationName("zerus-update-test");QCoreApplication::setApplicationName("isolated");QSettings::setDefaultFormat(QSettings::IniFormat);cleanup();}
    // Tests point the settings at temporary folders of their own. Later writes
    // (the next test, deferred work) go to empty settings that outlive the test
    // run instead of recreating a removed folder.
    void cleanup(){QVERIFY(m_settings.isValid());QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,m_settings.path());Settings::flush();QSettings().clear();}
    void validCatalog(){auto value=parsedFeed(QJsonDocument(raw()).toJson(),"stable");QVERIFY(value.valid);QCOMPARE(value.version,QString("0.37.0"));QCOMPARE(value.packages["zerus-git"].toString(),QString("0.37.0.r10.gaaaaaaa-1"));}
    void maliciousCatalog(){
        for(QString key:{"schemaVersion","product","platform","channel","releaseUrl","sourceCommit","publishedAt"}){auto value=raw();value[key]=key=="schemaVersion"?QJsonValue(2):QJsonValue("evil");QVERIFY2(!parsedFeed(QJsonDocument(value).toJson(),"stable").valid,qPrintable(key));}
        for(QString key:{"name","url","sha256","size","os","arch","kind"}){auto value=raw();auto artifact=value["artifacts"].toArray()[0].toObject();artifact[key]=key=="size"?QJsonValue(1e30):QJsonValue("../evil");value["artifacts"]=QJsonArray{artifact};QVERIFY2(!parsedFeed(QJsonDocument(value).toJson(),"stable").valid,qPrintable(key));}
        auto value=raw();value["sourceDirty"]=true;QVERIFY(!parsedFeed(QJsonDocument(value).toJson(),"stable").valid);
        value=raw();auto array=value["artifacts"].toArray();array.append(array[0]);value["artifacts"]=array;QVERIFY(!parsedFeed(QJsonDocument(value).toJson(),"stable").valid);
        QVERIFY(!parsedFeed(QByteArray(256*1024+1,' '),"stable").valid);
    }
    void wrongChannelAndMissing(){auto value=raw();value.remove("artifacts");QVERIFY(!parsedFeed(QJsonDocument(value).toJson(),"stable").valid);QVERIFY(!parsedFeed(QJsonDocument(raw()).toJson(),"nightly").valid);}
    void safeInstructions(){
        DesktopInstallation package{"package","zerus-ade-bin","0.36.2-1",true,"paru"};QCOMPARE(UpdateController::updateCommand(package),QString("paru -Syu zerus-ade-bin"));
        package.package="zerus-git";QCOMPARE(UpdateController::updateCommand(package),QString("paru -Syu --devel"));
        package.package="unrecognized; sudo command";QVERIFY(UpdateController::updateCommand(package).isEmpty());
        package.kind="source";package.package="zerus";QCOMPARE(UpdateController::updateCommand(package),QString("paru -Syu zerus-ade-bin"));
        auto catalog=parsedFeed(QJsonDocument(raw()).toJson(),"stable");
        DesktopInstallation unknown{"unknown",{},"0.99.0"};
        DesktopInstallation actual{"package","zerus-ade-bin","0.9.0-1"};
#ifdef Q_OS_MACOS
        const QString cask="brew install --cask ufna/zerus/zerus";
        QVERIFY(UpdateController::comparisonMessage(unknown,catalog,"0.99.0",0,false).contains("No macOS build"));
        QVERIFY(!UpdateController::comparisonMessage(unknown,catalog,"0.99.0",0,false).contains(cask));
        catalog=parsedFeed(QJsonDocument(withMacos(raw())).toJson(),"stable");
        QVERIFY(UpdateController::comparisonMessage(unknown,catalog,"0.99.0",0,false).contains("unconfirmed"));
        QVERIFY(UpdateController::comparisonMessage(unknown,catalog,"0.99.0",0,false).contains(cask));
        QVERIFY(UpdateController::comparisonMessage(unknown,catalog,"0.9.0",0,false).contains("Update available"));
        QVERIFY(UpdateController::comparisonMessage(unknown,catalog,"0.9.0",0,false).contains(cask));
        QVERIFY(!UpdateController::comparisonMessage(actual,catalog,"0.9.0",1,true).contains("No macOS installer"));
#else
        QVERIFY(UpdateController::comparisonMessage(unknown,catalog,"0.99.0",0,false).contains("unconfirmed"));
        QVERIFY(UpdateController::comparisonMessage(actual,catalog,"0.9.0",1,true).contains("Update available"));
        QVERIFY(UpdateController::comparisonMessage(actual,catalog,"0.9.0",-1,true).contains("at least"));
        QVERIFY(UpdateController::comparisonMessage(actual,catalog,"0.9.0",0,false).contains("No matching"));
#endif
    }
    void portableWindowsBuildFollowsStableAndNightlyReleases(){
        auto stable=parsedFeed(QJsonDocument(stableRaw("0.42.3")).toJson(),"stable");QVERIFY(stable.valid);
        auto last=parsedFeed(QJsonDocument(stableRaw("0.42.2")).toJson(),"stable");QVERIFY(last.valid);
        auto older=parsedFeed(QJsonDocument(raw()).toJson(),"stable");QVERIFY(older.valid);
        auto nightly=parsedFeed(QJsonDocument(nightlyRaw("0.38.0.r10.gaaaaaaa.n5")).toJson(),"nightly");QVERIFY(nightly.valid);
        DesktopInstallation portable{"portable",{},{}};
        // Stable releases after 0.42.2 carry the zip, which knows only its product version.
        QVERIFY(UpdateController::stableHasWindowsBuild("0.42.3"));QVERIFY(UpdateController::stableHasWindowsBuild("0.43.0"));QVERIFY(UpdateController::stableHasWindowsBuild("1.0.0"));
        QVERIFY(!UpdateController::stableHasWindowsBuild("0.42.2"));QVERIFY(!UpdateController::stableHasWindowsBuild("0.37.0"));QVERIFY(!UpdateController::stableHasWindowsBuild("0.42.3.r10.gaaaaaaa.n5"));
        QVERIFY(UpdateController::portableUpdateAvailable("0.42.2",stable));QVERIFY(UpdateController::portableUpdateAvailable("0.38.0",stable));
        QCOMPARE(UpdateController::comparisonMessage(portable,stable,"0.42.2",0,false),QString("Zerus 0.42.3 is available. Download its Windows zip from the release page."));
        QVERIFY(!UpdateController::portableUpdateAvailable("0.42.3",stable));QVERIFY(!UpdateController::portableUpdateAvailable("0.43.0",stable));QVERIFY(!UpdateController::portableUpdateAvailable("0.42.2-dev",stable));
        QCOMPARE(UpdateController::comparisonMessage(portable,stable,"0.42.3",0,false),QString("Portable Windows build: there is no self-update, and no newer stable release is published."));
        // Earlier stable releases have no zip: never announced to a portable build, even when newer.
        QVERIFY(!UpdateController::portableUpdateAvailable("0.42.0",last));QVERIFY(!UpdateController::portableUpdateAvailable("0.36.2",older));
        QCOMPARE(UpdateController::comparisonMessage(portable,last,"0.42.0",0,false),QString("Zerus 0.42.2 has no Windows build; stable releases after it carry one. Until then, choose the nightly channel to hear about newer Windows builds."));
        QVERIFY(UpdateController::comparisonMessage(portable,older,"0.36.2",0,false).contains("Zerus 0.37.0 has no Windows build"));
        QVERIFY(UpdateController::comparisonMessage(portable,last,"0.42.2",0,false).contains("no newer stable release"));
        // Nightlies carry it too.
        QVERIFY(UpdateController::portableUpdateAvailable("0.37.0",nightly));
        QCOMPARE(UpdateController::comparisonMessage(portable,nightly,"0.37.0",0,false),QString("Zerus nightly 0.38.0.r10.gaaaaaaa.n5 is available. Download its Windows zip from the release page."));
        QVERIFY(!UpdateController::portableUpdateAvailable("0.38.0",nightly));QVERIFY(!UpdateController::portableUpdateAvailable("0.39.0",nightly));
        QVERIFY(!UpdateController::portableUpdateAvailable("0.38.0-dev",nightly));QVERIFY(!UpdateController::portableUpdateAvailable("0.37.0",DesktopUpdateFeed{}));
        QVERIFY(UpdateController::comparisonMessage(portable,nightly,"0.38.0",0,false).contains("no self-update"));
        QVERIFY(UpdateController::updateCommand(portable).isEmpty());QVERIFY(UpdateController::updateCommand(portable,"nightly").isEmpty());
        // The catalog itself is unchanged: a Windows client accepts exactly what Linux clients do.
        for(auto value:{raw(),stableRaw("0.42.3"),nightlyRaw("0.38.0.r10.gaaaaaaa.n5")}){
            const QString tag=value["releaseUrl"].toString().section('/',-1),channel=value["channel"].toString();
            auto artifacts=value["artifacts"].toArray();
            artifacts.append(QJsonObject{{"kind","windows-zip"},{"name","zerus-windows-x86_64.zip"},{"url","https://zerus.dev/downloads/"+tag+"/zerus-windows-x86_64.zip"},{"sha256",QString(64,'c')},{"size",100},{"os","windows"},{"arch","x86_64"}});
            value["artifacts"]=artifacts;QVERIFY2(!parsedFeed(QJsonDocument(value).toJson(),channel).valid,qPrintable(channel));
        }
    }
    void portableBuildDefaultsToStableAndFollowsNightlyNeverDev(){
#ifdef Q_OS_WIN
        QTemporaryDir dir;QVERIFY(dir.isValid());QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);
        const auto version=QCoreApplication::applicationVersion();QCoreApplication::setApplicationVersion("0.42.2");const auto restore=qScopeGuard([version]{QCoreApplication::setApplicationVersion(version);});
        QFile executable(dir.path()+"/hgs-tray.exe");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();
        QFile info(dir.path()+"/build-info.json");QVERIFY(info.open(QIODevice::WriteOnly));info.close();
        QByteArray stableFeed=QJsonDocument(stableRaw("0.42.3")).toJson();
        const QByteArray nightlyFeed=QJsonDocument(nightlyRaw("0.43.0.r10.gaaaaaaa.n5")).toJson(),devFeed=QJsonDocument(devRaw()).toJson();QStringList requested;
        const auto fetch=[&](const QNetworkRequest &request){requested<<request.url().fileName();auto *reply=new FixtureReply;const auto name=request.url().fileName();const auto bytes=name=="nightly.json"?nightlyFeed:name=="dev.json"?devFeed:stableFeed;QTimer::singleShot(0,reply,[reply,bytes]{reply->finish(bytes);});return reply;};
        {
            UpdateController controller(nullptr,executable.fileName(),fetch);QSignalSpy notified(&controller,&UpdateController::newerReleaseVerified);
            // Stable releases carry the same tested zip as nightlies: a new portable installation follows stable.
            QTRY_COMPARE(controller.installation().kind,QString("portable"));QCOMPARE(controller.channel(),QString("stable"));
            QVERIFY(!Settings::contains("updates/channel"));
            controller.check();QTRY_VERIFY(!controller.busy());QCOMPARE(requested,QStringList{"stable.json"});
            QVERIFY(controller.updateAvailable());QCOMPARE(notified.size(),1);QCOMPARE(controller.feed().releaseUrl,QString("https://github.com/ufna/zerus/releases/tag/v0.42.3"));
            QVERIFY(controller.comparison().contains("Download its Windows zip"));
            // A nightly with a newer product version is announced on the nightly channel.
            controller.setChannel("nightly");controller.check();QTRY_VERIFY(!controller.busy());QCOMPARE(requested.last(),QString("nightly.json"));
            QVERIFY(controller.updateAvailable());QCOMPARE(notified.size(),2);QCOMPARE(controller.feed().releaseUrl,QString("https://github.com/ufna/zerus/releases/tag/nightly-123"));
            // Dev builds carry no Windows zip: never an update, never polled every 15 minutes.
            controller.setChannel("dev");controller.check();QTRY_VERIFY(!controller.busy());QCOMPARE(requested.last(),QString("dev.json"));
            QVERIFY(controller.feed().valid);QVERIFY(!controller.updateAvailable());QCOMPARE(notified.size(),2);QVERIFY(controller.comparison().contains("Dev builds do not include"));
            QVERIFY(controller.updateToken().isEmpty());
            // A stable release without the zip (0.42.2 and earlier) is never announced, even when newer.
            stableFeed=QJsonDocument(stableRaw("0.42.2")).toJson();QCoreApplication::setApplicationVersion("0.42.0");
            controller.setChannel("stable");controller.check();QTRY_VERIFY(!controller.busy());QCOMPARE(requested.last(),QString("stable.json"));
            QCOMPARE(controller.feed().version,QString("0.42.2"));QVERIFY(!controller.updateAvailable());QCOMPARE(notified.size(),2);QVERIFY(controller.comparison().contains("nightly channel"));
        }
        // A channel already saved, including the nightly default of earlier builds, is kept.
        QSettings().setValue("updates/channel","nightly");
        UpdateController restarted(nullptr,executable.fileName(),fetch);QTRY_COMPARE(restarted.installation().kind,QString("portable"));QCOMPARE(restarted.channel(),QString("nightly"));
#else
        QSKIP("Only Windows builds are portable");
#endif
    }
    void installerBuildsStayLinkOnly(){
        // installation.json as scripts/windows_installer.py writes it.
        const QByteArray record="{\n  \"schema\": 1,\n  \"kind\": \"installer\",\n  \"product\": \"Zerus\",\n  \"version\": \"0.45.0\",\n"
                                "  \"product_version\": \"0.45.0\",\n  \"source_commit\": \"cccccccccccccccccccccccccccccccccccccccc\"\n}\n";
        QVERIFY(isInstallerRecord(record));
        for(const QByteArray &other:{QByteArray(),QByteArray("[]"),QByteArray("{\"schema\":2,\"kind\":\"installer\",\"product\":\"Zerus\"}"),
                                     QByteArray("{\"schema\":1,\"kind\":\"portable\",\"product\":\"Zerus\"}"),QByteArray("{\"schema\":1,\"kind\":\"installer\",\"product\":\"Other\"}"),
                                     QByteArray("{\"schema\":\"1\",\"kind\":\"installer\",\"product\":\"Zerus\"}"),record.left(record.size()-3),
                                     QByteArray("{\"schema\":1,\"kind\":\"installer\",\"product\":\"Zerus\",\"pad\":\"")+QByteArray(UpdateController::installerRecordLimit,'x')+"\"}"})
            QVERIFY2(!isInstallerRecord(other),other.left(80).constData());
        auto stable=parsedFeed(QJsonDocument(stableRaw("0.42.3")).toJson(),"stable");QVERIFY(stable.valid);
        auto nightly=parsedFeed(QJsonDocument(nightlyRaw("0.43.0.r10.gaaaaaaa.n5")).toJson(),"nightly");QVERIFY(nightly.valid);
        DesktopUpdateFeed dev;dev.valid=true;dev.buildNumber=50;dev.version="0.42.0 (dev 50)";
        const DesktopInstallation installer{"installer",{},{}};
        // Like the portable zip: release page links only, no command, no download, no Dev builds.
        auto guide=UpdateController::installationGuide(installer,"stable",stable);
        QVERIFY(guide.ownership.contains("installed for this user by the Zerus installer"));QVERIFY(guide.ownership.contains("does not update itself yet"));
        QVERIFY(guide.command.isEmpty());QVERIFY(guide.copyName.isEmpty());QVERIFY(guide.instructions.isEmpty());
        QVERIFY(UpdateController::updateCommand(installer,"stable").isEmpty());QVERIFY(UpdateController::downloadUrl(installer,stable,"windows").isEmpty());
        QCOMPARE(UpdateController::comparisonMessage(installer,stable,"0.42.2",0,false),QString("Zerus 0.42.3 is available. Download its Windows zip from the release page."));
        QCOMPARE(UpdateController::comparisonMessage(installer,stable,"0.42.3",0,false),QString("This Windows build does not update itself yet, and no newer stable release is published."));
        QVERIFY(UpdateController::comparisonMessage(installer,nightly,"0.42.3",0,false).contains("Download its Windows zip"));
        QVERIFY(UpdateController::comparisonMessage(installer,dev,"0.42.0",0,false).contains("stable or nightly channel"));
#ifdef Q_OS_WIN
        // The installed folder: the zip's files and installation.json.
        QTemporaryDir dir;QVERIFY(dir.isValid());
        for(const char *file:{"hgs-tray.exe","build-info.json"}){QFile created(dir.filePath(file));QVERIFY(created.open(QIODevice::WriteOnly));}
        {QFile written(dir.filePath("installation.json"));QVERIFY(written.open(QIODevice::WriteOnly));written.write(record);}
        UpdateController installed(nullptr,dir.filePath("hgs-tray.exe"),[](const QNetworkRequest &){return new FixtureReply;});
        QTRY_COMPARE(installed.installation().kind,QString("installer"));QVERIFY(installed.installation().package.isEmpty());QCOMPARE(installed.channel(),QString("stable"));
        // Any other record leaves the folder a portable build.
        {QFile written(dir.filePath("installation.json"));QVERIFY(written.open(QIODevice::WriteOnly|QIODevice::Truncate));written.write("{\"schema\":1,\"kind\":\"portable\",\"product\":\"Zerus\"}");}
        UpdateController other(nullptr,dir.filePath("hgs-tray.exe"),[](const QNetworkRequest &){return new FixtureReply;});
        QTRY_COMPARE(other.installation().kind,QString("portable"));
#endif
    }
    void windowsUpdateTrustIsDormantAndStrict(){
        // No build carries trust anchors until the owner creates them: nothing it downloads ever runs.
        QVERIFY(WindowsUpdateTrust::manifestKey().isEmpty());QVERIFY(WindowsUpdateTrust::expectedPublisher().isEmpty());
        QVERIFY(!WindowsUpdateTrust::selfUpdateConfigured());
        // A test key and its ECDSA P-256 SHA-256 signature (r, s) of the exact bytes below.
        const QByteArray key=QByteArray::fromBase64("rGUNrSAoldAjUhZby9kLyh4p6ZKsQgleulLuuTlcnfspM5Rom5AN3xh6a+CdKq5tMNXhHkbqk55OgqRD0pyhKQ==");
        const QByteArray signature=QByteArray::fromBase64("0xylcsDMKZ7jNOvjMdpzCzfDmPA+udnjx93wk4BAEbswyZuqkCVdqbByrYkBQODoHBH8ulWjKnwf7Ff/gDRdRA==");
        const QByteArray data=R"({"schema":1,"product":"zerus","platform":"windows","channel":"stable","version":"0.45.0"})";
        QCOMPARE(key.size(),64);QCOMPARE(signature.size(),64);
        QByteArray tampered=data;tampered.replace("0.45.0","0.45.1");
        QByteArray forged=signature;forged[63]=char(forged[63]^1);
        QByteArray otherKey=key;otherKey[10]=char(otherKey[10]^1);
        QVERIFY(!WindowsUpdateTrust::verifyManifest(tampered,signature,key));QVERIFY(!WindowsUpdateTrust::verifyManifest(data,forged,key));
        QVERIFY(!WindowsUpdateTrust::verifyManifest(data,signature,otherKey));QVERIFY(!WindowsUpdateTrust::verifyManifest(data,signature.left(63),key));
        QVERIFY(!WindowsUpdateTrust::verifyManifest(data,signature,key.left(32)));QVERIFY(!WindowsUpdateTrust::verifyManifest(data,{},{}));
        // An unsigned file, such as this suite, and a missing one have no publisher.
        QVERIFY(WindowsUpdateTrust::authenticodePublisher(QCoreApplication::applicationFilePath()).isEmpty());
        QVERIFY(WindowsUpdateTrust::authenticodePublisher(QDir::temp().filePath("zerus-missing-installer.exe")).isEmpty());
#ifdef Q_OS_WIN
        QVERIFY(WindowsUpdateTrust::verifyManifest(data,signature,key));
#else
        // CNG exists on Windows only; elsewhere nothing verifies.
        QVERIFY(!WindowsUpdateTrust::verifyManifest(data,signature,key));
#endif
    }
    void portableBuildsNeverTakeDevBuilds(){
        // Dev builds carry a source and an Arch package, never a Windows zip.
        DesktopUpdateFeed dev;dev.valid=true;dev.buildNumber=50;dev.version="0.42.0 (dev 50)";
        DesktopInstallation portable;portable.kind="portable";DesktopInstallation source;source.kind="source";
        QVERIFY(UpdateController::comparisonMessage(portable,dev,"0.39.0",0,false).contains("stable or nightly channel"));
        QVERIFY(UpdateController::comparisonMessage(source,dev,"0.39.0",0,false).contains("main build 50"));
    }
    void wingetPortableInstallationsUpdateThroughWinget(){
        // winget extracts the stable zip under WinGet\Packages\<identifier>_<source>, per user or machine-wide.
        for(const QString &path:{QString("C:\\Users\\example\\AppData\\Local\\Microsoft\\WinGet\\Packages\\ufna.Zerus_Microsoft.Winget.Source_8wekyb3d8bbwe\\zerus-windows-x86_64\\hgs-tray.exe"),
                                 QString("C:/Program Files/WinGet/Packages/ufna.Zerus_Microsoft.Winget.Source_8wekyb3d8bbwe/zerus-windows-x86_64/hgs-tray.exe"),
                                 QString("D:\\winget\\packages\\UFNA.ZERUS_DefaultSource\\zerus-windows-x86_64\\hgs-tray.exe")})
            QCOMPARE(UpdateController::wingetPackage(path),QString("ufna.Zerus"));
        for(const QString &path:{QString("C:\\Users\\example\\Downloads\\zerus-windows-x86_64\\hgs-tray.exe"),
                                 QString("C:\\Tools\\ufna.Zerus_Microsoft.Winget.Source_8wekyb3d8bbwe\\hgs-tray.exe"),
                                 QString("C:\\WinGet\\Packages\\ufna.ZerusExtra_Microsoft.Winget.Source\\hgs-tray.exe"),
                                 QString("C:\\WinGet\\Packages\\ufna.Zerus_\\hgs-tray.exe"),
                                 QString("C:\\WinGet\\Packages\\other.App_Microsoft.Winget.Source\\hgs-tray.exe"),
                                 QString("C:\\Packages\\WinGet\\ufna.Zerus_Microsoft.Winget.Source\\hgs-tray.exe"),QString()})
            QVERIFY2(UpdateController::wingetPackage(path).isEmpty(),qPrintable(path));
        auto stable=parsedFeed(QJsonDocument(stableRaw("0.42.3")).toJson(),"stable");QVERIFY(stable.valid);
        auto nightly=parsedFeed(QJsonDocument(nightlyRaw("0.43.0.r10.gaaaaaaa.n5")).toJson(),"nightly");QVERIFY(nightly.valid);
        DesktopInstallation winget{"portable","ufna.Zerus",{}},portable{"portable",{},{}};
        // winget carries stable releases only: the command exists on Stable and nowhere else.
        QCOMPARE(UpdateController::updateCommand(winget,"stable"),QString("winget upgrade --id ufna.Zerus --exact"));
        QVERIFY(UpdateController::updateCommand(winget,"nightly").isEmpty());QVERIFY(UpdateController::updateCommand(winget,"dev").isEmpty());
        QVERIFY(UpdateController::updateCommand(portable,"stable").isEmpty());
        QVERIFY(UpdateController::updateCommand(DesktopInstallation{"portable","other.App",{}},"stable").isEmpty());
        auto guide=UpdateController::installationGuide(winget,"stable",stable);
        QVERIFY(guide.ownership.startsWith("winget package ufna.Zerus\nPortable Windows build "));
        QCOMPARE(guide.command,QString("winget: winget upgrade --id ufna.Zerus --exact"));QCOMPARE(guide.copyName,QString("Copy winget command"));
        QVERIFY(guide.instructions.contains("Quit Zerus"));QVERIFY(guide.instructions.contains("keep running"));
        guide=UpdateController::installationGuide(winget,"nightly",nightly);
        QVERIFY(guide.command.isEmpty());QVERIFY(guide.instructions.contains("stable releases only"));
        guide=UpdateController::installationGuide(portable,"stable",stable);
        QVERIFY(guide.command.isEmpty());QVERIFY(guide.instructions.isEmpty());QVERIFY(guide.ownership.contains("Download newer builds from the release page"));
        QCOMPARE(UpdateController::comparisonMessage(winget,stable,"0.42.2",0,false),QString("Zerus 0.42.3 is available. Quit Zerus, then update it with winget."));
        QCOMPARE(UpdateController::comparisonMessage(portable,stable,"0.42.2",0,false),QString("Zerus 0.42.3 is available. Download its Windows zip from the release page."));
        // A winget installation following nightlies downloads nightly zips like any portable build.
        QVERIFY(UpdateController::comparisonMessage(winget,nightly,"0.42.3",0,false).contains("Download its Windows zip"));
#ifdef Q_OS_WIN
        QTemporaryDir dir;QVERIFY(dir.isValid());const QString folder="WinGet/Packages/ufna.Zerus_Microsoft.Winget.Source_8wekyb3d8bbwe/zerus-windows-x86_64";
        QVERIFY(QDir(dir.path()).mkpath(folder));
        for(const char *file:{"hgs-tray.exe","build-info.json"}){QFile created(dir.filePath(folder+"/"+file));QVERIFY(created.open(QIODevice::WriteOnly));}
        UpdateController installed(nullptr,dir.filePath(folder+"/hgs-tray.exe"),[](const QNetworkRequest &){return new FixtureReply;});
        QTRY_COMPARE(installed.installation().kind,QString("portable"));QCOMPARE(installed.installation().package,QString("ufna.Zerus"));
#endif
    }
    void installationKindFollowsTheFolderLayout(){
        QTemporaryDir dir;QVERIFY(dir.isValid());
#ifdef Q_OS_WIN
        // Wherever the temporary folder lies: the portable bundle (build-info.json
        // next to hgs-tray.exe) stays portable inside a folder kept in Git, such as
        // a profile, while a build tree in that checkout is a source build.
        for(const char *folder:{"checkout/.git","checkout/bundle","checkout/build"})QVERIFY(QDir(dir.path()).mkpath(folder));
        for(const char *file:{"checkout/bundle/hgs-tray.exe","checkout/bundle/build-info.json","checkout/build/hgs-tray.exe"}){QFile created(dir.filePath(file));QVERIFY(created.open(QIODevice::WriteOnly));}
        UpdateController bundle(nullptr,dir.filePath("checkout/bundle/hgs-tray.exe"),[](const QNetworkRequest &){return new FixtureReply;});
        UpdateController build(nullptr,dir.filePath("checkout/build/hgs-tray.exe"),[](const QNetworkRequest &){return new FixtureReply;});
        QTRY_COMPARE(bundle.installation().kind,QString("portable"));QTRY_COMPARE(build.installation().kind,QString("source"));
#else
        QFile executable(dir.path()+"/hgs-tray.exe");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();
        UpdateController controller(nullptr,executable.fileName(),[](const QNetworkRequest &){return new FixtureReply;});
#ifdef Q_OS_LINUX
        QTRY_VERIFY(controller.installation().kind!="checking");QVERIFY(controller.installation().kind!="portable");
#else
        QTRY_COMPARE(controller.installation().kind,QString("unknown"));
#endif
#endif
    }
    void homebrewCaskOwnership(){
#ifdef Q_OS_WIN
        QSKIP("Homebrew ownership uses Unix symlinks; QFile::link creates shortcuts on Windows");
#endif
        QTemporaryDir dir;QVERIFY(dir.isValid());const QString exe=dir.path()+"/Applications/Zerus.app/Contents/MacOS/hgs-tray";QVERIFY(QDir().mkpath(QFileInfo(exe).path()));
        const QString brew=dir.path()+"/brew",nightly="0.38.0.r812.gabcdef0.n41";
        auto owner=[&](const QString &path,const QStringList &prefixes){return UpdateController::homebrewInstallation(path,prefixes);};
        QCOMPARE(owner(exe,{brew}).kind,QString());
        QVERIFY(QDir().mkpath(brew+"/Caskroom/zerus/0.38.0"));QVERIFY(QDir().mkpath(brew+"/Caskroom/zerus/.metadata"));
        // Merely having this cask installed must not claim another copy of the app.
        QCOMPARE(owner(exe,{brew}).kind,QString());
        QVERIFY(QFile::link(dir.path()+"/Applications/Zerus.app",brew+"/Caskroom/zerus/0.38.0/Zerus.app"));
        auto stable=owner(exe,{dir.path()+"/missing",brew});QCOMPARE(stable.kind,QString("homebrew-cask"));QCOMPARE(stable.package,QString("zerus"));QCOMPARE(stable.version,QString("0.38.0"));
        QCOMPARE(owner(dir.path()+"/Applications/hgs-tray.app/Contents/MacOS/hgs-tray",{brew}).kind,QString());
        QVERIFY(QDir().mkpath(brew+"/Caskroom/zerus/0.38.1"));QCOMPARE(owner(exe,{brew}).kind,QString());
        QVERIFY(QDir(brew+"/Caskroom/zerus").removeRecursively());
        QVERIFY(QDir().mkpath(brew+"/Caskroom/zerus@nightly/"+nightly));
        QVERIFY(QFile::link(dir.path()+"/Applications/Zerus.app",brew+"/Caskroom/zerus@nightly/"+nightly+"/Zerus.app"));
        auto daily=owner(exe,{brew});QCOMPARE(daily.kind,QString("homebrew-cask"));QCOMPARE(daily.package,QString("zerus@nightly"));QCOMPARE(daily.version,nightly);
        // Conflicting casks cannot both own the application; the owner stays unknown.
        QVERIFY(QDir().mkpath(brew+"/Caskroom/zerus/0.38.0"));QCOMPARE(owner(exe,{brew}).kind,QString());
        QVERIFY(QDir(brew+"/Caskroom/zerus").removeRecursively());QVERIFY(QDir().mkpath(brew+"/Caskroom/zerus/"+nightly));QCOMPARE(owner(exe,{brew}).kind,QString());
        QVERIFY(UpdateController::prefersNightly({"homebrew-cask","zerus@nightly",nightly}));QVERIFY(UpdateController::prefersNightly({"package","zerus-ade-nightly-bin","1"}));
        QVERIFY(!UpdateController::prefersNightly({"homebrew-cask","zerus","0.38.0"}));
        QVERIFY(QDir(brew+"/Caskroom").removeRecursively());
        const QString dev=brew+"/Caskroom/zerus@dev/0.39.0-dev.50";QVERIFY(QDir().mkpath(dev));
        QVERIFY(QFile::link(dir.path()+"/Applications/Zerus.app",dev+"/Zerus.app"));
        QCOMPARE(owner(exe,{brew}).package,QString("zerus@dev"));QCOMPARE(owner(exe,{brew}).version,QString("0.39.0-dev.50"));
        QVERIFY(QDir().mkpath(dir.path()+"/Other/Zerus.app/Contents/MacOS"));
        QCOMPARE(owner(dir.path()+"/Other/Zerus.app/Contents/MacOS/hgs-tray",{brew}).kind,QString());
    }
    void homebrewCaskInstructions(){
        const DesktopInstallation stable{"homebrew-cask","zerus","0.36.0"},nightly{"homebrew-cask","zerus@nightly","0.37.0.r60.gaaaaaaa.n7"};
        QCOMPARE(UpdateController::updateCommand(stable),QString("brew upgrade --cask zerus"));
        QCOMPARE(UpdateController::updateCommand(nightly,"nightly"),QString("brew upgrade --cask zerus@nightly"));
        QCOMPARE(UpdateController::updateCommand(stable,"nightly"),QString("brew uninstall --cask zerus && brew install --cask ufna/zerus/zerus@nightly"));
        QCOMPARE(UpdateController::updateCommand(nightly,"stable"),QString("brew uninstall --cask zerus@nightly && brew install --cask ufna/zerus/zerus"));
        QCOMPARE(UpdateController::updateCommand(stable,"dev"),QString("brew uninstall --cask zerus && brew install --cask ufna/zerus/zerus@dev"));
        QVERIFY(UpdateController::updateCommand({"homebrew-cask","zerus; sudo command","0.36.0"}).isEmpty());
        auto catalog=parsedFeed(QJsonDocument(withMacos(raw())).toJson(),"stable");
        QVERIFY(UpdateController::comparisonMessage(stable,catalog,"0.36.0",0,false).contains("brew upgrade --cask zerus"));
        auto current=stable;current.version="0.37.0";QVERIFY(UpdateController::comparisonMessage(current,catalog,"0.37.0",0,false).contains("at least as new"));
        QVERIFY(UpdateController::comparisonMessage(nightly,catalog,"0.37.0",0,false).contains("brew install --cask ufna/zerus/zerus"));
        auto daily=parsedFeed(QJsonDocument(withMacos(nightlyRaw("0.37.0.r61.gbbbbbbb.n8"))).toJson(),"nightly");QVERIFY(daily.valid);
        QVERIFY(UpdateController::comparisonMessage(nightly,daily,"0.37.0",0,false).contains("brew upgrade --cask zerus@nightly"));
        auto newest=nightly;newest.version="0.37.0.r61.gbbbbbbb.n8";QVERIFY(UpdateController::comparisonMessage(newest,daily,"0.37.0",0,false).contains("at least as new"));
        QVERIFY(UpdateController::comparisonMessage(stable,daily,"0.37.0",0,false).contains("ufna/zerus/zerus@nightly"));
        auto dev=devRaw();dev["versionName"]="0.38.0-dev.50";
        auto development=parsedFeed(QJsonDocument(withMacos(dev)).toJson(),"dev");
        QVERIFY(UpdateController::comparisonMessage(stable,development,"0.37.0",0,false).contains("zerus@dev"));
    }
    void installationGuidesNameTheirPackageManager(){
        const auto feed=parsedFeed(QJsonDocument(withMacos(nightlyRaw("0.37.0.r60.gaaaaaaa.n7"))).toJson(),"nightly");
        auto cask=UpdateController::installationGuide({"homebrew-cask","zerus@nightly","0.37.0.r60.gaaaaaaa.n7"},"nightly",feed);
        QVERIFY(cask.ownership.contains("Homebrew cask: zerus@nightly"));QVERIFY(cask.ownership.contains("0.37.0.r60.gaaaaaaa.n7"));
        QCOMPARE(cask.command,QString("Homebrew: brew upgrade --cask zerus@nightly"));
        QVERIFY(!cask.command.contains("AUR"));QVERIFY(!cask.instructions.contains("AUR"));QVERIFY(cask.instructions.contains("Restart to finish update"));
        auto dev=UpdateController::installationGuide({"homebrew-cask","zerus","0.37.0"},"dev");QVERIFY(dev.command.isEmpty());QVERIFY(dev.instructions.contains("Check for updates"));
        DesktopInstallation package{"package","zerus-ade-bin","0.36.2-1",true,"paru"};auto arch=UpdateController::installationGuide(package,"stable");
        QCOMPARE(arch.command,QString("Full system update: paru -Syu zerus-ade-bin"));QVERIFY(arch.ownership.contains("Arch-based"));QVERIFY(arch.instructions.contains("AUR helper"));
        DesktopInstallation local{"unknown",{},{},true,"yay"};auto optional=UpdateController::installationGuide(local,"stable");
        QCOMPARE(optional.command,QString("Optional AUR installation: yay -Syu zerus-ade-bin"));QCOMPARE(optional.copyName,QString("Copy optional installation command"));
        auto build=UpdateController::installationGuide({"source",{},{}},"stable");QVERIFY(build.command.isEmpty());QVERIFY(build.instructions.isEmpty());QVERIFY(build.ownership.contains("Git checkout"));
        auto portable=UpdateController::installationGuide({"portable",{},{}},"nightly");QVERIFY(portable.ownership.contains("Portable Windows build"));QVERIFY(portable.command.isEmpty());QVERIFY(portable.instructions.isEmpty());
    }
    void macosCatalogOriginsAndBuildSequences(){
        auto dev=devRaw();dev["versionName"]="0.38.0-dev.50";
        for(auto value:{withMacos(raw()),withMacos(nightlyRaw("0.37.0.r61.gbbbbbbb.n8")),withMacos(dev)}){
            const auto channel=value["channel"].toString();
            const auto feed=parsedFeed(QJsonDocument(value).toJson(),channel);QVERIFY(feed.valid);
            QVERIFY(feed.macosDmgUrl.endsWith("-macos-arm64.dmg"));
            QCOMPARE(UpdateController::downloadUrl({"macos-bundle",{},"0.37.0"},feed,"macos"),QUrl(feed.macosDmgUrl));
            QVERIFY(UpdateController::downloadUrl({"portable",{},{}},feed,"windows").isEmpty());
            for(const auto &key:{"url","name","size","sha256"}){
                auto bad=value;auto mac=bad["macos"].toObject();auto rows=mac["downloads"].toArray();auto row=rows[1].toObject();row[key]=QString("https://example.invalid/foreign.dmg");rows[1]=row;mac["downloads"]=rows;bad["macos"]=mac;
                QVERIFY2(!parsedFeed(QJsonDocument(bad).toJson(),channel).valid,key);
            }
        }
        auto feed=parsedFeed(QJsonDocument(withMacos(dev)).toJson(),"dev");
        for(const auto &kind:{"homebrew-cask","macos-bundle"}){
            DesktopInstallation installed{kind,"zerus@dev","0.38.0-dev.49"};
            QVERIFY(UpdateController::macosUpdateAvailable(installed,feed));
            installed.version="0.38.0-dev.50";QVERIFY(!UpdateController::macosUpdateAvailable(installed,feed));
            installed.version="0.38.0-dev.51";QVERIFY(!UpdateController::macosUpdateAvailable(installed,feed));
            installed.version="0.37.0";QVERIFY(!UpdateController::macosUpdateAvailable(installed,feed));
        }
        const DesktopInstallation manual{"macos-bundle",{},"0.38.0-dev.49"};
        auto guide=UpdateController::installationGuide(manual,"dev",feed);
        QVERIFY(guide.command.isEmpty());QVERIFY(guide.instructions.contains("DMG"));QVERIFY(guide.instructions.contains("Quit Zerus"));QVERIFY(guide.instructions.contains("open Zerus again"));
        const auto noMac=parsedFeed(QJsonDocument(raw()).toJson(),"stable");
        for(const auto &installation:{manual,DesktopInstallation{"homebrew-cask","zerus@dev","0.38.0-dev.49"}}){
            QVERIFY(!UpdateController::macosUpdateAvailable(installation,noMac));
            QVERIFY(UpdateController::downloadUrl(installation,noMac,"macos").isEmpty());
            guide=UpdateController::installationGuide(installation,"stable",noMac);
            QVERIFY(guide.command.isEmpty());QVERIFY(guide.instructions.contains("No macOS build"));
        }
    }
    void downloadedMacBundleIdentityAndChannel(){
        QTemporaryDir dir;const QString contents=dir.path()+"/Zerus.app/Contents",executable=contents+"/MacOS/hgs-tray";QVERIFY(QDir().mkpath(contents+"/MacOS"));
        {QFile file(executable);QVERIFY(file.open(QIODevice::WriteOnly));}
        auto write=[&](const QString &release,const QString &identifier=QStringLiteral("com.hgdev.hgs-tray")){
            QFile file(contents+"/Info.plist");if(!file.open(QIODevice::WriteOnly))return false;
            file.write(QString("<plist><dict><key>CFBundleIdentifier</key><string>%1</string><key>CFBundleExecutable</key><string>hgs-tray</string><key>CFBundleName</key><string>Zerus</string><key>CFBundleShortVersionString</key><string>0.37.0</string><key>ZerusReleaseVersion</key><string>%2</string></dict></plist>").arg(identifier,release).toUtf8());return true;
        };
        for(const auto &release:{"0.37.0","0.37.0.r61.gbbbbbbb.n8","0.38.0-dev.50"}){
            QVERIFY(write(release));const auto installation=UpdateController::macosBundleInstallation(executable);
            QCOMPARE(installation.kind,QString("macos-bundle"));QCOMPARE(installation.version,QString(release));
            QCOMPARE(UpdateController::preferredChannel(installation),UpdateController::releaseChannel(release));
        }
        for(const auto &release:{"../app","0.38.0","0.36.0-dev.50","0.38.0.r61.gbbbbbbb.n8"}){
            QVERIFY(write(release));QVERIFY(UpdateController::macosBundleInstallation(executable).kind.isEmpty());
        }
        QVERIFY(write("0.38.0-dev.50","app.example.foreign"));QVERIFY(UpdateController::macosBundleInstallation(executable).kind.isEmpty());
#ifdef Q_OS_MACOS
        QVERIFY(write("0.38.0-dev.50"));QSettings().setValue("updates/automatic",false);
        auto value=devRaw();value["versionName"]="0.38.0-dev.51";value["buildNumber"]=51;const auto bytes=QJsonDocument(withMacos(value)).toJson();
        auto fetch=[bytes](const QNetworkRequest &){auto *reply=new FixtureReply;QTimer::singleShot(0,reply,[reply,bytes]{reply->finish(bytes);});return reply;};
        UpdateController controller(nullptr,executable,fetch);QTRY_COMPARE(controller.installation().kind,QString("macos-bundle"));
        QCOMPARE(controller.channel(),QString("dev"));QCOMPARE(controller.installedDisplayVersion(),QString("0.38.0-dev.50"));
        UpdatesWidget widget(nullptr,&controller);UrlRecorder browser;controller.check();QTRY_VERIFY(controller.feed().valid);
        QVERIFY(controller.updateAvailable());auto *download=widget.findChild<QPushButton*>("updatesDownload");QVERIFY(download);QVERIFY(!download->isHidden());QCOMPARE(download->text(),QString("Download DMG"));
        download->click();QCOMPARE(browser.urls,QList<QUrl>{QUrl(controller.feed().macosDmgUrl)});
        controller.setChannel("stable");QCOMPARE(controller.installedDisplayVersion(),QString("0.38.0-dev.50"));
        UpdateController restored(nullptr,executable,fetch);QTRY_COMPARE(restored.installation().kind,QString("macos-bundle"));QCOMPARE(restored.channel(),QString("stable"));
        QVERIFY(write("0.38.0-dev.51"));controller.refreshBundleState();QVERIFY(controller.restartRequired());
#endif
    }
    void installationInstructionsOpenThePlatformSection(){
        // README.md has no Installation section any more: the guide is docs/installation.md.
        const QString guide="https://github.com/ufna/zerus/blob/main/docs/installation.md";
        const DesktopInstallation package{"package","zerus-ade-bin","0.36.2-1",true,"paru"},source{"source",{},{}},cask{"homebrew-cask","zerus","0.37.0"},portable{"portable",{},{}};
        auto archSource=source;archSource.arch=true;
        QCOMPARE(UpdateController::installationInstructionsUrl(package,"linux"),QUrl(guide+"#arch-linux"));
        QCOMPARE(UpdateController::installationInstructionsUrl(archSource,"linux"),QUrl(guide+"#arch-linux"));
        QCOMPARE(UpdateController::installationInstructionsUrl(source,"linux"),QUrl(guide+"#local-installation"));
        QCOMPARE(UpdateController::installationInstructionsUrl(cask,"macos"),QUrl(guide+"#macos-homebrew"));
        QCOMPARE(UpdateController::installationInstructionsUrl(source,"macos"),QUrl(guide+"#macos-homebrew"));
        QCOMPARE(UpdateController::installationInstructionsUrl(portable,"windows"),QUrl(guide+"#windows"));
        QCOMPARE(UpdateController::installationInstructionsUrl(source,"windows"),QUrl(guide+"#windows"));
        QCOMPARE(UpdateController::installationInstructionsUrl(source,"freebsd"),QUrl(guide));
#if defined(Q_OS_MACOS)
        QCOMPARE(UpdateController::runningPlatform(),QString("macos"));
#elif defined(Q_OS_WIN)
        QCOMPARE(UpdateController::runningPlatform(),QString("windows"));
#elif defined(Q_OS_LINUX)
        QCOMPARE(UpdateController::runningPlatform(),QString("linux"));
#endif
        QCOMPARE(UpdateController::installationInstructionsUrl(archSource),UpdateController::installationInstructionsUrl(archSource,UpdateController::runningPlatform()));
        // Every section the button can open is a heading of the guide in this checkout.
        const auto anchors=markdownAnchors(QStringLiteral(ZERUS_INSTALLATION_GUIDE));
        QVERIFY2(anchors.contains("checking-for-updates"),ZERUS_INSTALLATION_GUIDE);
        for(const char *platform:{"linux","macos","windows"})for(bool arch:{false,true}){
            DesktopInstallation installation{"unknown",{},{}};installation.arch=arch;
            const QString section=UpdateController::installationInstructionsUrl(installation,platform).fragment();
            QVERIFY2(anchors.contains(section),qPrintable(QString("%1 opens the missing section #%2").arg(platform,section)));
        }
    }
    void installationInstructionsButtonOpensThePlatformSection(){
        QSettings().setValue("updates/automatic",false);
        auto *controller=UpdateController::instance();
        // The shared controller settles its ownership in this test, not in a later one.
        QTRY_VERIFY_WITH_TIMEOUT(controller->installation().kind!="checking",15000);
        UrlRecorder browser;UpdatesWidget widget;
        auto *details=widget.findChild<QPushButton *>("updatesDetailsToggle");QVERIFY(details);details->click();
        auto *instructions=widget.findChild<QPushButton *>("updatesInstructions");QVERIFY(instructions);
        QCOMPARE(instructions->text(),QString("Installation instructions"));
        instructions->click();
        QUrl expected("https://github.com/ufna/zerus/blob/main/docs/installation.md");
#if defined(Q_OS_MACOS)
        expected.setFragment("macos-homebrew");
#elif defined(Q_OS_WIN)
        expected.setFragment("windows");
#elif defined(Q_OS_LINUX)
        expected.setFragment(controller->installation().arch?"arch-linux":"local-installation");
#endif
        QCOMPARE(browser.urls,QList<QUrl>{expected});
    }
    void nightlyBuildsCompareByBuildNumber(){
        QVERIFY(UpdateController::nightlyIsNewer("0.38.0.r812.gabcdef0.n41","0.38.0.r815.g1234567.n42"));
        QVERIFY(UpdateController::nightlyIsNewer("0.38.0.r812.gabcdef0.n9","0.38.0.r815.g1234567.n10"));
        QVERIFY(!UpdateController::nightlyIsNewer("0.38.0.r815.g1234567.n42","0.38.0.r812.gabcdef0.n41"));
        QVERIFY(!UpdateController::nightlyIsNewer("0.38.0.r812.gabcdef0.n41","0.38.0.r812.gabcdef0.n41"));
        QVERIFY(!UpdateController::nightlyIsNewer("0.38.0","0.38.0.r815.g1234567.n42"));
        QVERIFY(!UpdateController::nightlyIsNewer("0.38.0.r812.gabcdef0.n41","0.39.0"));
    }
    void replacedBundleRequestsRestart(){
        QTemporaryDir dir;const QString contents=dir.path()+"/Zerus.app/Contents";QVERIFY(QDir().mkpath(contents+"/MacOS"));
        auto plist=[&](const QByteArray &body){QFile file(contents+"/Info.plist");QVERIFY(file.open(QIODevice::WriteOnly|QIODevice::Truncate));file.write(body);};
        plist(R"(<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleName</key><string>Zerus</string><key>CFBundleShortVersionString</key><string>0.38.1</string></dict></plist>)");
        QCOMPARE(UpdateController::bundleRelease(contents+"/Info.plist"),QString("0.38.1"));
        QVERIFY(UpdateController::bundleReplaced(contents+"/MacOS/hgs-tray","0.38.0"));QVERIFY(!UpdateController::bundleReplaced(contents+"/MacOS/hgs-tray","0.38.1"));
        // A nightly keeps its product version; its release version tells builds apart.
        plist("<plist><dict><key>CFBundleShortVersionString</key><string>0.38.1</string><key>ZerusReleaseVersion</key><string>0.38.1.r812.gabcdef0.n41</string></dict></plist>");
        QCOMPARE(UpdateController::bundleRelease(contents+"/Info.plist"),QString("0.38.1.r812.gabcdef0.n41"));
        QVERIFY(UpdateController::bundleReplaced(contents+"/MacOS/hgs-tray","0.38.1.r800.g1234567.n40"));
        QVERIFY(!UpdateController::bundleReplaced(contents+"/MacOS/hgs-tray","0.38.1.r812.gabcdef0.n41"));
        plist("<plist><dict><key>ZerusReleaseVersion</key><string>../evil</string><key>CFBundleShortVersionString</key><string>0.38.1</string></dict></plist>");QCOMPARE(UpdateController::bundleRelease(contents+"/Info.plist"),QString("0.38.1"));
        plist("<plist><dict><key>CFBundleName</key><string>Zerus</string></dict></plist>");QVERIFY(UpdateController::bundleRelease(contents+"/Info.plist").isEmpty());QVERIFY(!UpdateController::bundleReplaced(contents+"/MacOS/hgs-tray","0.38.0"));
        plist("<plist><dict><key>CFBundleShortVersionString</key>");QVERIFY(UpdateController::bundleRelease(contents+"/Info.plist").isEmpty());
        QVERIFY(!UpdateController::bundleReplaced(contents+"/MacOS/hgs-tray",{}));
        QVERIFY(!UpdateController::bundleReplaced(dir.path()+"/plain/hgs-tray","0.38.0"));
    }
    void controllerOffersRestartAfterBundleReplacement(){
#ifdef Q_OS_MACOS
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);
        const QString contents=dir.path()+"/Zerus.app/Contents";QVERIFY(QDir().mkpath(contents+"/MacOS"));
        auto plist=[&](const QByteArray &release){QFile file(contents+"/Info.plist");QVERIFY(file.open(QIODevice::WriteOnly|QIODevice::Truncate));file.write("<plist><dict><key>CFBundleShortVersionString</key><string>0.38.0</string><key>ZerusReleaseVersion</key><string>"+release+"</string></dict></plist>");};
        plist("0.38.0.r812.gabcdef0.n41");
        UpdateController controller(nullptr,contents+"/MacOS/hgs-tray");QTRY_COMPARE(controller.installation().kind,QString("unknown"));QVERIFY(!controller.restartRequired());
        QSignalSpy changed(&controller,&UpdateController::changed);
        plist("0.38.0.r815.g1234567.n42");controller.refreshBundleState();QVERIFY(controller.restartRequired());QVERIFY(changed.count()>0);
#endif
    }
    void restartHelperIsDetachedWithOnlyStdio(){
#ifndef Q_OS_UNIX
        QSKIP("The restart helper reopens a macOS bundle");
#else
        if(!ChildProcess::inheritsStdioOnly)QSKIP("This Qt cannot close inherited descriptors");
        // In place of /bin/sh, record what the helper gets instead of reopening a bundle.
        QTemporaryDir dir;QVERIFY(dir.isValid());const QString shell=dir.path()+"/sh",bundle=dir.path()+"/Zerus.app";
        tool(shell,"#!/bin/sh\nout=$(dirname \"$0\")\nprintf '%s\\n' \"$@\" > \"$out/arguments\"\necho \"$PPID\" > \"$out/parent\"\nls /dev/fd > \"$out/fds.part\" && mv \"$out/fds.part\" \"$out/fds\"\n");
        // The GUI may be quitting while ProcessRunner creates a request's pipe; the helper
        // then waits for it to exit and must not keep that request's input open meanwhile.
        const CreatingPipe creating;QVERIFY(creating.valid());const qint64 pid=QCoreApplication::applicationPid();
        QVERIFY(UpdateController::startRestartHelper(bundle,pid,shell));
        QTRY_VERIFY(QFileInfo::exists(dir.path()+"/fds"));
        QFile arguments(dir.path()+"/arguments");QVERIFY(arguments.open(QIODevice::ReadOnly));
        QCOMPARE(QString::fromUtf8(arguments.readAll()).split('\n',Qt::SkipEmptyParts),QStringList({"-c","while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open -a \"$2\" --args --sessions","zerus-restart",QString::number(pid),bundle}));
        // Detached: not a child of this process, so it outlives it.
        QFile parent(dir.path()+"/parent");QVERIFY(parent.open(QIODevice::ReadOnly));const qint64 parentPid=parent.readAll().trimmed().toLongLong();
        QVERIFY(parentPid>0);QVERIFY(parentPid!=pid);
        const auto open=descriptors(dir.path()+"/fds");const auto shown=QDebug::toString(open).toLatin1();
        QVERIFY2(open.contains(1),shown.constData());for(int fd:creating.ends)QVERIFY2(!open.contains(fd),shown.constData());
#endif
    }
    void oldLoginItemIsRecognizedByItsPriorityLimit(){
        const QString label="com.hgdev.hgs-tray";
        // Measured under launchd: no ProcessType (or Standard) stops every thread at 20,
        // Background and Adaptive at 4; Interactive, like an app started by Finder, at 63.
        QVERIFY(UpdateController::launchedDeprioritized(20,label));
        QVERIFY(UpdateController::launchedDeprioritized(4,label));
        QVERIFY(!UpdateController::launchedDeprioritized(63,label));
        QVERIFY(!UpdateController::launchedDeprioritized(31,label));
        QVERIFY(!UpdateController::launchedDeprioritized(-1,label));
        // Only Zerus's own login item: LaunchServices names apps "application.<id>.<n>.<n>".
        QVERIFY(!UpdateController::launchedDeprioritized(20,"application.com.hgdev.hgs-tray.1234.5678"));
        QVERIFY(!UpdateController::launchedDeprioritized(20,{}));
#ifdef Q_OS_MACOS
        QVERIFY(UpdateController::threadMaxPriority()>0);
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);
        const auto restore=qScopeGuard([]{UpdateController::setLaunchForTests(-1);});
        const QString executable=dir.path()+"/Zerus.app/Contents/MacOS/hgs-tray";
        UpdateController::setLaunchForTests(20,label);
        UpdateController clamped(nullptr,executable);QVERIFY(clamped.loginItemDeprioritized());
        UpdateController::setLaunchForTests(63,label);
        UpdateController interactive(nullptr,executable);QVERIFY(!interactive.loginItemDeprioritized());
#else
        QCOMPARE(UpdateController::threadMaxPriority(),-1);
#endif
    }
    void loginItemUpdateOnlyRunsTheBundledSetup(){
#ifndef Q_OS_UNIX
        QSKIP("zerus-setup is part of the macOS bundle");
#else
        QTemporaryDir dir;QVERIFY(dir.isValid());QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);
        const QString contents=dir.path()+"/Zerus.app/Contents",setup=contents+"/Resources/bin/zerus-setup";
        QVERIFY(QDir().mkpath(contents+"/MacOS"));QVERIFY(QDir().mkpath(contents+"/Resources/bin"));
        UpdateController controller(nullptr,contents+"/MacOS/hgs-tray");
        // A source installation has no zerus-setup: say what to run instead.
        QVERIFY(controller.loginItemSetup().isEmpty());
        QString result=QStringLiteral("pending");controller.updateLoginItem([&](const QString &error){result=error;});
        QVERIFY2(result.contains("tray/install.sh"),qPrintable(result));
        tool(setup,"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/arguments\"\n");
        QCOMPARE(controller.loginItemSetup(),setup);
        result=QStringLiteral("pending");controller.updateLoginItem([&](const QString &error){result=error;});
        QTRY_COMPARE(result,QString());
        QFile arguments(contents+"/Resources/bin/arguments");QVERIFY(arguments.open(QIODevice::ReadOnly));QCOMPARE(arguments.readAll(),QByteArray("--autostart\n"));
        // A refusal reaches the person in zerus-setup's own words, from its stderr.
        QVERIFY(QFile::remove(setup));tool(setup,"#!/bin/sh\necho 'zerus-setup: the login item belongs to another installation' >&2\nexit 2\n");
        result=QStringLiteral("pending");controller.updateLoginItem([&](const QString &error){result=error;});
        QTRY_VERIFY(result!="pending");QVERIFY2(result.contains("belongs to another installation"),qPrintable(result));
#endif
    }
    void canonicalOwnershipAsync(){
#ifdef Q_OS_LINUX
        QTemporaryDir dir;QVERIFY(dir.isValid());QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();auto oldPath=qgetenv("PATH");qputenv("PATH",dir.path().toUtf8()+":"+oldPath);qputenv("ZERUS_TEST_LOG",(dir.path()+"/args.json").toUtf8());
        // The owner query answers only after the test has seen it start, which a probe
        // that held the event loop would prevent until its deadline failed the query.
        tool(dir.path()+"/pacman","#!/usr/bin/python3\nimport os,sys,json,time\nif sys.argv[1]=='-Qqo':\n open(os.environ['ZERUS_TEST_LOG'],'w').write(json.dumps(sys.argv[2:]))\n start=time.time()\n while not os.path.exists(os.environ['ZERUS_TEST_LOG']+'.release') and time.time()-start<20: time.sleep(0.01)\n print('zerus-ade-nightly-bin')\nelse: print('zerus-ade-nightly-bin 0.37.0.r100.gaaaaaaa.n1-1')\n");
        QFile binary(dir.path()+"/actual-gui");QVERIFY(binary.open(QIODevice::WriteOnly));binary.close();QVERIFY(QFile::link(binary.fileName(),dir.path()+"/gui-link"));
        UpdateController controller(nullptr,dir.path()+"/gui-link");QSignalSpy changes(&controller,&UpdateController::changed);
        QTRY_VERIFY(QFileInfo::exists(dir.path()+"/args.json"));QCOMPARE(controller.installation().kind,QString("checking"));QFile release(dir.path()+"/args.json.release");QVERIFY(release.open(QIODevice::WriteOnly));release.close();QVERIFY(changes.wait(2000));QTRY_VERIFY_WITH_TIMEOUT(!controller.installation().version.isEmpty(),2000);QCOMPARE(controller.installation().package,QString("zerus-ade-nightly-bin"));QCOMPARE(controller.channel(),QString("nightly"));
        QFile log(dir.path()+"/args.json");QVERIFY(log.open(QIODevice::ReadOnly));auto args=QJsonDocument::fromJson(log.readAll()).array();QCOMPARE(args.last().toString(),QFileInfo(binary).canonicalFilePath());QCOMPARE(args.first().toString(),QString("--"));qputenv("PATH",oldPath);qunsetenv("ZERUS_TEST_LOG");
#endif
    }
    void unknownOwnerNeverOffersReplacement(){
#ifdef Q_OS_LINUX
        QTemporaryDir dir;auto oldPath=qgetenv("PATH");qputenv("PATH",dir.path().toUtf8()+":"+oldPath);tool(dir.path()+"/pacman","#!/usr/bin/python3\nimport sys\nprint('custom-owner' if sys.argv[1]=='-Qqo' else 'custom-owner 2.1-1')\n");QFile executable(dir.path()+"/gui");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();UpdateController controller(nullptr,executable.fileName());QTRY_VERIFY_WITH_TIMEOUT(!controller.installation().version.isEmpty(),2000);QCOMPARE(controller.installation().package,QString("custom-owner"));QVERIFY(UpdateController::updateCommand(controller.installation()).isEmpty());qputenv("PATH",oldPath);
#endif
    }
    void packageProbesInheritOnlyStdio(){
#ifdef Q_OS_LINUX
        if(!ChildProcess::inheritsStdioOnly)QSKIP("This Qt cannot close inherited descriptors");
        QTemporaryDir dir;QVERIFY(dir.isValid());QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);
        auto oldPath=qgetenv("PATH");qputenv("PATH",dir.path().toUtf8()+":"+oldPath);const auto restorePath=qScopeGuard([oldPath]{qputenv("PATH",oldPath);});
        tool(dir.path()+"/pacman","#!/bin/sh\nif [ \"$1\" = -Qqo ]; then ls /dev/fd > \"$0.fds.part\" && mv \"$0.fds.part\" \"$0.fds\"; echo zerus-ade-bin; else echo 'zerus-ade-bin 0.36.2-1'; fi\n");
        QFile executable(dir.path()+"/gui");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();
        // A probe started while a request's pipe is being created must not keep that
        // request's input open, like every other child of the GUI (ChildProcess.h).
        const CreatingPipe creating;QVERIFY(creating.valid());
        UpdateController controller(nullptr,executable.fileName());QTRY_VERIFY_WITH_TIMEOUT(!controller.installation().version.isEmpty(),5000);
        const auto open=descriptors(dir.path()+"/pacman.fds");const auto shown=QDebug::toString(open).toLatin1();
        QVERIFY2(open.contains(0)&&open.contains(1)&&open.contains(2),shown.constData());for(int fd:creating.ends)QVERIFY2(!open.contains(fd),shown.constData());
#endif
    }
    void verifiedCatalogSurvivesRestartAndRejectsCorruption(){
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);QSettings().setValue("updates/channel","stable");QSettings().setValue("updates/catalog/stable",QJsonDocument(raw()).toJson());QSettings().setValue("updates/lastSuccess/stable",QDateTime::currentSecsSinceEpoch()-30);
        // The saved catalog is parsed off the GUI thread after startup (every suite fails on a GUI-thread parse).
        UpdateController restored(nullptr,dir.path()+"/missing");QSignalSpy changed(&restored,&UpdateController::changed);
        QTRY_VERIFY(restored.feed().valid);QVERIFY(restored.message().contains("cached"));QVERIFY(!changed.isEmpty());QCOMPARE(restored.feed().version,QString("0.37.0"));
        restored.setChannel("nightly");QVERIFY(!restored.feed().valid);preparationBarrier();QVERIFY(!restored.feed().valid);
        restored.setChannel("stable");QTRY_VERIFY(restored.feed().valid);QVERIFY(restored.message().contains("cached"));
        // A channel change before the restore applies drops it.
        UpdateController switched(nullptr,dir.path()+"/missing");switched.setChannel("nightly");preparationBarrier();QVERIFY(!switched.feed().valid);switched.setChannel("stable");
        QSettings().setValue("updates/catalog/stable",QByteArray("malformed"));UpdateController corrupt(nullptr,dir.path()+"/missing");preparationBarrier();QVERIFY(!corrupt.feed().valid);
    }
    void archVersionComparisonUsesEpochAndPackageRelease(){
#ifdef Q_OS_LINUX
        if(QStandardPaths::findExecutable("vercmp").isEmpty())QSKIP("Arch vercmp is unavailable");
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/channel","stable");auto value=raw();value["packages"]=QJsonObject{{"zerus-ade-bin","0.37.0-10"}};QSettings().setValue("updates/catalog/stable",QJsonDocument(value).toJson());QSettings().setValue("updates/lastSuccess/stable",QDateTime::currentSecsSinceEpoch()-30);
        auto oldPath=qgetenv("PATH");qputenv("PATH",dir.path().toUtf8()+":"+oldPath);tool(dir.path()+"/pacman","#!/usr/bin/python3\nimport os,sys\nprint('zerus-ade-bin' if sys.argv[1]=='-Qqo' else 'zerus-ade-bin '+os.environ['ZERUS_TEST_VERSION'])\n");QFile executable(dir.path()+"/gui");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();
        qputenv("ZERUS_TEST_VERSION","0.37.0-2");{UpdateController controller(nullptr,executable.fileName());QTRY_VERIFY_WITH_TIMEOUT(controller.message().contains("Update available"),2000);}
        qputenv("ZERUS_TEST_VERSION","1:0.37.0-2");{UpdateController controller(nullptr,executable.fileName());QTRY_VERIFY_WITH_TIMEOUT(controller.message().contains("at least"),2000);}
        qputenv("PATH",oldPath);qunsetenv("ZERUS_TEST_VERSION");
#endif
    }
    void distroParsingAndOptionalCommands(){
        QVERIFY(UpdateController::isArchLinux("ID=arch\n"));QVERIFY(UpdateController::isArchLinux("ID=manjaro\nID_LIKE=\"arch linux\"\n"));
        for(QByteArray value:{QByteArray("ID=ubuntu\nID_LIKE=debian"),QByteArray("ID=arch\nID=ubuntu"),QByteArray("ID=\"$(false)\""),QByteArray(8193,'x')})QVERIFY(!UpdateController::isArchLinux(value));
        DesktopInstallation local{"unknown",{},{},true,"yay"};QCOMPARE(UpdateController::updateCommand(local),QString("yay -Syu zerus-ade-bin"));QCOMPARE(UpdateController::updateCommand(local,"nightly"),QString("yay -Syu zerus-ade-nightly-bin"));local.arch=false;QVERIFY(UpdateController::updateCommand(local).isEmpty());local.arch=true;local.helper.clear();QVERIFY(UpdateController::updateCommand(local).isEmpty());
        QVERIFY(UpdateController::plainStableIsNewer("0.9.0","0.10.0"));QVERIFY(!UpdateController::plainStableIsNewer("0.37.0-dev","0.38.0"));QVERIFY(!UpdateController::plainStableIsNewer("0.37.0","0.37.0"));
    }
    void optOutStopsAutomaticRequestsAndKeepsManualCheck(){
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);int requests=0;
        UpdateController controller(nullptr,dir.path()+"/missing",[&](const QNetworkRequest &request){++requests;if(request.url()!=QUrl("https://zerus.dev/updates/v1/desktop/stable.json"))qFatal("Unexpected test endpoint");auto *reply=new FixtureReply;QTimer::singleShot(0,reply,[reply,this]{reply->finish(QJsonDocument(raw()).toJson());});return reply;});
        controller.checkOnStart();controller.checkAutomatically();controller.setChannel("nightly");controller.checkAutomatically();controller.setChannel("stable");QTest::qWait(50);QCOMPARE(requests,0);
        controller.check();QTRY_COMPARE(requests,1);QTRY_VERIFY(!controller.busy());QVERIFY(controller.feed().valid);QVERIFY(!controller.automaticChecks());
        UpdateController restored(nullptr,dir.path()+"/missing");QVERIFY(!restored.automaticChecks());
    }
    void selectedChannelChecksImmediatelyOnlyWhenEnabledAndDue(){
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();int requests=0;QString selected;
        UpdateController controller(nullptr,dir.path()+"/missing",[&](const QNetworkRequest &request){++requests;selected=request.url().fileName();return new FixtureReply;});QTRY_COMPARE(controller.installation().kind,QString("unknown"));controller.setChannel("nightly");QCOMPARE(requests,1);QCOMPARE(selected,QString("nightly.json"));controller.setChannel("nightly");QCOMPARE(requests,1);controller.setAutomaticChecks(false);controller.setChannel("stable");QCOMPARE(requests,1);
    }
    void disablingChecksCancelsInFlightAutomaticRequest(){
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QPointer<FixtureReply> pending;int requests=0;
        UpdateController controller(nullptr,dir.path()+"/missing",[&](const QNetworkRequest &){++requests;pending=new FixtureReply;return pending.data();});QTRY_COMPARE(controller.installation().kind,QString("unknown"));controller.checkAutomatically();QCOMPARE(requests,1);QVERIFY(controller.busy());controller.setAutomaticChecks(false);QVERIFY(!controller.busy());QSignalSpy notify(&controller,&UpdateController::newerReleaseVerified);QTest::qWait(30);QCOMPARE(notify.count(),0);controller.checkAutomatically();QCOMPARE(requests,1);
    }
    void optOutSuppressesDelayedAutomaticComparisonNotification(){
#ifdef Q_OS_LINUX
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();auto oldPath=qgetenv("PATH");qputenv("PATH",dir.path().toUtf8()+":"+oldPath);
        tool(dir.path()+"/pacman","#!/usr/bin/python3\nimport sys\nprint('zerus-ade-bin' if sys.argv[1]=='-Qqo' else 'zerus-ade-bin 0.36.2-1')\n");tool(dir.path()+"/vercmp","#!/usr/bin/python3\nimport time\ntime.sleep(0.4)\nprint(1)\n");QFile executable(dir.path()+"/gui");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();
        UpdateController controller(nullptr,executable.fileName(),[&](const QNetworkRequest &){auto *reply=new FixtureReply;QTimer::singleShot(0,reply,[reply,this]{reply->finish(QJsonDocument(raw()).toJson());});return reply;});QSignalSpy notify(&controller,&UpdateController::newerReleaseVerified);QTRY_VERIFY(!controller.installation().version.isEmpty());controller.checkAutomatically();QTRY_VERIFY(!controller.busy());controller.setAutomaticChecks(false);QTRY_VERIFY(controller.updateAvailable());QCOMPARE(notify.count(),0);
        controller.check();QTRY_COMPARE(notify.count(),1);qputenv("PATH",oldPath);
#endif
    }
    void notificationsAreFreshNewerOnlyAndDeduplicated(){
        QTemporaryDir dir;QSettings::setPath(QSettings::IniFormat,QSettings::UserScope,dir.path());Settings::flush();QSettings().clear();QSettings().setValue("updates/automatic",false);QCoreApplication::setApplicationVersion("0.36.2");QSettings().setValue("updates/catalog/stable",QJsonDocument(raw()).toJson());QSettings().setValue("updates/lastSuccess/stable",QDateTime::currentSecsSinceEpoch()-30);
        UpdateController controller(nullptr,dir.path()+"/missing",[&](const QNetworkRequest &){auto *reply=new FixtureReply;QTimer::singleShot(0,reply,[reply,this]{reply->finish(QJsonDocument(raw()).toJson());});return reply;});QSignalSpy notify(&controller,&UpdateController::newerReleaseVerified);QTRY_VERIFY(controller.updateAvailable());QCOMPARE(notify.count(),0);
        controller.check();QTRY_COMPARE(notify.count(),1);QCOMPARE(notify[0][0].toString(),QString("zerus-update:v1:stable:407198248"));QVERIFY(UpdateController::isUpdateToken(controller.updateToken()));QVERIFY(!UpdateController::isUpdateToken("session-attention:407198248"));controller.check();QTRY_VERIFY(!controller.busy());QCOMPARE(notify.count(),1);
        QCoreApplication::setApplicationVersion("0.37.0-dev");controller.check();QTRY_VERIFY(!controller.busy());QVERIFY(!controller.updateAvailable());QCOMPARE(notify.count(),1);
    }
    void subprocessTimeoutLeavesEventLoopResponsive(){
#ifdef Q_OS_LINUX
        // pacman never answers, so only the deadline ends the probe. A probe that waited
        // synchronously would stop every timer for that deadline, and a QTRY on a tick
        // still passes once the loop resumes: measure the longest pause between 20 ms
        // ticks from before the controller exists (a probe run from its constructor
        // counts too) until the probe times out. 1 s allows for a loaded runner and
        // stays well below the 1.5 s a blocking probe takes.
        constexpr int deadline=1500;UpdateController::setCommandTimeoutForTests(deadline);const auto restoreTimeout=qScopeGuard([]{UpdateController::setCommandTimeoutForTests(0);});
        QTemporaryDir dir;auto oldPath=qgetenv("PATH");qputenv("PATH",dir.path().toUtf8()+":"+oldPath);tool(dir.path()+"/pacman","#!/usr/bin/python3\nimport time\ntime.sleep(20)\n");QFile executable(dir.path()+"/gui");QVERIFY(executable.open(QIODevice::WriteOnly));executable.close();
        QElapsedTimer clock;clock.start();qint64 last=0,longest=0;const auto beat=[&]{const qint64 now=clock.elapsed();longest=qMax(longest,now-last);last=now;};QTimer ticker;connect(&ticker,&QTimer::timeout,beat);ticker.start(20);
        UpdateController controller(nullptr,executable.fileName());
        QTRY_COMPARE_WITH_TIMEOUT(controller.installation().kind,QString("unknown"),2*deadline);beat();QVERIFY2(longest<1000,qPrintable(QString("The event loop stopped for %1 ms").arg(longest)));qputenv("PATH",oldPath);
#endif
    }
private:
    QTemporaryDir m_settings;
};
QTEST_MAIN(TestUpdates)
#include "test_updates.moc"
