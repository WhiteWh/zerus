#include "ForkUpdater.h"
#include "ChildProcess.h"
#include "JsonParse.h"
#include "ProcessRunner.h"
#ifdef Q_OS_WIN
#include "RelayConfiguration.h"
#include "WindowsMobileServices.h"
#endif
#include <QCoreApplication>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QProcess>
#include <QRegularExpression>
#include <QTimer>
#include <QThread>

namespace {
#ifdef ZERUS_FORK_FEED_URL
const char *feedUrl = ZERUS_FORK_FEED_URL;
#else
const char *feedUrl = "";
#endif
// Every build comes from this fork's own releases.
const char *releasePrefix = "https://github.com/WhiteWh/zerus/releases/";
// A local portable build can check the latest WhiteWh release on request.
const char *latestFeed = "https://github.com/WhiteWh/zerus/releases/latest/download/zerus-fork-feed.json";
// The folder at the top of the portable zip (scripts/windows_portable.py).
const char *top = "zerus-windows-x86_64";
constexpr qint64 feedLimit = 64 * 1024;
constexpr qint64 zipLimit = 1024LL * 1024 * 1024;
constexpr int firstCheckMs = 2 * 60 * 1000, checkIntervalMs = 60 * 60 * 1000;

// Both helpers show a small always-on-top banner in a corner of the screen while
// they work: what they wait for, each step and the result. A click hides it.
// Without Windows Forms the helper still runs, only without the banner. Each
// helper defines Log first; the banner follows its parameters and log.
const char *bannerScript = R"PS(
$banner = $null
try {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    try { Add-Type -Namespace ZerusUpdate -Name Dpi -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'; [void][ZerusUpdate.Dpi]::SetProcessDPIAware() } catch { }
    $graphics = [System.Drawing.Graphics]::FromHwnd([IntPtr]::Zero); $scale = $graphics.DpiX / 96; $graphics.Dispose()
    function Px([double]$value) { [int][Math]::Round($value * $scale) }
    function Rgb([int]$r, [int]$g, [int]$b) { [System.Drawing.Color]::FromArgb($r, $g, $b) }
    $banner = New-Object System.Windows.Forms.Form
    $banner.Text = 'Zerus update'
    $banner.FormBorderStyle = 'None'; $banner.TopMost = $true; $banner.ShowInTaskbar = $true; $banner.StartPosition = 'Manual'
    try { $banner.Icon = [System.Drawing.Icon]::ExtractAssociatedIcon((Join-Path $New 'hgs-tray.exe')) } catch { }
    $banner.BackColor = Rgb 58 61 68
    $banner.Padding = New-Object System.Windows.Forms.Padding(1)
    $banner.ClientSize = New-Object System.Drawing.Size((Px 380), (Px 104))
    $area = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $banner.Location = New-Object System.Drawing.Point(($area.Right - $banner.Width - (Px 16)), ($area.Bottom - $banner.Height - (Px 16)))
    $body = New-Object System.Windows.Forms.Panel
    $body.Dock = 'Fill'; $body.BackColor = Rgb 31 33 38
    $banner.Controls.Add($body)
    $heading = New-Object System.Windows.Forms.Label
    $heading.Text = "Updating Zerus to build $Build"; $heading.ForeColor = Rgb 236 238 241
    $heading.Font = New-Object System.Drawing.Font('Segoe UI Semibold', 10)
    $heading.SetBounds((Px 16), (Px 12), (Px 346), (Px 22))
    $status = New-Object System.Windows.Forms.Label
    $status.ForeColor = Rgb 178 182 189
    $status.Font = New-Object System.Drawing.Font('Segoe UI', 9)
    $status.SetBounds((Px 16), (Px 36), (Px 346), (Px 38))
    $track = New-Object System.Windows.Forms.Panel
    $track.BackColor = Rgb 51 54 61
    $track.SetBounds((Px 16), (Px 82), (Px 346), (Px 6))
    $fill = New-Object System.Windows.Forms.Panel
    $fill.BackColor = Rgb 76 141 255
    $fill.SetBounds(0, 0, 0, (Px 6))
    $track.Controls.Add($fill)
    $body.Controls.AddRange(@($heading, $status, $track))
    foreach ($control in @($banner, $body, $heading, $status, $track, $fill)) { $control.Add_Click({ $banner.Hide() }) }
    $banner.Show()
    [System.Windows.Forms.Application]::DoEvents()
} catch { $banner = $null }
# Waits while the banner stays responsive.
function Pump([int]$milliseconds) {
    $until = (Get-Date).AddMilliseconds($milliseconds)
    do {
        if ($banner -and $banner.Visible) { [System.Windows.Forms.Application]::DoEvents() }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $until)
}
function Step([string]$text, [int]$percent) {
    if (-not $banner) { return }
    $status.Text = $text
    $fill.Width = [int]($track.Width * [Math]::Min(100, [Math]::Max(0, $percent)) / 100)
    $banner.Refresh()
    [System.Windows.Forms.Application]::DoEvents()
}
# The result: a failure is logged and stays on screen longer, even after the
# banner was hidden.
function Finish([string]$text, [bool]$failed) {
    if ($failed) { Log $text }
    if (-not $banner) { return }
    if ($failed) {
        $fill.BackColor = Rgb 224 82 82; $status.ForeColor = Rgb 255 160 160
        if (-not $banner.Visible) { $banner.Show() }
    }
    Step $text 100
    Pump $(if ($failed) { 10000 } else { 2500 })
    $banner.Close()
}
function WaitExit([int]$id, [int]$seconds) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Process -Id $id -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Pump 200 }
    -not (Get-Process -Id $id -ErrorAction SilentlyContinue)
}
# The new build counts as started once it runs 15 seconds. The banner leaves
# as soon as its window is up; a build that exits later still reports.
function Started($process) {
    for ($second = 1; $second -le 15; $second++) {
        Pump 1000
        if ($process.HasExited) { return $false }
        $process.Refresh()
        if ($banner -and $banner.Visible -and $second -ge 2 -and $process.MainWindowHandle -ne [IntPtr]::Zero) {
            Step "Zerus build $Build is running." 100; Pump 1200; $banner.Hide()
        } else { Step 'Starting Zerus...' (70 + 2 * $second) }
    }
    -not $process.HasExited
}
)PS";

// Runs detached once Zerus quits: waits until nothing runs from the installation,
// sets it aside as the single rollback copy, moves the verified build in and
// starts it. A build that exits within 15 seconds is replaced by the previous one.
const char *installHead = R"PS(param([int]$ParentPid, [string]$Install, [string]$New, [string]$Previous, [string]$Stage, [int]$Build)
$ErrorActionPreference = 'Stop'
$log = Join-Path (Split-Path -Parent $Install) 'zerus-fork-update.log'
function Log([string]$message) { try { Add-Content -LiteralPath $log -Value ("{0:u} {1}" -f (Get-Date), $message) } catch { } }
)PS";
const char *installBody = R"PS(
# The programs that still run from the installation, by name.
function Holders {
    $prefix = $Install.TrimEnd('\') + '\'
    @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -and $_.Path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) } catch { $false }
    } | ForEach-Object { $_.ProcessName + '.exe' } | Sort-Object -Unique)
}
function WaitIdle([int]$seconds) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        $holders = @(Holders)
        if ($holders.Count -eq 0) { return $true }
        Step ('Waiting for programs that run from the installation: ' + ($holders -join ', ')) 25
        Pump 500
    }
    @(Holders).Count -eq 0
}
$gui = Join-Path $Install 'hgs-tray.exe'
Step 'Waiting for Zerus to close...' 10
[void](WaitExit $ParentPid 120)
if (-not (WaitIdle 120)) { Start-Process -FilePath $gui; Finish 'Programs still run from the installation; the update waits for the next restart.' $true; exit 1 }
Step 'Setting the current build aside...' 45
# The old rollback copy goes only once the current build can move; a scanner or
# indexer may hold the folder for a moment, so the move is tried for 20 seconds.
$aside = $Install.TrimEnd('') + '.aside'
$problem = $null
for ($try = 0; $try -lt 40; $try++) {
    try { Move-Item -LiteralPath $Install -Destination $aside; $problem = $null; break }
    catch { $problem = "$_"; Pump 500 }
}
if ($problem) { Start-Process -FilePath $gui; Finish "Could not set the current build aside: $problem" $true; exit 1 }
try {
    if (Test-Path -LiteralPath $Previous) { Remove-Item -LiteralPath $Previous -Recurse -Force }
    Move-Item -LiteralPath $aside -Destination $Previous
} catch { Log "Kept the current build as $aside`: $_"; $Previous = $aside }
Step 'Moving the new build in...' 60
try { Move-Item -LiteralPath $New -Destination $Install }
catch {
    $problem = "$_"
    Move-Item -LiteralPath $Previous -Destination $Install
    Start-Process -FilePath $gui; Finish "Could not move the new build in: $problem" $true; exit 1
}
Step 'Starting Zerus...' 70
$started = Start-Process -FilePath $gui -PassThru
if (-not (Started $started)) {
    $code = $started.ExitCode
    Step 'The new build exited; restoring the previous build...' 90
    [void](WaitIdle 30)
    $failed = $Install.TrimEnd('\') + '.failed'
    if (Test-Path -LiteralPath $failed) { Remove-Item -LiteralPath $failed -Recurse -Force }
    Move-Item -LiteralPath $Install -Destination $failed
    Move-Item -LiteralPath $Previous -Destination $Install
    Start-Process -FilePath $gui
    Finish "The new build exited with code $code; the previous build runs again." $true
    exit 1
}
Log "Updated to the build in $Install."
Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
if ($banner -and $banner.Visible) { Finish "Zerus build $Build is running." $false } elseif ($banner) { $banner.Close() }
)PS";

// The versioned root's helper: the running build is never moved, so nothing has to
// stop running from it. Once Zerus quits, the verified build becomes <root>\app-<N>,
// <root>\current is re-pointed at it and started; a build that exits within 15
// seconds hands current back. The two newest builds stay, older app-* go.
const char *versionedHead = R"PS(param([int]$ParentPid, [string]$Root, [string]$Link, [string]$New, [string]$Target, [string]$Stage, [int]$Build)
$ErrorActionPreference = 'Stop'
$log = Join-Path $Root 'update.log'
function Log([string]$message) { try { Add-Content -LiteralPath $log -Value ("{0:u} {1}" -f (Get-Date), $message) } catch { } }
)PS";
const char *versionedBody = R"PS(
function Point([string]$folder) {
    # rmdir removes the junction itself and never the build it points to.
    if (Test-Path -LiteralPath $Link) { cmd /c rmdir "$Link" | Out-Null }
    New-Item -ItemType Junction -Path $Link -Target $folder | Out-Null
}
$gui = Join-Path $Link 'hgs-tray.exe'
$previous = ([string]((Get-Item -LiteralPath $Link).Target | Select-Object -First 1)).TrimEnd('\')
Step 'Waiting for Zerus to close...' 10
if (-not (WaitExit $ParentPid 120)) { Finish 'Zerus did not quit; the update waits for the next restart.' $true; exit 1 }
Step 'Switching to the new build...' 50
try {
    if (Test-Path -LiteralPath $Target) { Remove-Item -LiteralPath $Target -Recurse -Force }
    Move-Item -LiteralPath $New -Destination $Target
    Point $Target
} catch {
    $problem = "$_"
    if (-not (Test-Path -LiteralPath $Link)) { Point $previous }
    Start-Process -FilePath $gui; Finish "Could not switch to the new build: $problem" $true; exit 1
}
Step 'Starting Zerus...' 70
$started = Start-Process -FilePath $gui -PassThru
if (-not (Started $started)) {
    $code = $started.ExitCode
    Point $previous
    Start-Process -FilePath $gui
    Finish "The new build exited with code $code; current points to $previous again." $true
    exit 1
}
Log "Updated: current is $Target, the previous build stays in $previous."
Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem -LiteralPath $Root -Directory -Filter 'app-*' | Where-Object { $_.FullName -ne $Target -and $_.FullName -ne $previous } | ForEach-Object {
    try { Remove-Item -LiteralPath $_.FullName -Recurse -Force } catch { Log "Kept $($_.FullName): $_" }
}
if ($banner -and $banner.Visible) { Finish "Zerus build $Build is running." $false } elseif ($banner) { $banner.Close() }
)PS";
}

namespace {
// A folder with its links resolved. Qt 6.10 on Windows keeps a junction in
// canonicalFilePath(), so a junction resolves through its target first.
QString resolvedFolder(const QString &path)
{
    const QFileInfo info(path);
    QString target = path;
    if (info.isJunction()) target = info.junctionTarget();
    else if (info.isSymLink()) target = info.symLinkTarget();
    const QString canonical = QFileInfo(target).canonicalFilePath();
    return canonical.isEmpty() ? QFileInfo(target).absoluteFilePath() : canonical;
}
}

ForkUpdater::Layout ForkUpdater::layoutFor(const QString &executable)
{
    const QFileInfo exe(executable);
    Layout layout;
    const QString launched = exe.absolutePath();
    layout.install = resolvedFolder(launched);
    // Started as <root>\current\hgs-tray.exe or straight from <root>\app-<N>.
    const QFileInfo parent(QFileInfo(launched).fileName() == QLatin1String("current")
        ? QFileInfo(launched).absolutePath() : QFileInfo(layout.install).absolutePath());
    const QString root = parent.canonicalFilePath().isEmpty() ? parent.absoluteFilePath() : parent.canonicalFilePath();
    const QFileInfo link(QDir(root).filePath(QStringLiteral("current")));
    if (link.isSymLink() || link.isJunction()) {
        if (resolvedFolder(link.absoluteFilePath()) == layout.install) {
            layout.versioned = true;
            layout.root = root;
            layout.link = link.absoluteFilePath();
            layout.stage = QDir(root).filePath(QStringLiteral(".update"));
            return layout;
        }
        layout.superseded = true;
    }
    layout.stage = layout.install + QStringLiteral(".update");
    return layout;
}

ForkUpdater *ForkUpdater::instance()
{
    static auto *value = new ForkUpdater(QCoreApplication::instance());
    return value;
}

qint64 ForkUpdater::installedBuild()
{
#ifdef ZERUS_FORK_BUILD
    return qint64(ZERUS_FORK_BUILD);
#else
    return 0;
#endif
}

ForkUpdater::Offer ForkUpdater::offerFor(qint64 installed, const QString &runningCommit, qint64 build, const QString &commit)
{
    if (!runningCommit.isEmpty() && commit == runningCommit) return Offer::Current;
    if (installed > 0) return build > installed ? Offer::Newer : Offer::Current;
    return Offer::Replacement;
}

QString ForkUpdater::sourceCommit(const QString &folder)
{
    QFile file(QDir(folder).filePath(QStringLiteral("build-info.json")));
    if (!file.open(QIODevice::ReadOnly)) return {};
    const QString commit = JsonParse::object(file.read(feedLimit)).value("source_commit").toString();
    static const QRegularExpression full(QStringLiteral("\\A[0-9a-f]{40}\\z"));
    return full.match(commit).hasMatch() ? commit : QString();
}

bool ForkUpdater::linuxSourceInstall(const QString &executable, const QString &home)
{
#ifdef Q_OS_LINUX
    const QFileInfo exe(executable);
    const QDir bin(QDir(home).filePath(QStringLiteral(".local/bin")));
    return exe.fileName() == QLatin1String("hgs-tray")
        && resolvedFolder(exe.absolutePath()) == resolvedFolder(bin.absolutePath())
        && QFileInfo(bin.filePath(QStringLiteral("hgs"))).isExecutable();
#else
    Q_UNUSED(executable);
    Q_UNUSED(home);
    return false;
#endif
}

ForkUpdater::ForkUpdater(QObject *parent) : QObject(parent)
{
    connect(this, &ForkUpdater::installStarted, QCoreApplication::instance(), &QCoreApplication::quit);
    m_layout = layoutFor(QCoreApplication::applicationFilePath());
    m_install = m_layout.install;
    m_stage = m_layout.stage;
    // The preparation queue applies this before any later feed result, so the
    // offer comparison still sees the running commit without parsing on the GUI.
    if (!m_install.isEmpty())
        ProcessRunner::post(this, [folder = m_install] { return sourceCommit(folder); },
                            [this](const QString &commit) { if (!commit.isEmpty()) m_runningCommit = commit; });
#ifdef Q_OS_WIN
    // Only the portable zip carries build-info.json next to hgs-tray.exe.
    m_portable = !m_install.isEmpty() && QFileInfo::exists(m_install + QStringLiteral("/build-info.json")) && !m_layout.superseded;
#endif
#ifdef Q_OS_LINUX
    m_linux = linuxSourceInstall(QCoreApplication::applicationFilePath(), QDir::homePath());
    if (m_linux) {
        m_stage = QDir::home().filePath(QStringLiteral(".cache/hgs/fork-source-update"));
#ifdef ZERUS_BUILD_SOURCE_COMMIT
        if (m_runningCommit.isEmpty()) m_runningCommit = QStringLiteral(ZERUS_BUILD_SOURCE_COMMIT);
#endif
    }
#endif
    m_enabled = (m_portable || m_linux) && *feedUrl && QString::fromLatin1(feedUrl).startsWith(QLatin1String(releasePrefix));
    m_status = m_enabled ? tr("Fork updates have not been checked.")
             : (m_portable || m_linux) ? tr("This build was made locally: it checks the fork's releases only when you ask.") : QString();
    if (!m_enabled) return;
    m_network = new QNetworkAccessManager(this);
    QTimer::singleShot(firstCheckMs, this, &ForkUpdater::check);
    auto *periodic = new QTimer(this);
    periodic->setInterval(checkIntervalMs);
    connect(periodic, &QTimer::timeout, this, &ForkUpdater::check);
    periodic->start();
}

void ForkUpdater::finish(const QString &status)
{
    m_busy = false;
    m_status = status;
    emit changed();
}

QString ForkUpdater::unpacked() const
{
#ifdef Q_OS_LINUX
    if (m_linux) return QDir(m_stage).filePath(QStringLiteral("source"));
#endif
    return QDir(m_stage).filePath(QString::fromLatin1(top));
}

void ForkUpdater::check()
{
    if (!m_enabled || m_installing) return;
    fetch();
}

void ForkUpdater::checkNow()
{
    if ((!m_portable && !m_linux) || m_installing) return;
    if (!m_network) m_network = new QNetworkAccessManager(this);
    fetch();
}

void ForkUpdater::fetch()
{
    if (m_busy || m_installing) return;
    m_busy = true;
    m_offeredBuild = 0;
    m_status = tr("Checking the fork's releases…");
    emit changed();
    const QString feed = m_enabled ? QString::fromLatin1(feedUrl) : QString::fromLatin1(latestFeed);
    QNetworkRequest request{QUrl(feed)};
    request.setAttribute(QNetworkRequest::RedirectPolicyAttribute, QNetworkRequest::NoLessSafeRedirectPolicy);
    request.setTransferTimeout(30000);
    auto *reply = m_network->get(request);
    connect(reply, &QNetworkReply::finished, this, [this, reply] { reply->deleteLater(); feedFinished(reply); });
}

void ForkUpdater::feedFinished(QNetworkReply *reply)
{
    if (reply->error() != QNetworkReply::NoError)
        return finish(tr("Fork update check failed: %1").arg(reply->errorString()));
    const auto data = reply->read(feedLimit + 1);
    if (data.size() > feedLimit) return finish(tr("The fork update feed is too large."));
    ProcessRunner::post(this, [data] { return JsonParse::object(data); },
                        [this](const QJsonObject &feed) { applyFeed(feed); });
}

void ForkUpdater::applyFeed(const QJsonObject &feed)
{
    m_build = feed.value("build").toInteger();
    m_commit = feed.value("commit").toString();
    m_url = QUrl(feed.value("url").toString());
    m_size = feed.value("size").toInteger();
    m_sha256 = QByteArray::fromHex(feed.value("sha256").toString().toLatin1());
    static const QRegularExpression commit(QStringLiteral("\\A[0-9a-f]{40}\\z"));
    if (feed.value("schema").toInt() != 1 || m_build <= 0 || !commit.match(m_commit).hasMatch()
        || m_url.scheme() != QLatin1String("https") || !m_url.toString().startsWith(QLatin1String(releasePrefix))
        || m_size <= 0 || m_size > zipLimit || m_sha256.size() != 32)
        return finish(tr("The fork update feed is not valid."));
    switch (offerFor(installedBuild(), m_runningCommit, m_build, m_commit)) {
    case Offer::Current:
        return finish(installedBuild() > 0 ? tr("Fork build %1 is the newest.").arg(installedBuild())
                                           : tr("This build is the commit of fork build %1, the newest.").arg(m_build));
    case Offer::Replacement:
        if (m_build <= m_readyBuild) return finish(readyStatus());
        // No build number: the feed's build may be older; it is offered, never fetched.
        m_offeredBuild = m_build;
        return finish(tr("This build has no fork build number (commit %1). Fork build %2 (commit %3) can replace it; it is not known to be newer.")
                          .arg(m_runningCommit.isEmpty() ? tr("unknown") : m_runningCommit.left(10), QString::number(m_build), m_commit.left(10)));
    case Offer::Newer:
        break;
    }
    if (m_build <= m_readyBuild) return finish(readyStatus());
    download();
}

void ForkUpdater::confirmReplacement()
{
    if (m_busy || m_offeredBuild <= 0 || m_offeredBuild != m_build) return;
    m_offeredBuild = 0;
    m_busy = true;
    download();
}

void ForkUpdater::download()
{
    // A ready build stays where it is until the newer one is verified.
    m_download = ready() ? m_stage + QStringLiteral("-next") : m_stage;
    QDir stage(m_download);
    if (stage.exists() && !stage.removeRecursively())
        return finish(tr("Could not clear %1.").arg(QDir::toNativeSeparators(m_download)));
    if (!QDir().mkpath(m_download))
        return finish(tr("Could not create %1.").arg(QDir::toNativeSeparators(m_download)));
#ifdef Q_OS_LINUX
    if (m_linux) {
        const QString helper = QDir::home().filePath(QStringLiteral(".local/lib/hgs-tray/linux-source-update.sh"));
        if (!QFileInfo::exists(helper))
            return finish(tr("The Linux update helper is not installed. Run tray/install.sh first."));
        auto *process = new QProcess(this);
        process->setProgram(QStringLiteral("/bin/bash"));
        process->setArguments({helper, QStringLiteral("prepare"), m_download, m_commit, QString::number(m_build)});
        process->setWorkingDirectory(QDir::tempPath());
        process->setProcessChannelMode(QProcess::MergedChannels);
        connect(process, &QProcess::finished, this, &ForkUpdater::sourcePrepared);
        connect(process, &QProcess::errorOccurred, this, [this, process](QProcess::ProcessError error) {
            if (error != QProcess::FailedToStart) return;
            process->deleteLater();
            QDir(m_download).removeRecursively();
            finish(tr("Could not prepare fork build %1: %2").arg(m_build).arg(process->errorString()));
        });
        m_extract = process;
        m_status = tr("Preparing fork build %1 from its published source…").arg(m_build);
        emit changed();
        ChildProcess::inheritStdioOnly(*process);
        process->start();
        return;
    }
#endif
    m_file = new QFile(stage.filePath(QStringLiteral("update.zip")), this);
    if (!m_file->open(QIODevice::WriteOnly)) {
        m_file->deleteLater();
        return finish(tr("Could not write the fork update: %1").arg(m_file->errorString()));
    }
    m_hash.reset();
    m_received = 0;
    m_status = tr("Downloading fork build %1…").arg(m_build);
    emit changed();
    QNetworkRequest request(m_url);
    request.setAttribute(QNetworkRequest::RedirectPolicyAttribute, QNetworkRequest::NoLessSafeRedirectPolicy);
    // An inactivity bound: the transfer itself may take longer.
    request.setTransferTimeout(120000);
    auto *reply = m_network->get(request);
    connect(reply, &QNetworkReply::readyRead, this, [this, reply] {
        const auto chunk = reply->readAll();
        m_received += chunk.size();
        if (m_received > m_size || !m_file || m_file->write(chunk) != chunk.size()) { reply->abort(); return; }
        m_hash.addData(chunk);
    });
    connect(reply, &QNetworkReply::finished, this, [this, reply] { reply->deleteLater(); downloadFinished(reply); });
}

void ForkUpdater::downloadFinished(QNetworkReply *reply)
{
    const QString zip = m_file ? m_file->fileName() : QString();
    if (m_file) { m_file->close(); m_file->deleteLater(); }
    if (reply->error() != QNetworkReply::NoError || m_received != m_size || m_hash.result() != m_sha256) {
        QDir(m_download).removeRecursively();
        return finish(reply->error() != QNetworkReply::NoError
            ? tr("Fork build %1 did not download: %2").arg(m_build).arg(reply->errorString())
            : tr("Fork build %1 does not match its published size and SHA-256.").arg(m_build));
    }
    // Windows 10 and later ship bsdtar, which reads zip archives and refuses
    // absolute and parent paths.
    auto *process = new QProcess(this);
    process->setProgram(QDir(qEnvironmentVariable("SystemRoot", QStringLiteral("C:\\Windows"))).filePath(QStringLiteral("System32/tar.exe")));
    process->setArguments({QStringLiteral("-xf"), QDir::toNativeSeparators(zip), QStringLiteral("-C"), QDir::toNativeSeparators(m_download)});
    process->setProcessChannelMode(QProcess::MergedChannels);
    connect(process, &QProcess::finished, this, &ForkUpdater::extracted);
    connect(process, &QProcess::errorOccurred, this, [this, process](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart) { process->deleteLater(); finish(tr("Could not unpack the fork update: %1").arg(process->errorString())); }
    });
    m_extract = process;
    ChildProcess::inheritStdioOnly(*process);
    process->start();
}

void ForkUpdater::extracted()
{
    const bool unpackedOk = m_extract && m_extract->exitStatus() == QProcess::NormalExit && m_extract->exitCode() == 0;
    const auto output = m_extract ? QString::fromLocal8Bit(m_extract->readAll()).trimmed() : QString();
    if (m_extract) m_extract->deleteLater();
    QFile::remove(QDir(m_download).filePath(QStringLiteral("update.zip")));
    const QDir build(QDir(m_download).filePath(QString::fromLatin1(top)));
    QFile manifest(build.filePath(QStringLiteral("build-info.json")));
    const bool complete = unpackedOk && build.exists(QStringLiteral("hgs-tray.exe")) && build.exists(QStringLiteral("hgs.exe"))
        && manifest.open(QIODevice::ReadOnly);
    // The zip must be the build of the commit the feed names. Parse it off the GUI thread.
    const QByteArray bytes = complete ? manifest.read(feedLimit) : QByteArray();
    manifest.close();
    ProcessRunner::post(this, [bytes] { return JsonParse::object(bytes); },
                        [this, complete, output](const QJsonObject &info) { applyExtracted(complete, output, info); });
}

void ForkUpdater::sourcePrepared()
{
    const bool complete = m_extract && m_extract->exitStatus() == QProcess::NormalExit && m_extract->exitCode() == 0;
    if (m_extract) m_extract->deleteLater();
    if (!complete) {
        const QString log = QDir(m_download).filePath(QStringLiteral("prepare.log"));
        return finish(tr("Could not prepare fork build %1. See %2.").arg(m_build).arg(QDir::toNativeSeparators(log)));
    }
    acceptPrepared();
}

void ForkUpdater::applyExtracted(bool complete, const QString &output, const QJsonObject &info)
{
    if (!complete || info.value("source_commit").toString() != m_commit) {
        QDir(m_download).removeRecursively();
        return finish(output.isEmpty() ? tr("Fork build %1 is incomplete.").arg(m_build)
                                       : tr("Could not unpack fork build %1: %2").arg(m_build).arg(output));
    }
    acceptPrepared();
}

void ForkUpdater::acceptPrepared()
{
    // The newer build takes the waiting one's place.
    if (m_download != m_stage) {
        if (!QDir(m_stage).removeRecursively() || !QDir().rename(m_download, m_stage)) {
            QDir(m_download).removeRecursively();
            if (!QFileInfo::exists(unpacked())) { m_readyBuild = 0; m_readyCommit.clear(); }
            return finish(ready() ? readyStatus() : tr("Could not replace the waiting fork update in %1.").arg(QDir::toNativeSeparators(m_stage)));
        }
    }
    m_readyBuild = m_build;
    m_readyCommit = m_commit;
    finish(readyStatus());
    emit buildReady(m_build);
}

QString ForkUpdater::readyStatus() const
{
    return installedBuild() > 0 ? tr("Fork build %1 is ready. Restart Zerus to finish the update.").arg(m_readyBuild)
                                : tr("Fork build %1 is ready to replace this build. Restart Zerus to replace it.").arg(m_readyBuild);
}

bool ForkUpdater::startInstall()
{
    if (m_installing || !ready() || !QFileInfo::exists(unpacked())) return false;
#ifdef Q_OS_LINUX
    if (m_linux) {
        const QString helper = QDir::home().filePath(QStringLiteral(".local/lib/hgs-tray/linux-source-update.sh"));
        if (!QFileInfo::exists(helper)) return false;
        const QStringList arguments{helper, QStringLiteral("install"), m_stage, m_readyCommit, QString::number(m_readyBuild)};
        if (!ChildProcess::startDetached(QStringLiteral("/bin/bash"), arguments, QDir::tempPath())) return false;
        m_installing = true;
        m_status = tr("Installing fork build %1. Zerus restarts only when the verified build is ready.").arg(m_readyBuild);
        emit changed();
        auto *watch = new QTimer(this);
        watch->setInterval(1000);
        connect(watch, &QTimer::timeout, this, [this, watch] {
            QFile result(QDir(m_stage).filePath(QStringLiteral("install.exit")));
            if (!result.open(QIODevice::ReadOnly)) return;
            watch->stop();
            watch->deleteLater();
            m_installing = false;
            if (result.readAll().trimmed() == "0") {
                m_readyBuild = 0;
                m_readyCommit.clear();
                finish(tr("The fork build is installed. Start Zerus again to use it."));
            } else {
                const QString log = QDir(m_stage).filePath(QStringLiteral("install.log"));
                finish(tr("Could not install fork build %1. See %2.").arg(m_readyBuild).arg(QDir::toNativeSeparators(log)));
            }
        });
        watch->start();
        return true;
    }
#endif
    const QString script = QDir::temp().filePath(QStringLiteral("zerus-fork-install-%1.ps1").arg(m_readyBuild));
    QFile file(script);
    const QByteArray text = m_layout.versioned ? QByteArray(versionedHead) + bannerScript + versionedBody
                                               : QByteArray(installHead) + bannerScript + installBody;
    if (!file.open(QIODevice::WriteOnly | QIODevice::Truncate) || file.write(text) != text.size()) return false;
    file.close();
    const auto native = [](const QString &path) { return QDir::toNativeSeparators(path); };
    QStringList args{QStringLiteral("-NoProfile"), QStringLiteral("-NonInteractive"), QStringLiteral("-ExecutionPolicy"), QStringLiteral("Bypass"),
                          QStringLiteral("-WindowStyle"), QStringLiteral("Hidden"), QStringLiteral("-File"), native(script),
                          QStringLiteral("-ParentPid"), QString::number(QCoreApplication::applicationPid()),
                          QStringLiteral("-New"), native(unpacked()), QStringLiteral("-Stage"), native(m_stage),
                          QStringLiteral("-Build"), QString::number(m_readyBuild)};
    if (m_layout.versioned)
        args << QStringLiteral("-Root") << native(m_layout.root) << QStringLiteral("-Link") << native(m_layout.link)
                  << QStringLiteral("-Target") << native(QDir(m_layout.root).filePath(QStringLiteral("app-%1").arg(m_readyBuild)));
    else
        args << QStringLiteral("-Install") << native(m_install)
                  << QStringLiteral("-Previous") << native(m_install + QStringLiteral(".previous"));
    QProcess process;
    process.setProgram(QDir(qEnvironmentVariable("SystemRoot", QStringLiteral("C:\\Windows"))).filePath(QStringLiteral("System32/WindowsPowerShell/v1.0/powershell.exe")));
    process.setArguments(args);
    // Never inside the folder it moves: a current directory there keeps it in use.
    process.setWorkingDirectory(QDir::tempPath());
#ifdef Q_OS_WIN
    // CREATE_NO_WINDOW: the helper outlives Zerus without a console window; its
    // banner is its only window.
    process.setCreateProcessArgumentsModifier([](QProcess::CreateProcessArguments *arguments) { arguments->flags |= 0x08000000; });
#endif
    ChildProcess::inheritStdioOnly(process);
    if (!process.startDetached()) return false;
    m_installing = true;
#ifdef Q_OS_WIN
    // The detached helper waits for the portable folder. Stop its connector on
    // a worker and keep this process alive until that request has completed.
    if (!m_layout.versioned && QFileInfo::exists(RelaySettings::configPath())) {
        const auto config = RelaySettings::configPath();
        auto *worker = QThread::create([config] {
            const auto status = WindowsMobileServices::manage(QStringLiteral("status"), config, {{QStringLiteral("revision"), QString()}});
            if (status.value(QStringLiteral("running")).toBool())
                WindowsMobileServices::manage(QStringLiteral("stop"), config,
                    {{QStringLiteral("revision"), status.value(QStringLiteral("revision")).toString()},
                     {QStringLiteral("generation"), status.value(QStringLiteral("generation")).toString()}});
        });
        connect(worker, &QThread::finished, this, &ForkUpdater::installStarted);
        connect(worker, &QThread::finished, worker, &QObject::deleteLater);
        worker->start();
        return true;
    }
#endif
    QTimer::singleShot(0, this, &ForkUpdater::installStarted);
    return true;
}
