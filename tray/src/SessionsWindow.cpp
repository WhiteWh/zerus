#include "Settings.h"
#include "ChildProcess.h"
#include "ConversationSource.h"
#include "WslFileOpenDialog.h"
#include "SettingsJson.h"
#include "SlowEventLog.h"
#include "ArchiveBrowser.h"
#include "NativeSessionCapabilities.h"
#include "SessionsWindow.h"
#include "SlowEventLog.h"
#include "AgentWorking.h"
#include "EventStream.h"
#include <QStandardPaths>
#include "WorktreeBatchCleanupDialog.h"
#include "WorktreeCleanupStatus.h"
#include "SessionNotifications.h"
#include "DownloadCache.h"
#include "SessionFileDrop.h"
#include "SessionCommands.h"
#include "SessionCardDelegate.h"
#include "SessionPanelDock.h"
#include "ExternalFile.h"
#include "LocalFile.h"
#include "SessionPresentation.h"
#include "TeamAttentionNotices.h"
#include "AccountUsage.h"
#include "AccountSwitch.h"
#include "SessionUsage.h"
#include "CacheKeepalive.h"
#include "SessionStates.h"
#include "CacheStatus.h"
#include "ComposerToolbar.h"
#include "AccountUsageStore.h"
#include "IdentityBadge.h"
#include "ProjectAppearance.h"
#include "SessionStatusBadge.h"
#include "SessionElapsed.h"
#include "SessionTag.h"
#include "SessionTooltip.h"
#include "ProjectsDialog.h"
#include "SwarmController.h"
#include "SwarmDialog.h"
#include "WorktreePanel.h"
#include "SocratiCodePanel.h"
#include "TeamController.h"   // WhiteWh fork: agent teams
#include "TeamPanel.h"
#include "TeamNewDialog.h"
#include "TeamCatalogEditor.h"
#include "TeamWorkspace.h"
#include "TeamProposalCard.h"
#include "TeamDesignerPage.h"
#include "TeamWaitingPage.h"
#include "ModelServersPage.h"
#include "ModelServerSettings.h"
#include "TeamWaitingBanner.h"
#include "TeamNewTab.h"
#include <memory>
#include "Extensions.h"
#include "WindowTitleBar.h"
#include "IronRulesPanel.h"
#include "InspectorCards.h"
#include "SessionLeadPanel.h"
#include "SessionLeads.h"
#include "WorkingLine.h"
#include "HeldInputBar.h"
#include "TerminalDraftBar.h"
#include "MediaViewer.h"
#include "NewSessionDialog.h"
#include "SessionCreationController.h"
#include <QPlainTextEdit>
#include "ActivityView.h"
#include "ProcessView.h"
#include "TasksView.h"
#include "TaskInspector.h"
#include "AttachmentViewer.h"
#include "NativeBrowserPlugin.h"
#include "NativeUiView.h"
#include "MessageComposer.h"
#include "OpenCodeModels.h"
#include "speech/SpeechComposer.h"
#include "QuestionCard.h"
#include "RecoveryWidgets.h"
#include "SettingsPage.h"
#include "Shortcuts.h"
#include "ContentScale.h"
#include "ActivityWidth.h"
#include "UpdateController.h"
#include "ForkLog.h"
#include "WorkspaceSkin.h"
#include <QFileSystemWatcher>
#include "ProcessSettings.h"
#include "TerminalView.h"
#include "TerminalScreen.h"
#include "SessionList.h"
#include "WorkspaceIcons.h"
#include "WorkspaceStyle.h"
#include "BusyIndicator.h"
#include "StatusMessage.h"
#include "WorkspaceFocus.h"
#include "WindowLayer.h"
#include "SwarmButton.h"
#include "MachinesPage.h"
#include "MachineFilter.h"
#include "ProjectFilter.h"
#include "AccountFilter.h"
#include "MachineAppearance.h"
#include "MachinePlatform.h"
#include "LocalLaunchWarning.h"
#include "DashboardPage.h"
#include "AccountsPage.h"
#include "SessionSearch.h"
#include "SessionFileReference.h"
#include "SessionGoal.h"
#include <cmath>

#include <QApplication>
#include <QCloseEvent>
#include <QDateTime>
#include <QDesktopServices>
#include <QFileInfo>
#include "PathProbe.h"
#include <QFileDialog>
#include <QStandardPaths>
#include <QProgressBar>
#include <QDialog>
#include <QDialogButtonBox>
#include <QComboBox>
#include <QCheckBox>
#include <QSlider>
#include <QInputDialog>
#include <QWidgetAction>
#include <QFontDatabase>
#include <QHBoxLayout>
#include <QGridLayout>
#include <QJsonDocument>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMenu>
#include <QMessageBox>
#include <QProcess>
#include <QPainter>
#include <QPushButton>
#include <QScrollBar>
#include <QScopedValueRollback>
#include <QScreen>
#include <QSettings>
#include <QShortcut>
#include <QSplitter>
#include <QSplitterHandle>
#include <QMouseEvent>
#include <QStackedWidget>
#include <QStyledItemDelegate>
#include <QStyle>
#include <QTabWidget>
#include <QTabBar>
#include <QTextBrowser>
#include <QTextCursor>
#include <QTextDocument>
#include <QTextLayout>
#include <QTextTable>
#include <QToolTip>
#include <QVBoxLayout>
#include <QUuid>

#include <algorithm>

namespace {

QString fileReferenceContext(const SessionInfo &session, const QJsonObject &details, const QString &childConversation = {})
{
    // Either source can first reveal a replacement run. Never let an older
    // accepted inspection conceal a newer listing identity while a dialog is open.
    return QString::fromUtf8(QJsonDocument(QJsonArray{session.runId, session.conversationId,
        details.value("run_id"), details.value("conversation_id"), childConversation}).toJson(QJsonDocument::Compact));
}

QJsonObject sendEvidence(const QJsonObject &data)
{
    QJsonObject result;
    for (const auto *field : {"run_id", "conversation_id", "runtime_state", "process_state", "pending_questions", "cache_hint",
            "agent", "backend", "external_protocol", "send_delivery", "external_cloud_control", "client_context",
            "tracked", "send_available", "send_supported"})
        result.insert(QString::fromLatin1(field), data.value(QString::fromLatin1(field)));
    result.insert("session_usage", QJsonObject{{"prompt_cache", data.value("session_usage").toObject().value("prompt_cache")}});
    return result;
}

bool observedIdentityMatches(const SessionInfo &current, const SessionInfo &original,
                             const QString &run, const QString &conversation)
{
    // Inspection is authoritative and may precede the next fleet refresh.
    // Accept that refresh catching up, while refusing a third incarnation.
    return (current.runId == original.runId && current.conversationId == original.conversationId)
        || (current.runId == run && current.conversationId == conversation);
}

enum Roles { KeyRole = SessionRoles::Key, TitleRole, MetaRole, StatusRole, DetailRole, AgentRole, HostRole, ChildrenRole };

class SessionIdentityLabel : public QLabel {
public:
    using QLabel::QLabel;
    QSize sizeHint() const override {
        // QLabel's wrapped size hint may prefer two lines even when one fits.
        // Prefer the complete ID; heightForWidth still handles a narrower pane.
        const auto insets = contentsMargins();
        return {fontMetrics().horizontalAdvance(text()) + insets.left() + insets.right() + 2 * margin(),
                fontMetrics().height() + insets.top() + insets.bottom() + 2 * margin()};
    }
protected:
    // #542: on a line too short for the whole ID, its middle elides instead
    // of the end being clipped; selection works on the whole ID again once it fits.
    void paintEvent(QPaintEvent *event) override {
        const auto area = contentsRect().adjusted(margin(), margin(), -margin(), -margin());
        if (fontMetrics().horizontalAdvance(text()) <= area.width()) { QLabel::paintEvent(event); return; }
        QPainter painter(this); painter.setPen(palette().color(foregroundRole())); painter.setFont(font());
        painter.drawText(area, Qt::AlignLeft | Qt::AlignVCenter, fontMetrics().elidedText(text(), Qt::ElideMiddle, area.width()));
    }
};

QColor tone(const QString &status, bool dark)
{
    if (status == "Needs approval" || status == "Needs input" || status == "Sign in") return QColor(dark ? "#f0c77b" : "#885400");
    if (status == "Error") return QColor(dark ? "#ff9298" : "#b52b3d");
    if (status == "Background") return QColor(dark ? "#aac0df" : "#466b99");
    if (status == "Working" || status == "Using a tool" || status == "Compacting") return QColor(dark ? "#55e39a" : "#12834e");
    if (status == "Ready") return QColor(dark ? "#78dabb" : "#11775d");
    if (status == "Paused") return QColor(dark ? "#b9a5ed" : "#7051a4");
    return QColor(dark ? "#9aa4b2" : "#646f7e");
}

using SessionPresentation::displayTitle;
using SessionPresentation::sessionLabel;
using SessionPresentation::projectContext;
using SessionPresentation::currentActivity;
using SessionPresentation::currentAction;
using SessionPresentation::scheduled;
using SessionPresentation::childCount;


QString eventTitle(const QString &type)
{
    static const QMap<QString, QString> titles{
        {"SessionStart", "Session started"}, {"SessionEnd", "Session closed"},
        {"UserPromptSubmit", "You sent a message"}, {"TurnStarted", "Turn started"},
        {"UserPromptQueued", "Message queued"}, {"PreToolUse", "Tool started"},
        {"PostToolUse", "Tool finished"}, {"PostToolUseFailure", "Tool failed"},
        {"PermissionRequest", "Approval requested"}, {"PermissionResult", "Approval resolved"},
        {"Stop", "Response finished"}, {"StopFailure", "Response failed"},
        {"Interrupt", "Turn interrupted"}, {"PreCompact", "Compacting context"},
        {"PostCompact", "Context compacted"}, {"SubagentStart", "Subagent started"},
        {"SubagentStop", "Subagent finished"}, {"TaskStarted", "Background task started"},
        {"Notification", "Agent notification"}};
    return titles.value(type, type);
}

QString clockText(double seconds)
{
    return QDateTime::fromSecsSinceEpoch(qint64(seconds)).toLocalTime().toString("HH:mm:ss");
}

QString htmlText(const QString &text)
{
    return text.toHtmlEscaped().replace('\n', "<br>");
}

SessionTooltip sessionTooltip(const QStringList &fields, int width)
{
    return sessionTooltip(fields, width, QToolTip::font());
}

void updateBrowser(QTextBrowser *browser, const QString &html, QString &previous, bool preservePosition = true)
{
    if (html == previous && preservePosition) return;
    previous = html;
    const int scroll = preservePosition ? browser->verticalScrollBar()->value() : 0;
    const auto cursor = browser->textCursor();
    const auto selection = preservePosition ? cursor.selectedText() : QString();
    const bool updatesEnabled = browser->updatesEnabled();
    browser->setUpdatesEnabled(false);
    browser->setHtml(html);
    if (!selection.isEmpty()) {
        // Counters above the selected field can change length. Restore the
        // nearest surviving occurrence instead of stale character offsets.
        QTextCursor found(browser->document()), closest;
        while (!(found = browser->document()->find(selection, found)).isNull())
            if (closest.isNull() || qAbs(found.selectionStart() - cursor.selectionStart())
                < qAbs(closest.selectionStart() - cursor.selectionStart())) closest = found;
        if (!closest.isNull()) {
            if (cursor.position() < cursor.anchor()) {
                const int start = closest.selectionStart();
                closest.setPosition(closest.selectionEnd());
                closest.setPosition(start, QTextCursor::KeepAnchor);
            }
            browser->setTextCursor(closest);
        }
    }
    browser->verticalScrollBar()->setValue(scroll);
    browser->setUpdatesEnabled(updatesEnabled);
}


QLabel *label(const QString &text, const char *name, QWidget *parent = nullptr)
{
    auto *l = new QLabel(text, parent); l->setObjectName(QLatin1String(name));
    l->setTextFormat(Qt::PlainText); return l;
}
}

QString SessionsWindow::status(const SessionInfo &s, bool reachable)
{
    return SessionPresentation::status(s, reachable);
}

SessionsWindow::SessionsWindow(const QString &hgsPath, QWidget *parent)
    : QWidget(parent), m_client(hgsPath, this)
{
    WorkspaceFocus::install();
    const QString version = QCoreApplication::applicationVersion();
    setObjectName("sessionsWindow"); setWindowTitle(QStringLiteral("Zerus"));
    resize(1200, 790); setMinimumSize(840, 560);
    QSettings settings;
    const auto expanded = settings.value("workspace/expandedSessions").toStringList();
    m_expandedSessions = QSet<QString>(expanded.begin(), expanded.end());
    m_sessionRowPreferences = SessionRowPreferences(settings.value("workspace/collapsedSessionRows").toByteArray());
    m_savedOrganization = SettingsJson::bytes("workspace/organization");
    const auto savedProjects = SettingsJson::object("workspace/organization");
    if (!savedProjects.isEmpty() && !savedProjects.contains("projects") && !settings.contains("workspace/organizationBeforeProjects"))
        Settings::set("workspace/organizationBeforeProjects", m_savedOrganization);
    m_organization = SessionOrganization(savedProjects);
    m_pendingLaunches = SettingsJson::object("workspace/pendingLaunches");
    m_creations=new SessionCreationController(m_client.executable(),this);
    m_confirmPoll=new QTimer(this);m_confirmPoll->setInterval(2000);connect(m_confirmPoll,&QTimer::timeout,this,&SessionsWindow::pollConfirmingCreations);
    if (!settings.value("sessions/geometry").toByteArray().isEmpty()) restoreGeometry(settings.value("sessions/geometry").toByteArray());
    // WhiteWh fork: a skin may put its own title bar above the rail and the pages.
    auto *frame = new QVBoxLayout(this); frame->setContentsMargins(0, 0, 0, 0); frame->setSpacing(0);
    m_titleBar = new WindowTitleBar; m_titleBar->hide(); frame->addWidget(m_titleBar);
    m_titleBar->search = [this] { openGoTo(); };
    auto *root = new QHBoxLayout; root->setContentsMargins(0, 0, 0, 0); root->setSpacing(0); frame->addLayout(root, 1);
    auto *sidebar = m_sidebar = new QWidget; sidebar->setObjectName("sidebar"); sidebar->setFixedWidth(56);
    auto *side = m_railLayout = new QVBoxLayout(sidebar); side->setContentsMargins(7, 10, 7, 10); side->setSpacing(8);
    auto iconButton = [](const QString &glyph, const QString &caption, const char *name) {
        auto *button = new QPushButton; button->setObjectName(QLatin1String(name));
        button->setProperty("glyph", glyph); button->setToolTip(caption); button->setAccessibleName(caption);
        button->setIconSize(QSize(20, 20)); button->setFixedSize(34, 32); return button;
    };
    m_brand = new SwarmButton; m_brand->setObjectName("brandMark");
    m_brand->setFixedSize(42,44); m_brand->setCheckable(true); m_brand->setProperty("caption", tr("Home — Zerus %1").arg(version)); m_brand->setAccessibleName(tr("Home dashboard"));
    connect(m_brand,&QPushButton::clicked,this,[this] { m_pages->setCurrentIndex(3); m_brand->setChecked(true); emit refreshRequested(); });
    side->addWidget(m_brand); side->addSpacing(8);
    // WhiteWh fork: the cards view collapses and expands the session list here.
    m_railSessionsToggle = iconButton("collapse-sessions", tr("Collapse session list"), "railSessionsToggle");
    m_railSessionsToggle->setFixedSize(42, 36); m_railSessionsToggle->hide(); side->addWidget(m_railSessionsToggle);
    connect(m_railSessionsToggle, &QPushButton::clicked, this, [this] { if (m_sessionDock) m_sessionDock->toggle(); });
    auto *create = iconButton("add", tr("New session"), "newSession");
    create->setFixedSize(42,44);create->setIconSize(QSize(24,24));side->addWidget(create); m_railNewSession = create;
    connect(create, &QPushButton::clicked, this, [this]() { showNewSession({}, {}, selectedHostTarget()); });
    m_sessionsNav = iconButton("sessions", tr("Sessions"), "railButton"); m_sessionsNav->setCheckable(true); m_sessionsNav->setChecked(true);
    m_projectsNav = iconButton("projects", tr("Projects"), "railButton"); m_projectsNav->setCheckable(true);
    m_machinesNav = iconButton("machines", tr("Machines and connection status"), "railButton"); m_machinesNav->setCheckable(true);
    m_accountsNav = iconButton("accounts", tr("Agent accounts"), "railButton"); m_accountsNav->setCheckable(true);
    connect(m_accountsNav, &QPushButton::clicked, this, [this] { m_pages->setCurrentIndex(4); m_accountsNav->setChecked(true); });
    // WhiteWh fork: agent teams. The Team designer mode: the universal catalog.
    m_teamDesignerNav = iconButton("team", tr("Team designer"), "railButton"); m_teamDesignerNav->setProperty("teamDesignerNav", true); m_teamDesignerNav->setCheckable(true);
    connect(m_teamDesignerNav, &QPushButton::clicked, this, [this] { showTeamDesigner(); });
    // U50: what waits for you across every team, beside the Team designer.
    m_teamWaitingNav = iconButton("inbox", tr("Waiting for you"), "railButton"); m_teamWaitingNav->setProperty("teamWaitingNav", true); m_teamWaitingNav->setCheckable(true);
    connect(m_teamWaitingNav, &QPushButton::clicked, this, [this] { showTeamWaiting(); });
    // U60 (#656): Model servers, only while this computer has one.
    m_modelServersNav = iconButton("server", tr("Model servers"), "railButton"); m_modelServersNav->setProperty("modelServersNav", true); m_modelServersNav->setCheckable(true);
    connect(m_modelServersNav, &QPushButton::clicked, this, [this] { showModelServers(); });
    for (auto *button : {m_sessionsNav, m_projectsNav, m_machinesNav, m_accountsNav, m_teamDesignerNav, m_teamWaitingNav, m_modelServersNav}) { button->setFixedSize(42, 44); button->setIconSize(QSize(24, 24)); side->addWidget(button); }
    m_modelServersNav->hide();
    m_attentionBadge = new QLabel(m_sessionsNav); m_attentionBadge->setObjectName("railAttentionBadge");
    m_attentionBadge->setAlignment(Qt::AlignCenter); m_attentionBadge->setAttribute(Qt::WA_TransparentForMouseEvents);
    m_attentionBadge->setGeometry(24, 3, 17, 16); m_attentionBadge->hide();
    connect(m_sessionsNav, &QPushButton::clicked, this, [this] {
        if (m_pages->currentIndex() == 0) { m_filter = "all"; m_folderFilterPath.clear(); m_hostFilters.clear(); m_projectFilters.clear(); m_accountFilters.clear(); m_search->clear(); }
        showSessionList();
    });
    connect(m_projectsNav, &QPushButton::clicked, this, [this]() { showProjects(selectedHostTarget()); });
    connect(m_machinesNav, &QPushButton::clicked, this, [this]() {
        m_pages->setCurrentIndex(2); m_machinesNav->setChecked(true); m_sessionsNav->setChecked(false); m_projectsNav->setChecked(false);
    });
    side->addStretch();
    auto *windowPin = m_windowPin = iconButton("unpinned", tr("Keep Zerus above other windows"), "windowPin"); windowPin->setCheckable(true);
    windowPin->setFixedSize(42, 44); windowPin->setIconSize(QSize(24,24)); side->addWidget(windowPin);
    auto *settingsButton = m_settingsNav = iconButton("settings", tr("Settings"), "workspaceSettings"); settingsButton->setCheckable(true); settingsButton->setFixedSize(42, 44); settingsButton->setIconSize(QSize(24,24)); side->addWidget(settingsButton);
    connect(settingsButton, &QPushButton::clicked, this, &SessionsWindow::showWorkspaceSettings);
    auto *versionButton = m_versionButton = new QPushButton(UpdateController::buildInfo()["versionName"].toString().section('-', 0, 0));
    versionButton->setObjectName("appVersion"); versionButton->setFixedSize(42, 24);
    versionButton->setCursor(Qt::PointingHandCursor); versionButton->setAutoDefault(false);
    versionButton->setAccessibleName(tr("About Zerus"));
    versionButton->setToolTip(tr("Zerus %1\nOpen About").arg(UpdateController::installedVersion()));
    versionButton->setAccessibleDescription(versionButton->toolTip()); side->addWidget(versionButton);
    connect(versionButton, &QPushButton::clicked, this, [this] { showWorkspaceSettings(); m_settingsPage->openAbout(); });
    // The stylesheet paints each rail button's full rect. macOS layout margins
    // (4 px above, 8 px below a push button) would let neighbours overlap.
    for (int i = 0; i < side->count(); ++i)
        if (auto *widget = side->itemAt(i)->widget()) widget->setAttribute(Qt::WA_LayoutUsesWidgetRect);
    root->addWidget(sidebar);

    auto *body = new QWidget; auto *layout = new QVBoxLayout(body);
    layout->setContentsMargins(8, 8, 8, 8); layout->setSpacing(6);
    m_splitter = new QSplitter; m_splitter->setChildrenCollapsible(false); m_splitter->setHandleWidth(8);
    auto *listPanel = new QWidget; listPanel->setObjectName("sessionListPanel"); listPanel->setMinimumWidth(270);
    auto *listLayout = new QVBoxLayout(listPanel); listLayout->setContentsMargins(4, 2, 4, 0); listLayout->setSpacing(7);
    auto *header = new QHBoxLayout; header->setSpacing(4);
    m_heading = label(tr("Sessions"), "heading"); header->addWidget(m_heading, 1);
    auto *groupsMenu = new QMenu(listPanel); groupsMenu->setObjectName("sessionProjectsMenu");
    // Unhandled context events from the margins, filter chips and other panel
    // children bubble here. Text fields and session rows keep their own menus.
    listPanel->setContextMenuPolicy(Qt::CustomContextMenu);
    connect(listPanel, &QWidget::customContextMenuRequested, this, [listPanel, groupsMenu](const QPoint &position) {
        groupsMenu->popup(listPanel->mapToGlobal(position));
    });
    auto *newSession = groupsMenu->addAction(tr("New session…"));
    connect(newSession, &QAction::triggered, this, [this]() { showNewSession({}, {}, selectedHostTarget()); });
    auto *newGroup = groupsMenu->addAction(tr("New project…")); newGroup->setObjectName("createSessionGroup");
    connect(newGroup, &QAction::triggered, this, [this]() { createGroup(); });
    // WhiteWh fork: agent teams.
    auto *newTeam = groupsMenu->addAction(tr("New team…")); newTeam->setObjectName("newTeam");
    connect(newTeam, &QAction::triggered, this, [this]() { showNewTeam(); });
    auto *designer = groupsMenu->addAction(tr("Team designer…")); designer->setObjectName("teamDesigner");
    connect(designer, &QAction::triggered, this, [this]() { showTeamDesigner(); });
    groupsMenu->addSeparator();
    for (bool collapse : {false, true}) connect(groupsMenu->addAction(collapse ? tr("Collapse all projects") : tr("Expand all projects")), &QAction::triggered, this, [this, collapse]() {
        const auto groups = m_organization.groups(); for (const auto &g : groups) m_organization.setCollapsed(g.id, collapse);
        saveOrganization(); rebuild();
    });
    m_expandAllRowsAction = new QAction(tr("Expand all collapsed"), this); m_expandAllRowsAction->setObjectName("expandAllCollapsedSessionRows");
    m_expandAllRowsAction->setToolTip(tr("Restore full session rows, including sessions hidden by filters"));
    m_expandAllRowsAction->setEnabled(!m_sessionRowPreferences.empty());
    connect(m_expandAllRowsAction, &QAction::triggered, this, [this] {
        m_sessionRowPreferences.expandAll(); saveSessionRowPreferences(); updateSessionRowPresentation();
    });
    groupsMenu->addAction(m_expandAllRowsAction);
    m_markAllReadAction = new QAction(tr("Mark all as read"), this); m_markAllReadAction->setObjectName("markAllSessionsRead");
    m_markAllReadAction->setToolTip(tr("Mark current replies across all machines as read, including hidden sessions. Pending approvals, questions and errors stay highlighted. Sessions you marked as needing attention keep their reply unread."));
    m_markAllReadAction->setEnabled(false);
    connect(m_markAllReadAction, &QAction::triggered, this, &SessionsWindow::markAllRepliesRead);
    // Right-clicking the Sessions icon in the rail offers the same action.
    auto *railMenu = new QMenu(m_sessionsNav); railMenu->setObjectName("sessionsNavMenu"); railMenu->setToolTipsVisible(true);
    railMenu->addAction(m_markAllReadAction); railMenu->addAction(m_expandAllRowsAction); m_sessionsNav->setContextMenuPolicy(Qt::CustomContextMenu);
    connect(m_sessionsNav, &QWidget::customContextMenuRequested, railMenu, [this, railMenu](const QPoint &at) { railMenu->popup(m_sessionsNav->mapToGlobal(at)); });
    groupsMenu->addSeparator(); groupsMenu->addAction(m_markAllReadAction); groupsMenu->setToolTipsVisible(true);
    m_clearArchiveAction = new QAction(tr("Clear archive…"), this); m_clearArchiveAction->setObjectName("clearArchive");
    connect(m_clearArchiveAction, &QAction::triggered, this, &SessionsWindow::clearArchive);
    groupsMenu->addAction(m_clearArchiveAction);
    auto *batchButton = iconButton("more", tr("Session and machine actions"), "batchActions");
    auto *batchMenu = new QMenu(batchButton); batchButton->setMenu(batchMenu); batchMenu->setToolTipsVisible(true);
    connect(batchMenu, &QMenu::aboutToShow, this, [this, batchMenu]() { populateBatchActions(batchMenu); });
    m_panelNewSession = iconButton("add", tr("New session"), "sessionPanelNewSession"); header->addWidget(m_panelNewSession);
    connect(m_panelNewSession, &QPushButton::clicked, this, [this]() { showNewSession({}, {}, selectedHostTarget()); });
    m_sessionsToggle = iconButton("collapse-sessions", tr("Collapse session list"), "sessionPanelToggle"); header->addWidget(m_sessionsToggle);
    header->addWidget(batchButton); m_batchButton = batchButton; m_sessionHeader = header;
    listLayout->addLayout(header);
    m_search = new QLineEdit; m_search->setObjectName("search"); m_search->setPlaceholderText(tr("Search sessions, IDs and messages…"));
    m_search->setClearButtonEnabled(true); m_search->setAccessibleName(tr("Search sessions"));
    connect(m_search, &QLineEdit::textChanged, this, [this]() {
        m_revealCreation.clear();m_creationSessionToSelect.clear();
        m_archiveSearchRows.clear();
        m_archiveBrowser->setQuery(m_search->text());
        m_searchResults->setQuery(m_filter=="archived"?QString():m_search->text());
        m_listStack->setCurrentIndex(m_searchResults->active() ? 1 : 0);
        if (!m_searchResults->active()) m_activityView->clearSearchResult();
        rebuild();
    }); listLayout->addWidget(m_search);
    // In the strip, search opens the panel over the conversation with focus in the field.
    m_stripSearch = iconButton("search", tr("Search sessions"), "sessionStripSearch"); m_stripSearch->hide();
    auto *stripSearchRow = new QHBoxLayout; stripSearchRow->setContentsMargins(SessionStrip::tileColumn());
    stripSearchRow->addWidget(m_stripSearch, 0, Qt::AlignHCenter); listLayout->addLayout(stripSearchRow);
    connect(m_stripSearch, &QPushButton::clicked, this, [this] { m_focusSearch = true; m_sessionDock->peek(); });
    connect(m_search, &QLineEdit::textEdited, this, [this]() { m_restoreKey.clear(); m_renameKey.clear(); });
    auto *filterRow = m_filterRow = new QHBoxLayout; filterRow->setSpacing(3);
    const QList<QPair<QString, QString>> filters{{"all", tr("All sessions")}, {"attention", tr("Needs attention")},
        {"active", tr("Active")}, {"paused", tr("Saved sessions")}, {"archived", tr("Archive")}};
    for (const auto &pair : filters) {
        auto *button = new QPushButton; button->setObjectName("sessionFilter"); button->setProperty("glyph", pair.first);
        button->setProperty("filter", pair.first); button->setProperty("caption", pair.second); button->setIconSize(QSize(16, 16));
        button->setCheckable(true); button->setChecked(pair.first == "all"); button->setAccessibleName(pair.second);
        filterRow->addWidget(button, 1); m_filters.append(button);
        connect(button, &QPushButton::clicked, this, [this, id = pair.first]() {
            m_revealCreation.clear();m_creationSessionToSelect.clear();
            m_restoreKey.clear(); m_renameKey.clear(); m_pages->setCurrentIndex(0); m_projectsNav->setChecked(false); m_filter = id; m_searchResults->setQuery(id=="archived"?QString():m_search->text());m_listStack->setCurrentIndex(m_searchResults->active()?1:0);if(id=="archived")m_archiveBrowser->setQuery(m_search->text()); if(id=="all")m_folderFilterPath.clear(); rebuild();
        });
    }
    m_machineFilter = new MachineFilter;
    m_projectFilter = new ProjectFilter; m_accountFilter = new AccountFilter;
    filterRow->addWidget(m_machineFilter->button()); filterRow->addWidget(m_projectFilter->button()); filterRow->addWidget(m_accountFilter->button());
    listLayout->addLayout(filterRow); listLayout->addWidget(m_machineFilter); listLayout->addWidget(m_projectFilter); listLayout->addWidget(m_accountFilter);
    connect(m_projectFilter, &ProjectFilter::selectionChanged, this, [this](const QSet<QString> &projects) { m_revealCreation.clear();m_creationSessionToSelect.clear();m_projectFilters = projects; rebuild(); });
    connect(m_accountFilter, &AccountFilter::selectionChanged, this, [this](const QSet<QString> &accounts) { m_revealCreation.clear();m_creationSessionToSelect.clear();m_accountFilters = accounts; rebuild(); });
    connect(m_accountFilter, &AccountFilter::opened, this, [this] { m_accountsPage->ensureCatalogs(true); });
    connect(m_machineFilter, &MachineFilter::selectionChanged, this, [this](const QSet<QString> &hosts) { m_revealCreation.clear();m_creationSessionToSelect.clear();m_hostFilters = hosts; rebuild(); });
    m_archiveBrowser=new ArchiveBrowser(m_client,this);listLayout->addWidget(m_archiveBrowser);
    connect(m_archiveBrowser,&ArchiveBrowser::changed,this,[this]{if(m_filter=="archived")rebuild();});
    connect(m_archiveBrowser,&ArchiveBrowser::windowChanged,this,[this]{if(m_filter=="archived")m_sessions->scrollToTop();});
    m_folderFilterClear=new QPushButton;m_folderFilterClear->setObjectName("clearFolderFilter");m_folderFilterClear->hide();m_folderFilterClear->setAutoDefault(false);
    connect(m_folderFilterClear,&QPushButton::clicked,this,[this]{m_folderFilterPath.clear();rebuild();});listLayout->addWidget(m_folderFilterClear);
    // Holds the height of filter rows the strip hides, so the list does not move.
    m_stripSpacer = new QWidget; m_stripSpacer->hide(); listLayout->addWidget(m_stripSpacer);
    m_sessions = new SessionList; m_sessions->setObjectName("sessionList"); m_sessions->setMinimumWidth(250);
    m_sessions->setProperty("compact", settings.value("workspace/compact", true).toBool());
    m_sessions->setItemDelegate(new SessionDelegate(m_sessions)); m_sessions->setMouseTracking(true);
    m_sessions->setVerticalScrollMode(QAbstractItemView::ScrollPerPixel); m_sessions->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_sessions->setAccessibleName(tr("Agent sessions; Ctrl-click to select several; drag to organize, left/right to collapse or expand a group"));
    setupSelectionActions();
    m_listStack = new QStackedWidget; m_listStack->addWidget(m_sessions);
    m_searchResults = new SessionSearch(hgsPath); m_listStack->addWidget(m_searchResults); listLayout->addWidget(m_listStack, 1);
    connect(m_searchResults,&SessionSearch::archiveSessionsReady,this,[this](const QString &host,const QJsonArray &rows){m_archiveSearchRows[host]=HgsClient::parseSessions(rows);rebuild();});
    connect(m_searchResults, &SessionSearch::resultActivated, this,
        [this](const QString &host, const QString &name, const QString &archive, const QString &run, const QJsonObject &event, const QString &query) {
            const QString key = host + '\n' + name + (archive.isEmpty() ? QString() : '\n' + archive);
            const auto found = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &e) { return e.key == key; });
            if (found == m_entries.cend() || (!run.isEmpty() && found->session.runId != run)) {
                showNotice(tr("This session changed. Search again to see its current history."), true); return;
            }
            pushSelectionChanged(key);
            // The search list owns selection while it is shown, including archive
            // hits. Keep the regular list's selection in sync for normal navigation.
            m_selectedKey.clear();
            for (int i = 0; i < m_sessions->count(); ++i) if (m_sessions->item(i)->data(KeyRole).toString() == key) {
                const QSignalBlocker block(m_sessions); m_sessions->setCurrentRow(i); break;
            }
            closeSubagent();
        m_selectedKey = key; m_details = {}; m_events = {}; m_cursor = 0; m_inspectError.clear(); m_childrenHtml.clear();
        // A pending Team-tab Stop belongs to the session it named.
        m_terminateAfterInspect.clear();
        if (!m_restoredInspections.contains(key)) m_restoredInspections.clear();
            m_activityView->setSessionKey(key); m_composer->setSessionKey(key);
            m_terminal->setSession(m_client.executable(), host, name);
            m_detailTabs->setCurrentIndex(0); inspect(); renderDetails();
            if (!event.isEmpty()) m_activityView->showSearchResult(event, query);
            else m_activityView->clearSearchResult();
        });
    connect(m_searchResults,&SessionSearch::creationActivated,this,[this](const QString &id){
        selectCreation(id);
    });
    m_count = label({}, "listSummary"); m_count->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    auto *summary = new QHBoxLayout; summary->addWidget(m_count, 1);
    auto *creationStatus=iconButton("clock",tr("Session creation status"),"creationStatus");summary->addWidget(creationStatus);
    connect(creationStatus,&QPushButton::clicked,this,[this,creationStatus]{
        auto *menu=new QMenu(this);const auto operations=m_creations->operations();
        auto ids=operations.keys();std::sort(ids.begin(),ids.end(),[&](const QString &a,const QString &b){const auto left=operations[a].toObject()["created"].toInteger(),right=operations[b].toObject()["created"].toInteger();return left==right?a<b:left>right;});
        auto *page=menu;int count=0;
        for(const auto &id:ids){
            const auto value=operations[id].toObject();if(value["state"]=="bound")continue;
            if(count&&count%50==0)page=page->addMenu(tr("Older creations…"));++count;
            auto *action=page->addAction(value["tag"].toString()+" · "+SessionCreationController::caption(value));
            connect(action,&QAction::triggered,this,[this,id]{
                if(!m_creations->owns(id)||m_creations->operation(id)["state"]=="bound"){showNotice(tr("This creation has already been resolved."));return;}
                m_revealCreation=id;m_creationSessionToSelect.clear();selectCreation(id);
                m_creations->show(id);m_listStack->setCurrentIndex(0);showSessionList();rebuild();renderCreation();
            });
        }
        if(menu->isEmpty())menu->addAction(tr("No pending session creations"))->setEnabled(false);
        connect(menu,&QMenu::aboutToHide,menu,&QObject::deleteLater);menu->popup(creationStatus->mapToGlobal(QPoint(0,creationStatus->height())));
    });
    m_markAllReadButton = iconButton("read-all", tr("Mark all as read"), "markAllReadButton"); m_markAllReadButton->setAutoDefault(false);
    m_markAllReadButton->setEnabled(false); summary->addWidget(m_markAllReadButton);
    connect(m_markAllReadButton, &QPushButton::clicked, this, &SessionsWindow::markAllRepliesRead);
    auto *drafts = m_savedDrafts = iconButton("drafts", tr("Saved drafts"), "savedDrafts"); drafts->setAutoDefault(false);
    drafts->setToolTip(tr("Saved drafts: recover local drafts, including those from ended sessions")); summary->addWidget(drafts);
    connect(drafts, &QPushButton::clicked, this, [this] {
        (m_subagentId.isEmpty() ? m_composer : m_subagentComposer)->showSavedDrafts();
    }); listLayout->addLayout(summary);
    // The panel floats over this placeholder so it can collapse into a strip and
    // grow over the conversation without resizing it.
    auto *sessionSlot = new QWidget; sessionSlot->setObjectName("sessionListSlot"); sessionSlot->setMinimumWidth(listPanel->minimumWidth());
    m_splitter->addWidget(sessionSlot); listPanel->setParent(body); listPanel->setAttribute(Qt::WA_StyledBackground);
    m_sessions->installEventFilter(this);
    m_sessions->setContextMenuPolicy(Qt::CustomContextMenu);
    m_sessionMenu = new QMenu(this); m_sessionMenu->setObjectName("sessionContextMenu"); m_sessionMenu->setToolTipsVisible(true);
    connect(m_sessions, &QWidget::customContextMenuRequested, this, [this, groupsMenu](const QPoint &position) {
        const auto *item = m_sessions->itemAt(position);
        const auto global = m_sessions->viewport()->mapToGlobal(position);
        if (selectionEntries().size() > 1) { showSelectionMenu(global); return; }
        if (item && item->data(SessionRoles::Header).toBool()) showGroupMenu(item->data(SessionRoles::Group).toString(), global);
        else if (item && !item->data(SessionRoles::LaunchId).toString().isEmpty()) {
            const auto id = item->data(SessionRoles::LaunchId).toString();
            auto *menu = new QMenu(this); menu->setObjectName("pendingLaunchMenu");
            auto *dismiss = menu->addAction(tr("Hide launch status"));
            connect(dismiss, &QAction::triggered, this, [this, id] {
                if(m_creations->owns(id)){m_creations->hide(id);return;}
                if (!m_pendingLaunches.contains(id)) return;
                auto launch = m_pendingLaunches.value(id).toObject(); launch["show_pending"] = false;
                m_pendingLaunches[id] = launch; savePendingLaunches(); rebuild();
            });
            connect(menu, &QMenu::aboutToHide, menu, &QObject::deleteLater); menu->popup(global);
        }
        else if (item && !item->data(SessionRoles::ChildId).toString().isEmpty()) return;
        else if (item && sessionCardRect(m_sessions->visualItemRect(item)).contains(position)) showSessionMenu(item->data(KeyRole).toString(), global);
        else groupsMenu->popup(global);
    });
    connect(m_sessions, &SessionList::groupToggled, this, [this](const QString &id) {
        if (const auto *group = m_organization.group(id)) m_organization.setCollapsed(id, !group->collapsed);
        saveOrganization(); rebuild();
    });
    connect(m_sessions, &SessionList::childrenToggled, this, [this](const QString &key) {
        // A team's lead folds and unfolds its members (and its subagents with them).
        if (m_teams) for (const auto &entry : std::as_const(m_entries)) if (entry.key == key) {
            const auto *team = m_teams->teamOfSession(entry.host, entry.session.name);
            const auto *member = team ? team->memberOfSession(entry.host, entry.session.name) : nullptr;
            if (!member || !member->lead || team->status == "archived") break;
            const bool fold = !m_collapsedTeams.contains(team->id);
            if (fold) { m_collapsedTeams.insert(team->id); m_expandedSessions.remove(key); }
            else m_collapsedTeams.remove(team->id);
            Settings::set("workspace/collapsedTeams", QStringList(m_collapsedTeams.cbegin(), m_collapsedTeams.cend()));
            Settings::set("workspace/expandedSessions", QStringList(m_expandedSessions.begin(), m_expandedSessions.end()));
            rebuild(); return;
        }
        if (m_expandedSessions.contains(key)) {
            m_expandedSessions.remove(key);
            if (m_selectedKey == key && !m_subagentId.isEmpty()) closeSubagent();
        } else m_expandedSessions.insert(key);
        Settings::set("workspace/expandedSessions", QStringList(m_expandedSessions.begin(), m_expandedSessions.end()));
        rebuild();
    });
    connect(m_sessions, &SessionList::rowExpanded, this, [this](const QString &key) {
        const auto found = std::find_if(m_entries.cbegin(), m_entries.cend(), [&key](const Entry &e) { return e.key == key; });
        if (found != m_entries.cend()) setSessionRowCollapsed(*found, false);
    });
    connect(m_sessions, &SessionList::rowCompacted, this, [this](const QString &key) {
        const auto found = std::find_if(m_entries.cbegin(), m_entries.cend(), [&key](const Entry &e) { return e.key == key; });
        if (found != m_entries.cend()) setSessionRowCollapsed(*found, true);
    });
    connect(&m_client, &HgsClient::notificationsFinished, this, [this](quint64 request, bool ok, const QJsonObject &result, const QString &error) {
        const auto found = m_pendingNotifications.find(request); if (found == m_pendingNotifications.end()) return;
        const auto original = found->entry; const bool thenUnmute = found->thenUnmute; m_pendingNotifications.erase(found);
        const auto *box = original.host.isEmpty() ? &m_fleet.local() : m_fleet.peer(original.host);
        const bool current = box && std::any_of(box->sessions.cbegin(), box->sessions.cend(), [&](const SessionInfo &s) {
            return s.name == original.session.name && s.runId == original.session.runId
                && s.conversationId == original.session.conversationId && s.created == original.session.created && s.state != "archived";
        });
        if (!current) return; // A late answer cannot touch or refresh a replacement run.
        if (!ok) { showNotice(error, true); return; }
        const auto preferences = result.value("preferences").toObject();
        if (!m_fleet.applyNotificationPreferences(original.host, original.session, preferences)) {
            showNotice(tr("The shared notification setting changed while this request was pending. Refresh the session."), true); return;
        }
        rebuild(); updateDashboard();
        emit notificationPreferencesChanged(original.host, original.session.name, original.session.runId,
                                            original.session.conversationId, preferences);
        if (!thenUnmute) return;
        // The signal's receiver (TrayAgent) calls setFleet at once, which
        // replaces m_fleet and frees the box read above: read it again.
        box = original.host.isEmpty() ? &m_fleet.local() : m_fleet.peer(original.host);
        if (!box) return;
        for (const auto &s : box->sessions) if (s.name == original.session.name && s.runId == original.session.runId
                                                && s.conversationId == original.session.conversationId && s.state != "archived") {
            Entry next = original; next.session = s;
            const QJsonObject payload{{"run_id", s.runId}, {"conversation_id", s.conversationId}, {"muted", false},
                                      {"expected_revision", s.notificationPreferences.value("revision")}};
            m_pendingNotifications.insert(m_client.requestNotifications(next.host, s.name, payload), {next});
            return;
        }
    });
    connect(m_sessions, &SessionList::sessionMoved, this, [this](const QString &id, const QString &group, const QString &before) {
        m_organization.moveSession(id, group, before); saveOrganization(); rebuild();
    });
    connect(m_sessions, &SessionList::groupMoved, this, [this](const QString &id, const QString &before) {
        m_organization.moveGroup(id, before); saveOrganization(); rebuild();
    });
    connect(m_sessions, &SessionList::dragFinished, this, &SessionsWindow::rebuild);
    connect(m_sessions, &SessionList::taskDropped, this, [this](const TaskDrag &task, const QString &host, const QString &session) {
        m_tasks->retarget(task, host, session);
    });
    connect(m_sessions, &QListWidget::currentRowChanged, this, [this]() { if (!m_rebuilding) selectSession(); });
    connect(m_sessions, &QListWidget::itemActivated, this, &SessionsWindow::jumpToLatestActivity);

    m_detailStack = new QStackedWidget; m_detailStack->setObjectName("detail");
    auto *empty = new QWidget; auto *emptyLayout = new QVBoxLayout(empty); emptyLayout->addStretch();
    auto *emptyTitle = label(tr("A little more focus."), "emptyTitle"); emptyTitle->setAlignment(Qt::AlignCenter); emptyLayout->addWidget(emptyTitle);
    auto *emptyText = label(tr("Select a session to see what your agent is doing.\nNew sessions appear here automatically."), "muted");
    emptyText->setAlignment(Qt::AlignCenter); emptyText->setWordWrap(true); emptyLayout->addWidget(emptyText); emptyLayout->addStretch();
    m_detailStack->addWidget(empty);
    auto *details = new QWidget;details->setObjectName("sessionDetailPanel"); auto *detail = new QVBoxLayout(details); detail->setContentsMargins(12, 8, 12, 10); detail->setSpacing(4);
    auto *badges = new QHBoxLayout; badges->setSpacing(4);
    m_title = label({}, "detailTitle"); m_title->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred); m_title->setMinimumWidth(70);
    m_sessionLoading = new BusyIndicator(m_title); m_sessionLoading->setObjectName("sessionLoading");
    m_sessionLoading->setAccessibleName(tr("Loading activity…"));
    m_sessionLoadingTimer.setSingleShot(true);
    connect(&m_sessionLoadingTimer, &QTimer::timeout, this, &SessionsWindow::updateSessionLoading);
    connect(&m_client, &HgsClient::inspectionLoadingChanged, this,
        [this](const QString &host, const QString &name, const QString &archive, const QString &agent, bool loading) {
            // WhiteWh fork: local reads count too; a DeepSeek host can be slow.
            const auto key = host + '\n' + name + '\n' + archive + '\n' + agent;
            if (loading) {
                if (!m_sessionReads.contains(key)) {
                    SessionRead read{host, name, archive, agent, {}}; read.age.start(); m_sessionReads.insert(key, read);
                }
            } else m_sessionReads.remove(key);
            updateSessionLoading();
        });
    badges->addWidget(m_title, 1);
    // U25 (spec 3): a lead session's team status follows the title (TeamWorkspace::headerStatus).
    badges->addStretch(0); m_titleRow = badges;
    connect(&m_client,&HgsClient::sessionReferenceReady,this,[this](const QString &uri){emit copyTextRequested(uri);showNotice(tr("Session reference copied"));});
    connect(&m_client,&HgsClient::sessionReferenceFailed,this,[this](const QString &error){showNotice(error,true);});
    m_badge = label({}, "badge"); m_badge->setFixedHeight(24);
    m_badgeGlyph = new QLabel(m_badge); m_badgeGlyph->setObjectName("badgeGlyph"); m_badgeGlyph->setGeometry(9, 6, 12, 12);
    m_badgeGlyph->setAttribute(Qt::WA_TransparentForMouseEvents); m_badgeGlyph->hide();
    // Its own style: the pill's frame and padding must not reach the glyph.
    m_badgeGlyph->setStyleSheet(QStringLiteral("border:none; background:transparent; padding:0;"));
    badges->addWidget(m_badge, 0, Qt::AlignVCenter); badges->addSpacing(6);
    m_model = label({}, "sessionModel", details); m_model->setMaximumWidth(260);
    m_shell = iconButton("terminal", tr("Open shell in session folder"), "openFolderShellAction");
    connect(m_shell, &QPushButton::clicked, this, &SessionsWindow::openFolderShell);
    m_fileManager = iconButton("folder", tr("Open session folder in file manager"), "openSessionFolderAction");
    connect(m_fileManager, &QPushButton::clicked, this, &SessionsWindow::openSessionFolder);
    // #744: the session folder's state comes from PathProbe's worker; its answer
    // updates the button and an open menu's Open folder.
    connect(&PathProbe::instance(), &PathProbe::answered, this, [this](const QString &path) {
        if (!m_folderButtonPath.isEmpty() && path == m_folderButtonPath) showFolderState(m_fileManager, nullptr, true, path);
        if (m_menuFolderAction && path == m_menuFolderPath) showFolderState(nullptr, m_menuFolderAction, true, path);
    });
    // The header copies the same identity as Details; connect commands live in the menu.
    m_copyHeaderId = iconButton("copy", tr("Copy session ID"), "headerCopySessionIdAction");
    connect(m_copyHeaderId, &QPushButton::clicked, this, &SessionsWindow::copySessionIdentity);
    m_clear = iconButton("clear", tr("Clear session"), "clearSessionAction");
    connect(m_clear, &QPushButton::clicked, this, &SessionsWindow::clearContext);
    // Pause and resume live in the menu; only an archived session keeps a header Restore.
    m_restore = iconButton("play", tr("Restore session"), "restoreSessionAction"); m_restore->setProperty("archiveRestore", true);
    connect(m_restore, &QPushButton::clicked, this, &SessionsWindow::changeSession);
    badges->addWidget(m_fileManager); badges->addWidget(m_shell); badges->addWidget(m_copyHeaderId); badges->addWidget(m_clear); badges->addWidget(m_restore);
    m_more = iconButton("more", tr("Session actions"), "more");
    for (auto *button : {m_fileManager, m_shell, m_copyHeaderId, m_clear, m_restore, m_more}) {
        button->setProperty("sessionHeaderAction", true);
        configureWorkspaceIconButton(button, button->property("glyph").toString(), button->toolTip(), m_dark);
    }
    m_clear->setToolTip(tr("Clear session\nStart this agent's conversation from scratch, like /clear in Terminal. Your unsent draft is kept."));
    auto *moreMenu = new QMenu(m_more);
    // The menu repeats the header icons as named items: each one clicks its icon,
    // so it shares the icon's state, guards and handler.
    QList<QPair<QAction *, QPushButton *>> mirrors;
    const auto mirror = [&mirrors, moreMenu](QPushButton *button, const QString &text, const char *name) {
        auto *action = moreMenu->addAction(text); action->setObjectName(QLatin1String(name));
        QObject::connect(action, &QAction::triggered, button, &QPushButton::click); mirrors.append({action, button}); return action;
    };
    mirror(m_fileManager, tr("Open folder"), "menuOpenSessionFolderAction");
    mirror(m_shell, tr("Open shell in session folder"), "menuOpenFolderShellAction");
    auto *copyId = mirror(m_copyHeaderId, tr("Copy session ID"), "menuCopySessionIdAction");
    moreMenu->addSeparator();
    m_renameAction = moreMenu->addAction(tr("Rename…")); m_renameAction->setObjectName("renameSessionAction");
    connect(m_renameAction, &QAction::triggered, this, &SessionsWindow::renameSession);
    m_forkAction = moreMenu->addAction(tr("Fork session…")); m_forkAction->setObjectName("forkSessionAction");
    connect(m_forkAction, &QAction::triggered, this, &SessionsWindow::forkSession);
    moreMenu->addSeparator();
    m_openAction = moreMenu->addAction(tr("Copy connect command")); m_openAction->setObjectName("openSessionAction");
    connect(m_openAction, &QAction::triggered, this, &SessionsWindow::openSession);
    m_changeAction = moreMenu->addAction(tr("Pause session")); m_changeAction->setObjectName("pauseAction");
    connect(m_changeAction, &QAction::triggered, this, &SessionsWindow::changeSession);
    moreMenu->addSeparator();
    // A live session clears; a stopped one starts fresh in the same place.
    auto *clear = mirror(m_clear, tr("Clear session…"), "menuClearSessionAction");
    connect(moreMenu, &QMenu::aboutToShow, this, [this, mirrors, copyId, clear] {
        for (const auto &[action, button] : mirrors) {
            action->setVisible(button->isVisibleTo(button->parentWidget())); action->setEnabled(button->isEnabled());
        }
        copyId->setText(m_copyHeaderId->accessibleName()); clear->setText(m_clearRequest ? tr("Clearing…") : tr("Clear session…"));
    });
    m_freshAction = moreMenu->addAction(tr("Start fresh…")); m_freshAction->setObjectName("freshSessionAction");
    m_freshAction->setToolTip(tr("Start this stopped session with a new conversation and the same folder, account and model."));
    connect(m_freshAction, &QAction::triggered, this, &SessionsWindow::startFresh);
    m_archiveAction = moreMenu->addAction(tr("Move to archive")); m_archiveAction->setObjectName("archiveSessionAction");
    connect(m_archiveAction, &QAction::triggered, this, &SessionsWindow::archiveSession);
    m_forgetAction = moreMenu->addAction(tr("Terminate / forget session…")); m_forgetAction->setObjectName("forgetSessionAction");
    connect(m_forgetAction, &QAction::triggered, this, &SessionsWindow::terminateSession); m_more->setMenu(moreMenu); badges->addWidget(m_more); detail->addLayout(badges);
    m_meta = label({}, "sessionMeta"); m_meta->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    m_meta->setTextInteractionFlags(Qt::TextSelectableByMouse);
    m_providerBadge = new IdentityBadge(IdentityBadges::Provider); m_providerBadge->setObjectName("sessionProviderBadge");
    m_machineBadge = new IdentityBadge(IdentityBadges::Machine); m_machineBadge->setObjectName("sessionMachineBadge");
    // WhiteWh fork: the cards view names the session's branch as a chip too.
    m_branchChip = new QFrame; m_branchChip->setObjectName("sessionBranchChip"); m_branchChip->setFixedHeight(18); m_branchChip->hide();
    {auto *h=new QHBoxLayout(m_branchChip);h->setContentsMargins(5,0,6,0);h->setSpacing(3);
     m_branchIcon=new QLabel;m_branchText=new QLabel;m_branchText->setObjectName("sessionBranchText");h->addWidget(m_branchIcon);h->addWidget(m_branchText);}
    auto *metadata = new QHBoxLayout; metadata->setSpacing(6); metadata->addWidget(m_providerBadge); metadata->addWidget(m_machineBadge); metadata->addWidget(m_branchChip); metadata->addWidget(m_meta, 1); metadata->addWidget(m_model);
    detail->addLayout(metadata); m_chipsRow = metadata;
    m_usageStore=new AccountUsageStore(hgsPath,this);
    m_usageStrip=new QWidget;m_usageStrip->setObjectName("sessionUsageStrip");m_usageStrip->setFixedHeight(22);m_usageStrip->hide();
    auto *usageLayout=new QHBoxLayout(m_usageStrip);usageLayout->setContentsMargins(0,0,0,0);usageLayout->setSpacing(4);
    m_accountUsage=new AccountUsage::Button;usageLayout->addWidget(m_accountUsage);
    m_usageRefresh=new AccountUsage::RefreshButton;usageLayout->addWidget(m_usageRefresh);
    connect(m_usageRefresh,&QPushButton::clicked,this,[this]{refreshAccountUsage(true);});
    connect(m_accountUsage,&QPushButton::clicked,this,[this]{
        const auto *entry=selected();if(!entry)return;
        m_pages->setCurrentIndex(4);m_accountsPage->showAccount(entry->host,m_accountUsageData.value("id").toString());
    });
    m_meta->installEventFilter(this); m_title->installEventFilter(this);
    // Context feedback belongs to the fixed composer/footer rows, never the header.
    m_hint = label({}, "hint", details); m_hint->hide();
    m_goal = new QPushButton; m_goal->setObjectName("sessionGoal"); m_goal->hide();
    m_goal->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Fixed); detail->addWidget(m_goal);
    connect(m_goal, &QPushButton::clicked, this, [this] { openInspectorCard(InspectorCards::Tasks); });
    auto *tabs = new QTabWidget; m_detailTabs = tabs; tabs->setObjectName("sessionDetailTabs");
    tabs->setDocumentMode(true); tabs->tabBar()->setDrawBase(false);
    auto browser = [this]() {
        auto *view = new QTextBrowser; view->setOpenLinks(false); view->setOpenExternalLinks(false);
        view->setFrameShape(QFrame::NoFrame); view->document()->setDocumentMargin(4);
        view->setWordWrapMode(QTextOption::WrapAtWordBoundaryOrAnywhere); return view;
    };
    auto *activityPage = new QWidget; auto *activityLayout = new QVBoxLayout(activityPage);
    activityLayout->setContentsMargins(0, 0, 0, 0); activityLayout->setSpacing(0);
    // U55: what waits for the owner, at the top of the chat (filled with the teams).
    m_waitingHost=new QWidget;m_waitingHost->setObjectName("waitingBannerHost");
    {auto *host=new QVBoxLayout(m_waitingHost);host->setContentsMargins(0,0,0,0);host->setSpacing(0);}
    activityLayout->addWidget(m_waitingHost);
    // WhiteWh fork: agent teams. A member session names its team and task here.
    m_teamStrip=new QFrame;m_teamStrip->setObjectName("teamStrip");m_teamStrip->hide();
    {auto *strip=new QHBoxLayout(m_teamStrip);strip->setContentsMargins(10,4,8,4);strip->setSpacing(8);
     m_teamStripText=new QLabel;m_teamStripText->setObjectName("teamStripText");m_teamStripText->setTextFormat(Qt::RichText);
     connect(m_teamStripText,&QLabel::linkActivated,this,[this](const QString &link){if(link.startsWith("task:"))openTeamTask(link.mid(5).toInt());});
     auto *open=new QPushButton(tr("Open team panel"));open->setObjectName("teamStripOpen");open->setFlat(true);open->setCursor(Qt::PointingHandCursor);
     connect(open,&QPushButton::clicked,this,[this]{if(const auto *entry=selected())if(const auto *team=m_teams?m_teams->teamOfSession(entry->host,entry->session.name):nullptr)showTeam(team->id);});
     strip->addWidget(m_teamStripText,1);strip->addWidget(open);}
    activityLayout->addWidget(m_teamStrip);
    m_activityView = new ActivityView; m_activityView->setObjectName("mainActivity"); m_composer = new MessageComposer; m_question = new QuestionCard;
    auto *voice = new SpeechComposer(hgsPath, m_composer, [this] {
        SpeechComposer::Target target;
        const auto *entry = selected();
        if (!entry || m_details.isEmpty()) return target;
        target.key=entry->key;target.host=entry->host;target.session=entry->session.name;
        target.run=m_details.value("run_id").toString();target.conversation=m_details.value("conversation_id").toString();
        // Voice messages to local Windows sessions are a Phase 2 non-goal (spec D16).
        const bool localWindows=entry->host.isEmpty()&&MachinePlatform::of(m_fleet.local().metrics,true)==MachinePlatform::Style::Windows;
        target.available=!localWindows && entry->online && entry->session.archiveId.isEmpty() && messageBlockReason(*entry).isEmpty();
        if(localWindows)target.reason=tr("Voice messages cannot be sent to sessions on this Windows computer yet.");
        if(CacheStatus::expired(m_details))target.warning=CacheStatus::warning(m_details)+tr("\n\nSend this voice message with the full conversation context?");
        return target;
    });
    connect(voice,&SpeechComposer::messageChanged,this,[this](const QString &key,const QJsonObject &message){
        auto &rows=m_localMessages[key];const auto id=message.value("id").toString();
        for(qsizetype i=rows.size();i-->0;)if(rows[i].toObject().value("message_id")==id)rows.removeAt(i);
        rows.append(QJsonObject{{"id","voice-"+id},{"message_id",id},{"request_id",id},{"source","hgs_voice"},{"type","UserMessage"},
            {"voice",message},{"text",message.value("transcript")},{"status","sent"},{"run_id",message.value("run_id")},
            {"conversation_id",message.value("conversation_id")},{"submitted_at",message.value("created_at")},{"attachments",QJsonArray{message.value("audio")}}});
        if(key==m_selectedKey){reconcileMessages();renderDetails();m_activityView->jumpToLatest();inspect();}
    });
    connect(&m_client, &HgsClient::downloadCacheWarning, this, [this](const QString &error) { showNotice(error, true); });
    auto openFileReference = [this](const QString &href) {
        const auto *entry = selected(); if (!entry) return;
        const auto target = SessionFileReference::parse(href); if (!target.valid()) return;
        const QString host = entry->host, machine = entry->machine;
        const bool local = host.isEmpty(), online = entry->online;
        const QString path = SessionFileReference::resolve(target, m_subagentId.isEmpty() ? selectedDirectory() : m_subagentCwd, local ? QDir::homePath() : QString());
        // Files open after the dialog closes; folders open as they are.
        QUrl desktopTarget; bool desktopFile = false, revealFile = false;
        QDialog dialog(this); dialog.setObjectName("sessionFileDialog");
        dialog.setWindowTitle(tr("File reference — Zerus")); dialog.resize(local ? 560 : 650, 230);
        auto *layout = new QVBoxLayout(&dialog); layout->setContentsMargins(20, 20, 20, 20); layout->setSpacing(12);
        auto *source = new QLabel(local ? tr("File on this machine") : tr("File on %1").arg(machine));
        source->setTextFormat(Qt::PlainText); layout->addWidget(source);
        // This computer shows and copies its paths with native separators; a peer's
        // stay POSIX. path keeps the Qt form that QFileInfo and QUrl read.
        const auto shown = MachinePlatform::native(path.isEmpty() ? target.path : path, MachinePlatform::of(m_fleet.metricsOf(host), local));
        auto *address = new QLineEdit(shown); address->setReadOnly(true);
        address->setObjectName("sessionFilePath");
        auto *copy = new QPushButton;copy->setObjectName("copySessionFilePath");
        configureWorkspaceIconButton(copy,"copy",tr("Copy path"),m_dark);
        copy->setAutoDefault(false);
        auto *pathRow=new QHBoxLayout;pathRow->setSpacing(8);pathRow->addWidget(address,1);pathRow->addWidget(copy);layout->addLayout(pathRow);
        connect(copy, &QPushButton::clicked, &dialog, [this, address] { emit copyTextRequested(address->text()); });
        if (!target.location.isEmpty()) {
            auto *position = new QLabel(tr("Source position: %1").arg(target.location));
            position->setTextFormat(Qt::PlainText); layout->addWidget(position);
        }
        auto *hint = new QLabel(local ? (path.isEmpty() ? tr("The session folder is unknown. Copy the path to locate this file.")
            : tr("Open with your default application, or copy the path."))
            : tr("Download… lets you choose where to save. Download and open saves a copy in Downloads/Zerus Files and opens it with your default application."));
        hint->setObjectName("sessionFileHint");
        hint->setTextInteractionFlags(Qt::TextSelectableByMouse);
        hint->setTextFormat(Qt::PlainText); hint->setWordWrap(true); layout->addWidget(hint);
        auto *buttons = new QDialogButtonBox; layout->addWidget(buttons);
        QString savedPath;
        quint64 pending = 0, cacheRequest = 0; bool openAfterDownload = false;
        if (local && !path.isEmpty()) {
            auto *open = buttons->addButton(tr("Open file"), QDialogButtonBox::ActionRole); open->setObjectName("openSessionFile");
            // #744: checked by PathProbe's worker; the dialog keeps answering.
            connect(open, &QPushButton::clicked, &dialog, [path, hint, &dialog, &desktopTarget, &desktopFile] {
                hint->setText(tr("Checking the file…"));
                PathProbe::instance().check(path, &dialog, [path, hint, &dialog, &desktopTarget, &desktopFile](const PathProbe::Info &file) {
                    if (file.state == PathProbe::Info::Unresponsive) { hint->setText(tr("The file does not answer: its disk may be busy.")); return; }
                    if (!file.exists) { hint->setText(tr("This file is no longer available at this path.")); return; }
                    desktopTarget = QUrl::fromLocalFile(path); desktopFile = true;
                    dialog.accept();
                });
            });
            auto *folder = buttons->addButton(tr("Open folder"), QDialogButtonBox::ActionRole); folder->setObjectName("openSessionFileFolder");
            connect(folder, &QPushButton::clicked, &dialog, [path, hint, &dialog, &desktopTarget] {
                const auto directory = QFileInfo(path).absolutePath();
                hint->setText(tr("Checking the folder…"));
                PathProbe::instance().check(directory, &dialog, [directory, hint, &dialog, &desktopTarget](const PathProbe::Info &folder) {
                    if (folder.state == PathProbe::Info::Unresponsive) { hint->setText(tr("The folder does not answer: its disk may be busy.")); return; }
                    if (!folder.dir) { hint->setText(tr("This folder is no longer available at this path.")); return; }
                    desktopTarget = QUrl::fromLocalFile(directory);
                    dialog.accept();
                });
            });
        } else if (!local) {
            auto *progress = new QProgressBar; progress->setObjectName("sessionFileProgress");
            progress->setAccessibleName(tr("Downloading file from %1").arg(machine));
            progress->setRange(0, 0); progress->setTextVisible(false); progress->hide(); layout->insertWidget(layout->count() - 1, progress);
            auto *download = buttons->addButton(tr("Download…"), QDialogButtonBox::ActionRole); download->setObjectName("downloadSessionFile");
            auto *open = buttons->addButton(tr("Download and open"), QDialogButtonBox::ActionRole); open->setObjectName("downloadOpenSessionFile");
            open->setToolTip(tr("Save automatically to %1 and open the local copy.").arg(QDir::toNativeSeparators(DownloadCache::directory())));
            auto *localActions = new QWidget;
            auto *localLayout = new QHBoxLayout(localActions); localLayout->setContentsMargins(0, 0, 0, 0);
            auto *savedOpen = new QPushButton(tr("Open local copy")); savedOpen->setObjectName("openDownloadedSessionFile");
            auto *showFile = new QPushButton(tr("Show file")); showFile->setObjectName("showDownloadedSessionFile");
            localLayout->addWidget(savedOpen); localLayout->addWidget(showFile); localLayout->addStretch();
            localActions->hide(); layout->insertWidget(layout->count() - 1, localActions);
            download->setAutoDefault(false); open->setAutoDefault(false); showFile->setAutoDefault(false);
            const QString remotePath = path.isEmpty() && target.path.startsWith("~/") ? target.path : path;
            const bool downloadable = online && !remotePath.isEmpty();
#ifdef Q_OS_WIN
            // Keep the source conversation immutable across the nested modal.
            // Native proof, not a lexical mapping, authorizes each read.
            QJsonObject scope{{"session",entry->session.name},{"run_id",entry->session.runId},
                {"conversation_id",entry->session.conversationId},{"path",remotePath}};
            if(!entry->session.archiveId.isEmpty())scope["archive_id"]=entry->session.archiveId;
            if(!m_subagentId.isEmpty())scope["agent_id"]=m_subagentId;
            auto *mapped=new QPushButton(tr("Open mapped Windows original…"));mapped->setObjectName("openMappedSessionFile");mapped->setAutoDefault(false);
            mapped->setEnabled(downloadable&&!entry->session.runId.isEmpty()&&!entry->session.conversationId.isEmpty()&&remotePath.startsWith('/'));
            layout->insertWidget(layout->count()-1,mapped);
            connect(mapped,&QPushButton::clicked,&dialog,[this,host,scope,&dialog]{
                WslFileOpenDialog original(m_client.executable(),host,scope,&dialog);
                if(original.exec()==QDialog::Accepted)showNotice(original.outcomeNotice());
            });
#endif
            download->setEnabled(downloadable); open->setEnabled(downloadable);
            if (!online) hint->setText(tr("%1 is offline. Connect to this machine to download its files.").arg(machine));
            else if (remotePath.isEmpty()) hint->setText(tr("The session folder is unknown. Copy the path or use the SSH terminal to locate the file."));
            const auto showCopy = [&, open, savedOpen, localActions](const QString &destination) {
                savedPath = destination;
                hint->setText(tr("Saved on this computer: %1\nOpen the downloaded copy, or download again to get an updated file.").arg(destination));
                localActions->show(); savedOpen->setDefault(true); open->setText(tr("Download again"));
            };
            auto startDownload = [&, download, open, localActions, progress, remotePath](bool opening) {
                if (pending) return;
                QString destination = DownloadCache::directory();
                if (!opening) {
                    const auto downloads = QStandardPaths::writableLocation(QStandardPaths::DownloadLocation);
                    QFileDialog save(&dialog, tr("Save local copy"), QDir(downloads.isEmpty() ? QDir::homePath() : downloads).filePath(QFileInfo(remotePath).fileName()));
                    save.setObjectName("sessionFileSaveDialog"); save.setAcceptMode(QFileDialog::AcceptSave); save.setFileMode(QFileDialog::AnyFile);
                    if (save.exec() != QDialog::Accepted || save.selectedFiles().isEmpty()) return;
                    destination = save.selectedFiles().first();
                }
                const bool refresh = opening && !savedPath.isEmpty();
                // A later Save As completion owns the shown copy, even when an
                // earlier cache check finishes after a large-file hash.
                cacheRequest = 0;
                download->setEnabled(false); open->setEnabled(false); localActions->hide();
                progress->setAccessibleName(tr("Downloading file from %1").arg(machine));
                progress->show(); hint->setText(tr("Downloading from %1 to %2…\nThe download continues if you close this dialog.").arg(machine, destination));
                openAfterDownload = opening;
                pending = opening ? m_client.downloadFileToCache(host, remotePath, refresh) : m_client.downloadFile(host, remotePath, destination);
                auto *completion = new QObject(this);
                const QPointer<QDialog> shownDialog(&dialog);
                const auto request = pending;
                connect(&m_client, &HgsClient::fileDownloaded, completion, [this, completion, shownDialog, request, opening](quint64 id, const QString &localPath) {
                    if (id != request) return;
                    // Like a browser download, the copy is marked as coming from another machine.
                    ExternalFile::markDownloaded(localPath);
                    if (!shownDialog) {
                        if (opening) QTimer::singleShot(0, this, [this, localPath] { openExternalFile(localPath, true); });
                        else showNotice(tr("Downloaded to %1").arg(localPath));
                    }
                    completion->deleteLater();
                });
                connect(&m_client, &HgsClient::fileDownloadFailed, completion, [this, completion, shownDialog, request](quint64 id, const QString &error) {
                    if (id != request) return;
                    if (!shownDialog) showNotice(tr("Download failed: %1").arg(error), true);
                    completion->deleteLater();
                });
            };
            connect(download, &QPushButton::clicked, &dialog, [startDownload] { startDownload(false); });
            connect(open, &QPushButton::clicked, &dialog, [startDownload] { startDownload(true); });
            connect(&m_client, &HgsClient::fileDownloadFailed, &dialog, [&, progress, download, open, localActions, downloadable](quint64 request, const QString &error) {
                if (!pending || request != pending) return;
                pending = 0; progress->hide(); download->setEnabled(downloadable); open->setEnabled(downloadable);
                localActions->setVisible(LocalFile::readable(QFileInfo(savedPath)));
                hint->setText(tr("Download failed: %1").arg(error));
            });
            connect(&m_client, &HgsClient::fileDownloaded, &dialog, [&, progress, download, open, showCopy, downloadable](quint64 request, const QString &destination) {
                if (!pending || request != pending) return;
                pending = 0; progress->hide();
                download->setEnabled(downloadable); open->setEnabled(downloadable);
                showCopy(destination);
                if (openAfterDownload) { desktopTarget = QUrl::fromLocalFile(destination); desktopFile = true; dialog.accept(); }
            });
            connect(savedOpen, &QPushButton::clicked, &dialog, [&] {
                if (!LocalFile::readable(QFileInfo(savedPath))) { hint->setText(tr("The local copy is no longer available.")); return; }
                desktopTarget = QUrl::fromLocalFile(savedPath); desktopFile = true; dialog.accept();
            });
            connect(showFile, &QPushButton::clicked, &dialog, [&] {
                if (!LocalFile::readable(QFileInfo(savedPath))) { hint->setText(tr("The local copy is no longer available.")); return; }
                desktopTarget = QUrl::fromLocalFile(savedPath); revealFile = true; dialog.accept();
            });
            const auto downloadHint = hint->text();
            connect(&m_client, &HgsClient::fileCacheChecked, &dialog, [&, showCopy, progress, open, downloadHint, downloadable](quint64 request, const QString &destination, const QString &error) {
                if (request != cacheRequest) return;
                cacheRequest = 0;
                if (pending) return;
                progress->hide(); open->setEnabled(downloadable);
                if (!error.isEmpty()) { hint->setText(tr("Cannot check the downloaded copy: %1").arg(error)); return; }
                if (!destination.isEmpty()) showCopy(destination);
                else hint->setText(downloadHint);
            });
            if (!remotePath.isEmpty()) {
                progress->setAccessibleName(tr("Checking the downloaded copy…"));
                open->setEnabled(false); progress->show(); hint->setText(tr("Checking the downloaded copy…"));
                cacheRequest = m_client.findCachedFile(host, remotePath);
            }
            auto *ssh = buttons->addButton(tr("Open SSH terminal"), QDialogButtonBox::ActionRole); ssh->setObjectName("openSessionFileSsh");
            connect(ssh, &QPushButton::clicked, &dialog, [this, host, &dialog] { emit machineSshRequested(host); dialog.accept(); });
            auto *close = buttons->addButton(QDialogButtonBox::Close);
            close->setAutoDefault(false);
        }
        connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
        dialog.exec();
        if (!desktopTarget.isEmpty()) {
            // Wayland defers launch until the focused window receives an
            // activation token. Destroying that window cancels the callback.
            // Dispatch after the modal has gone, using the persistent workspace.
            // Folders take the same path: a macOS bundle folder would launch.
            QTimer::singleShot(0, this, [this, desktopTarget, desktopFile, revealFile, local] {
                const auto path = desktopTarget.toLocalFile();
                if (revealFile) {
                    if (!ExternalFile::reveal(path)) showNotice(tr("The file manager could not show the saved copy at %1.").arg(QDir::toNativeSeparators(path)), true);
                } else openExternalFile(path, desktopFile && !local);
            });
        }
    };
    connect(m_activityView, &ActivityView::fileReferenceActivated, this, openFileReference);
    m_nativeSignIn = new QWidget; m_nativeSignIn->setObjectName("nativeSignInBanner");
    auto *signInLayout = new QVBoxLayout(m_nativeSignIn); signInLayout->setContentsMargins(14, 12, 14, 12);
    auto *signInText = new QLabel(tr("Sign in to DeepSeek to start using this session. Complete sign-in in the official DeepSeek UI, then return here."));
    signInText->setWordWrap(true); signInText->setTextFormat(Qt::PlainText); signInLayout->addWidget(signInText);
    auto *signInButton = new QPushButton(tr("Sign in to DeepSeek…")); signInButton->setObjectName("nativeSignInButton");
    signInLayout->addWidget(signInButton, 0, Qt::AlignLeft); m_nativeSignIn->hide();
    connect(signInButton, &QPushButton::clicked, this, [this] {
        const auto *entry = selected();
        if (entry && entry->online && entry->session.cmd == "dsh" && m_details.value("auth_required").toBool())
            showNativeUi(*entry);
    });
    // Activity's transcript keeps the full pane for its scroll bar. The content
    // around it shares the transcript's centered reading column.
    const auto activityColumn = [this](QBoxLayout *page) {
        auto *column = new ActivityWidth::ColumnLayout; page->addLayout(column); m_activityColumns.append(column); return column;
    };
    activityColumn(activityLayout)->addWidget(m_nativeSignIn);
    m_openCodeService = new QWidget; m_openCodeService->setObjectName("openCodeServiceBanner");
    auto *serviceLayout = new QVBoxLayout(m_openCodeService); serviceLayout->setContentsMargins(14, 12, 14, 12);
    m_openCodeServiceText = new QLabel; m_openCodeServiceText->setWordWrap(true); m_openCodeServiceText->setTextFormat(Qt::PlainText);
    m_openCodeServiceText->setObjectName("openCodeServiceReason"); serviceLayout->addWidget(m_openCodeServiceText);
    m_openCodeServiceStart = new QPushButton; m_openCodeServiceStart->setObjectName("openCodeServiceStart");
    m_openCodeServiceStart->setToolTip(tr("Start the shared OpenCode service on this computer. Zerus never stops it."));
    serviceLayout->addWidget(m_openCodeServiceStart, 0, Qt::AlignLeft); m_openCodeService->hide();
    connect(m_openCodeServiceStart, &QPushButton::clicked, this, &SessionsWindow::startOpenCodeService);
    activityColumn(activityLayout)->addWidget(m_openCodeService);
    connect(&m_client, &HgsClient::accountsReady, this, [this](quint64 id, const QString &, const QJsonObject &) {
        if (!m_openCodeStartRequest || id != m_openCodeStartRequest) return;
        m_openCodeStartRequest = 0; showNotice(tr("OpenCode started on %1.").arg(m_openCodeStartMachine));
        emit refreshRequested(); renderDetails(); inspect();
    });
    connect(&m_client, &HgsClient::accountsFailed, this, [this](quint64 id, const QString &, const QString &error) {
        if (!m_openCodeStartRequest || id != m_openCodeStartRequest) return;
        m_openCodeStartRequest = 0; showNotice(tr("Could not start OpenCode on %1: %2").arg(m_openCodeStartMachine, error), true);
        renderDetails();
    });
    m_contextUsage=new SessionUsage::ContextButton;m_contextUsage->setObjectName("activityContext");
    activityLayout->addWidget(m_activityView, 1);
    // WhiteWh fork: U57, the lead's open proposals as cards above the question card.
    m_proposalCards = new TeamProposalCards; m_proposalCards->setObjectName("mainProposalCards");
    auto *inputColumn = activityColumn(activityLayout); inputColumn->addWidget(m_proposalCards); inputColumn->addWidget(m_question);
    m_usageLimit=new ToolbarChip;m_usageLimit->setObjectName("usageLimitChip");m_usageLimit->setTone(ChipTone::Danger);m_usageLimit->setIconName("attention");
    auto *usagePanel=new QWidget;usagePanel->setObjectName("usageLimitPopover");
    auto *limitLayout=new QVBoxLayout(usagePanel);limitLayout->setContentsMargins(14,12,14,12);limitLayout->setSpacing(8);
    m_usageWarning=new QLabel;m_usageWarning->setObjectName("activityUsageWarning");m_usageWarning->setTextFormat(Qt::PlainText);m_usageWarning->setWordWrap(true);
    auto *usageActions=new QHBoxLayout;usageActions->addStretch();
    auto *usageRefresh=new QPushButton(tr("Refresh usage"));usageRefresh->setObjectName("usageLimitRefresh");usageRefresh->setAutoDefault(false);
    usageActions->addWidget(usageRefresh);limitLayout->addWidget(m_usageWarning);limitLayout->addLayout(usageActions);
    m_usageLimit->setPopoverContent(usagePanel);m_composer->toolbar()->add(ComposerToolbar::Slot::UsageLimit,m_usageLimit);
    connect(usageRefresh,&QPushButton::clicked,this,[this]{refreshAccountUsage(true);});
    m_recovery = new RecoveryUi::Panel;
    m_recoveryChip=new ToolbarChip;m_recoveryChip->setObjectName("recoveryChip");m_recoveryChip->setIconName("refresh");
    m_recoveryChip->setPopoverContent(m_recovery);m_composer->toolbar()->add(ComposerToolbar::Slot::Recovery,m_recoveryChip);
    m_recovery->summaryChanged = [this] {
        const auto summary=m_recovery->summary();
        m_recoveryChip->setLabels(summary.label,summary.shortLabel);m_recoveryChip->setTone(summary.tone);
        m_recoveryChip->setDetail(m_recovery->detailText());m_recoveryChip->setActive(m_recovery->active());
    };
    m_recovery->openTerminal = [this] {
        m_recoveryChip->closePopover();
        const auto *entry=selected();if(!entry || !entry->online)return;
        if(entry->session.cmd=="dsh"){showNativeUi(*entry);return;}
        if(!NativeSessionCapabilities::terminal(entry->session,m_details)){showNotice(NativeSessionCapabilities::terminalReason(entry->session,m_details),true);return;}
        m_detailTabs->setCurrentWidget(m_terminal);m_terminal->connectSession();
    };
    m_recovery->refreshUsage = [this] { refreshAccountUsage();inspect();emit refreshRequested(); };
    m_recovery->openSettings = [this] {
        m_recoveryChip->closePopover();showWorkspaceSettings();m_settingsPage->openRecovery();
    };
    m_recovery->action = [this](const QJsonObject &payload) {
        const auto *entry=selected();if(!entry||!entry->online)return;
        m_recoveryActionKey=m_selectedKey;
        m_recoveryAction=m_client.requestRecovery(entry->host,"action",payload);
    };
    connect(&m_client,&HgsClient::recoveryFinished,this,[this](quint64 id,bool ok,const QJsonObject &,const QString &error) {
        if(id!=m_recoveryAction)return;m_recoveryAction=0;
        if(m_recoveryActionKey==m_selectedKey)m_recovery->finished(ok?QString():error);
        inspect();emit refreshRequested();
    });
    auto *toolbar=m_composer->toolbar();
    m_compactCancel = new QPushButton(tr("Cancel send")); m_compactCancel->setObjectName("cancelCompactSend");
    m_compactCancel->setToolTip(tr("Cancel the pending send and unlock the message for editing. Compaction or startup continues; use Stop to interrupt the agent."));
    m_compactCancel->setAutoDefault(false); m_compactCancel->setFixedHeight(20);
    m_compactCancel->setStyleSheet(QStringLiteral("QPushButton { min-height: 20px; max-height: 20px; padding: 0 6px; margin: 0; border: none; background: transparent; } QPushButton:hover { text-decoration: underline; }"));
    m_compactCancel->hide();
    connect(m_compactCancel,&QPushButton::clicked,this,[this] {
        cancelCompactContinuation(m_selectedKey, tr("Pending send cancelled. Your draft is unchanged."));
        if (m_startSend.key == m_selectedKey) cancelStartContinuation(tr("Pending send cancelled. Your draft is unchanged."));
        if (m_composer->isInputVisible()) m_composer->editor()->setFocus(Qt::OtherFocusReason);
        renderDetails();
    });
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &receipt,const QString &error) {
        for (const auto &key : m_compacts.keys()) {
            auto &pending = m_compacts[key];
            if (!pending.request || id != pending.request) continue;
            pending.request = 0;
            if (!ok) cancelCompactContinuation(key, error);
            else { pending.commandId=receipt.value("request_id").toString(); pollCompactContinuations(); }
            renderDetails(); return;
        }
    });
    m_compactTimer.setInterval(2500);
    connect(&m_compactTimer, &QTimer::timeout, this, &SessionsWindow::pollCompactContinuations);
    m_markRead=new ToolbarChip;m_markRead->setObjectName("activityMarkRead");
    m_markRead->setLabels(tr("Mark as read"));m_markRead->setIconName("read-all");m_markRead->setTone(ChipTone::Accent);
    toolbar->add(ComposerToolbar::Slot::MarkRead,m_markRead);
    m_activityView->useCompactJumpButton();toolbar->add(ComposerToolbar::Slot::Latest,m_activityView->jumpButton());
    auto *compaction=m_activityView->compactionIndicator();compaction->layout()->setContentsMargins(0,0,0,0);
    toolbar->add(ComposerToolbar::Slot::Compaction,compaction);toolbar->add(ComposerToolbar::Slot::CompactionCancel,m_compactCancel);
    m_cacheStatus=new CacheStatus::Chip;
    auto *cachePanel=new QWidget;cachePanel->setObjectName("cachePopover");
    auto *cacheLayout=new QVBoxLayout(cachePanel);cacheLayout->setContentsMargins(14,12,14,12);cacheLayout->setSpacing(8);
    m_cacheWarning=new QLabel;m_cacheWarning->setObjectName("cacheWarning");m_cacheWarning->setWordWrap(true);m_cacheWarning->setTextFormat(Qt::PlainText);
    m_cacheDetail=new QLabel;m_cacheDetail->setObjectName("cacheDetail");m_cacheDetail->setWordWrap(true);m_cacheDetail->setTextFormat(Qt::PlainText);
    auto *cacheActions=new QHBoxLayout;cacheActions->setSpacing(8);cacheActions->addStretch();
    auto *cacheUsage=new QPushButton(tr("Usage details"));cacheUsage->setObjectName("cacheUsageDetails");cacheUsage->setAutoDefault(false);
    m_cacheClear=new QPushButton(tr("Clear context"));m_cacheClear->setObjectName("cacheClearContext");m_cacheClear->setAutoDefault(false);
    m_cacheClear->setToolTip(tr("Clear this agent’s conversation, like /clear in Terminal. Your unsent message stays here."));
    cacheActions->addWidget(cacheUsage);cacheActions->addWidget(m_cacheClear);
    cacheLayout->addWidget(m_cacheWarning);cacheLayout->addWidget(m_cacheDetail);cacheLayout->addLayout(cacheActions);
    m_cacheStatus->setPopoverContent(cachePanel);
    toolbar->add(ComposerToolbar::Slot::Cache,m_cacheStatus);toolbar->add(ComposerToolbar::Slot::Context,m_contextUsage);
    connect(cacheUsage,&QPushButton::clicked,this,[this]{m_cacheStatus->closePopover();openInspectorCard(InspectorCards::Environment);});
    // Close first: the confirmation dialog must not open over a popup.
    connect(m_cacheClear,&QPushButton::clicked,this,[this]{m_cacheStatus->closePopover();clearContext();});
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &,const QString &error) {
        if(!m_forgetBindingRequest||id!=m_forgetBindingRequest)return;
        m_forgetBindingRequest=0;m_pending=false;
        showNotice(ok?tr("Zerus forgot %1. The conversation stays in OpenCode.").arg(m_forgetBindingName):error,!ok);
        emit refreshRequested();rebuild();inspect();
    });
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &,const QString &error) {
        if(!m_selectionForgetRequest||id!=m_selectionForgetRequest)return;
        // A failed or unconfirmed receipt counts as failed and is never retried.
        m_selectionForgetRequest=0;finishSelectionAction(m_selectionCommand,ok,error);
    });
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &,const QString &error) {
        if(!m_removeQueuedRequest||id!=m_removeQueuedRequest)return;
        m_removeQueuedRequest=0;m_removeQueuedMessage.clear();
        if(m_selectedKey==m_removeQueuedKey){showNotice(ok?tr("Queued message removed."):error,!ok);renderDetails();inspect();}
        else {if(!ok)showNotice(tr("Could not remove the queued message from %1: %2").arg(m_removeQueuedName,error),true);renderDetails();}
        emit refreshRequested();
    });
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &receipt,const QString &error) {
        if(id!=m_clearRequest)return;
        m_clearRequest=0;
        if(m_selectedKey==m_clearKey) {
            showNotice(ok?(receipt.value("status")=="confirmed"?tr("Context cleared. Your draft is ready to send."):tr("Clear command sent. Waiting for the agent to confirm the new conversation…")):error,!ok);
            inspect();renderDetails();
        }
        emit refreshRequested();
    });
    // WhiteWh fork: what the selected session is doing, above the message field.
    m_workingLine = new WorkingLine; inputColumn->addWidget(m_workingLine);
    // U35: messages hgs holds until the agent takes the previous one, above the field.
    m_heldBar = new HeldInputBar; m_heldBar->setObjectName("sessionHeldInputs"); inputColumn->addWidget(m_heldBar);
    // Text left in the agent's Terminal input holds every send: show it here.
    m_terminalDraft = new TerminalDraftBar(&m_client); m_terminalDraft->setObjectName("sessionTerminalDraft"); inputColumn->addWidget(m_terminalDraft);
    m_terminalDraft->openTerminal = [this] { if (const auto *entry = selected()) openSessionTerminal(entry->host, entry->session.name); };
    m_terminalDraft->submitted = [this] { inspect(); };
    m_heldBar->cancel = [this](const QString &id, const QString &text) {
        const auto *entry = selected(); if (!entry) return;
        m_heldCancels.insert(m_client.requestCancelHeld(entry->host, entry->session.name, id), {entry->key, text});
    };
    connect(&m_client, &HgsClient::heldCancelFinished, this, [this](quint64 request, const QString &, const QString &, bool ok, const QJsonObject &receipt, const QString &error) {
        if (!m_heldCancels.contains(request)) return;
        const auto [key, text] = m_heldCancels.take(request);
        if (!ok) { m_heldBar->cancelFinished(receipt.value("request_id").toString()); showNotice(error, true); inspect(); return; }
        auto &held = m_justHeld[key];
        for (int i = int(held.size()); i-- > 0;) if (held[i].toObject().value("request_id") == receipt.value("request_id")) held.removeAt(i);
        // The cancelled text comes back to an empty composer.
        if (key == m_selectedKey && m_composer->editor()->toPlainText().trimmed().isEmpty()) m_composer->editor()->setPlainText(text);
        if (key == m_selectedKey) { renderHeld(); inspect(); }
    });
    inputColumn->addWidget(m_composer);
    connect(m_composer, &MessageComposer::sessionRefreshRequested, this, [this](const QString &key) {
        if (key != m_selectedKey) return;
        emit refreshRequested(); inspect();
    });
    connect(m_markRead,&QPushButton::clicked,this,[this]{
        const auto *entry=selected();if(!entry||entry->session.state=="archived"||!entryNeedsAttention(*entry))return;
        const auto original=*entry;
        if(m_markRead->hasFocus())m_activityView->browser()->setFocus(Qt::OtherFocusReason);
        runSessionMenuAction(original,SessionMenuAction::MarkRead);
    });
    m_activityStack = new QStackedWidget; m_activityStack->setObjectName("activityStack");
    m_activityStack->addWidget(activityPage);
    auto *subagentPage = new QWidget; auto *subagentLayout = new QVBoxLayout(subagentPage);
    subagentLayout->setContentsMargins(0, 8, 0, 0); subagentLayout->setSpacing(8);
    auto *subagentTop = activityColumn(subagentLayout); subagentTop->setSpacing(8);
    auto *subagentHeader = new QHBoxLayout;
    auto *back = new QPushButton(tr("← Main activity")); back->setObjectName("subagentBack");
    connect(back, &QPushButton::clicked, this, &SessionsWindow::closeSubagent);
    subagentHeader->addWidget(back);
    m_subagentTitle = new QLabel; m_subagentTitle->setTextFormat(Qt::PlainText);
    m_subagentTitle->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    subagentHeader->addWidget(m_subagentTitle, 1); subagentTop->addLayout(subagentHeader);
    m_subagentHint = new QLabel; m_subagentHint->setTextFormat(Qt::PlainText); m_subagentHint->setWordWrap(true);
    subagentTop->addWidget(m_subagentHint);
    m_subagentView = new ActivityView; m_subagentView->setObjectName("subagentActivity"); m_subagentView->browser()->setObjectName("subagentJournal"); subagentLayout->addWidget(m_subagentView, 1);
    m_subagentComposer = new MessageComposer; m_subagentComposer->setObjectName("subagentComposer"); m_subagentComposer->setInputVisible(false);
    // Voice delivery is scoped to a main conversation until the durable voice
    // protocol supports an exact child identity; never route a child to its parent.
    m_subagentContextUsage=new SessionUsage::ContextButton;m_subagentContextUsage->setObjectName("subagentContext");
    m_subagentComposer->toolbar()->add(ComposerToolbar::Slot::Context,m_subagentContextUsage);
    m_subagentView->useCompactJumpButton();m_subagentComposer->toolbar()->add(ComposerToolbar::Slot::Latest,m_subagentView->jumpButton());
    activityColumn(subagentLayout)->addWidget(m_subagentComposer);
    for(auto *context:{m_contextUsage,m_subagentContextUsage})connect(context,&QPushButton::clicked,this,[this]{openInspectorCard(InspectorCards::Context);});
    connect(m_subagentComposer, &MessageComposer::sendRequested, this,
        [this](const QString &key, const QString &text, const QList<MessageAttachment> &files) {
            const auto *entry = selected();
            if (!entry || m_subagentId.isEmpty() || key != childKey(*entry, m_subagentId)
                || !entry->online || !m_subagentDetails.value("send_supported").toBool() || m_subagentComposer->isSending(key)) return;
            const Entry target = *entry;
            const QString run = m_details.value("run_id").toString(), parentConversation = m_details.value("conversation_id").toString();
            const QString conversation = m_subagentConversation, agent = m_subagentId;
            beginSendEvidence(target.key, m_details);
            m_subagentComposer->prepareSendingAsync(key, false, [this, target, key, text, files, run, parentConversation, conversation, agent](bool okay) {
                if (!okay) { finishSendEvidence(target.key); return; }
                const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &value) { return value.key == target.key; });
                const auto evidence = m_pendingSendEvidence.value(target.key);
                finishSendEvidence(target.key);
                const bool inspectingChild = m_selectedKey == target.key && m_subagentId == agent;
                if (current == m_entries.cend() || current->host != target.host || current->session.name != target.session.name
                    || current->session.created != target.session.created
                    || !observedIdentityMatches(current->session, target.session, run, parentConversation) || !current->online || isTerminating(*current)
                    || m_pending || !m_renameKey.isEmpty()
                    || (!evidence.isEmpty() && (evidence.value("run_id").toString() != run || evidence.value("runtime_state") != "live" || evidence.value("process_state") != "running"))
                    || (inspectingChild && (m_subagentConversation != conversation || !m_subagentDetails.value("send_supported").toBool()))) {
                    m_subagentComposer->deliveryFinished(key, false, tr("The agent changed. Your draft was kept without sending.")); return;
                }
                const auto request = m_client.requestSendMessage(target.host, target.session.name, text, files, run, conversation, agent);
                m_childMessages.insert(request, key);
            });
        });
    connect(m_subagentView, &ActivityView::fileReferenceActivated, this, openFileReference);
    auto *attachments=new AttachmentLoader(&m_client,this);
    for(auto *view:{m_activityView,m_subagentView}) {
        connect(view,&ActivityView::attachmentActivated,this,[this,view,attachments](const QJsonObject &file){
            const auto *entry=selected();if(!entry)return;
            showNotice(tr("Opening attachment…"));
            attachments->open(entry->host,entry->session.name,m_details.value("conversation_id").toString(),
                entry->session.archiveId,view==m_subagentView?m_subagentId:QString(),file,
                [this](const QString &,const QString &error,const QString &notice){
                    // A plain success leaves "Opening attachment…" to expire on its own.
                    if(!error.isEmpty())showNotice(error,true);else if(!notice.isEmpty())showNotice(notice);
                });
        });
        connect(view,&ActivityView::voicePlaybackRequested,this,[this,view,attachments](const QString &key,const QJsonObject &file){
            const auto *entry=selected();if(!entry)return;
            attachments->fetch(entry->host,entry->session.name,m_details.value("conversation_id").toString(),entry->session.archiveId,
                view==m_subagentView?m_subagentId:QString(),file,[view,key](const QString &path,const QString &error){view->playVoice(key,path,error);},true);
        });
        connect(view,&ActivityView::attachmentPreviewRequested,this,[this,view,attachments](const QString &key,const QJsonObject &file){
            const auto *entry=selected();if(!entry)return;
            if(view==m_subagentView && m_subagentId.isEmpty())return;
            attachments->fetch(entry->host,entry->session.name,m_details.value("conversation_id").toString(),
                entry->session.archiveId,view==m_subagentView?m_subagentId:QString(),file,
                [view,key](const QString &path,const QString &error){
                    if(error.isEmpty())view->setAttachmentPreview(key,AttachmentLoader::thumbnail(path));
                });
        });
    }
    // WhiteWh fork: pictures, animations and videos the agent names, read from the
    // session's computer: in place on this one, through the download cache from a peer.
    for (auto *view : {m_activityView, m_subagentView}) {
        connect(view, &ActivityView::mediaPreviewRequested, this, [this, view](const QString &key, const QString &href) {
            if (view == m_subagentView && m_subagentId.isEmpty()) return;
            loadMedia(view, key, href, false);
        });
        connect(view, &ActivityView::mediaActivated, this, [this, view](const QString &key, const QString &href, const QString &path) {
            if (!path.isEmpty() && QFileInfo::exists(path)) { (new MediaViewer(path, QFileInfo(path).fileName() + tr(" — Zerus"), this))->show(); return; }
            loadMedia(view, key, href, true);
        });
    }
    connect(&m_client, &HgsClient::fileDownloaded, this, [this](quint64 request, const QString &destination) {
        const auto media = m_mediaDownloads.take(request); if (!media.view) return;
        media.view->setMediaPreview(media.key, destination);
        if (media.open) (new MediaViewer(destination, QFileInfo(destination).fileName() + tr(" — Zerus"), this))->show();
    });
    connect(&m_client, &HgsClient::fileDownloadFailed, this, [this](quint64 request, const QString &error) {
        const auto media = m_mediaDownloads.take(request); if (!media.view) return;
        media.view->setMediaPreview(media.key, {}, error);
        if (media.open) showNotice(error, true);
    });
    m_activityStack->addWidget(subagentPage);
    for (auto *view : {m_activityView, m_subagentView}) {
        // Activity's column edges and menu change the preference for every column.
        const auto changed = [this] { applyActivityWidth(); m_settingsPage->syncActivityWidth(); };
        connect(view, &ActivityView::columnWidthRequested, this, [changed](int width) {
            ActivityWidth::setWidth(qRound(width / ContentScale::factor())); ActivityWidth::setFullWidth(false); changed();
        });
        connect(view, &ActivityView::columnResetRequested, this, [changed] {
            ActivityWidth::setWidth(ActivityWidth::Default); ActivityWidth::setFullWidth(false); changed();
        });
        connect(view, &ActivityView::fullWidthRequested, this, [changed](bool full) { ActivityWidth::setFullWidth(full); changed(); });
    }
    m_question->hide();
    connect(m_question, &QuestionCard::answerRequested, this, &SessionsWindow::answerQuestion);
    connect(m_question, &QuestionCard::fileReferenceActivated, this, openFileReference);
    connect(m_question, &QuestionCard::queueNavigationRequested, this, [this](int direction) {
        if(const auto *entry=selected())renderQuestion(*entry,direction);
    });
    connect(m_question, &QuestionCard::openTerminalRequested, this, [this] {
        const auto *entry=selected();if(!entry)return;
        if(entry->session.cmd=="dsh") {showNativeUi(*entry);return;}
        if(!NativeSessionCapabilities::terminal(entry->session,m_details)){showNotice(NativeSessionCapabilities::terminalReason(entry->session,m_details),true);return;}
        m_detailTabs->setCurrentWidget(m_terminal); m_terminal->connectSession();
    });
    connect(&m_client, &HgsClient::nativeSessionLaunched, this, [this](const QString &id, bool ok, const QString &error) {
        if(!m_creations->forLaunch(id).isEmpty()){
            m_creations->launchFinished(id,ok,error);emit refreshRequested();return;
        }
        // A listing can report the launch ended before its command returns; its
        // notice stays and nothing waits to open a session that never appears.
        if (ok) {
            if (!takeEndedLaunch(id)) { m_nativeLaunchToOpen=id; showNotice(tr("Session started. Loading activity…")); }
        } else {
            newSessionLaunchFailed(id, error);
        }
        emit refreshRequested();
    });
    connect(&m_client, &HgsClient::questionAnswered, this, [this](quint64 request, const QString &, const QString &name, const QJsonObject &receipt) {
        const auto pending = m_pendingAnswers.value(request);
        // Hook review and Codex's native installer continue in Terminal.
        const bool openTerminal = !pending.key.isEmpty() && pending.key == m_selectedKey
            && (pending.questionId.startsWith("codex-hooks-trust:") || pending.questionId.startsWith("codex-update:"))
            && pending.questionHash == receipt.value("question_hash").toString()
            && m_details.value("run_id") == receipt.value("run_id")
            && receipt.value("open_terminal").toBool();
        // After Update now, the receipt says whether Codex still runs its
        // installer or has exited; older hosts omit it.
        QString updateNotice;
        if (!pending.key.isEmpty() && pending.questionId.startsWith("codex-update:")
            && pending.questionHash == receipt.value("question_hash").toString()) {
            const auto entry = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &value) { return value.key == pending.key; });
            const QString target = entry == m_entries.cend() ? name : tr("%1 (%2)").arg(entry->session.name, entry->machine);
            const QString phase = receipt.value("codex_update").toString();
            if (phase == "installing")
                updateNotice = tr("Codex in %1 is installing its update in Terminal. Resume the session after Codex exits.").arg(target);
            else if (phase == "exited")
                updateNotice = tr("Codex in %1 exited after its update. Check the result in Terminal, then resume the session.").arg(target);
        }
        finishQuestion(request, true);
        if (openTerminal) { m_detailTabs->setCurrentWidget(m_terminal); m_terminal->connectSession(); }
        if (!updateNotice.isEmpty()) showNotice(updateNotice);
    });
    connect(&m_client, &HgsClient::questionAnswerFailed, this, [this](quint64 request, const QString &, const QString &, const QString &error, bool uncertain) {
        finishQuestion(request, false, error, uncertain);
    });
    connect(&m_client, &HgsClient::questionAnswerSubmitted, this, [this](quint64 request, const QString &, const QString &, const QJsonObject &) {
        finishQuestion(request, true, {}, false, true);
    });
    connect(m_composer,&MessageComposer::sessionReferenceRequested,this,[this](const QString &key){
        if(const auto *entry=selected();entry&&entry->key==key)copySessionReference(*entry);
    });
    connect(m_composer,&MessageComposer::ironRulesRequested,this,[this]{
        if(!m_ironRules||!m_inspector)return;
        openInspectorCard(InspectorCards::IronRules);
    });
    connect(m_composer,&MessageComposer::interruptRequested,this,[this](const QString &key) {
        const auto *entry=selected();
        if(!entry||key!=m_selectedKey||m_interruptRequest||m_details.value("interrupt_supported")!=true||awaitingRestoredInspection(*entry))return;
        cancelCompactContinuation(m_selectedKey, tr("Interrupted. Your draft was kept without sending."));
        if (m_startSend.key == m_selectedKey) cancelStartContinuation(tr("Interrupted. Your draft was kept without sending."));
        m_interruptKey=key;
        m_interruptName=entry->session.name; m_interruptMachine=entry->machine;
        // As in Terminal, the prompt of an interrupted turn returns to the message field.
        const QString prompt=interruptedPrompt(key);
        if(prompt.isEmpty())m_interruptPrompts.remove(key);
        else m_interruptPrompts.insert(key,{m_details.value("run_id").toString(),m_details.value("conversation_id").toString(),prompt});
        m_interruptRequest=m_client.requestInterrupt(entry->host,entry->session.name,m_details.value("run_id").toString(),
            m_details.value("conversation_id").toString(),m_details.value("turn_started").toDouble());
        renderDetails();
    });
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &,const QString &error) {
        if(id!=m_interruptRequest)return;
        m_interruptRequest=0;
        if(!ok)m_interruptPrompts.remove(m_interruptKey);
        const auto target = tr("%1 (%2)").arg(m_interruptName, m_interruptMachine);
        const bool alreadyStopped = error.trimmed() == "hgs: This turn is no longer working"
            || error.trimmed() == "This turn is no longer working";
        if (ok) showNotice(tr("Interrupt sent to %1. Waiting for the agent to stop…").arg(target));
        else if (alreadyStopped) showNotice(tr("Stop %1: the turn already ended or is waiting for input. No action needed.").arg(target));
        else showNotice(tr("Stop %1 failed: %2\nOpen Terminal to check the agent before trying again.").arg(target, error), true);
        if(m_selectedKey==m_interruptKey)inspect();
        emit refreshRequested();
        renderDetails();
    });
    connect(m_question, &QuestionCard::draftRestored, this, [this] { renderDetails(); });
    connect(m_composer, &MessageComposer::sendRequested, this, [this](const QString &key,const QString &text,const QList<MessageAttachment> &files){sendMessage(key,text,files);});
    // Send and push now: only this field's messages enter the session's native queue.
    m_composer->setPushEnabled(true);
    connect(m_composer, &MessageComposer::pushRequested, this, &SessionsWindow::armPush);
    connect(m_composer, &MessageComposer::sendNowRequested, this, [this] { sendQueuedNow(); });
    // An unsent draft becomes the row's status (see SessionDelegate::statusOf).
    const auto draftChanged = [this](const QString &key) {
        for (int row = 0; row < m_sessions->count(); ++row) {
            auto *item = m_sessions->item(row);
            if (item->data(SessionRoles::Key).toString() == key && item->data(SessionRoles::ChildId).toString().isEmpty()) {
                // The next applied rows rewrite an item changed here (applyListRows).
                item->setData(SessionRoles::Draft, m_composer->hasDraft(key)); item->setData(SessionRoles::Digest, QByteArray());
            }
        }
        // Presence changes once when typing starts or the draft clears, rather
        // than on each keystroke. Defer list rebuilding until the editor returns.
        if (m_draftRefreshPending) return;
        m_draftRefreshPending = true;
        QTimer::singleShot(0, this, [this] {
            m_draftRefreshPending = false;
            if (isVisible()) rebuild();
            updateDashboard();
        });
    };
    connect(m_composer, &MessageComposer::draftChanged, this, draftChanged);
    connect(m_subagentComposer, &MessageComposer::draftChanged, this, draftChanged);
    connect(m_composer, &MessageComposer::draftLoaded, this, draftChanged);
    connect(m_subagentComposer, &MessageComposer::draftLoaded, this, draftChanged);
    for (auto *composer : {m_composer, m_subagentComposer}) {
        composer->canOpenDraftSession = [this](const QString &key) { return draftOwner(key) != nullptr; };
        composer->savedDraftSessionRoles = [this](const QString &key) {
            QMap<int,QVariant> roles;
            const auto *entry=draftOwner(key);if(!entry)return roles;
            const auto &session=entry->session;
            roles={{SessionRoles::Title,sessionLabel(session)},{SessionRoles::Agent,session.cmd},
                {SessionRoles::Host,entry->machine},{SessionRoles::MachineName,entry->machine},
                {SessionRoles::MachineColor,MachineAppearance::color(entry->machine)},
                {SessionRoles::Meta,SessionPresentation::checkoutLine(session)},
                {SessionRoles::MetaIcon,SessionPresentation::checkoutIcon(session)}};
            if(ComposerDraftStore::owner(key).kind!=ComposerDraftOwner::Kind::Subagent){roles[SessionRoles::Model]=session.model;roles[SessionRoles::Effort]=session.effort;}
            return roles;
        };
        connect(composer, &MessageComposer::draftSessionRequested, this, &SessionsWindow::openDraftSession);
    }
    connect(m_activityView, &ActivityView::messageActionRequested, this, &SessionsWindow::messageAction);
    connect(m_composer, &MessageComposer::settingsTerminalRequested, this, [this](const QString &key) {
        if (key != m_selectedKey || !selected()) return;
        m_detailTabs->setCurrentWidget(m_terminal); m_terminal->connectSession();
    });
    connect(m_composer, &MessageComposer::settingsRequested, this, &SessionsWindow::changeModelSettings);
    // U53: the owner's confirmed account choice; one switch at a time.
    m_composer->accountChip()->switchRequested = [this](const QString &key, const QString &account) {
        const auto *entry = selected();
        if (m_accountSwitchRequest || !entry || entry->key != key || !accountSwitchBlockReason(*entry).isEmpty()) {
            m_composer->accountChip()->finish(key, {}, tr("the session changed before the switch began; choose again"));
            return renderDetails();
        }
        m_accountSwitchKey = key;
        m_accountSwitchRequest = m_client.requestSwitchAccount(entry->host, entry->session.name, account);
    };
    connect(&m_client, &HgsClient::switchAccountFinished, this, [this](quint64 request, const QString &, const QString &, const QJsonObject &receipt, const QString &error) {
        if (request != m_accountSwitchRequest) return;
        m_accountSwitchRequest = 0;
        const auto key = std::exchange(m_accountSwitchKey, QString());
        const auto outcome = m_composer->accountChip()->finish(key, receipt, error);
        if (!outcome.text.isEmpty()) showNotice(outcome.text, outcome.kind != AccountSwitch::Outcome::Switched);
        // The session runs again (or still) under the account the receipt names.
        refreshRequested(); if (const auto *entry = selected(); entry && entry->key == key) inspect();
    });
    connectOpenCodeModels();
    connect(&m_client, &HgsClient::settingsFinished, this, [this](quint64 request, bool ok, const QJsonObject &receipt, const QString &error) {
        if (request != m_settingsRequest) return;
        m_settingsRequest = 0;
        // Dispatch schedules an automatic apply when the session left Ready
        // after the idle listing, as for a chooser that appeared meanwhile. It
        // stays queued for one more attempt after the next Ready transition;
        // a failure, a refusal and an applied change end automatic attempts.
        const bool scheduledAgain = ok && receipt.value("status") == "scheduled" && m_settingsAutomatic;
        if (scheduledAgain) m_scheduledSettings[m_settingsKey] = {m_settingsIdentity};
        else m_scheduledSettings.remove(m_settingsKey);
        if (ok && receipt.value("status") == "scheduled" && !m_settingsAutomatic) {
            m_queuedSettings[m_settingsKey] = {m_settingsHost, m_settingsName, m_settingsRun, m_settingsConversation,
                receipt.value("model").toString(), receipt.value("effort").toString(), receipt.value("pending_settings_id").toString(receipt.value("request_id").toString())};
        } else if (!scheduledAgain) m_queuedSettings.remove(m_settingsKey);
        m_settingsIdentity.clear();
        if (m_selectedKey == m_settingsKey && m_details.value("run_id").toString() == m_settingsRun
            && m_details.value("conversation_id").toString() == m_settingsConversation && ok) {
            const bool scheduled = receipt.value("status") == "scheduled";
            if (scheduled) {
                m_details["pending_model"] = receipt.value("model"); m_details["pending_effort"] = receipt.value("effort");
                m_details["pending_settings_id"] = receipt.value("pending_settings_id").toString(receipt.value("request_id").toString());
            } else {
                m_details["model"] = receipt.value("model"); m_details["effort"] = receipt.value("effort");
                m_details.remove("pending_model"); m_details.remove("pending_effort"); m_details.remove("pending_settings_id");
            }
            // inspect() below starts nothing while one runs, and that one read
            // the session before this change.
            if (const auto *entry = selected();
                entry && m_client.inspectionInFlight(entry->host, entry->session.name, entry->session.archiveId))
                m_inspectionBehindSettings.insert(m_settingsKey);
        }
        // A send or another client can consume/replace the pending request.
        // These exact preflight refusals send no native input; refresh below
        // without reporting a failed mutation or retrying the stale request.
        const QString detail = error.startsWith("hgs: ") ? error.mid(5) : error;
        const bool stalePending = m_settingsAutomatic &&
            (detail == "pending settings changed; refresh before applying"
             || detail == "pending settings target changed; refresh before applying");
        if (!ok && !stalePending) showNotice(tr("Model settings for %1: %2").arg(m_settingsName, error), true);
        m_settingsKey.clear(); m_settingsRun.clear(); m_settingsConversation.clear();
        renderDetails(); emit refreshRequested(); inspect();
    });
    connect(&m_client, &HgsClient::messageSent, this, [this](quint64 request, const QString &, const QString &, const QJsonObject &receipt) {
        finishMessage(request, true, receipt);
    });
    connect(&m_client, &HgsClient::messageFailed, this, [this](quint64 request, const QString &, const QString &, const QString &error, bool uncertain) {
        finishMessage(request, false, {}, error, uncertain);
    });
    m_terminal = new TerminalView;
    m_children = browser(); m_children->setObjectName("subagents"); m_children->setAccessibleName(tr("Subagents")); m_children->viewport()->installEventFilter(this);
    connect(m_children, &QTextBrowser::anchorClicked, this, [this](const QUrl &url) {
        if (url.scheme() == "hgs-agent") openSubagent(url.path(QUrl::FullyDecoded));
        else if (url.scheme() == "hgs-fold") toggleInspectorSection(url.path(QUrl::FullyDecoded));
        else if (url.scheme() == "hgs-children") { m_showFinishedChildren = true; renderDetails(); }
    });
    m_info = browser(); m_info->setObjectName("sessionInfo"); m_info->setAccessibleName(tr("Session details"));
    connect(m_info, &QTextBrowser::anchorClicked, this, [this](const QUrl &url) {
        if (url.scheme() == "hgs-fold") toggleInspectorSection(url.path(QUrl::FullyDecoded));
    });
    tabs->addTab(m_activityStack, tr("Activity")); tabs->addTab(m_terminal, tr("Terminal"));
    // The right inspector owns the only board surface. Keep its shared
    // controller hidden; a selection opens the central Task View.
    m_tasks = new TasksView(&m_client, this); m_tasks->hide();
    m_tasks->setOpenInInspector(true);
    connect(m_tasks, &TasksView::notice, this, [this](const QString &message, bool error) { showNotice(message, error); });
    m_nativeUi = new NativeUiView(&m_client, [] { return NativeBrowserPlugin::factory(); });
    tabs->addTab(m_nativeUi, tr("Native UI")); tabs->setTabVisible(tabs->indexOf(m_nativeUi), false);
    m_processes=new ProcessView(&m_client);tabs->addTab(m_processes,tr("Processes"));tabs->setTabVisible(tabs->indexOf(m_processes),ProcessSettings::enabled());
    connect(m_processes,&ProcessView::refreshRequested,this,&SessionsWindow::inspect);
    connect(m_activityView,&ActivityView::teamTaskActivated,this,&SessionsWindow::openTeamTask);   // WhiteWh fork: agent teams
    for(auto *view:{m_activityView,m_subagentView})connect(view,&ActivityView::processRequested,this,[this](const QString &id){if(!ProcessSettings::enabled())return;m_detailTabs->setCurrentWidget(m_processes);m_processes->selectProcess(id);});
    connect(m_nativeUi, &NativeUiView::responseRequested, this, [this] { closeSubagent(); m_detailTabs->setCurrentWidget(m_activityStack); });
    m_workSplitter = new QSplitter(Qt::Horizontal); m_workSplitter->setObjectName("sessionWorkSplitter");
    m_workSplitter->setHandleWidth(8);
    m_workSplitter->setChildrenCollapsible(false);
    m_workSplitter->addWidget(tabs); tabs->setMinimumWidth(280);
    m_inspectorPanel = new QFrame; m_inspectorPanel->setObjectName("sessionInspectorPanel");
    auto *inspectorLayout = new QVBoxLayout(m_inspectorPanel);
    inspectorLayout->setContentsMargins(0, 0, 0, 0); inspectorLayout->setSpacing(0);
    // U25 part 6 (spec 6): one column of collapsible cards, no tabs.
    m_inspector = new InspectorCards;
    m_worktrees=new WorktreePanel(&m_client);
    m_worktrees->filterRequested=[this](const QString &path,bool checkout){if(const auto *entry=selected())filterFolder(entry->host,path,checkout);};
    m_worktrees->sessionRequested=[this](const QString &name,const QString &archive){if(const auto *entry=selected())openRelatedSession(entry->host,name,archive);};
    m_worktrees->launchRequested=[this](const QString &path){if(const auto *entry=selected())showNewSession({},m_organization.groupFor(entry->identity),entry->host,{},path);};
    // Environment (spec 6): the Session ID row with its copy button, then
    // machine, folder, branch, model, last event and Keep cache warm.
    auto *environment=m_inspector->body(InspectorCards::Environment);
    const auto envIcon=[](const char *glyph){auto *icon=new QLabel;icon->setObjectName("inspectorEnvIcon");icon->setProperty("glyph",QString::fromLatin1(glyph));icon->setFixedSize(14,14);return icon;};
    m_detailsIdTitle = label(tr("Session ID"), "detailsIdentityTitle");
    m_detailsIdValue = new SessionIdentityLabel(tr("Not confirmed"));
    m_detailsIdValue->setObjectName("detailsIdentityValue"); m_detailsIdValue->setTextFormat(Qt::PlainText);
    m_detailsIdValue->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred); m_detailsIdValue->setMinimumWidth(0);
    m_detailsIdValue->setWordWrap(false);
    m_detailsIdValue->setTextInteractionFlags(Qt::TextSelectableByMouse | Qt::TextSelectableByKeyboard);
    m_copySessionId = iconButton("copy", tr("Copy session ID"), "copySessionIdAction"); m_copySessionId->setAutoDefault(false);
    m_copySessionId->setProperty("sessionHeaderAction", true);
    configureWorkspaceIconButton(m_copySessionId, "copy", tr("Copy session ID"), m_dark);
    connect(m_copySessionId, &QPushButton::clicked, this, &SessionsWindow::copySessionIdentity);
    auto *identity = new InspectorIdentityRow(envIcon("key"), m_detailsIdTitle, m_detailsIdValue, m_copySessionId);
    identity->setObjectName("detailsIdentity");
    environment->addWidget(identity);
    for(const auto &[glyph,name,mode]:{std::tuple{"monitor","machine",Qt::ElideRight},std::tuple{"folder","folder",Qt::ElideRight},
            std::tuple{"git-branch","branch",Qt::ElideMiddle},std::tuple{"asterisk","model",Qt::ElideRight},std::tuple{"clock","last event",Qt::ElideRight}}) {
        auto *row=new QWidget;row->setObjectName("inspectorEnvRow");row->setProperty("row",QString::fromLatin1(name));
        auto *h=new QHBoxLayout(row);h->setContentsMargins(0,0,0,0);h->setSpacing(9);
        auto *value=new InspectorValue(mode);value->setObjectName("inspectorEnvValue");value->setProperty("row",QString::fromLatin1(name));
        auto *key=new QLabel(tr(name));key->setObjectName("inspectorEnvKey");
        h->addWidget(envIcon(glyph));h->addWidget(value,1);h->addWidget(key);
        m_environment.insert(QString::fromLatin1(name),value);
        environment->addWidget(row);
    }
    m_detailsContext=new QLabel;m_detailsContext->setObjectName("detailsContext");m_detailsContext->setWordWrap(true);m_detailsContext->hide();
    m_detailsClear=new QPushButton(tr("Clear session"));m_detailsClear->setObjectName("clearSessionFromDetails");m_detailsClear->setAutoDefault(false);
    m_detailsClear->setToolTip(tr("Start this agent's conversation from scratch, like /clear in Terminal. Your unsent draft is kept."));
    connect(m_detailsClear,&QPushButton::clicked,this,&SessionsWindow::clearContext);
    // WhiteWh fork: keep the prompt cache warm while the person is away.
    auto *warmRow=new QHBoxLayout;m_keepWarm=new QCheckBox;m_keepWarm->setObjectName("keepCacheWarm");m_keepWarm->setAccessibleName(tr("Keep cache warm"));
    m_keepWarm->setToolTip(tr("Shortly before the provider's prompt cache expires, Zerus sends the idle agent a one-word service message, "
        "so the next turn reads the context from the cache. Each one uses a little of the account's limit; it is never shown as a reply."));
    m_keepWarmHours=new QSlider(Qt::Horizontal);m_keepWarmHours->setObjectName("keepCacheWarmHours");
    m_keepWarmHours->setRange(1,qMin(8,CacheKeepalive::MaxHours));m_keepWarmHours->setPageStep(1);/* spec 6: 1-8 h */m_keepWarmHours->setValue(CacheKeepalive::DefaultHours);
    m_keepWarmHours->setToolTip(tr("Stop warming after this long without a turn"));
    m_keepWarmLimit=new QLabel;m_keepWarmLimit->setObjectName("keepCacheWarmLimit");
    // The slider (1-8 h) shows while the switch is on (spec 6).
    m_keepWarmHours->setFixedWidth(70);
    auto *warmLabel=new QLabel(tr("Keep cache warm"));warmLabel->setObjectName("keepCacheWarmLabel");
    warmRow->setContentsMargins(0,0,0,0);warmRow->setSpacing(9);
    warmRow->addWidget(envIcon("bolt"));warmRow->addWidget(warmLabel,1);warmRow->addWidget(m_keepWarmHours);warmRow->addWidget(m_keepWarmLimit);warmRow->addWidget(m_keepWarm);
    environment->addLayout(warmRow);
    connect(m_keepWarm,&QCheckBox::toggled,this,[this](bool on){
        m_keepWarmHours->setEnabled(on);m_keepWarmHours->setVisible(on);m_keepWarmLimit->setVisible(on);setKeepWarm(on?m_keepWarmHours->value():0);});
    connect(m_keepWarmHours,&QSlider::valueChanged,this,[this](int hours){
        m_keepWarmLimit->setText(tr("%n h",nullptr,hours));if(m_keepWarm->isChecked())setKeepWarm(hours);});
    // Context (spec 6): what hgs reports - the share used, used / limit, the
    // bar, what is left and Clear session; the category rows only when an
    // agent reports them. Then the session's own usage details.
    auto *context=m_inspector->body(InspectorCards::Context);
    {auto *head=new QHBoxLayout;head->setContentsMargins(0,0,0,0);
     m_contextUsed=new QLabel;m_contextUsed->setObjectName("inspectorContextUsed");
     m_contextTokens=new QLabel;m_contextTokens->setObjectName("inspectorContextTokens");
     head->addWidget(m_contextUsed,1);head->addWidget(m_contextTokens);context->addLayout(head);}
    m_contextBar=new ContextBar;context->addWidget(m_contextBar);
    m_contextRows=new QWidget;m_contextRows->setObjectName("inspectorContextRows");new QVBoxLayout(m_contextRows);
    m_contextRows->layout()->setContentsMargins(0,0,0,0);m_contextRows->hide();context->addWidget(m_contextRows);
    {auto *row=new QHBoxLayout;row->setContentsMargins(0,0,0,0);
     m_contextLeft=new QLabel;m_contextLeft->setObjectName("inspectorContextLeft");
     row->addWidget(m_contextLeft,1);row->addWidget(m_detailsClear);context->addLayout(row);}
    context->addWidget(m_detailsContext);
    context->addWidget(m_info);
    // WhiteWh fork: the SocratiCode index of this session's folder.
    m_socratiCode=new SocratiCodePanel(&m_client);m_inspector->body(InspectorCards::SocratiCode)->addWidget(m_socratiCode);
    // WhiteWh fork: the iron rules of this session's project.
    // WhiteWh fork: Tasks & agents holds the agent's text, then the session's
    // place in its project and the project's iron rules.
    // Tasks (spec 6): the agent's plan and subagents, then Reports to.
    auto *agentsLayout=m_inspector->body(InspectorCards::Tasks);
    agentsLayout->addWidget(m_children);
    m_ironRules=new IronRulesPanel;
    m_ironRules->changed=[this](int count){m_composer->setIronRulesCount(count);m_inspector->setValue(InspectorCards::IronRules,count>0?QString::number(count):QString());};
    // WhiteWh fork: lead and subordinate sessions of this project.
    m_leadPanel=new SessionLeadPanel;agentsLayout->addWidget(m_leadPanel);
    m_inspector->body(InspectorCards::IronRules)->addWidget(m_ironRules);
    // The card's header names the rules and counts them.
    for(const char *name:{"inspectorPanelTitle","ironRulesCount"})if(auto *label=m_ironRules->findChild<QLabel *>(name))label->hide();
    m_leadPanel->setLead=[this](const QString &identity,const QString &lead){
        if(!SessionLeads::setLead(identity,lead)){showNotice(tr("A lead cannot report to another session, and a subordinate cannot lead."),true);updateLeadPanel();return;}
        rebuild();updateLeadPanel();
    };
    m_leadPanel->open=[this](const QString &identity){
        for(const auto &entry:std::as_const(m_entries))if(entry.identity==identity){showSession(entry.host,entry.session.name);return;}
    };
    m_leadPanel->sendReport=[this]{sendReportToLead();};
    m_inspector->body(InspectorCards::Worktrees)->addWidget(m_worktrees);
    // Changes (spec 6): "+A −D" in the header; the files with their lines,
    // Commit changes… (fills the composer, never sends) and Open diff.
    {auto *changes=m_inspector->body(InspectorCards::Changes);
     m_changesList=new QWidget;m_changesList->setObjectName("inspectorChanges");auto *list=new QVBoxLayout(m_changesList);
     list->setContentsMargins(0,0,0,0);list->setSpacing(4);changes->addWidget(m_changesList);
     m_changesEmpty=new QLabel(tr("Working tree is clean."));m_changesEmpty->setObjectName("inspectorChangesEmpty");changes->addWidget(m_changesEmpty);
     auto *row=new QHBoxLayout;row->setContentsMargins(0,0,0,0);
     m_commitChanges=new QPushButton(tr("Commit changes…"));m_commitChanges->setObjectName("inspectorCommitChanges");m_commitChanges->setAutoDefault(false);
     m_commitChanges->setToolTip(tr("Ask this session's agent to commit: the request goes into the composer, not sent"));
     m_openDiff=new QPushButton(tr("Open diff"));m_openDiff->setObjectName("inspectorOpenDiff");m_openDiff->setAutoDefault(false);m_openDiff->setProperty("ghost",true);
     row->addWidget(m_commitChanges);row->addWidget(m_openDiff);row->addStretch();changes->addLayout(row);
     connect(m_commitChanges,&QPushButton::clicked,this,[this]{
         auto *editor=m_composer->editor();const QString ask=tr("Commit the current changes with a short message.");
         editor->setPlainText(editor->toPlainText().trimmed().isEmpty()?ask:editor->toPlainText().trimmed()+QStringLiteral("\n")+ask);
         editor->moveCursor(QTextCursor::End);editor->setFocus(Qt::OtherFocusReason);});
     connect(m_openDiff,&QPushButton::clicked,this,[this]{
         const auto *entry=selected();if(!entry)return;
         m_diffRequest=m_client.requestGitDiff(entry->host,m_details.value("cwd").toString(entry->session.cwd));});
     connect(&m_client,&HgsClient::gitDiffReady,this,[this](quint64 request,const QJsonObject &data,const QString &error){
         if(request==m_diffRequest)showDiff(data,error);});
     connect(&m_client,&HgsClient::gitStatusChanged,this,[this](const QString &host,const QString &path){
         const auto *entry=selected();
         if(entry&&entry->host==host&&m_details.value("cwd").toString(entry->session.cwd)==path)updateChangesCard();});}
    m_inspector->toggled=[this](InspectorCards::Card card,bool open){
        if(!open)return;
        if(card==InspectorCards::Worktrees)updateWorktrees();
        if(card==InspectorCards::Changes)updateChangesCard();
    };
    // WhiteWh fork: agent teams. The Team tab shows the selected session's team
    // or a team selected in the list; `?N` counts questions in member sessions.
    m_teams=new TeamController(new HgsClient(hgsPath,this),this);
    m_teamPanel=new TeamPanel(m_teams);
    m_proposalCards->decide=[this](const QJsonObject &input,const TeamProposalCards::Done &done){m_teams->act(QStringLiteral("decide-proposal"),input,done);};
    m_proposalCards->current=[this]()->const Teams::Team *{
        const QString id=m_proposalCards->teamId();
        if(const auto *watched=m_teams->watched();watched&&watched->id==id)return watched;
        return m_teams->team(id);
    };
    m_teamPanel->openSession=[this](const QString &host,const QString &name){showSession(host,name);};
    m_teamPanel->sendMessage=[this](const QString &host,const QString &name,const QString &text){
        for(const auto &entry:std::as_const(m_entries))if(entry.host==host&&entry.session.name==name&&entry.session.state!="archived"){
            m_client.requestSendMessage(host,name,text,{},entry.session.runId,entry.session.conversationId);showNotice(tr("Reminder sent to %1").arg(name));return;}
        showNotice(tr("%1 is not listed; open it to send the reminder").arg(name),true);
    };
    m_teamPanel->answersChanged=[this](int answers){
        m_inspector->setProperty("teamAnswers",answers);updateEnvironmentCard();
    };
    m_teamPanel->setLive([this](const Teams::Member &member){return teamLive(member);});
    // F7: sessions a Done/Stopped team left running are stopped here, after the
    // Team tab confirms; "Take into a new team" opens the New team flow, whose
    // member editor can adopt the session.
    m_teamPanel->stopSession=[this](const QString &host,const QString &name){stopTeamSession(host,name);};
    m_teamPanel->takeIntoTeam=[this](const QString &host,const QString &name){showNewTeam(host,name);};
    // F8: the free sessions a member may adopt: listed, running, and not a
    // member of a team that is still going.
    m_teamPanel->sessionChoices=[this]{
        QList<Teams::SessionChoice> choices;
        for(const auto &entry:std::as_const(m_entries)){
            if(entry.session.state!="running")continue;
            if(const auto *team=m_teams->teamOfSession(entry.host,entry.session.name))
                if(team->status=="planning"||team->status=="running"||team->status=="paused")continue;
            choices.append({entry.host,entry.session.name,entry.session.runId,entry.session.conversationId,entry.session.cmd,entry.session.cwd});
        }
        return choices;
    };
    m_collapsedTeams=[]{const auto list=Settings::value("workspace/collapsedTeams").toStringList();return QSet<QString>(list.cbegin(),list.cend());}();
    connect(m_sessions,&SessionList::teamActivated,this,&SessionsWindow::showTeam);
    // U25 (spec 2.1): a project's '+' opens New session for that project.
    connect(m_sessions,&SessionList::projectNewSession,this,[this](const QString &group){showNewSession({},group,selectedHostTarget());});
    // U25 (spec 2.2): a chip opens where its count lives: services and
    // background commands in Processes (or Terminal), git in the inspector.
    // U30 (#508): a cold card's + New starts a session in the same folder and
    // machine, with its agent and the name "<name>-2"; Resume opens it.
    connect(m_sessions,&SessionList::coldActionRequested,this,[this](const QString &key,const QString &action){
        const auto found=std::find_if(m_entries.cbegin(),m_entries.cend(),[&](const Entry &e){return e.key==key;});
        if(found==m_entries.cend())return;
        const Entry entry=*found;
        if(action=="new"){
            const QString name=sessionLabel(entry.session);
            showNewSession(entry.session.cmd,m_organization.groupFor(entry.identity),entry.host,{},entry.session.cwd,{},{},{},{},name+QStringLiteral("-2"));
            return;
        }
        for(int row=0;row<m_sessions->count();++row)if(m_sessions->item(row)->data(KeyRole).toString()==key){m_sessions->setCurrentRow(row);break;}
        showNotice(tr("Continuing %1 · the first message rebuilds the cache").arg(sessionLabel(entry.session)));
        m_composer->setFocus();
    });
    // Cooling counts down and turns Cold by itself between polls.
    m_stateClock.setInterval(30000);
    connect(&m_stateClock,&QTimer::timeout,this,[this]{if(m_sessions->isVisible())m_sessions->viewport()->update();});
    m_stateClock.start();
    connect(m_sessions,&SessionList::chipActivated,this,[this](const QString &key,int kind){
        for(int row=0;row<m_sessions->count();++row)if(m_sessions->item(row)->data(KeyRole).toString()==key){m_sessions->setCurrentRow(row);break;}
        const auto *entry=selected();if(!entry||entry->key!=key)return;
        if(SessionRowChips::isGit(SessionRowChips::Kind(kind))){openInspectorCard(InspectorCards::Changes);return;}
        if(ProcessSettings::enabled()&&entry->online){m_detailTabs->setCurrentWidget(m_processes);return;}
        if(entry->session.cmd=="dsh"){showNativeUi(*entry);return;}
        if(!NativeSessionCapabilities::terminal(entry->session,m_details)){showNotice(NativeSessionCapabilities::terminalReason(entry->session,m_details),true);return;}
        m_detailTabs->setCurrentWidget(m_terminal);m_terminal->connectSession();
    });
    m_teamPanel->setEmbedded(true);m_inspector->body(InspectorCards::Team)->addWidget(m_teamPanel);
    // The team workspace: the Team tab next to Activity, shown while a team is.
    m_teamWorkspace=new TeamWorkspace(m_teams,m_teamPanel,this);
    m_teamWorkspace->openSession=m_teamPanel->openSession;
    m_teamWorkspace->openTerminal=[this](const QString &host,const QString &name){openSessionTerminal(host,name);};
    m_teamWorkspace->sendMessage=m_teamPanel->sendMessage;
    m_teamWorkspace->openTeamTab=[this]{openInspectorCard(InspectorCards::Team);};
    m_teamWorkspace->setLive([this](const Teams::Member &member){return teamLive(member);});
    // U25 (spec 3): the team's status and actions live in the lead session's
    // header; the Team tab has no name bar of its own.
    m_teamWorkspace->takeHeader();
    m_titleRow->insertWidget(1,m_teamWorkspace->headerStatus());
    m_chipsRow->addWidget(m_teamWorkspace->headerActions());
    m_teamWorkspace->openWorkspace=[this]{if(m_teamPanel->openWorkspace)m_teamPanel->openWorkspace();};
    // U55: one feed for both banners; it reads while the window shows.
    m_waitingFeed=new TeamWaitingFeed(m_teams,this);
    connect(m_waitingFeed,&TeamWaitingFeed::notify,this,[this](const QString &token,const QString &title,const QString &body){emit teamAttentionRaised(token,title,body);});
    // #724: an item answered in a banner or on the page leaves both at once and
    // its notifications are withdrawn.
    connect(m_waitingFeed,&TeamWaitingFeed::withdrawn,this,[this](const QStringList &tokens){for(const auto &token:tokens)emit teamAttentionCleared(token);});
    connect(m_waitingFeed,&TeamWaitingFeed::dropped,this,[this](const QString &id){if(m_teamWaitingPage)m_teamWaitingPage->drop(id);});
    for(auto **banner:{&m_waitingBanner,&m_teamWaitingBanner}){
        *banner=new TeamWaitingBanner(m_waitingFeed);
        (*banner)->openItem=[this](const TeamWaitingPage::Item &item){openWaitingItem(item.team,item.kind,item.phase,item.task);};
        (*banner)->openAll=[this]{showTeamWaiting();};
    }
    m_waitingBanner->setObjectName("sessionWaitingBanner");m_waitingHost->layout()->addWidget(m_waitingBanner);
    m_teamWaitingBanner->setObjectName("workspaceWaitingBanner");m_teamWorkspace->setWaitingBanner(m_teamWaitingBanner);
    // The tab exists only while a team is shown.
    m_teamWorkspace->hide();
    m_teamNewTab=new QWidget(this);m_teamNewTab->setObjectName("teamNewTab");
    {auto *v=new QVBoxLayout(m_teamNewTab);v->setContentsMargins(0,0,0,0);}
    m_teamNewTab->hide();
    connect(m_detailTabs,&QTabWidget::currentChanged,this,[this]{if(m_detailTabs->currentWidget()==m_teamNewTab)ensureTeamNewForm();});
    m_teamPanel->newTeam=[this]{showNewTeam();};
    m_teamPanel->openDesigner=[this]{showTeamDesigner();};
    m_teamPanel->openWorkspace=[this]{if(m_detailTabs->indexOf(m_teamWorkspace)<0)m_detailTabs->addTab(m_teamWorkspace,tr("Team"));m_detailTabs->setCurrentWidget(m_teamWorkspace);};
    // N7 (U18): every team's attention items reach the desktop notifications,
    // once each; the known ones are kept across restarts.
    m_teamNotices=std::make_shared<TeamAttentionNotices>(Settings::value("teams/attentionNotified").toStringList());
    connect(m_teams,&TeamController::watchedChanged,this,[this]{updateProposalCards();});
    connect(m_teams,&TeamController::teamsChanged,this,[this]{
        const SlowEventLog::Scope scope(this,"teams changed");
        // U25 (spec 2.3): members other than the lead are muted by default.
        QSet<QString> members;
        for(const auto &team:m_teams->teams())if(team.status!="archived")
            for(const auto &member:team.members)if(!member.lead&&member.id!=team.lead&&!member.session.isEmpty())members.insert(member.host+'\n'+member.session);
        if(members!=m_teamMembers){m_teamMembers=members;m_fleet.setTeamMembers(members);emit teamMembersChanged(members);}
        rebuild();updateTeamPanel();
        if(!m_pendingTeamAttention.isEmpty()&&m_teams->team(TeamAttentionNotices::teamOf(m_pendingTeamAttention)))showTeamAttention(m_pendingTeamAttention);
        auto changes=m_teamNotices->observe(m_teams->teams());
        if(changes.raised.isEmpty()&&changes.cleared.isEmpty())return;
        Settings::set("teams/attentionNotified",m_teamNotices->known());
        // U55: a thing the waiting banner announced already is not announced twice.
        TeamWaitingNotices::dropAnnounced(changes,m_teams->teams());
        for(const auto &token:changes.cleared)emit teamAttentionCleared(token);
        for(const auto &notice:changes.raised)emit teamAttentionRaised(notice.token,notice.title,notice.body);
    });
    const auto taskLaunchBytes = settings.value("tasks/pendingExecutorLaunches").toByteArray();
    const auto taskLaunchDoc = SettingsJson::document(QStringLiteral("tasks/pendingExecutorLaunches"), 65536);
    m_taskLaunches = taskLaunchDoc.object();
    bool validLaunches = taskLaunchBytes.isEmpty() || taskLaunchDoc.isObject();
    if (m_taskLaunches.size() > 16) validLaunches = false;
    for (auto it = m_taskLaunches.constBegin(); it != m_taskLaunches.constEnd() && validLaunches; ++it) {
        const auto record = it.value().toObject();
        validLaunches = !QUuid(it.key()).isNull() && !record.value("task").toString().isEmpty() && !record.value("project").toString().isEmpty()
            && !record.value("parent").toString().isEmpty() && !record.value("cmd").toString().isEmpty()
            && QStringList{"waiting", "reserved", "review"}.contains(record.value("phase").toString());
    }
    if (!validLaunches) { m_taskLaunches = {}; m_taskLaunchRecoveryBlocked = true; }
    m_tasks->setExecutorLaunches(m_taskLaunches);
    connect(m_tasks, &TasksView::newExecutorRequested, this, [this](const TaskItem &task, const QString &project) {
        if (task.targetId.isEmpty() || task.targetMachine.isEmpty()) { showNotice(tr("Choose a parent with a confirmed computer and session identity."), true); return; }
        if (m_taskLaunchRecoveryBlocked) { showNotice(tr("The saved executor launch records could not be read. Review them before creating another task executor."), true); return; }
        if (m_taskLaunches.size() >= 16) { showNotice(tr("Finish or review the pending task executor launches first."), true); return; }
        for (const auto &value : m_taskLaunches) if (value.toObject().value("task").toString() == task.uuid) {
            showNotice(tr("This task already has an executor launch pending. Check its session and task history before starting it again."), true); return;
        }
        showNewSession(task.targetSession.section('/', 0, 0), project, task.targetHost, {}, {}, {}, {}, task.uuid, task.targetMachine + QLatin1Char('/') + task.targetId);
    });
    connect(m_tasks, &TasksView::executorLaunchReviewRequested, this, [this](const QString &uuid) {
        for (auto it = m_taskLaunches.constBegin(); it != m_taskLaunches.constEnd(); ++it) {
            const auto record = it.value().toObject(); if (record.value("task").toString() != uuid) continue;
            const auto launch = it.key();
            QMessageBox review(QMessageBox::Information, tr("Task executor launch"),
                tr("Session: %1\nLaunch: %2\nState: %3\n%4\n\nCheck the task history and this executor before allowing another launch. Releasing the launch record does not stop an agent or retry delivery.")
                    .arg(record.value("session").toString(record.value("tag").toString()), launch, record.value("phase").toString(), record.value("error").toString()),
                QMessageBox::Close, this);
            review.setTextFormat(Qt::PlainText); review.setObjectName("taskExecutorLaunchReviewDialog");
            auto *release = review.addButton(tr("I reviewed it · allow another launch"), QMessageBox::ActionRole);
            release->setEnabled(record.value("phase") != "waiting"); review.exec();
            if (review.clickedButton() == release && m_taskLaunches.value(launch).toObject() == record) {
                m_taskLaunches.remove(launch);
                if (!saveTaskLaunches()) { m_taskLaunches[launch] = record; m_tasks->setExecutorLaunches(m_taskLaunches); showNotice(tr("Could not release the executor launch record."), true); }
            }
            return;
        }
    });
    connect(&m_client, &HgsClient::taskReady, this, [this](quint64 id, const QJsonObject &reply) {
        const auto launch = m_taskLaunchRequests.take(id); if (launch.isEmpty()) return;
        auto record = m_taskLaunches.value(launch).toObject(); const auto task = reply.value("task").toObject();
        const auto receipt = reply.value("receipt").toObject().value("request_id").toString();
        QString recorded; for (const auto &value : task.value("log").toArray()) if (value.toObject().value("kind") == "sent") recorded = value.toObject().value("request_id").toString();
        const auto owner = task.value("session").toObject();
        if (task.value("uuid").toString() != record.value("task").toString() || task.value("stage") != "active"
            || owner.value("session_id").toString() != QStringLiteral("launch_id:") + launch || owner.value("host") != record.value("host")
            || receipt.isEmpty() || recorded != receipt) {
            record["phase"] = "review"; record["error"] = tr("Task delivery reply did not confirm this exact executor and receipt");
            m_taskLaunches[launch] = record; saveTaskLaunches();
            showNotice(record.value("error").toString(), true); return;
        }
        m_taskLaunches.remove(launch); saveTaskLaunches(); m_tasks->refresh();
    });
    connect(&m_client, &HgsClient::taskFailed, this, [this](quint64 id, const QString &error) {
        const auto launch = m_taskLaunchRequests.take(id); if (launch.isEmpty()) return;
        auto record = m_taskLaunches.value(launch).toObject(); record["error"] = error.left(2048); record["phase"] = "review";
        m_taskLaunches[launch] = record; saveTaskLaunches();
        showNotice(tr("The executor was created, but task delivery needs review: %1. Check task history before retrying.").arg(error), true);
    });
    m_taskInspector = new TaskInspector(&m_client, m_tasks);
    // Upstream's Tasks tab is a card here, after Changes: the fork's inspector
    // has no tabs, and its Tasks card holds the agent's plan and subagents.
    m_inspector->body(InspectorCards::ProjectTasks)->addWidget(m_taskInspector);
    m_taskView = m_taskInspector->takeDetailsPane();
    new SpeechComposer(m_client.executable(), m_taskInspector->composer(), [this] {
        SpeechComposer::Target target; target.key = m_taskInspector->composer()->sessionKey(); target.draftOnly = true;
        target.session = m_taskInspector->selectedTask(); target.available = m_taskInspector->canDictate(); return target;
    });
    const int taskViewIndex = m_detailTabs->addTab(m_taskView, tr("Task View"));
    m_detailTabs->setTabVisible(taskViewIndex, false);
    connect(m_taskInspector, &TaskInspector::taskViewRequested, this, [this] {
        m_detailTabs->setTabVisible(m_detailTabs->indexOf(m_taskView), true); m_detailTabs->setCurrentWidget(m_taskView);
    });
    connect(m_taskInspector, &TaskInspector::subagentRequested, this, [this](const QString &host, const QString &name, const QString &parentId, const QString &conversation, const QString &child) {
        for (const auto &entry : std::as_const(m_entries)) {
            const QString retained = entry.session.launchId.isEmpty() ? QStringLiteral("run_id:") + entry.session.runId : QStringLiteral("launch_id:") + entry.session.launchId;
            if (entry.host != host || retained != parentId || entry.session.conversationId != conversation) continue;
            const auto task = m_taskInspector->selectedTask();
            openRelatedSession(host, entry.session.name, {}); closeSubagent(); openSubagent(child); m_taskInspector->selectTask(task); return;
        }
        showNotice(tr("This worker belongs to an earlier or unavailable parent conversation. Open the coordinator's history to inspect it."), true);
    });
    connect(m_taskInspector, &TaskInspector::executorRequested, this, [this](const QString &host, const QString &name, const QString &id) {
        // Recheck the retained identity before following a mutable display name.
        for (const auto &entry : std::as_const(m_entries)) {
            const QString retained = entry.session.launchId.isEmpty()
                ? (entry.session.runId.isEmpty() ? QString() : QStringLiteral("run_id:") + entry.session.runId)
                : QStringLiteral("launch_id:") + entry.session.launchId;
            if (entry.host != host || (id.isEmpty() ? entry.session.name != name : retained != id)) continue;
            const QString task = m_taskInspector->selectedTask();
            openRelatedSession(entry.host, entry.session.name, {}); closeSubagent();
            m_detailTabs->setCurrentWidget(m_activityStack); m_taskInspector->selectTask(task); return;
        }
        showNotice(tr("The task's session identity is no longer available."), true);
    });
    connect(m_tasks, &TasksView::taskSelected, this, [this](const QString &uuid) {
        openInspectorCard(InspectorCards::ProjectTasks); m_taskInspector->openTask(uuid);
    });
    inspectorLayout->addWidget(m_inspector);
    // In the scrolling column the two text views are as tall as their text.
    for(auto *text:{m_children,m_info}){
        text->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);text->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
        text->setFrameShape(QFrame::NoFrame);text->setSizePolicy(QSizePolicy::Preferred,QSizePolicy::Fixed);
        connect(text->document()->documentLayout(),&QAbstractTextDocumentLayout::documentSizeChanged,text,[text](const QSizeF &size){
            const int height=qMax(0,qCeil(size.height())+text->frameWidth()*2);
            if(text->height()!=height)text->setFixedHeight(height);});
    }
    connect(m_sessions,&SessionList::gitActivated,this,[this](const QString &key){
        Q_UNUSED(key);openInspectorCard(InspectorCards::Changes);});
    m_workSplitter->addWidget(m_inspectorPanel);
    m_workSplitter->installEventFilter(this);
    m_workSplitter->handle(1)->installEventFilter(this);
    m_workSplitter->handle(1)->setToolTip(tr("Drag to resize the side panel. Double-click to reset its width."));
    m_workSplitter->setStretchFactor(0, 1); m_workSplitter->setStretchFactor(1, 0);
    m_inspectorToggle = new QPushButton; m_inspectorToggle->setObjectName("toggleInspector");
    m_inspectorToggle->setProperty("glyph", "inspector"); m_inspectorToggle->setCheckable(true);
    m_inspectorToggle->setProperty("sessionHeaderAction", true);
    configureWorkspaceIconButton(m_inspectorToggle, "inspector", tr("Show tasks, plan, agents, details and worktrees"), m_dark);
    m_inspectorCorner = new QWidget; m_inspectorCorner->setObjectName("inspectorCorner");
    auto *cornerLayout = new QHBoxLayout(m_inspectorCorner); cornerLayout->setContentsMargins(4, 4, 6, 4);
    cornerLayout->addWidget(m_inspectorToggle, 0, Qt::AlignVCenter);
    tabs->setCornerWidget(m_inspectorCorner);
    connect(m_inspectorToggle, &QPushButton::toggled, this, &SessionsWindow::setInspectorVisible);
    connect(m_workSplitter, &QSplitter::splitterMoved, this, [this] {
        if (!m_inspectorPanel->isHidden()) { m_inspectorWidth = m_inspectorPanel->width(); Settings::set("workspace/inspectorWidth", m_inspectorWidth); }
    });
    bool widthValid = false;
    const int savedWidth = settings.value("workspace/inspectorWidth", DefaultInspectorWidth).toInt(&widthValid);
    m_inspectorWidth = widthValid && savedWidth > 0 ? qBound(240, savedWidth, QWIDGETSIZE_MAX) : DefaultInspectorWidth;
    m_inspectorPanel->hide();
    if (settings.value("workspace/inspectorVisible", false).toBool())
        QTimer::singleShot(0, this, [this] { setInspectorVisible(true); });
    connect(tabs, &QTabWidget::currentChanged, this, [this](int) {
        updateSessionFeedback();
        if (m_detailTabs->currentWidget() == m_terminal) m_terminal->connectSession();
        if (m_detailTabs->currentWidget() == m_processes) m_processPollAge.invalidate();
        if(m_detailTabs->currentWidget()!=m_terminal && m_detailTabs->currentWidget()!=m_nativeUi)inspect();
    });
    detail->addWidget(m_workSplitter, 1); m_detailStack->addWidget(details); m_splitter->addWidget(m_detailStack);
    m_creationPanel=new QWidget;m_creationPanel->setObjectName("creationDetailPanel");
    auto *creationLayout=new QVBoxLayout(m_creationPanel);
    m_creationTitle=label({},"creationTitle");m_creationTitle->setWordWrap(true);m_creationTitle->setMinimumWidth(0);
    m_creationTitle->setSizePolicy(QSizePolicy::Ignored,QSizePolicy::Preferred);creationLayout->addWidget(m_creationTitle);
    m_creationText=new QPlainTextEdit;m_creationText->setObjectName("creationDetails");m_creationText->setReadOnly(true);
    m_creationText->setAccessibleName(tr("Session creation progress"));creationLayout->addWidget(m_creationText,1);
    auto *creationActions=new QGridLayout;
    m_creationContinue=new QPushButton(tr("Continue setup"));m_creationContinue->setObjectName("creationContinue");
    m_creationCheck=new QPushButton(tr("Check result"));m_creationCheck->setObjectName("creationCheck");
    m_creationStop=new QPushButton(tr("Stop creating"));m_creationStop->setObjectName("creationStop");
    auto *creationHide=new QPushButton(tr("Hide status"));creationHide->setObjectName("creationHide");
    auto *creationReturn=new QPushButton(tr("Return to filters"));creationReturn->setObjectName("creationReturnToFilters");
    m_creationEdit=new QPushButton(tr("Edit choices…"));m_creationEdit->setObjectName("creationEdit");
    m_creationFixAccess=new QPushButton(tr("Fix folder rights"));m_creationFixAccess->setObjectName("creationFixAccess");
    m_creationFixAccess->setToolTip(tr("Give your Windows user full control of this folder, which the DeepSeek sandbox needs"));m_creationFixAccess->hide();
    creationActions->addWidget(m_creationContinue,0,0);creationActions->addWidget(m_creationCheck,0,1);
    creationActions->addWidget(m_creationStop,1,0,1,2);creationActions->addWidget(m_creationEdit,2,0);creationActions->addWidget(creationHide,2,1);
    creationActions->addWidget(creationReturn,3,0,1,2);creationActions->addWidget(m_creationFixAccess,4,0,1,2);
    creationLayout->addLayout(creationActions);m_detailStack->addWidget(m_creationPanel);
    connect(m_creationEdit,&QPushButton::clicked,this,[this]{
        if(!m_creations->canEdit(m_selectedCreation))return;const auto plan=m_creations->operation(m_selectedCreation);
        showNewSession(plan["cmd"].toString(),plan["group"].toString(),plan["host"].toString(),{},{},plan["account"].toString(),plan);
    });
    connect(creationReturn,&QPushButton::clicked,this,[this]{
        m_revealCreation.clear();m_creationSessionToSelect.clear();m_searchResults->setQuery(m_filter=="archived"?QString():m_search->text());
        m_listStack->setCurrentIndex(m_searchResults->active()?1:0);rebuild();
    });
    connect(m_creationContinue,&QPushButton::clicked,this,[this]{m_creations->resume(m_selectedCreation);});
    connect(m_creationFixAccess,&QPushButton::clicked,this,[this]{
        const auto id=m_selectedCreation;const auto folder=sandboxFolderOf(m_creations->operation(id)["error"].toString());
        if(folder.isEmpty())return;
        if(QMessageBox::question(this,tr("Fix folder rights"),
            tr("Give your Windows user full control of\n%1\n\nThe DeepSeek sandbox needs it to start a session there. Only your own access to this folder changes.").arg(folder))!=QMessageBox::Yes)return;
        m_creationFixAccess->setEnabled(false);m_creationFixAccess->setText(tr("Fixing folder rights…"));
        auto *process=new QProcess(this);
        ChildProcess::inheritStdioOnly(*process);
        connect(process,&QProcess::finished,this,[this,process,id](int code,QProcess::ExitStatus status){
            process->deleteLater();
            const auto error=QString::fromUtf8(process->readAllStandardError()).trimmed();
            if(status!=QProcess::NormalExit||code!=0){
                m_creationFixAccess->setEnabled(true);m_creationFixAccess->setText(tr("Fix folder rights"));
                showNotice(tr("The folder rights were not changed: %1").arg(error.isEmpty()?tr("unknown error"):error),true);return;
            }
            m_creationFixAccess->setText(tr("Folder rights fixed"));
            showNotice(tr("Folder rights fixed; starting the session again."),false);
            if(SessionCreationController::canResume(m_creations->operation(id)))m_creations->resume(id);else m_creations->check(id);
        });
        process->start(m_client.executable(),{QStringLiteral("sandbox-folder"),QStringLiteral("grant"),folder,QStringLiteral("--json")});
    });
    connect(m_creationCheck,&QPushButton::clicked,this,[this]{m_creations->check(m_selectedCreation);emit refreshRequested();});
    connect(m_creationStop,&QPushButton::clicked,this,[this]{m_creations->stop(m_selectedCreation);});
    connect(creationHide,&QPushButton::clicked,this,[this]{m_creations->hide(m_selectedCreation);});
    connect(m_creations,&SessionCreationController::clockTick,this,[this]{if(!m_selectedCreation.isEmpty())renderCreation();});
    connect(m_creations,&SessionCreationController::removed,this,[this](const QString &id){m_pendingLaunches.remove(id);});
    m_creations->resolvePlacement=[this](QJsonObject &plan,const QJsonObject &evidence,QString &error){
        auto group=plan["group"].toString();
        if(evidence["cloned"].toBool()&&plan["new_project"].toBool()){
            group=m_organization.createGroup(plan["project_name"].toString(),plan["reserved_project"].toString());
            plan["group"]=group;plan["new_project"]=false;
        }
        if(plan["implicit_inbox"].toBool()&&group=="ungrouped"){
            const auto hint=evidence["project_inference"].toObject();
            if(hint["schema"]==1&&!plan["swarm"].toString().isEmpty()&&hint["swarm_id"]==plan["swarm"]
                &&m_organization.group(hint["project_id"].toString())){
                group=hint["project_id"].toString();plan["group"]=group;plan["add_folder"]=false;
            }
            if(!evidence.contains("project_inference")){
                QSet<QString> matches;const auto path=evidence["path"].toString();
                const auto style=MachinePlatform::windowsShaped(path)?MachinePlatform::Style::Windows:MachinePlatform::Style::Posix;
                for(const auto &project:m_organization.groups())for(const auto &folder:project.folders)
                    if(folder.machine==plan["machine"].toString()&&MachinePlatform::same(folder.path,path,style))matches.insert(project.id);
                if(matches.size()==1){group=*matches.begin();plan["group"]=group;plan["add_folder"]=false;}
            }
        }
        if(!m_organization.group(group)){
            error=tr("The selected project changed. Prepared files are kept. Choose a project before continuing.");return false;
        }
        if(evidence["cloned"].toBool()){
            const auto path=evidence["path"].toString();
            m_organization.addFolder(group,plan["machine"].toString(),path,{},MachinePlatform::windowsShaped(path)?MachinePlatform::Style::Windows:MachinePlatform::Style::Posix);
            saveOrganization();
        }
        plan["group_name"]=m_organization.group(group)->name;
        return true;
    };
    connect(m_creations,&SessionCreationController::changed,this,[this](const QString &id,const QJsonObject &operation){
        if(operation["state"]=="bound")m_pendingLaunches.remove(id);
        else m_pendingLaunches[id]=operation;
        // A launched session waits to be listed; its computer is read until then.
        if((operation["state"]=="confirming"||operation["state"]=="recover")&&!m_confirmPoll->isActive()){m_confirmPollAge.start();m_confirmPoll->start();}
        if(!operation["show_pending"].toBool()){
            if(m_revealCreation==id)m_revealCreation.clear();
            if(m_selectedCreation==id)m_selectedCreation.clear();
        }
        if(m_rebuilding||m_admittingCreation)return;
        rebuild();if(m_selectedCreation==id)renderCreation();
        const auto state=operation["state"].toString();
        if(state=="failed"||state=="unknown"||state=="storage_error"){
            const auto notice=state+'\n'+operation["error"].toString();
            if(m_creationNotices.value(id)!=notice){
                m_creationNotices[id]=notice;if(m_creationNotices.size()>128)m_creationNotices.erase(m_creationNotices.begin());
                showNotice(tr("Creation %1: %2\n%3").arg(operation["tag"].toString(),SessionCreationController::caption(operation),operation["error"].toString()),true);
            }
        }
    });
    connect(m_creations,&SessionCreationController::launchRequested,this,[this](const QString &,const QJsonObject &plan){
        const auto host=plan["host"].toString(),cmd=plan["cmd"].toString(),target=plan["target"].toString();
        const auto name=plan["tag"].toString(),account=plan["account"].toString(),launch=plan["launch_id"].toString();
        // WhiteWh fork: the session's permission mode and a local model account's model.
        const auto permissions=plan["permissions"].toString(),localModel=cmd=="claude"?plan["local_model"].toString():QString();
        if(plan["terminal"].toBool())emit newSessionRequested(host,cmd,target,name,account,launch,permissions,localModel,plan["expected_machine"].toString());
        else m_client.launchDetachedSession(host,cmd,target,name,account,launch,cmd=="claude"?localModel:plan["model"].toString(),plan["effort"].toString(),permissions,plan["expected_machine"].toString());
        emit refreshRequested();
    });
    m_fileDrop=new SessionFileDrop(details,[this]()->MessageComposer *{
        if (m_detailTabs->currentWidget() == m_taskView) return m_taskInspector->composer();
        if(!selected()||m_detailTabs->currentWidget()==m_terminal||m_detailTabs->currentWidget()==m_tasks)return nullptr;
        if(m_detailTabs->currentIndex()==0&&!m_subagentId.isEmpty())return m_subagentComposer->isInputVisible()?m_subagentComposer:nullptr;
        return m_composer;
    },[this](MessageComposer *composer){if(composer==m_taskInspector->composer()){m_detailTabs->setCurrentWidget(m_taskView);return;}if(composer==m_composer)closeSubagent();m_detailTabs->setCurrentIndex(0);},
    [this]{return terminalDropTarget();},[this](const QStringList &paths){dropTerminalFiles(paths);});
    connect(&m_client,&HgsClient::sessionActionFinished,this,[this](quint64 id,bool ok,const QJsonObject &result,const QString &error) {
        if(id!=m_terminalDropRequest)return;
        m_terminalDropRequest=0;
        // An hgs that predates untracked drops asks for a saved binding.
        if(!ok&&!m_terminalDropUntracked.isEmpty()&&error.contains("no saved binding")) {
            showNotice(tr("Update hgs on %1 to drop files into an untracked session.").arg(m_terminalDropUntracked),true);return;
        }
        if(!ok){showNotice(error,true);return;}
        if(terminalDropTarget()!=m_terminalDropKey) {showNotice(tr("Terminal changed during file transfer. Drop the files again in the intended session."),true);return;}
        QStringList paths;
        for(const auto &file:result.value("files").toArray()) {
            const auto path=file.toObject().value("path").toString();
            if(!QDir::isAbsolutePath(path)||path.contains(QRegularExpression("[\\x00-\\x1f\\x7f]"))) {showNotice(tr("Invalid transferred file path"),true);return;}
            paths<<path;
        }
        if(paths.size()!=m_terminalDropCount){showNotice(tr("Incomplete file transfer"),true);return;}
        pasteTerminalPaths(paths);
    });
    m_splitter->setStretchFactor(0, 0); m_splitter->setStretchFactor(1, 1); m_splitter->setSizes({310, 814});
    if (!settings.value("workspace/splitter").toByteArray().isEmpty()) m_splitter->restoreState(settings.value("workspace/splitter").toByteArray());
    layout->addWidget(m_splitter, 1);
    m_sessionDock = new SessionPanelDock(m_splitter, sessionSlot, listPanel, m_sessions, this);
    m_sessionDock->layoutChanged = [this] { applySessionStrip(); };
    m_sessionDock->keepPeek = [this] { return m_search->hasFocus() || !m_search->text().isEmpty(); };
    m_sessionDock->modeChanged = [this] {
        const auto mode = m_sessionDock->mode();
        if (mode != SessionPanelDock::Peek) Settings::set("workspace/sessionsCollapsed", mode == SessionPanelDock::Collapsed);
        if (mode != SessionPanelDock::Docked && m_sessions->bulkSelecting()) m_sessions->clearBulkSelection();
        updateSessionsToggle(); QTimer::singleShot(0, this, &SessionsWindow::updateInspectorMinimum);
    };
    m_sessionDock->setHoverExpands(settings.value("workspace/expandSessionsOnHover", true).toBool());
    m_sessionDock->restore(settings.value("workspace/sessionsCollapsed", false).toBool(), settings.value("workspace/sessionsWidth", 0).toInt());
    connect(m_sessionsToggle, &QPushButton::clicked, this, [this] { m_sessionDock->toggle(); });
    m_pages = new QStackedWidget; m_pages->addWidget(body);
    connect(m_pages, &QStackedWidget::currentChanged, this, [this](int index) {
        if (index != 0) m_terminal->disconnectSession();
    });
    auto *projectClient = new HgsClient(hgsPath, this);
    projectClient->setObjectName("projectClient");
    m_projectsPage = new ProjectsDialog(projectClient, this, true, &m_organization); m_pages->addWidget(m_projectsPage);
    connect(m_projectsPage, &ProjectsDialog::projectsChanged, this, &SessionsWindow::projectsChanged);
    connect(m_projectsPage, &ProjectsDialog::organizationChanged, this, [this] { saveOrganization(); rebuild(); });
    m_swarm=new SwarmController(projectClient,this);
    connect(m_projectsPage,&ProjectsDialog::catalogDraftEditing,m_swarm,&SwarmController::setDraftEditing);
    connect(m_swarm,&SwarmController::organizationReady,this,[this](const QJsonObject &organization){
        // The controller now holds the catalog's form; the next save sends this desktop's again.
        m_savedOrganizationRevision = 0;
        const SessionOrganization updated(organization);
        if(updated.toJson()==m_organization.toJson())return;
        m_organization=updated;saveOrganization();m_projectsPage->refresh();rebuild();
    });
    connect(m_swarm,&SwarmController::statusChanged,m_projectsPage,&ProjectsDialog::setSwarmStatus);
    connect(m_swarm,&SwarmController::snapshotChanged,m_projectsPage,&ProjectsDialog::setSwarmSnapshot);
    connect(m_projectsPage,&ProjectsDialog::connectSwarmRequested,this,[this,projectClient](const QString &computer){
        showSwarmDialog(projectClient,m_swarm,m_fleet,this,[this]{m_pages->setCurrentIndex(2);},true,computer);
    });
    connect(m_projectsPage,&ProjectsDialog::swarmRequested,this,[this,projectClient]{
        showSwarmDialog(projectClient,m_swarm,m_fleet,this,[this]{m_pages->setCurrentIndex(2);});
    });
    QTimer::singleShot(0,this,[this]{m_swarm->start(m_organization.toJson());});
    connect(m_projectsPage,&ProjectsDialog::folderSessionsRequested,this,&SessionsWindow::filterFolder);
    connect(m_projectsPage,&ProjectsDialog::relatedSessionRequested,this,&SessionsWindow::openRelatedSession);
    connect(m_projectsPage,&ProjectsDialog::newPathSessionRequested,this,[this](const QString &project,const QString &host,const QString &path){showNewSession({},project,host,{},path);});
    connect(m_projectsPage,&ProjectsDialog::newSessionRequested,this,[this](const QString &project,const QString &host,const QString &folder){showNewSession({},project,host,folder);});
    // U25 part 7 (spec 7): New team… from a project, and its sessions with
    // their team role, machine, branch and status.
    connect(m_projectsPage,&ProjectsDialog::newTeamRequested,this,[this](const QString &){m_pages->setCurrentIndex(0);showNewTeam();});
    // N23: the Sessions block lists the newest rows; the rest open here.
    connect(m_projectsPage,&ProjectsDialog::projectSessionsRequested,this,[this](const QString &project){
        m_filter = "all"; m_folderFilterPath.clear(); m_hostFilters.clear(); m_projectFilters = {project}; m_search->clear(); showSessionList();
    });
    m_projectsPage->setSessionsProvider([this](const QString &project){
        QList<ProjectsDialog::ProjectSession> rows;
        for(const auto &entry:std::as_const(m_entries)){
            if(m_organization.groupFor(entry.identity)!=project||entry.session.state=="archived")continue;
            const auto &s=entry.session;
            ProjectsDialog::ProjectSession row;
            row.host=entry.host;row.name=s.name;row.agent=s.cmd;row.machine=entry.machine;row.branch=s.gitBranch;
            row.title=s.tag.isEmpty()?s.name:s.tag;row.cwd=s.canonicalCwd.isEmpty()?s.cwd:s.canonicalCwd;row.created=s.created;
            if(m_teams)if(const auto *team=[&]()->const Teams::Team *{for(const auto &t:m_teams->teams())if(t.memberOfSession(entry.host,s.name))return &t;return nullptr;}()){
                row.team=team->id;const auto *member=team->memberOfSession(entry.host,s.name);row.role=team->roleOf(member->id);
                // Its own worktree or clone goes with the team (the lead and a
                // shared member work in the project's folder).
                row.teamFolder=!member->lead&&(member->folder=="worktree"||member->folder=="clone");
            }
            // N21: the session list's status (CLAUDE.md status contract): an
            // idle agent with live jobs is Background, a tool call or a
            // compaction is not Ready. A reply to read shows once the work
            // is done; native work and attention come first.
            const bool working=SessionPresentation::working(s,entry.online);
            const bool reply=entry.online&&!working&&!s.needsAction()&&s.visibleUnreadReply(entry.online);
            row.status=reply?tr("New reply"):SessionPresentation::status(s,entry.online);
            row.tone=!entry.online?QStringLiteral("offline"):s.needsAction()||reply?QStringLiteral("warn"):working?QStringLiteral("active")
                    :s.state=="paused"?QStringLiteral("paused"):QStringLiteral("muted");
            rows<<row;
        }
        return rows;
    });
    // #646: each project's session count for the project list, in one pass.
    m_projectsPage->setSessionCountsProvider([this]{
        QHash<QString,int> counts;
        for(const auto &entry:std::as_const(m_entries))if(entry.session.state!="archived")++counts[m_organization.groupFor(entry.identity)];
        return counts;
    });
    m_machinesPage = new MachinesPage(hgsPath); m_pages->addWidget(m_machinesPage);
    connect(m_machinesPage, &MachinesPage::machinesChanged, this, &SessionsWindow::refreshRequested);
    connect(m_machinesPage, &MachinesPage::sshTerminalRequested, this, &SessionsWindow::machineSshRequested);
    connect(m_machinesPage, &MachinesPage::viewSessionsRequested, this, [this](const QString &host) {
        m_filter = "all"; m_folderFilterPath.clear(); m_projectFilters.clear(); m_accountFilters.clear(); m_hostFilters = {host.isEmpty() ? QStringLiteral("@local") : host}; m_search->clear(); showSessionList();
    });
    m_dashboard = new DashboardPage; m_pages->addWidget(m_dashboard);
    connect(m_dashboard,&DashboardPage::refreshRequested,this,[this]{emit refreshRequested();updateDashboardAccounts(true,true);});
    connect(m_dashboard,&DashboardPage::accountRequested,this,[this](const QString &host,const QString &id){m_pages->setCurrentIndex(4);m_accountsPage->showAccount(host,id);});
    connect(m_dashboard,&DashboardPage::accountsRequested,this,[this]{m_pages->setCurrentIndex(4);});
    connect(m_dashboard,&DashboardPage::sessionRequested,this,&SessionsWindow::showAttentionSession);
    connect(m_dashboard,&DashboardPage::machinesRequested,this,[this](const QString &host) { m_pages->setCurrentIndex(2); m_machinesPage->selectMachine(host); });
    connect(m_dashboard,&DashboardPage::filterRequested,this,[this](const QString &host,const QString &filter) {
        m_filter=filter; m_folderFilterPath.clear(); m_projectFilters.clear(); m_accountFilters.clear(); m_hostFilters = host == "@all" ? QSet<QString>() : QSet<QString>{host.isEmpty()?QStringLiteral("@local"):host}; m_search->clear(); showSessionList();
    });
    m_accountsPage = new AccountsPage(hgsPath,nullptr,m_usageStore); m_pages->addWidget(m_accountsPage);
    m_settingsPage=new SettingsPage(hgsPath);m_pages->addWidget(m_settingsPage);
    // WhiteWh fork: Settings > Teams, the team catalog.
    m_teamCatalog=new TeamCatalogEditor(m_teams);m_teamCatalog->askDesigner=[this]{showTeamDesigner();};m_teamCatalog->setDesignerButton(true);
    m_teamCatalog->confirm=[this](const QString &name,const QString &title,const QString &text,const QString &action){return m_teamPanel->confirm(name,title,text,action);};
    // U25: Prompt order shows the selected session's project rules.
    m_teamCatalog->projectRules=[this]{return teamProjectRules();};
    m_teamSettingsSection=m_settingsPage->addSection(tr("Teams"),m_teamCatalog);
    // U60 (#656): this computer's model servers - the rail's tab while it has
    // one, and their settings; the configuration stays with hgs.
    m_modelServers=new ModelServers::Controller(&m_client,this);
    m_modelServerSettings=new ModelServerSettings(m_modelServers);
    m_modelServerSettings->machines=[this]{return m_fleet.peerNames();};
    m_modelServerSettingsSection=m_settingsPage->addSection(tr("Model servers"),m_modelServerSettings);
    connect(m_modelServers,&ModelServers::Controller::changed,this,[this]{
        const bool any=m_modelServers->available();
        if(m_modelServersNav->isHidden()==any)m_modelServersNav->setVisible(any);
        if(!any&&m_modelServersPage&&m_pages->currentWidget()==m_modelServersPage)m_pages->setCurrentIndex(0);
    });
    m_modelServers->refreshList();
    connect(m_swarm,&SwarmController::snapshotChanged,this,[this](const QJsonObject &snapshot){m_settingsPage->setSwarmSnapshot(snapshot);});
    m_settingsPage->pollingChanged=[this]{
        const bool enabled=ProcessSettings::enabled();
        m_processes->applyPreferences();m_processPollAge.invalidate();
        if(!enabled){m_details.remove("processes");m_subagentDetails.remove("processes");m_processes->setSession({},{},{},{},false,false);}
        if(!enabled && m_detailTabs->currentWidget()==m_processes)m_detailTabs->setCurrentIndex(0);
        m_detailTabs->setTabVisible(m_detailTabs->indexOf(m_processes),enabled);renderDetails();
    };
    m_settingsPage->appearanceChanged=[this]{
        m_sessions->setProperty("compact",Settings::value("workspace/compact",false).toBool());m_sessions->doItemsLayout();
        m_sessionDock->setStripWidth(sessionCardTileInset() ? 0 : SessionStrip::width(m_sessions->property("compact").toBool()));
        m_sessionDock->setHoverExpands(Settings::value("workspace/expandSessionsOnHover",true).toBool());
        const auto usageIndicator=AccountUsage::indicator();m_accountUsage->setIndicator(usageIndicator);m_dashboard->setUsageIndicator(usageIndicator);
        applyTheme();rebuild();
    };
    m_settingsPage->contentScaleChanged=[this]{applyContentScale();};
    m_settingsPage->extensionsChanged=[this]{updateWorktrees();};
    m_settingsPage->activityWidthChanged=[this]{applyActivityWidth();};
    // The rail pin and Settings → Appearance switch the same preference.
    auto *windowLayer=new WindowLayer(this);
    const auto updateWindowLayer=[this,windowPin,windowLayer]{
        const QSignalBlocker block(windowPin);windowPin->setChecked(windowLayer->onTop());
        // Disabling a focused control moves focus to the next rail button.
        // WindowLayer already rejects duplicate requests while KWin is busy.
        windowPin->setVisible(windowLayer->supported());windowPin->setEnabled(windowLayer->supported());
        updateWindowPinAppearance();
        m_settingsPage->setWindowLayerState(windowLayer->onTop(),windowLayer->supported(),windowLayer->hint());
    };
    m_settingsPage->windowLayerChanged=[windowLayer](bool on){windowLayer->request(on);};
    connect(windowPin,&QPushButton::toggled,windowLayer,&WindowLayer::request);
    connect(windowLayer,&WindowLayer::changed,this,updateWindowLayer);
    updateWindowLayer();
    connect(m_accountsPage,&AccountsPage::loginRequested,this,&SessionsWindow::accountLoginRequested);
    connect(m_accountsPage,&AccountsPage::installRequested,this,&SessionsWindow::accountInstallRequested);
    connect(m_accountsPage,&AccountsPage::accountsChanged,this,&SessionsWindow::refreshRequested);
    connect(m_accountsPage,&AccountsPage::notice,this,[this](const QString &message, bool error){showNotice(message,error);});
    connect(m_accountsPage,&AccountsPage::catalogChanged,this,[this]{
        updateDashboardAccounts(m_pages->currentIndex()==3);
        const auto profiles=m_accountsPage->catalogProfiles(), details=m_accountsPage->profiles(true);
        if(profiles!=m_sessionAccountProfiles || details!=m_sessionAccountDetailsProfiles){m_sessionAccountProfiles=profiles;m_sessionAccountDetailsProfiles=details;QTimer::singleShot(0,this,[this]{rebuild();});}
    });
    // U25 part 7 (spec 9): "Used by teams" - the teams whose members run with
    // this profile (its agent on its machine; no account means the default).
    m_accountsPage->setTeamUses([this](const QJsonObject &profile){
        QList<AccountsPage::TeamUse> uses;
        if(!m_teams)return uses;
        const auto provider=profile["provider"].toString(),host=profile["host"].toString();
        const auto id=profile["id"].toString(),label=profile["label"].toString();
        for(const auto &team:m_teams->teams()){
            if(team.status=="done"||team.status=="stopped"||team.status=="archived")continue;
            QStringList roles;
            for(const auto &member:team.members){
                if(member.agent!=provider||member.computer!=host)continue;
                const bool same=member.account.isEmpty()?profile["is_default"].toBool():(member.account==id||member.account==label);
                if(same)roles<<team.roleOf(member.id);
            }
            if(!roles.isEmpty())uses<<AccountsPage::TeamUse{team.id,team.name,roles.join(QStringLiteral(", "))};
        }
        return uses;
    });
    connect(m_accountsPage,&AccountsPage::openTeamRequested,this,[this](const QString &team){m_pages->setCurrentIndex(0);showTeam(team);});
    connect(m_usageStore,&AccountUsageStore::changed,this,[this](const QString &){renderAccountUsage();updateDashboardAccounts();});
    connect(m_machinesPage,&MachinesPage::accountsRequested,this,[this](const QString &host) { m_pages->setCurrentIndex(4); m_accountsPage->showMachine(host); });
    connect(m_machinesPage,&MachinesPage::appearanceChanged,this,[this] { m_machineFilter->refreshColors(); m_accountsPage->refreshAppearance(); rebuild(); updateDashboard(); });
    connect(m_pages,&QStackedWidget::currentChanged,this,[this](int index) {
        m_sessionsNav->setChecked(index==0); m_projectsNav->setChecked(index==1); m_machinesNav->setChecked(index==2); m_brand->setChecked(index==3); m_accountsNav->setChecked(index==4); m_settingsNav->setChecked(index==5);
        if (m_teamDesignerNav) m_teamDesignerNav->setChecked(index==m_teamDesignerPageIndex);
        if (m_teamWaitingNav) m_teamWaitingNav->setChecked(index==m_teamWaitingPageIndex);
        if (m_modelServersNav) m_modelServersNav->setChecked(index==m_modelServersPageIndex);
        // WhiteWh fork: the catalog's Computer and Account dropdowns, fresh on each visit.
        if (index==5 && m_teamCatalog) m_teamCatalog->setSources(&m_client, teamComputers());
        if (index==m_teamDesignerPageIndex && m_teamDesignerPage) m_teamDesignerPage->editor()->setSources(&m_client, teamComputers());
        renderAccountUsage();if(index==3)updateDashboardAccounts(true);updateInspectorMinimum();
        if (index == 3 && m_dashboardStale) updateDashboard();
        // Callers that show Sessions rebuild right away; this covers any other switch.
        if (index == 0 && m_sessionsStale) QTimer::singleShot(0, this, [this] { if (m_sessionsStale && m_pages->currentIndex() == 0) rebuild(); });
    });
    auto *workspace = new QVBoxLayout; workspace->setContentsMargins(0, 0, 0, 0); workspace->setSpacing(0);
    workspace->addWidget(m_pages, 1);
    m_terminationCleanup = new WorktreeCleanupStatus(&m_client, this);
    workspace->addWidget(m_terminationCleanup);
    m_terminationCleanup->foldersRemoved = [this](const QList<QJsonObject> &entries) {
        for(const auto &entry:entries)
            if(m_archivedWorktree.size()==4 && entry["host"]==m_archivedWorktree[0] && entry["row"].toObject()["path"]==m_archivedWorktree[2])
                {m_archivedWorktree.clear();m_worktreeCleanupOffered=false;m_worktreeCleanup->hide();}
        emit refreshRequested();updateWorktrees();
    };
    auto *statusBar = new QFrame; statusBar->setObjectName("workspaceStatus"); statusBar->setFixedHeight(28);
    auto *statusLayout = m_statusLayout = new QHBoxLayout(statusBar); statusLayout->setContentsMargins(12, 3, 12, 3);
    m_connectionStatus = new StatusMessage; m_connectionStatus->setObjectName("workspaceConnectionStatus");
    statusLayout->addWidget(m_connectionStatus, 1);
    auto *updateEntry = new QPushButton(tr("New version"));
    updateEntry->setObjectName("updatesAvailable");
    updateEntry->setProperty("glyph", "zerus");
    updateEntry->setIconSize(QSize(14, 14));
    updateEntry->setAttribute(Qt::WA_LayoutUsesWidgetRect);
    statusLayout->addWidget(updateEntry);
    auto *updates = UpdateController::instance();
    auto updateEntryState = [updates, updateEntry] {
        updateEntry->setVisible(updates->updateAvailable());
        const auto description = tr("Zerus %1 is available. Open Updates.").arg(updates->feed().version);
        updateEntry->setToolTip(description);
        updateEntry->setAccessibleName(description);
    };
    connect(updates, &UpdateController::changed, this, updateEntryState);
    connect(updateEntry, &QPushButton::clicked, this, &SessionsWindow::showUpdates);
    updateEntryState();
    auto *restartEntry = new QPushButton(tr("Restart to finish update"));
    restartEntry->setObjectName("restartToUpdate");
    restartEntry->setProperty("glyph", "zerus");
    restartEntry->setIconSize(QSize(14, 14));
    restartEntry->setAttribute(Qt::WA_LayoutUsesWidgetRect);
    restartEntry->setToolTip(tr("Zerus was updated on disk. Drafts are kept; sessions keep running."));
    restartEntry->setAccessibleName(restartEntry->toolTip());
    statusLayout->addWidget(restartEntry);
    auto restartEntryState = [updates, restartEntry] { restartEntry->setVisible(updates->restartRequired()); };
    connect(updates, &UpdateController::changed, this, restartEntryState);
    connect(restartEntry, &QPushButton::clicked, updates, &UpdateController::restartIntoInstalledBundle);
    restartEntryState();
    // A login item without ProcessType Interactive starts the GUI as a daemon, which any
    // CPU load starves. Nothing changes until the person chooses to update it.
    auto *loginItemEntry = new QPushButton(tr("Fix login item"));
    loginItemEntry->setObjectName("fixLoginItem");
    loginItemEntry->setProperty("glyph", "zerus");
    loginItemEntry->setIconSize(QSize(14, 14));
    loginItemEntry->setAttribute(Qt::WA_LayoutUsesWidgetRect);
    loginItemEntry->setToolTip(tr("macOS starts Zerus at background priority, so it can stop responding while the computer is busy."));
    loginItemEntry->setAccessibleName(loginItemEntry->toolTip());
    loginItemEntry->setVisible(updates->loginItemDeprioritized());
    statusLayout->addWidget(loginItemEntry);
    connect(loginItemEntry, &QPushButton::clicked, this, [this, updates, loginItemEntry] {
        QMessageBox dialog(this);
        dialog.setIcon(QMessageBox::Warning);
        dialog.setWindowTitle(tr("Fix the Zerus login item"));
        dialog.setText(tr("macOS runs Zerus at background priority."));
        dialog.setInformativeText(tr("Zerus was started by its login item, which macOS runs at background priority, so "
                                     "Zerus can stop responding while other programs keep the computer busy. Updating "
                                     "the login item makes macOS start Zerus like other apps.\n\n"
                                     "Update Only applies this at your next login; Zerus keeps running.\n"
                                     "Update and Restart applies it now: Zerus quits and opens again. Saved drafts, "
                                     "sessions and agents stay; a voice or video message that is still recording "
                                     "stops and is not kept."));
        auto *only = dialog.addButton(tr("Update Only"), QMessageBox::AcceptRole);
        auto *restart = dialog.addButton(tr("Update and Restart"), QMessageBox::ActionRole);
        auto *cancel = dialog.addButton(QMessageBox::Cancel);
        dialog.setDefaultButton(only);
        dialog.setEscapeButton(cancel);
        dialog.exec();
        if (dialog.clickedButton() != restart && dialog.clickedButton() != only)
            return;
        const bool restartNow = dialog.clickedButton() == restart;
        loginItemEntry->setEnabled(false);
        updates->updateLoginItem([self = QPointer<SessionsWindow>(this), entry = QPointer<QPushButton>(loginItemEntry),
                                  updates, restartNow](const QString &error) {
            if (entry)
                entry->setEnabled(true);
            if (!error.isEmpty()) {
                if (self)
                    QMessageBox::warning(self, tr("Fix the Zerus login item"),
                                         tr("The login item was not changed.\n\n%1").arg(error));
                return;
            }
            if (entry)
                entry->hide();
            if (restartNow)
                updates->restartIntoInstalledBundle();
            else if (self)
                self->showNotice(tr("The Zerus login item is updated; it applies at your next login."));
        });
    });
    m_connectionRetry = new QPushButton(tr("Retry now")); m_connectionRetry->setObjectName("connectionRetry");
    m_connectionRetry->setFlat(true); m_connectionRetry->hide(); statusLayout->addWidget(m_connectionRetry);
    connect(m_connectionRetry, &QPushButton::clicked, this, &SessionsWindow::refreshRequested);
    // WhiteWh fork: the full log behind the one-line notices of this bar.
    auto *openLog = new QPushButton(tr("Log")); openLog->setObjectName("openLog");
    openLog->setFlat(true); openLog->setToolTip(tr("Open the Zerus log")); statusLayout->addWidget(openLog);
    connect(openLog, &QPushButton::clicked, this, [this] {
        if (!ForkLog::open()) showNotice(tr("Could not open the log %1").arg(QDir::toNativeSeparators(ForkLog::path())), true);
    });
    m_notice = new StatusMessage; m_notice->hide(); statusLayout->addWidget(m_notice, 1);
    m_sessionFeedback = new StatusMessage; m_sessionFeedback->setObjectName("sessionFeedback");
    m_sessionFeedback->hide(); statusLayout->addWidget(m_sessionFeedback, 1);
    connect(m_pages, &QStackedWidget::currentChanged, this, [this] { updateSessionFeedback(); });
    connect(m_pages, &QStackedWidget::currentChanged, this, [this](int index) {
        if (m_railSessionsToggle) m_railSessionsToggle->setVisible(sessionCardTileInset() && index == 0);
    });
    m_noticeDismiss = new QPushButton; m_noticeDismiss->setObjectName("dismissNotice");
    m_noticeDismiss->setProperty("glyph", "close"); m_noticeDismiss->setIconSize(QSize(14, 14));
    m_noticeDismiss->setFixedSize(20, 20); m_noticeDismiss->setFlat(true);
    m_noticeDismiss->setAttribute(Qt::WA_LayoutUsesWidgetRect);
    m_noticeDismiss->setToolTip(tr("Dismiss notification")); m_noticeDismiss->setAccessibleName(m_noticeDismiss->toolTip());
    m_noticeDismiss->hide(); statusLayout->addWidget(m_noticeDismiss);
    connect(m_noticeDismiss, &QPushButton::clicked, this, &SessionsWindow::clearNotice);
    m_worktreeCleanup = new QPushButton(tr("Clean up worktree…")); m_worktreeCleanup->setObjectName("archivedWorktreeCleanup");
    m_worktreeCleanup->setFlat(true); m_worktreeCleanup->setAttribute(Qt::WA_LayoutUsesWidgetRect);
    m_worktreeCleanup->setSizePolicy(QSizePolicy::Preferred, QSizePolicy::Fixed);
    m_worktreeCleanup->hide(); statusLayout->addWidget(m_worktreeCleanup);
    connect(m_worktreeCleanup, &QPushButton::clicked, this, &SessionsWindow::cleanUpArchivedWorktree);
    statusLayout->addWidget(m_usageStrip,0,Qt::AlignRight);
    workspace->addWidget(statusBar); root->addLayout(workspace, 1);

    // Search, refresh and the other commands are keyboard actions (setupShortcuts()).
    auto *escape = new QShortcut(QKeySequence(Qt::Key_Escape), this);
    // Escape never closes Zerus. A search being typed is cleared first;
    // otherwise, like Escape in Terminal, it stops the selected session's
    // working turn. Terminal and the message field handle their own Escape.
    connect(escape, &QShortcut::activated, this, [this] {
        if (m_sessionDock->dismissPeek()) m_search->clear();
        else if (m_search->hasFocus() && !m_search->text().isEmpty()) m_search->clear();
        else if (m_pages->currentIndex() == 0 && m_composer->requestInterrupt()) return;
        else if (!m_search->text().isEmpty()) m_search->clear();
    });
    connect(m_activityView, &ActivityView::queueSendNowRequested, this, [this](const QString &id) { if (!id.isEmpty()) sendQueuedNow(id); });
    connect(&m_client, &HgsClient::queueSendFinished, this, [this](quint64 request, bool ok, const QJsonObject &, const QString &error) {
        if (request != m_queueSendRequest) return;
        const bool current = m_queueSendKey == m_selectedKey; m_queueSendRequest = 0; m_queueSendKey.clear();
        if (current) {
            m_details.remove("input_queue");
            const QString detail = error.trimmed();
            const bool staleQueue = !ok && (detail == "hgs: The native queue is no longer visible; nothing was sent"
                || detail == "The native queue is no longer visible; nothing was sent");
            const QString message = ok ? tr("Send now requested. Waiting for the agent to accept the queue.")
                : staleQueue ? tr("The native queue is no longer visible; nothing was sent. Activity is being refreshed; check Terminal or the current queue before retrying.") : error;
            showNotice(message, !ok, staleQueue); renderDetails(); inspect();
        }
        emit refreshRequested();
    });
    connect(&m_client, &HgsClient::inspectionReady, this, &SessionsWindow::acceptInspection);
    if (auto *streams = EventStreams::instance()) {
        connect(streams, &EventStreams::activityReady, this, [this](const QString &host, const QString &name, const QJsonObject &inspection) {
            acceptInspection(host, name, inspection, {});
        });
        // The stream stopped following this run (it ended or changed): poll it instead.
        connect(streams, &EventStreams::activityEnded, this, [this](const QString &host, const QString &name) {
            if (const auto *entry = selected(); entry && entry->host == host && entry->session.name == name) {
                m_streamEnded.insert(host + '\n' + name + '\n' + entry->session.runId); inspect();
            }
        });
        // Followed checkout folders replace their git-status polls (updateGitWatch).
        connect(streams, &EventStreams::gitStatusReady, &m_client, &HgsClient::acceptGitStatus);
    }
    connect(&m_client, &HgsClient::subagentInspectionReady, this,
        [this](const QString &host,const QString &name,const QString &id,const QString &archive,const QJsonObject &data) {
            const auto *entry = selected();
            if (!entry || entry->host != host || entry->session.name != name || entry->session.archiveId != archive
                || m_subagentId != id || m_subagentConversation != data.value("parent_conversation_id").toString()
                || m_details.value("run_id") != data.value("run_id")) return;
            m_subagentDetails = data;
            m_subagentCwd = data.value("cwd").toString();
            m_subagentHint->setText(data.value("history_scope").toString()
                + (data.value("history_truncated").toBool() ? tr(". Showing recent activity.") : QString())
                + (data.value("send_supported").toBool() ? tr(" Messages go directly to this agent.")
                    : tr(" Read only. This agent is managed by the parent conversation.")));
            m_subagentComposer->setInputVisible(data.value("send_supported").toBool());
            m_subagentComposer->setAvailability(!isTerminating(*entry) && entry->online && data.value("send_supported").toBool());
            m_subagentView->setActivity(data,data.value("events").toArray()); renderDetails();
        });
    connect(&m_client, &HgsClient::subagentInspectionFailed, this,
        [this](const QString &host,const QString &name,const QString &id,const QString &archive,const QString &error) {
            const auto *entry = selected();
            if (entry && entry->host == host && entry->session.name == name && entry->session.archiveId == archive && m_subagentId == id)
                m_subagentHint->setText(error);
        });
    connect(&m_client, &HgsClient::inspectionFailed, this, [this](const QString &host, const QString &name, const QString &error, const QString &archiveId) {
        // N7: a Team-tab Stop that waited for these details cannot decide the
        // real action; say so instead of doing nothing.
        if (m_terminateAfterInspect == host + '\n' + name) {
            m_terminateAfterInspect.clear();
            showNotice(tr("Could not load %1 to stop it: %2").arg(name, error), true);
        }
        if (!m_pending && !m_renameKey.isEmpty()) return;
        const auto *entry = selected(); if (!entry || entry->host != host || entry->session.name != name || entry->session.archiveId != archiveId) return;
        // WhiteWh fork: a paused DeepSeek session has no running host until
        // Resume starts one; that is its normal state, not a failed refresh. The
        // last inspection's actions belonged to the running agent, so Resume
        // falls back to the listing's.
        if (entry->session.cmd == "dsh" && entry->session.state != "running" && error.contains("DeepSeek host is unavailable")) {
            m_details.remove("allowed_actions"); m_inspectError.clear(); renderDetails(); return;
        }
        m_inspectError = tr("Could not refresh activity: %1").arg(error); renderDetails();
    });
    connect(&m_client, &HgsClient::writeDone, this, [this](const QString &operation, bool ok, const QString &detail) {
        if (finishSelectionAction(operation, ok, detail)) {if(!m_selectionRunning)m_archiveBrowser->refresh();return;}
        if(ok&&(operation.startsWith("archive ")||operation.startsWith("restore ")||operation.startsWith("forget archive ")||operation.startsWith("rename ")))m_archiveBrowser->refresh();
        if (m_archivedWorktree.size() == 4 && (operation == QStringLiteral("archive %1").arg(m_archivedWorktree[1]) || operation == QStringLiteral("terminate %1").arg(m_archivedWorktree[1]))) {
            const auto offer=m_archivedWorktree;const auto cleanupOffer=offer+QStringList{m_archivedWorktreeOwnerId};
            const bool requested=m_removeTerminatedWorktree && operation==QStringLiteral("terminate %1").arg(offer[1]);
            m_removeTerminatedWorktree=false;
            if (ok) {
                if(requested && !offer[2].isEmpty() && !offer[3].isEmpty()) {
                    m_worktreeCleanupOffered=false;m_worktreeCleanup->hide();
                    m_terminationCleanupResults.insert(offer[0]+'\n'+offer[1]+'\n'+offer[2],{});
                    QTimer::singleShot(0,this,[this,cleanupOffer]{reviewTerminatedWorktrees({cleanupOffer});});
                } else if(!m_worktreeCleanupOffered)offerWorktreeCleanup();
            } else m_archivedWorktree.clear();
        }
        // U3: hgs refused a stale resume; the owner chooses how to continue.
        // L5: a queued start-and-send must survive that refusal - the choice's
        // own resume continues it once the session is ready.
        const bool behind = !ok && operation.startsWith(QStringLiteral("resume ")) && detail.contains(QStringLiteral("transcript_behind"));
        if (behind) resumeBehind(operation.mid(7), detail);
        if (!behind && !ok && (operation == QStringLiteral("resume %1").arg(m_startSend.name) || operation == QStringLiteral("fresh %1").arg(m_startSend.name)))
            cancelStartContinuation(detail);
        if (!m_terminationCommand.isEmpty() && (operation == QStringLiteral("kill %1").arg(m_terminating.value(m_terminationCommand).session.name) || operation == QStringLiteral("terminate %1").arg(m_terminating.value(m_terminationCommand).session.name))) {
            if(ok)m_terminationFinished.insert(m_terminationCommand);
            else m_terminating.remove(m_terminationCommand);
            m_terminationCommand.clear();
        }
        m_pending = false;
        const bool renamed = !m_renameKey.isEmpty();
        if (!m_renameKey.isEmpty()) {
            if (ok) {
                if (const auto *old = selected(); old && !m_renameArchived) {
                    m_organization.renameSession(old->identity, old->machine + '\n' + m_renameKey.section('\n', 1, 1));
                    saveOrganization();
                }
                m_composer->renameDraft(m_selectedKey, m_renameKey);
                if (m_localMessages.contains(m_selectedKey)) m_localMessages.insert(m_renameKey, m_localMessages.take(m_selectedKey));
                if (!m_renameArchived) m_terminal->updateSessionName(m_renameKey.section('\n', 1, 1));
                m_filter = m_renameArchived ? "archived" : "all";
                m_search->clear(); m_pages->setCurrentIndex(0); m_projectsNav->setChecked(false);
                m_details = {}; m_events = {}; m_cursor = 0; m_inspectError.clear();
                m_childrenHtml.clear();
            } else m_renameKey.clear();
        }
        if (ok && !m_forkKey.isEmpty()) {
            m_filter = "all"; m_folderFilterPath.clear(); m_hostFilters.clear(); m_projectFilters.clear(); m_accountFilters.clear(); m_search->clear(); m_pages->setCurrentIndex(0); m_projectsNav->setChecked(false);
        }
        if (!ok && !m_forkKey.isEmpty()) { m_forkKey.clear(); m_forkGroup.clear(); m_forkBefore.clear(); }
        if (auto restore = m_restores.find(m_restoreWriteKey); restore != m_restores.end()
            && operation == QStringLiteral("restore %1").arg(restore->source.session.name)) {
            restore->succeeded = ok;
            // Only the list filter changes. Keep machine/project/search
            // scope and a conversation the user selected in the meantime.
            if (ok && m_restoreKey == m_restoreWriteKey && m_selectedKey == restore->source.key && m_filter == "archived") {
                m_filter = "all";
                m_searchResults->setQuery(m_search->text());
                m_listStack->setCurrentIndex(m_searchResults->active() ? 1 : 0);
            }
            if (!ok) { if (m_restoreKey == m_restoreWriteKey) m_restoreKey.clear(); m_restores.erase(restore); }
            m_restoreWriteKey.clear();
        }
        showNotice(ok ? (renamed ? tr("Session renamed. Refreshing…") : tr("Session updated. Refreshing…")) : detail, !ok);
        emit refreshRequested(); rebuild(); inspect();
    });
    connect(&m_client, &HgsClient::worktreeCleanupFinished, this, [this](const QString &host, const QString &operation, const QJsonObject &result) {
        const auto pending=host+'\n'+operation.mid(QStringLiteral("terminate ").size())+'\n'+result["path"].toString();
        if(operation.startsWith("terminate ") && m_terminationCleanupResults.contains(pending)) {
            m_terminationCleanupResults[pending]=result;return;
        }
        if (m_archivedWorktree.size() != 4 || m_archivedWorktree[0] != host || (!m_archivedWorktree[2].isEmpty() && m_archivedWorktree[2] != result["path"].toString())
            || (!m_archivedWorktree[3].isEmpty() && m_archivedWorktree[3] != result["common_dir"].toString())
            || (operation != "archive " + m_archivedWorktree[1] && operation != "terminate " + m_archivedWorktree[1])) return;
        m_archivedWorktree[2] = result["path"].toString(); m_archivedWorktree[3] = result["common_dir"].toString();
        if (result["status"] == "removed") {
            m_removeTerminatedWorktree=false;
            if(m_selectionCleanupTarget.size()>=4 && m_selectionCleanupTarget[0]==host && m_selectionCleanupTarget[2]==result["path"].toString())m_selectionCleanupTarget.clear();
            m_worktreeCleanupOffered = false; m_worktreeCleanup->hide(); m_archivedWorktree.clear();
            showNotice(tr("Worktree removed automatically. The branch and archived conversation are kept.")); updateWorktrees();
        } else {
            m_worktreeCleanupReason = result["reason"].toString();
            m_worktreeCleanupUnconfirmed = result["status"] == "unconfirmed";
            m_worktreeCleanupOffered = true; updateWorktreeCleanupOffer();
            showNotice(tr("Worktree kept for review: %1").arg(result["reason"].toString()));
        }
    });
    connect(&m_client, &HgsClient::gitStatusChanged, this, [this](const QString &host, const QString &path) {
        // Update only the Git role. Asynchronous repository reads must not run
        // session selection/inspection or move composer focus. Rows prepared
        // from the previous status are prepared again rather than applied.
        if (m_rowsPending) requestRows();
        for (int i = 0; i < m_sessions->count(); ++i) {
            auto *item = m_sessions->item(i);
            if (item->data(SessionRoles::GitPath).toString() != path || item->data(SessionRoles::GitHost).toString() != host) continue;
            for (const auto &entry : std::as_const(m_entries)) if (entry.key == item->data(KeyRole).toString()) {
                const auto data = SessionPresentation::gitStatus(entry.session, m_client.gitStatusSnapshot(host, path), entry.online);
                item->setData(SessionRoles::GitStatus, data);
                item->setData(Qt::AccessibleDescriptionRole, GitStatusBadge::tooltip(data));
                item->setData(SessionRoles::Digest, QByteArray());
                break;
            }
        }
        updateDashboard();
    });
    m_timer.setInterval(500); connect(&m_timer, &QTimer::timeout, this, [this]() {
        const bool background=m_detailTabs->currentWidget()==m_terminal || m_detailTabs->currentWidget()==m_nativeUi;
        const int interval=background?ProcessSettings::backgroundMs():m_detailTabs->currentWidget()==m_processes?ProcessSettings::activeMs():m_activityPollMs;
        if(!m_inspectionAge.isValid() || m_inspectionAge.elapsed()>=interval)inspect();
        if (m_teams) m_teams->setActive(isVisible());   // WhiteWh fork: agent teams poll while shown
        if (m_waitingFeed) m_waitingFeed->setActive(isVisible());   // U55: so does what waits for the owner
        if (++m_tick % 10 == 0) {
            emit refreshRequested();
            updateGitWatch();
            for (const auto &entry : std::as_const(m_entries)) {
                const auto path = SessionPresentation::gitStatusPath(entry.session);
                if (entry.online && !path.isEmpty()) m_client.requestGitStatus(entry.host, path);
            }
            // Expire verification visually even if fleet polling has stalled.
            m_sessions->viewport()->update(); updateDashboard();
        }
    });
    m_readTimer.setInterval(400); connect(&m_readTimer, &QTimer::timeout, this, &SessionsWindow::checkViewedReply);
    applyTheme(); applyContentScale(); rebuild();
    setupShortcuts();
}

// Only session content follows this preference. Workspace chrome, including
// both sidebars, the session header and its tabs, keeps the native size.
void SessionsWindow::applyContentScale()
{
    const double scale = ContentScale::factor();
    for (auto *view : {m_activityView, m_subagentView}) view->setContentScale(scale);
    m_taskInspector->setContentScale(scale);
    for (auto *composer : {m_composer, m_subagentComposer}) composer->setContentScale(scale);
    m_question->setContentScale(scale); m_terminal->setContentScale(scale);
    applyActivityWidth();
}

void SessionsWindow::applyActivityWidth()
{
    const int column = ActivityWidth::column(ContentScale::factor());
    for (auto *view : {m_activityView, m_subagentView}) view->setColumnWidth(column);
    for (auto *layout : m_activityColumns) layout->setColumnWidth(column);
    m_taskInspector->setColumnWidth(column);
}

void SessionsWindow::updateWindowPinAppearance()
{
    if (!m_windowPin) return;
    const bool pinned = m_windowPin->isChecked();
    const QString glyph = pinned ? QStringLiteral("pinned") : QStringLiteral("unpinned");
    const QColor color = pinned ? QColor(m_dark ? "#ffda76" : "#a66000")
                                : (m_muted.isEmpty() ? palette().color(QPalette::WindowText) : QColor(m_muted));
    // Native state polling should not rebuild the icon when nothing changed.
    if (m_windowPin->property("glyph").toString() != glyph || m_windowPin->property("iconColor").value<QColor>() != color) {
        m_windowPin->setProperty("glyph", glyph); m_windowPin->setProperty("iconColor", color);
        m_windowPin->setIcon(workspaceIcon(glyph, color));
    }
    const QString action = pinned ? tr("Unpin Zerus") : tr("Keep Zerus above other windows");
    m_windowPin->setToolTip(action); m_windowPin->setAccessibleName(action);
    m_windowPin->setAccessibleDescription(pinned ? tr("Zerus stays above other windows.") : tr("Other windows can cover Zerus."));
}

void SessionsWindow::applyTheme()
{
    const QString theme = Settings::value("workspace/theme", "system").toString();
    m_dark = theme == "dark" || (theme != "light" && qApp->palette().color(QPalette::Window).lightness() < 128);
    m_sessionLoading->setTheme(m_dark);
    // WhiteWh fork: the selected skin supplies the workspace colors and may
    // append its own stylesheet (WorkspaceSkin.h); Zerus keeps these exactly.
    const auto skin = WorkspaceSkin::current();
    const auto &colors = skin.palette(m_dark);
    m_fg = colors.foreground;
    m_muted = colors.muted;
    m_surface = colors.surface;
    m_border = colors.border;
    m_accent = colors.accent;
    const QString bg = colors.background;
    const QString hover = colors.hover;
    const QString selected = colors.selected;
    // The whole window follows the theme, not only what the stylesheet names:
    // a widget it does not cover paints the palette, and a light Windows
    // palette under the dark theme left pages and views light (Settings >
    // Worktrees and Keyboard, user report 2026-10-28). Placeholder text is
    // painted from the palette too.
    QPalette themedPalette = palette();
    const QColor window(bg), text(colors.foreground), base(colors.surface), muted(colors.muted);
    for (const auto group : {QPalette::Active, QPalette::Inactive, QPalette::Disabled}) {
        const QColor foreground = group == QPalette::Disabled ? muted : text;
        themedPalette.setColor(group, QPalette::Window, window);
        themedPalette.setColor(group, QPalette::AlternateBase, window);
        themedPalette.setColor(group, QPalette::Base, base);
        themedPalette.setColor(group, QPalette::Button, base);
        themedPalette.setColor(group, QPalette::ToolTipBase, base);
        themedPalette.setColor(group, QPalette::WindowText, foreground);
        themedPalette.setColor(group, QPalette::Text, foreground);
        themedPalette.setColor(group, QPalette::ButtonText, foreground);
        themedPalette.setColor(group, QPalette::ToolTipText, text);
        themedPalette.setColor(group, QPalette::PlaceholderText, muted);
    }
    setPalette(themedPalette);
    // WhiteWh fork: selected text needs a colour apart from the canvas. A skin whose
    // selected rows are a faint tint of it (Compact) selects text in its accent instead.
    QString textSelection = selected;
    if (qAbs(QColor(selected).lightness() - QColor(bg).lightness()) < 24) {
        const QColor accent(m_accent), canvas(bg);
        const auto mix = [](int a, int b) { return qRound(a * 0.38 + b * 0.62); };
        textSelection = QColor(mix(accent.red(), canvas.red()), mix(accent.green(), canvas.green()), mix(accent.blue(), canvas.blue())).name();
    }
    // A saved change to a user skin applies at once; editors may replace the file.
    if (!m_skinWatcher) {
        m_skinWatcher = new QFileSystemWatcher(this);
        connect(m_skinWatcher, &QFileSystemWatcher::fileChanged, this, [this] { QTimer::singleShot(150, this, &SessionsWindow::applyTheme); });
    }
    if (!m_skinWatcher->files().isEmpty()) m_skinWatcher->removePaths(m_skinWatcher->files());
    if (!skin.path.isEmpty()) m_skinWatcher->addPath(skin.path);
    // #737: a dialog is a window and takes the system's palette, not this
    // window's: every dialog opened over it draws the theme's surface and text
    // (the New team dialog drew a light surface under the dark theme's labels).
    setStyleSheet(QString(R"(
        QDialog { background:%1; color:%2; }
        QWidget#settingsPage, QWidget#recoverySettingsPage, QWidget#sessionsWindow, QWidget#machinesPage, QWidget#accountsPage, QDialog#accountDialog, QDialog#accountPermissionsDialog, QDialog#machineSetupDialog, QDialog#projectsPage, QDialog#projectColorDialog, QDialog#projectFolderDialog, QDialog#deleteProjectDialog, QDialog#newSessionDialog, QDialog#folderBrowser, QDialog#forkSessionDialog, QDialog#renameSessionDialog, QDialog#workspaceSettingsDialog, QDialog#sessionFileDialog, QDialog#swarmDialog, QDialog#speechProfileEditor { background:%1; color:%2; }
        QColorDialog#workspaceColorDialog { background:%1; color:%2; }
        QDialog#worktreeBatchCleanupDialog, QDialog#cleanupFileInspection { background:%1; color:%2; }
        QFrame#terminationCleanupStatus { background:%3; border:0; border-top:1px solid %4; }
        QPlainTextEdit#worktreeCleanupDecision, QDialog#cleanupFileInspection QPlainTextEdit, QDialog#cleanupFileInspection QTreeWidget, QDialog#cleanupFileInspection QListWidget { background:%3; color:%2; border:1px solid %4; border-radius:7px; selection-background-color:%10; selection-color:%2; }
        QDialog#phonePairingDialog { background:%1; color:%2; }
        QDialog#phonePairingDialog QScrollArea { background:transparent; border:0; }
        QPlainTextEdit#phonePairingCodeText { background:%3; color:%2; border:1px solid %4; border-radius:7px; padding:4px; selection-background-color:%10; selection-color:%2; }
        QDialog#savedDraftsDialog { background:%1; color:%2; }
        QDialog#taskDialog, QDialog#taskCancelDialog, QDialog#taskSettingsDialog { background:%1; color:%2; }
        QPlainTextEdit#taskPrompt, QPlainTextEdit#taskTemplate, QPlainTextEdit#cancelTemplate { background:%11; color:%2; border:1px solid %4; border-radius:9px; padding:6px; selection-background-color:%10; selection-color:%2; }
        QPlainTextEdit#taskPrompt:focus, QPlainTextEdit#taskTemplate:focus, QPlainTextEdit#cancelTemplate:focus { border-color:%5; }
        QScrollArea#taskSessionChoices, QScrollArea#taskSessionChoices > QWidget, QScrollArea#taskSessionChoices > QWidget > QWidget { background:transparent; }
        QPushButton[taskSessionChoice="true"] { padding:4px 10px; min-height:18px; font-size:12px; }
        QPushButton[taskSessionChoice="true"]:checked { background:%10; color:%5; border-color:%5; font-weight:600; }
        QLabel[taskSection="true"] { color:%6; }
        QLabel#taskCancelText { background:%3; border-radius:8px; padding:9px 11px; }
        QPushButton#confirmTaskCancel { color:%12; border-color:%12; font-weight:600; }
        QWidget#automaticApprovalSettings { background:transparent; color:%2; }
        QPlainTextEdit#approvalJournalDetail { background:%3; color:%2; border:1px solid %4; border-radius:8px; padding:10px; selection-background-color:%10; selection-color:%2; }
        QTableWidget#approvalJournal::item { padding:7px 10px; border-bottom:1px solid %4; }
        QListWidget#savedDraftList { background:%1; color:%2; border:1px solid %4; border-radius:8px; padding:4px; outline:0; }
        QPlainTextEdit#savedDraftPreview { background:%3; color:%2; border:1px solid %4; border-radius:8px; padding:10px; selection-background-color:%10; selection-color:%2; }
        QWidget { font-size:13px; }
        QLabel { color:%2; background:transparent; }
        QWidget#sidebar { background:%3; border-right:1px solid %4; }
        QLabel#brand { font-size:32px; font-weight:700; letter-spacing:-2px; color:%5; }
        QPushButton#appVersion { font-size:9px; color:%6; background:transparent; border:1px solid transparent; border-radius:5px; padding:0; min-height:22px; }
        QPushButton#appVersion:hover { color:%2; background:%7; }
        QPushButton#appVersion:focus[keyboardFocus="true"] { border-color:%5; }
        QLabel#eyebrow { font-size:10px; font-weight:600; letter-spacing:1px; color:%6; }
        QLabel#heading { font-size:16px; font-weight:600; }
        QLabel#detailTitle { font-size:16px; font-weight:600; }
        QLabel#detailsIdentityTitle { font-size:14px; font-weight:600; }
        QLabel#emptyTitle { font-size:21px; font-weight:600; margin-bottom:8px; }
        QLabel#muted, QLabel#footnote, QLabel#sessionMeta, QLabel#listSummary { color:%6; }
        QLabel#sessionMeta, QLabel#listSummary { font-size:11px; }
        QLabel#footnote { font-size:11px; margin-top:12px; }
        QPushButton { background:%3; color:%2; border:1px solid %4; border-radius:7px; padding:8px 13px; min-height:18px; }
        QPushButton:hover { background:%7; }
        QToolButton#manageLaunchProject { color:%6; background:transparent; border:1px solid transparent; border-radius:5px; padding:6px; }
        QToolButton#manageLaunchProject:hover { color:%2; background:%7; }
        QToolButton#manageLaunchProject:focus { border-color:%5; }
        QToolButton#manageLaunchProject:disabled { color:%6; }
        QPushButton:focus[keyboardFocus="true"] { border:1px solid %5; }
        QPushButton:disabled { color:%6; background:%1; border-color:%4; }
        QPushButton#primary, QPushButton[archiveRestore="true"], QPushButton[primaryAction="true"] { background:%5; color:%8; border-color:%5; font-weight:600; }
        QPushButton#primary:hover, QPushButton[archiveRestore="true"]:hover, QPushButton[primaryAction="true"]:hover { background:%9; }
        QPushButton#primary:disabled, QPushButton[archiveRestore="true"]:disabled, QPushButton[primaryAction="true"]:disabled { color:%6; background:%1; border-color:%4; }
        QPushButton#nav { text-align:left; border:1px solid transparent; padding:10px 9px; background:transparent; }
        QPushButton#nav:hover { background:%7; }
        QPushButton#nav:checked { background:%10; color:%5; font-weight:600; }
        QPushButton#nav:focus[keyboardFocus="true"] { border-color:%5; }
        QPushButton#brandMark { min-height:42px; max-height:42px; padding:0; border:1px solid transparent; background:transparent; }
        QPushButton#brandMark:hover, QPushButton#brandMark:checked { background:%10; }
        QPushButton#railButton, QPushButton#workspaceSettings, QPushButton#windowPin { min-height:42px; max-height:42px; background:transparent; border:1px solid transparent; padding:0; }
        QPushButton#railButton:hover, QPushButton#workspaceSettings:hover, QPushButton#windowPin:hover { background:%7; }
        QPushButton#workspaceSettings:checked { background:%7; border-color:%4; }
        QPushButton#windowPin:focus[keyboardFocus="true"] { border-color:%5; }
        QWidget#windowTitleBar QPushButton#railButton, QWidget#windowTitleBar QPushButton#workspaceSettings, QWidget#windowTitleBar QPushButton#windowPin,
        QWidget#windowTitleBar QPushButton#brandMark, QWidget#windowTitleBar QPushButton#newSession, QWidget#windowTitleBar QPushButton#railSessionsToggle { min-height:28px; max-height:28px; border-radius:7px; }
        QFrame#settingsCard { background:%3;border:1px solid %4;border-radius:9px; }
        QListWidget#settingsSections { background:transparent;color:%2;border:0;outline:0; }
        QListWidget#settingsSections::item { padding:11px 10px;margin-bottom:5px;border-radius:6px; }
        QListWidget#settingsSections::item:selected { background:%10;color:%5; }
        QListWidget#settingsSections::item:hover { background:%7; }
        QWidget#settingsPage QScrollArea, QWidget#settingsContent, QWidget#relaySettings, QWidget#speechSettings, QWidget#downloadCacheSettings, QDialog#speechProfileEditor QScrollArea, QWidget#speechEditorContent, QDialog#swarmDialog QScrollArea, QWidget#swarmConflictContent { background:transparent;border:0; }
        QPushButton#railButton:checked { background:%10; border-color:%4; }
        QPushButton#sessionFilter { padding:4px 2px; min-height:22px; font-size:11px; background:transparent; border-color:transparent; }
        QPushButton#sessionFilter:checked { background:%10; border-color:%4; color:%5; }
        QPushButton#sessionFilter:hover { background:%7; }
        QPushButton#newSession, QPushButton#sessionPanelNewSession, QPushButton#batchActions, QPushButton#sessionPanelToggle, QPushButton#savedDrafts, QPushButton#markAllReadButton { padding:0; background:transparent; border-color:transparent; }
        QPushButton#newSession:hover, QPushButton#sessionPanelNewSession:hover, QPushButton#batchActions:hover, QPushButton#sessionPanelToggle:hover, QPushButton#sessionStripSearch:hover, QPushButton#savedDrafts:hover, QPushButton#markAllReadButton:hover { background:%7; }
        QPushButton#sessionStripSearch { padding:0; background:%3; border:1px solid %4; }
        )" R"(
        QWidget#sessionListPanel { background:%1; }
        QPushButton#newSession { background:%10; }
        QPushButton#machineFilter::menu-indicator, QPushButton#projectFilter::menu-indicator, QPushButton#accountFilter::menu-indicator, QPushButton#batchActions::menu-indicator { image:none; width:0; }
        QPushButton#hostFilterChip { padding:3px 8px; font-size:11px; text-align:left; color:%5; background:%10; }
        QPushButton[sessionHeaderAction="true"], QPushButton[settingReset="true"] { background:transparent; border:1px solid transparent; border-radius:6px; padding:0; }
        QPushButton[sessionHeaderAction="true"]:hover, QPushButton[settingReset="true"]:hover { background:%7; }
        QPushButton[sessionHeaderAction="true"]:checked, QPushButton[sessionHeaderAction="true"]:pressed, QPushButton[settingReset="true"]:pressed { background:%10; }
        QPushButton#toggleInspector:checked { background:transparent; border-color:transparent; }
        QPushButton#toggleInspector:hover { background:%7; }
        QPushButton[sessionHeaderAction="true"]:disabled { background:transparent; border-color:transparent; }
        QPushButton[sessionHeaderAction="true"]:focus[keyboardFocus="true"], QPushButton[settingReset="true"]:focus[keyboardFocus="true"] { border-color:%5; }
        QPushButton[sessionHeaderAction="true"]::menu-indicator { image:none; width:0; }
        QMenu { background:%3; color:%2; border:1px solid %4; padding:4px; }
        QMenu::item { padding:8px 32px 8px 12px; min-width:110px; border-radius:4px; }
        QMenu::item:selected { background:%10; color:%2; }
        QMenu::item:disabled { color:%6; }
        QMenu::separator { height:1px; background:%4; margin:4px 8px; }
        QMenu::right-arrow { image:url(:/hgs/chevron-right.svg); width:14px; height:14px; right:8px; }
        QLineEdit#search { background:%3; color:%2; border:1px solid %4; border-radius:7px; padding:7px 10px; selection-background-color:%10; }
        QLineEdit#search:focus { border-color:%5; }
        QLineEdit, QComboBox { background:%3; color:%2; border:1px solid %4; border-radius:7px; padding:9px 10px; selection-background-color:%10; }
        QLineEdit:focus, QComboBox:focus { border-color:%5; }
        QLineEdit:disabled, QComboBox:disabled { color:%6; background:%1; }
        QComboBox::drop-down { border:0; width:24px; }
        QComboBox::down-arrow { image:url(:/hgs/chevron-down.svg); width:14px; height:14px; }
        QComboBox QAbstractItemView { background:%3; color:%2; selection-background-color:%10; selection-color:%2; }
        QTableWidget, QTreeWidget#worktreeCatalog, QTreeWidget#batchCleanupList, QTreeWidget#processList, QListWidget#folderList { background:%3; color:%2; border:1px solid %4; border-radius:8px; gridline-color:%4; selection-background-color:%10; selection-color:%2; }
        QHeaderView::section { background:%3; color:%6; border:0; border-bottom:1px solid %4; padding:10px; }
        QListWidget#folderList::item { padding:9px 12px; }
        QListWidget#logicalProjects, QListWidget#swarmConflicts { background:%3; color:%2; border:1px solid %4; border-radius:8px; padding:5px; outline:0; }
        QListWidget#logicalProjects::item, QListWidget#swarmConflicts::item { padding:12px 10px; border-radius:6px; margin:2px 0; }
        QListWidget#logicalProjects::item:selected, QListWidget#swarmConflicts::item:selected { background:%7; color:%2; }
        QListWidget#logicalProjects::item:hover, QListWidget#swarmConflicts::item:hover { background:%10; }
        QSplitter#swarmConflictSplit::handle { width:14px; }
        QTableWidget#swarmConflictChoices::item { padding:8px 10px; }
        QTreeWidget#worktreeCatalog::item, QTreeWidget#batchCleanupList::item, QTreeWidget#processList::item { padding:6px 4px; }
        QTableWidget#projectFolders::item { padding:0 10px; }
        QSpinBox, QDoubleSpinBox#downloadCacheSize { background:%3; color:%2; border:1px solid %4; border-radius:7px; padding:8px 10px; }
        QDoubleSpinBox#downloadCacheSize:disabled { color:%6; background:%1; }
        QListWidget#machineProfiles, QListWidget#accountProfiles, QListWidget#speechProfiles, QListWidget#launchAccount { background:%3; color:%2; border:1px solid %4; border-radius:8px; }
        QListWidget#machineProfiles::item, QListWidget#speechProfiles::item { padding:14px 10px; border-bottom:1px solid %4; }
        QListWidget#accountProfiles::item { padding:6px 10px; border-bottom:1px solid %4; }
        QListWidget#machineProfiles::item:selected, QListWidget#accountProfiles::item:selected, QListWidget#speechProfiles::item:selected, QListWidget#launchAccount::item:selected { background:%10; color:%2; }
        QListWidget#launchAccount::item:hover { background:%7; }
        QPlainTextEdit#machineSetupLog, QPlainTextEdit#processOutput { background:%3; color:%2; border:1px solid %4; border-radius:7px; }
        QPushButton#machineFilter, QPushButton#projectFilter, QPushButton#accountFilter { padding:0; border-color:transparent; background:transparent; }
        QCheckBox { color:%2; spacing:8px; }
        QSlider#workspaceContentScale, QSlider#workspaceActivityWidth { min-height:22px; }
        QSlider#workspaceContentScale::groove:horizontal, QSlider#workspaceActivityWidth::groove:horizontal { height:4px; background:%4; border-radius:2px; }
        QSlider#workspaceContentScale::sub-page:horizontal, QSlider#workspaceActivityWidth::sub-page:horizontal { background:%5; border-radius:2px; }
        QSlider#workspaceContentScale::handle:horizontal, QSlider#workspaceActivityWidth::handle:horizontal { background:%5; width:16px; margin:-6px 0; border-radius:8px; }
        QSlider#workspaceContentScale::handle:horizontal:hover, QSlider#workspaceActivityWidth::handle:horizontal:hover { background:%9; }
        QSlider#workspaceActivityWidth::sub-page:horizontal:disabled, QSlider#workspaceActivityWidth::handle:horizontal:disabled { background:%6; }
        QListWidget#sessionList { background:transparent; border:0; outline:0; padding-right:2px; }
        QListWidget#hosts { background:transparent; border:0; outline:0; color:%6; }
        QListWidget#hosts::item { padding:9px 8px; border-radius:6px; }
        QListWidget#hosts::item:selected { background:%7; color:%2; }
        QListWidget#hosts::item:hover { background:%7; }
        QStackedWidget#detail { background:%3; border:1px solid %4; border-radius:12px; }
        QTabWidget::pane { border:0; border-top:1px solid %4; }
        QTabBar::tab { background:transparent; color:%6; padding:8px 9px; border-bottom:2px solid transparent; }
        /* Qt sizes corner hosts from the tab size hint, ignoring a bar's
           minimumHeight. These headers must fit the 34px control and its inset. */
        QTabWidget#sessionDetailTabs QTabBar::tab, QTabWidget#sessionInspector QTabBar::tab { min-height:26px; }
        QTabBar::tab:selected { color:%2; border-bottom:2px solid %5; }
        QTabBar::tab:focus { color:%5; }
        QTextBrowser { background:transparent; color:%2; border:0; padding-top:12px; selection-background-color:%10; }
        QLabel#sessionModel { color:%6; font-size:11px; }
        QLabel#hint { color:%6; font-size:12px; }
        QFrame#workspaceStatus { background:%1; border:0; border-top:1px solid %4; }
        QLabel#notice { color:%6; font-size:11px; }
        QLabel#workspaceConnectionStatus { color:%12; font-size:11px; }
        QPushButton#connectionRetry, QPushButton#openLog { color:%2; background:transparent; border:0; padding:0 6px; min-height:18px; font-size:11px; }
        QPushButton#dismissNotice { background:transparent; border:0; padding:0; min-height:0; }
        QPushButton#dismissNotice:hover { background:%7; border-radius:4px; }
        QPushButton#archivedWorktreeCleanup { color:%5; background:transparent; border:1px solid transparent; border-radius:4px; padding:0 6px; min-height:18px; max-height:18px; font-size:11px; }
        QPushButton#archivedWorktreeCleanup:hover { background:%7; border-color:%4; }
        QPushButton#updatesAvailable, QPushButton#restartToUpdate { color:%5; background:%10; border:1px solid %5; border-radius:5px; padding:0 7px; min-height:18px; max-height:18px; font-size:11px; }
        QPushButton#updatesAvailable:hover, QPushButton#restartToUpdate:hover { background:%7; }
        QLabel#notice[error="true"] { color:%12; }
        QSplitter::handle { background:transparent; width:8px; }
        QFrame#sessionInspectorPanel { background:transparent; border:0; }
    )").arg(bg, m_fg, m_surface, m_border, m_accent, m_muted, hover,
              colors.accentText, colors.accentHover, selected,
              colors.inset, tone("Error", m_dark).name()) + workspaceSpinButtons(hover) + workspaceScrollbars(m_dark) + skin.stylesheet);
    if (textSelection != selected)
        setStyleSheet(styleSheet().replace(QStringLiteral("selection-background-color:") + selected + ';',
            QStringLiteral("selection-background-color:") + textSelection + QStringLiteral("; selection-color:") + m_fg + ';'));
    for (auto *button : findChildren<QPushButton *>()) if (button != m_windowPin && !button->property("glyph").toString().isEmpty())
        button->setIcon(workspaceIcon(button->property("glyph").toString(), QColor(button->objectName() == "updatesAvailable" || button->objectName() == "restartToUpdate" ? m_accent : m_muted)));
    updateWindowPinAppearance(); updateWorktreeCleanupOffer();
    if (m_sessionDock) m_sessionDock->setEdgeColor(QColor(m_border));
    if (m_titleBar) m_titleBar->setColors(QColor(bg), QColor(m_border), QColor(colors.inset), QColor(m_border), QColor(m_fg), QColor(m_muted), QColor(hover));
    if (m_brand) m_brand->setAppearance(m_dark ? QColor(Qt::white) : QColor(m_accent), QColor(m_dark ? "#ffda76" : "#a66000"),
        m_attentionCount > 0, Settings::value("workspace/reduceMotion", false).toBool());
    m_sessions->setProperty("hgsDark", m_dark);
    m_sessions->setActivityAnimationEnabled(Settings::value("workspace/animateActivity", true).toBool());
    AgentWorking::setAnimated(Settings::value("workspace/animateActivity", true).toBool());   // U25: the working dots
    m_projectsPage->setTheme(m_dark); m_accountsPage->setTheme(m_dark); m_machinesPage->setTheme(m_dark); m_providerBadge->setTheme(m_dark); m_machineBadge->setTheme(m_dark);
    m_accountUsage->setTheme(m_dark);
    m_usageWarning->setStyleSheet(QString("QLabel{color:%1;}").arg(m_dark?"#ffabab":"#a42330"));
    m_usageRefresh->setTheme(m_dark);
    m_contextUsage->setTheme(m_dark);m_subagentContextUsage->setTheme(m_dark);
    // U25 part 6: the inspector cards and their row icons follow the skin.
    if(m_inspector){
        m_inspector->setDark(m_dark);
        const auto muted=WorkspaceSkin::color(WorkspaceSkin::active(),m_dark,QStringLiteral("muted"));
        for(auto *icon:m_inspector->findChildren<QLabel *>("inspectorEnvIcon"))icon->setPixmap(workspaceIcon(icon->property("glyph").toString(),muted).pixmap(14,14));
        updateContextCard();updateChangesCard();
    }
    m_cacheStatus->setTheme(m_dark);
    m_cacheWarning->setStyleSheet(QString("QLabel{color:%1;font-size:12px;}").arg(m_dark?"#efbd78":"#91621a"));
    m_cacheDetail->setStyleSheet(QString("QLabel{color:%1;font-size:11px;}").arg(m_muted));
    m_cacheClear->setIcon(workspaceIcon("refresh",QColor(m_muted)));m_cacheClear->setIconSize(QSize(14,14));
    m_fileDrop->setTheme(m_dark);
    m_recovery->setStyleSheet(QStringLiteral("QFrame#recoveryPanel {background:transparent;border:0;} QLabel {border:0;background:transparent;}"));
    m_archiveBrowser->setTheme(m_dark);
    m_machineFilter->setTheme(m_dark); m_projectFilter->setTheme(m_dark); m_accountFilter->setTheme(m_dark); m_dashboard->setTheme(m_dark); m_searchResults->setTheme(m_dark); m_activityView->setTheme(m_dark); m_composer->setTheme(m_dark); m_question->setTheme(m_dark); m_proposalCards->setDark(m_dark); m_terminal->setTheme(m_dark);
    m_subagentView->setTheme(m_dark); m_subagentComposer->setTheme(m_dark);
    if (m_teamPanel) m_teamPanel->setTheme(m_dark);   // WhiteWh fork: agent teams
    if (m_teamCatalog) m_teamCatalog->setDark(m_dark);
    if (m_teamWorkspace) m_teamWorkspace->setTheme(m_dark);
    if (m_teamDesignerPage) m_teamDesignerPage->setDark(m_dark);
    if (m_teamWaitingPage) m_teamWaitingPage->setDark(m_dark);
    if (m_modelServersPage) m_modelServersPage->setDark(m_dark);
    if (m_modelServerSettings) m_modelServerSettings->setDark(m_dark);
    if (m_settingsPage) m_settingsPage->setDark(m_dark);   // #650 N1: its Phones panel
    for (auto *banner : {m_waitingBanner, m_teamWaitingBanner}) if (banner) banner->setDark(m_dark);
    if (m_teamNewForm) m_teamNewForm->setDark(m_dark);
    m_processes->setTheme(m_dark);
    m_tasks->setTheme(m_dark); m_taskInspector->setTheme(m_dark);
    m_childrenHtml.clear(); m_sessions->viewport()->update(); renderDetails();
    applyWorkspaceView();
}

// The exact text sent from Activity when it is this turn's prompt, otherwise
// the recorded prompt. Clipped prompts and automatic continuations are skipped.
QString SessionsWindow::interruptedPrompt(const QString &key) const
{
    const QString prompt = m_details.value("prompt").toString();
    if (prompt.isEmpty() || prompt.startsWith(QStringLiteral("[HGS automatic recovery]"))) return {};
    const bool clipped = prompt.size() > 4000 && prompt.endsWith(QChar(0x2026));
    const QString known = clipped ? prompt.chopped(1) : prompt;
    const auto messages = m_localMessages.value(key);
    for (int i = messages.size(); i-- > 0;) {
        const auto message = messages[i].toObject(); const QString text = message.value("text").toString();
        if (message.value("status") != "error" && message.value("run_id") == m_details.value("run_id")
            && (clipped ? text.startsWith(known) : text.trimmed() == known.trimmed())) return text;
    }
    return clipped ? QString() : prompt;
}

void SessionsWindow::restoreInterruptedPrompt()
{
    const auto pending = m_interruptPrompts.constFind(m_selectedKey);
    // Wait for this session's inspection; selection briefly renders without it.
    if (pending == m_interruptPrompts.cend() || m_details.value("run_id").toString().isEmpty()) return;
    const QString phase = m_details.value("phase").toString();
    if (m_details.value("run_id") != pending->run || m_details.value("conversation_id") != pending->conversation
        || !QStringList{"working", "tool", "compacting", "interrupted"}.contains(phase)) {
        m_interruptPrompts.remove(m_selectedKey); return; // The turn ended another way.
    }
    if (phase != "interrupted") return;
    const QString text = pending->text; m_interruptPrompts.remove(m_selectedKey);
    if (m_composer->offerDraft(m_selectedKey, text)) m_composer->editor()->setFocus(Qt::OtherFocusReason);
}

void SessionsWindow::updateSessionsToggle()
{
    const auto mode = m_sessionDock->mode();
    const QString glyph = mode == SessionPanelDock::Docked ? "collapse-sessions" : mode == SessionPanelDock::Collapsed ? "expand-sessions" : "pin";
    const QString caption = mode == SessionPanelDock::Docked ? tr("Collapse session list")
        : mode == SessionPanelDock::Collapsed ? tr("Expand session list") : tr("Keep session list open");
    m_sessionsToggle->setProperty("glyph", glyph); m_sessionsToggle->setToolTip(caption); m_sessionsToggle->setAccessibleName(caption);
    m_sessionsToggle->setIcon(workspaceIcon(glyph, QColor(m_muted)));
    if (m_railSessionsToggle) {
        m_railSessionsToggle->setProperty("glyph", glyph); m_railSessionsToggle->setToolTip(caption); m_railSessionsToggle->setAccessibleName(caption);
        m_railSessionsToggle->setIcon(workspaceIcon(glyph, QColor(m_muted)));
    }
    applyWorkspaceView();
}

// WhiteWh fork: the skin's "cards" view of the workspace. Each session card has
// its own surface, the rail holds the list toggle and offers New session only
// while the list is collapsed (the list header has its own), and SocratiCode
// leads the Details page.
void SessionsWindow::applyWorkspaceView()
{
    const auto skin = WorkspaceSkin::current();
    const bool cards = skin.view(QStringLiteral("sessions")) == QLatin1String("cards");
    const QStringList names{"cardSurface", "cardBorder", "cardHover", "cardSelected", "cardSelectedBorder"};
    const QStringList keys{"card", "cardBorder", "cardHover", "cardSelected", "cardSelectedBorder"};
    for (int at = 0; at < names.size(); ++at) {
        const auto value = skin.named(m_dark, keys[at]);
        m_sessions->setProperty(names[at].toLatin1().constData(), cards && !value.isEmpty() ? QVariant(QColor(value)) : QVariant());
    }
    sessionCardTileInset() = cards ? 38 : 0;
    SessionStatusBadge::outlined() = cards;
    m_sessions->viewport()->update();
    const bool docked = !m_sessionDock || m_sessionDock->mode() == SessionPanelDock::Docked;
    // The list toggle belongs to Sessions; other pages have no session list.
    if (m_railSessionsToggle) m_railSessionsToggle->setVisible(cards && (!m_pages || m_pages->currentIndex() == 0));
    // Cards: a collapsed list leaves only the rail, which expands it again.
    if (m_sessionDock) m_sessionDock->setStripWidth(cards ? 0 : SessionStrip::width(m_sessions->property("compact").toBool()));
    const bool compactComposer = skin.view(QStringLiteral("composer")) == QLatin1String("compact");
    // The compact composer shows the model; the header does not repeat it.
    if (m_model) m_model->setVisible(!compactComposer && !m_model->text().isEmpty() && selected());
    if (m_titleBar) applyWindowFrame();
    if (m_railNewSession) m_railNewSession->setVisible(!cards || !docked);
    if (m_sessionsToggle) m_sessionsToggle->setVisible(!cards);
    const bool compact = skin.view(QStringLiteral("composer")) == QLatin1String("compact");
    if (m_composer) m_composer->setCompact(compact);
    if (m_activityView) m_activityView->setBubbles(skin.view(QStringLiteral("activity")) == QLatin1String("bubbles"));
    if (m_subagentComposer) m_subagentComposer->setCompact(compact);
    if (m_socratiCode) if (auto *layout = qobject_cast<QBoxLayout *>(m_socratiCode->parentWidget() ? m_socratiCode->parentWidget()->layout() : nullptr)) {
        // Cards: SocratiCode leads the page; classic: it closes it.
        const int at = layout->indexOf(m_socratiCode), wanted = cards ? 0 : layout->count() - 1;
        if (at >= 0 && at != wanted) { layout->removeWidget(m_socratiCode); layout->insertWidget(wanted, m_socratiCode); }
    }
}

// The strip keeps the list and swaps the panel chrome for compact controls:
// one toggle, a search button, the active filter and the session count.
void SessionsWindow::applySessionStrip()
{
    if (!m_sessionDock) return;
    const bool strip = m_sessionDock->chromeNarrow(), filters = m_sessionDock->filtersNarrow();
    // Rebuilds follow every poll; the layout only changes with the strip state.
    if (m_stripLayout != int(strip) * 2 + int(filters)) {
        if (strip && !(m_stripLayout & 2)) {
            // Measured before hiding, while these rows still have their docked height.
            int reserve = 0, rows = 0;
            for (QWidget *row : {static_cast<QWidget *>(m_machineFilter), static_cast<QWidget *>(m_projectFilter), static_cast<QWidget *>(m_accountFilter), static_cast<QWidget *>(m_folderFilterClear)})
                if (row->isVisible()) { reserve += row->height(); ++rows; }
            m_stripReserve = rows ? reserve + (rows - 1) * m_stripSpacer->parentWidget()->layout()->spacing() : 0;
        }
        m_stripLayout = int(strip) * 2 + int(filters);
        m_sessionHeader->setAlignment(m_sessionsToggle, strip ? Qt::AlignHCenter : Qt::Alignment());
        m_filterRow->setContentsMargins(filters ? SessionStrip::tileColumn() : QMargins());
    }
    m_heading->setVisible(!strip); m_panelNewSession->setVisible(!strip); m_batchButton->setVisible(!strip); m_savedDrafts->setVisible(!strip); m_markAllReadButton->setVisible(!strip);
    // The search button stands in for the field at the same height and as wide
    // as a tile, border included. A widget style survives the window style's
    // repolish, which resets button minimums.
    const int side = SessionStrip::cardSide(m_sessions->property("compact").toBool());
    setWorkspaceStyle(m_stripSearch, QString("QPushButton { min-height:%1px; max-height:%1px; min-width:%2px; max-width:%2px; }")
        .arg(qMax(0, m_search->sizeHint().height() - 2)).arg(side - 2));
    // Rows hide before others show: showing a widget lays the panel out at once,
    // and a row still to go would hold the list a row lower until the next layout.
    const bool folder = !strip && !m_folderFilterPath.isEmpty(), spacer = strip && m_stripReserve > 0;
    (strip ? static_cast<QWidget *>(m_search) : m_stripSearch)->hide();
    if (strip) { m_machineFilter->setConcealed(true); m_projectFilter->setConcealed(true); m_accountFilter->setConcealed(true); }
    if (!folder) m_folderFilterClear->hide();
    if (!spacer) m_stripSpacer->hide();
    m_stripSpacer->setFixedHeight(m_stripReserve);
    (strip ? static_cast<QWidget *>(m_stripSearch) : m_search)->show();
    if (spacer) m_stripSpacer->show();
    if (!strip) { m_machineFilter->setConcealed(false); m_projectFilter->setConcealed(false); m_accountFilter->setConcealed(false); }
    if (folder) m_folderFilterClear->show();
    // Narrow filters keep the active one and the most urgent beside it: sessions
    // that need attention, otherwise active ones, otherwise all of them.
    QString urgent;
    for (const char *id : {"attention", "active", "all"}) {
        const auto found = std::find_if(m_filters.cbegin(), m_filters.cend(), [id](auto *button) { return button->property("filter").toString() == id; });
        if (found == m_filters.cend() || (*found)->isChecked() || (QString(id) != "all" && (*found)->property("count").toInt() == 0)) continue;
        urgent = id; break;
    }
    QList<QPushButton *> shown;
    for (auto *button : m_filters) {
        const bool visible = !filters || button->isChecked() || button->property("filter").toString() == urgent;
        button->setVisible(visible); if (visible) shown.append(button);
    }
    // Together they span a tile and meet in the middle.
    for (auto *button : m_filters) {
        const int index = shown.indexOf(button);
        const Qt::Alignment align = !filters ? Qt::Alignment() : shown.size() < 2 ? Qt::AlignHCenter : index == 0 ? Qt::AlignRight : Qt::AlignLeft;
        const int half = (side - m_filterRow->spacing()) / 2;
        if (filters) button->setFixedWidth(shown.size() < 2 ? side : index == 0 ? half : side - m_filterRow->spacing() - half);
        else { button->setMinimumWidth(0); button->setMaximumWidth(QWIDGETSIZE_MAX); }
        if (m_filterRow->itemAt(m_filterRow->indexOf(button))->alignment() != align) m_filterRow->setAlignment(button, align);
    }
    m_machineFilter->button()->setVisible(!filters); m_projectFilter->button()->setVisible(!filters); m_accountFilter->button()->setVisible(!filters);
    m_listStack->setCurrentIndex(!strip&&m_searchResults->active()&&m_revealCreation.isEmpty()&&m_creationSessionToSelect.isEmpty()?1:0);
    m_count->setText(strip ? m_countShort : m_countFull);
    m_count->setAlignment(strip ? Qt::AlignCenter : Qt::AlignLeft | Qt::AlignVCenter);
    m_count->setVisible(strip || !m_searchResults->active());
    if (!strip && m_focusSearch) { m_focusSearch = false; m_search->setFocus(Qt::OtherFocusReason); }
}

void SessionsWindow::updateInspectorMinimum()
{
    if(!m_inspectorPanel || !m_inspectorToggle || !m_splitter)return;
    const int header=240;
    if(m_inspectorPanel->minimumWidth()!=header)m_inspectorPanel->setMinimumWidth(header);
    int minimum=840;
    if(!m_inspectorPanel->isHidden() && m_pages && m_pages->currentIndex()==0) {
        const auto body=m_splitter->parentWidget()->layout()->contentsMargins();
        const auto detail=m_workSplitter->parentWidget()->layout()->contentsMargins();
        const auto frame=m_detailStack->contentsMargins();
        const auto *sidebar=findChild<QWidget *>(QStringLiteral("sidebar"),Qt::FindDirectChildrenOnly);
        const int rail=sidebar?sidebar->width():0;
        // An explicit window minimum overrides Qt's automatic layout minimum.
        // Account for the full header through both nested splitters and margins.
        minimum=qMax(minimum,rail+body.left()+body.right()+detail.left()+detail.right()+frame.left()+frame.right()
            +m_splitter->handleWidth()+m_workSplitter->handleWidth()+m_splitter->widget(0)->minimumWidth()
            +m_detailTabs->minimumWidth()+header);
    }
    setMinimumWidth(minimum);
    applyInspectorWidth();
}

void SessionsWindow::setInspectorVisible(bool visible)
{
    // One control follows the right edge, rather than offering two hide actions.
    const bool restoreFocus = m_inspectorToggle->hasFocus();
    const bool keyboardFocus = m_inspectorToggle->property("keyboardFocus").toBool();
    // The cards have no tab bar: the corner (the toggle) stays in the session's tab corner.
    if (m_detailTabs->cornerWidget() != m_inspectorCorner) m_detailTabs->setCornerWidget(m_inspectorCorner);
    m_inspectorPanel->setVisible(visible);
    const QSignalBlocker blocker(m_inspectorToggle); m_inspectorToggle->setChecked(visible);
    m_inspectorToggle->setProperty("glyph", visible ? "collapse-panel" : "inspector");
    m_inspectorToggle->setIcon(workspaceIcon(m_inspectorToggle->property("glyph").toString(), QColor(m_muted)));
    m_inspectorToggle->setAccessibleName(visible ? tr("Collapse side panel") : tr("Show tasks, plan, agents, details and worktrees"));
    m_inspectorToggle->setToolTip(ShortcutRegistry::instance().tooltip(m_inspectorToggle->accessibleName(), "panel.toggle"));
    m_inspectorCorner->show(); m_inspectorToggle->show();
    if (restoreFocus) m_inspectorToggle->setFocus(keyboardFocus ? Qt::TabFocusReason : Qt::MouseFocusReason);
    if (visible) {
        m_inspectorPanel->setMinimumWidth(240);
        applyInspectorWidth();
    }
    updateInspectorMinimum();
    Settings::set("workspace/inspectorVisible", visible);
    Settings::set("workspace/inspectorWidth", m_inspectorWidth);
    updateWorktrees();
}

void SessionsWindow::applyInspectorWidth()
{
    if (!m_inspectorPanel || m_inspectorPanel->isHidden()) return;
    const int available = m_workSplitter->width() - m_workSplitter->handleWidth();
    const int minimum = m_inspectorPanel->minimumWidth();
    const int side = qBound(minimum, m_inspectorWidth, qMax(minimum, available - m_detailTabs->minimumWidth()));
    if (m_workSplitter->sizes().value(1) != side)
        m_workSplitter->setSizes({qMax(m_detailTabs->minimumWidth(), available - side), side});
}

void SessionsWindow::saveSessionRowPreferences()
{
    Settings::set("workspace/collapsedSessionRows", m_sessionRowPreferences.save());
    m_expandAllRowsAction->setEnabled(!m_sessionRowPreferences.empty());
}

void SessionsWindow::setSessionRowCollapsed(const Entry &entry, bool collapsed)
{
    if (!m_sessionRowPreferences.setCollapsed(entry.host, entry.session, collapsed)) {
        if (collapsed && !m_sessionRowPreferences.collapsed(entry.host, entry.session))
            showNotice(tr("The compact row limit has been reached. Expand another session row first."), true);
        return;
    }
    saveSessionRowPreferences(); updateSessionRowPresentation();
}

void SessionsWindow::updateSessionRowPresentation()
{
    // Rows prepared before this density change are prepared again.
    if (m_rowsPending) requestRows();
    QHash<QString, bool> collapsed;
    for (const auto &entry : std::as_const(m_entries)) collapsed.insert(entry.key, m_sessionRowPreferences.collapsed(entry.host, entry.session));
    for (int i = 0; i < m_sessions->count(); ++i) {
        auto *item = m_sessions->item(i);
        if (item->data(SessionRoles::Header).toBool() || !item->data(SessionRoles::ChildId).toString().isEmpty()
            || !item->data(SessionRoles::LaunchId).toString().isEmpty()) continue;
        const bool value = collapsed.value(item->data(KeyRole).toString());
        item->setData(SessionRoles::RowCollapsed, value);
        item->setData(Qt::AccessibleDescriptionRole, value ? tr("Collapsed session row. Press Right to expand.")
            : GitStatusBadge::tooltip(item->data(SessionRoles::GitStatus).toJsonObject()));
        item->setData(SessionRoles::Digest, QByteArray());
    }
    // A density edit changes no selection, native state, inspection or read mark.
    m_sessions->doItemsLayout(); m_sessions->syncRowExpandButtons(); m_sessions->syncActivityAnimation(); m_sessions->viewport()->update();
    updateSelectionActions();
}

void SessionsWindow::saveOrganization()
{
    // Every rebuild saves; an organization unchanged since its last save is
    // neither serialized nor sent to the swarm again.
    const auto revision = m_organization.revision();
    if (revision == m_savedOrganizationRevision) return;
    const auto organization = m_organization.toJson();
    if(m_swarm)m_swarm->edit(organization);
    const auto data = QJsonDocument(organization).toJson(QJsonDocument::Compact);
    if (data == m_savedOrganization) { m_savedOrganizationRevision = revision; return; }
    // Its own file, written on the files' writer (SettingsJson.h).
    SettingsJson::store("workspace/organization", data, QJsonDocument(organization));
    m_savedOrganization = data; m_savedOrganizationRevision = revision;
}

void SessionsWindow::updateHeaderText()
{
    // Always reserve the indicator's space; appearing must not re-elide the title.
    // U25: beside a lead's team status the title takes only its own width.
    const int titleWidth = m_teamHeader ? m_title->fontMetrics().horizontalAdvance(m_fullTitle) + 24 : QWIDGETSIZE_MAX;
    if (m_title->maximumWidth() != titleWidth) m_title->setMaximumWidth(titleWidth);
    m_title->setText(m_title->fontMetrics().elidedText(m_fullTitle, Qt::ElideMiddle, qMax(20, m_title->width() - 22)));
    m_sessionLoading->move(qMin(m_title->fontMetrics().horizontalAdvance(m_title->text()) + 6, m_title->width() - 16),
        qMax(0, (m_title->height() - 16) / 2));
    m_meta->setText(m_meta->fontMetrics().elidedText(m_fullMeta, Qt::ElideMiddle, qMax(20, m_meta->width())));
    m_title->setAccessibleName(m_fullTitle); m_meta->setAccessibleName(m_fullMeta);
}

void SessionsWindow::updateSessionLoading()
{
    if (!m_sessionLoading) return;
    const auto *entry = selected();
    bool pending = false; int remaining = 2000;
    const auto consider = [&](const QElapsedTimer &age) {
        pending = true; remaining = qMin(remaining, int(qMax<qint64>(0, 2000 - age.elapsed())));
    };
    if (entry && entry->online) {
        if (!entry->host.isEmpty() && m_remotePollAges.contains(entry->host)) consider(m_remotePollAges.value(entry->host));
        for (const auto &read : m_sessionReads)
            if (read.host == entry->host && read.name == entry->session.name && read.archive == entry->session.archiveId
                && (read.agent.isEmpty() || read.agent == m_subagentId)) consider(read.age);
    }
    m_sessionLoading->setRunning(pending && remaining == 0);
    m_title->setAccessibleDescription(pending && remaining == 0 ? tr("Loading activity…") : QString());
    // WhiteWh fork: before the first answer the status line says so too.
    if (m_selectedLoading != pending) { m_selectedLoading = pending; renderDetails(); }
    if (pending && remaining > 0) m_sessionLoadingTimer.start(remaining);
    else m_sessionLoadingTimer.stop();
}

void SessionsWindow::updateSessionFeedback()
{
    if (!m_sessionFeedback) return;
    const bool outsideComposer = m_detailTabs->currentWidget() != m_activityStack || !m_subagentId.isEmpty()
        || !m_composer->isInputVisible();
    const bool visible = m_pages->currentIndex() == 0 && selected() && outsideComposer
        && m_hint->property("active").toBool() && m_notice->isHidden();
    m_sessionFeedback->setText(visible ? m_hint->text() : QString());
    m_sessionFeedback->setToolTip(Qt::convertFromPlainText(m_sessionFeedback->text()));
    m_sessionFeedback->setAccessibleName(m_sessionFeedback->text());
    setWorkspaceStyle(m_sessionFeedback, QString("color:%1;").arg(m_hint->property("error").toBool()
        ? (m_dark ? "#f4ab9b" : "#a13224") : (m_dark ? "#efc978" : "#92641b")));
    m_sessionFeedback->setVisible(visible);
}

void SessionsWindow::revealSession(const QString &key)
{
    for (const auto &entry : std::as_const(m_entries)) if (entry.key == key) {
        const auto id = m_organization.groupFor(entry.identity);
        if (const auto *group = m_organization.group(id); group && group->collapsed) {
            m_organization.setCollapsed(id, false); saveOrganization(); rebuild();
        }
        return;
    }
}

void SessionsWindow::createGroup(const QString &session)
{
    bool ok = false;
    const auto name = QInputDialog::getText(this, tr("New project"), tr("Project name"), QLineEdit::Normal, {}, &ok);
    if (!ok || name.trimmed().isEmpty()) return;
    const auto id = m_organization.createGroup(name);
    if (!session.isEmpty()) m_organization.moveSession(session, id);
    saveOrganization(); rebuild();
}

void SessionsWindow::showGroupMenu(const QString &id, const QPoint &position)
{
    const auto *group = m_organization.group(id); if (!group) return;
    auto *menu = new QMenu(this); menu->setObjectName("sessionGroupMenu");
    connect(menu->addAction(tr("New session…")), &QAction::triggered, this,
            [this, id]() { showNewSession({}, id, selectedHostTarget()); });
    menu->addSeparator();
    auto *only = menu->addAction(workspaceIcon("projects", QColor(m_muted)), tr("Filter this project")); only->setObjectName("filterOnlyProject");
    connect(only, &QAction::triggered, this, [this,id] { if (m_organization.group(id)) m_projectFilter->setSelection({id}); });
    menu->addSeparator();
    const bool collapsed = group->collapsed;
    connect(menu->addAction(collapsed ? tr("Expand project") : tr("Collapse project")), &QAction::triggered, this, [this, id, collapsed]() {
        m_organization.setCollapsed(id, !collapsed); saveOrganization(); rebuild();
    });
    auto *rename = menu->addAction(tr("Rename project…")); rename->setEnabled(true);
    connect(rename, &QAction::triggered, this, [this, id]() {
        const auto *g = m_organization.group(id); if (!g) return;
        bool ok = false; const auto title = QInputDialog::getText(this, tr("Rename project"), tr("Project name"), QLineEdit::Normal, g->name, &ok);
        if (ok) { m_organization.renameGroup(id, title); saveOrganization(); rebuild(); }
    });
    auto *settings = menu->addAction(tr("Project settings…"));
    connect(settings, &QAction::triggered, this, [this,id] { showProjects(); m_projectsPage->selectProject(id); });
    menu->addSeparator();
    const auto groups = m_organization.groups(); int index = 0;
    while (index < groups.size() && groups[index].id != id) ++index;
    auto *up = menu->addAction(tr("Move project up")); up->setEnabled(index > 0);
    auto *down = menu->addAction(tr("Move project down")); down->setEnabled(index + 1 < groups.size());
    connect(up, &QAction::triggered, this, [this, id, groups, index]() {
        if (index > 0) { m_organization.moveGroup(id, groups[index - 1].id); saveOrganization(); rebuild(); }
    });
    connect(down, &QAction::triggered, this, [this, id, groups, index]() {
        m_organization.moveGroup(id, index + 2 < groups.size() ? groups[index + 2].id : QString()); saveOrganization(); rebuild();
    });
    auto *sort = menu->addAction(tr("Sort sessions by name"));
    connect(sort, &QAction::triggered, this, [this, id]() {
        QList<Entry> members;
        for (const auto &e : std::as_const(m_entries)) if (m_organization.groupFor(e.identity) == id) members.append(e);
        std::stable_sort(members.begin(), members.end(), [](const Entry &a, const Entry &b) {
            const int compared = QString::localeAwareCompare(sessionLabel(a.session), sessionLabel(b.session));
            return compared ? compared < 0 : a.identity < b.identity;
        });
        for (const auto &e : members) m_organization.moveSession(e.identity, id);
        saveOrganization(); rebuild();
    });
    menu->addSeparator();
    auto *remove = menu->addAction(tr("Delete project…")); remove->setEnabled(groups.size()>1);
    connect(remove, &QAction::triggered, this, [this, id]() { m_projectsPage->deleteProject(id); });
    connect(menu, &QMenu::aboutToHide, menu, &QObject::deleteLater); menu->popup(position);
}

QString SessionsWindow::terminalDropTarget() const
{
    const auto *entry=selected();
    if(!entry||!entry->online||m_detailTabs->currentWidget()!=m_terminal||!m_terminal->isConnected())return {};
    return m_selectedKey+'\n'+QString::number(m_terminal->connectionGeneration());
}

void SessionsWindow::pasteTerminalPaths(const QStringList &paths)
{
    // Windows defaults to PowerShell: use its shared literal quoting, including
    // typographic quote delimiters. POSIX machines, including every peer, keep
    // their own single-quote escaping.
    const auto *entry=selected();
    const bool windows=entry&&MachinePlatform::of(m_fleet.metricsOf(entry->host), entry->host.isEmpty())==MachinePlatform::Style::Windows;
    QStringList quoted;
    for(auto path:paths) {
        if(path.contains(QRegularExpression("[\\x00-\\x1f\\x7f]"))) {showNotice(tr("File names containing control characters cannot be pasted into Terminal."),true);return;}
        if(windows) {
            path=MachinePlatform::native(path,MachinePlatform::Style::Windows);
            quoted<<SessionCommands::powerShellQuote(path);
            continue;
        }
        path.replace('\'',QStringLiteral("'\\''"));quoted<<'\''+path+'\'';
    }
    m_terminal->pasteText(quoted.join(' ')+" ");m_terminal->setFocus(Qt::OtherFocusReason);
}

void SessionsWindow::dropTerminalFiles(const QStringList &paths)
{
    const auto *entry=selected();if(!entry||terminalDropTarget().isEmpty())return;
    if(m_terminalDropRequest){showNotice(tr("Wait for the current file transfer to finish."));return;}
    if(entry->host.isEmpty()){pasteTerminalPaths(paths);return;}
    const auto run=m_details.value("run_id").toString(entry->session.runId);
    const auto conversation=m_details.value("conversation_id").toString(entry->session.conversationId);
    // An untracked shell (hgs sh -d) never gets a run identity: its listed
    // creation time binds the copy to that live session instead.
    const bool untracked=run.isEmpty()&&conversation.isEmpty()&&!entry->session.tracked&&!m_details.value("tracked").toBool();
    if(run.isEmpty()&&!untracked){showNotice(tr("Wait for the session identity to load, then drop the files again."),true);return;}
    m_terminalDropKey=terminalDropTarget();m_terminalDropCount=paths.size();m_terminalDropUntracked=untracked?entry->machine:QString();
    m_terminalDropRequest=m_client.requestStageFiles(entry->host,entry->session.name,paths,run,conversation,untracked?entry->session.created:-1);
    showNotice(tr("Transferring files to %1…").arg(entry->machine));
}

void SessionsWindow::showUpdates(){m_pages->setCurrentIndex(5);m_settingsPage->openUpdates();}

void SessionsWindow::showWorkspaceSettings()
{
    m_pages->setCurrentIndex(5);m_settingsPage->refresh();
}

void SessionsWindow::setFleet(const FleetState &fleet) { applyFleet(fleet, false); }
void SessionsWindow::refreshFleet(const FleetState &fleet) { applyFleet(fleet, true); }
void SessionsWindow::applyFleet(const FleetState &fleet, bool refresh) {
    const auto now = QDateTime::currentMSecsSinceEpoch();
    // A refresh changed observation fields only. A peer whose poll age crossed
    // FleetState::kPeerStaleMs since the rows were rendered changes reachability.
    if (refresh && (!m_fleet.refreshObservations(fleet) || m_fleet.freshness(now) != m_renderedFreshness)) refresh = false;
    // A native clean exit may remove its managed checkout without a GUI write.
    // Expire only repositories whose exact live run has newly reached Archive.
    QSet<QString> expired;
    const auto completed = [&](const BoxState &before, const BoxState &after, const QString &host) {
        for (const auto &old : before.sessions) {
            if (old.state == "archived" || !old.gitWorktree || old.gitCommonDir.isEmpty() || old.runId.isEmpty()) continue;
            for (const auto &current : after.sessions) if (current.state == "archived" && current.runId == old.runId && current.conversationId == old.conversationId) {
                const auto key=host+'\n'+old.gitCommonDir;
                if (!expired.contains(key)) { expired.insert(key); m_client.invalidateWorktreeCache(host,old.gitCommonDir); }
                break;
            }
        }
    };
    if (!refresh) {
        completed(m_fleet.local(),fleet.local(),{});
        for (const auto &host : fleet.peerNames()) if (const auto *before=m_fleet.peer(host)) completed(*before,*fleet.peer(host),host);
    }
    if (auto *launch=qobject_cast<NewSessionDialog *>(QApplication::activeModalWidget()); launch && launch->parentWidget()==this) launch->setFleet(fleet);
    m_settingsPage->setLocalSessions(fleet.local().localSessions, LocalSessionHost::canEnable(fleet.local())); m_settingsPage->setPeers(fleet.peerNames());
    m_settingsPage->setMobileGateway(fleet.local().localSessions||MachinePlatform::of(fleet.local().metrics,true)==MachinePlatform::Style::Windows);
    if (!refresh) { m_fleet = fleet; m_fleet.setTeamMembers(m_teamMembers); m_fleet.setReadState(m_fleet.readReplies(), SettingsJson::object("attention/sessionMarks"), ReadMarkStore::load()); }
    reconcileTerminations();m_usageStore->setFleet(fleet); m_accountsPage->setFleet(fleet); updateDashboard(); m_projectsPage->setFleet(fleet); m_machinesPage->setFleet(fleet);
    // Unchanged rows keep their entries unless the clock or the selected
    // session's latest inspection changes what they show.
    if (isVisible() && (!refresh || now >= m_rowsChangeAt || !refreshEntries())) rebuild();
    else if (refresh && isVisible()) { m_searchResults->refreshFleet(m_fleet); updateWorktreeCleanupOffer(); }
    m_renderedFreshness = m_fleet.freshness(now);
    applyQueuedModelSettings();
    updateConnectionStatus();
    if (!m_nativeLaunchToOpen.isEmpty()) {
        for (const auto &entry : std::as_const(m_entries)) if (entry.session.launchId==m_nativeLaunchToOpen) {
            const QString host=entry.host, name=entry.session.name;
            m_nativeLaunchToOpen.clear();showSession(host,name);break;
        }
    }
}
// After a refresh: each entry takes its row's new observation times. False when
// one would read differently from its rendering, as rebuild() makes it: the
// selected session merges its latest inspection, which may also have changed.
bool SessionsWindow::refreshEntries()
{
    QHash<QString, const SessionInfo *> rows;
    const auto collect = [&rows](const QString &host, const BoxState &box) {
        for (const auto &s : box.sessions) if (s.state != "archived") rows.insert(host + '\n' + s.name, &s);
    };
    collect({}, m_fleet.local());
    for (const auto &peer : m_fleet.peerNames()) collect(peer, *m_fleet.peer(peer));
    for (auto &entry : m_entries) {
        if (entry.session.state == "archived") continue;
        const auto *row = rows.value(entry.key);
        if (!row) return false;
        const auto current = entry.key == m_selectedKey && m_inspectError.isEmpty() ? SessionPresentation::inspected(*row, m_details) : *row;
        if (!SessionPresentation::sameInspection(current, entry.session)) return false;
        entry.session.backgroundObservedAt = current.backgroundObservedAt; entry.session.providerStatusAt = current.providerStatusAt;
    }
    return true;
}
void SessionsWindow::setClipboardMode(bool clipboard) { m_clipboard = clipboard; renderDetails(); }
void SessionsWindow::updateDashboard()
{
    // Overview renders while shown; showing it renders the latest fleet.
    if (!isVisible() || m_pages->currentIndex() != 3) { m_dashboardStale = true; return; }
    m_dashboardStale = false;
    auto fleet = m_fleet;
    auto enrichGit = [this](BoxState box, const QString &host, bool online) {
        for (auto &session : box.sessions) {
            const auto path = SessionPresentation::gitStatusPath(session);
            session.gitStatus = SessionPresentation::gitStatus(session, m_client.gitStatusSnapshot(host, path), online);
        }
        return box;
    };
    const auto now = QDateTime::currentMSecsSinceEpoch();
    fleet.setLocal(enrichGit(fleet.local(), {}, fleet.local().ok && m_connectionError.isEmpty()), now);
    for (const auto &host : fleet.peerNames()) {
        const auto box = *fleet.peer(host);
        auto display = enrichGit(box, host, box.ok && now - fleet.peerPolledAt(host) < FleetState::kPeerStaleMs);
        // FleetState keeps existing sessions on a failed poll. Enrich those
        // retained rows in this display copy, then preserve the offline state.
        display.ok = true;
        fleet.setPeer(display, fleet.peerPolledAt(host));
        if (!box.ok) fleet.setPeer(box, fleet.peerPolledAt(host));
    }
    if (!m_connectionError.isEmpty()) { auto local = fleet.local(); local.ok = false; local.error = m_connectionError; fleet.setLocal(local, 0); }
    QSet<QString> drafts;
    const auto collectDrafts = [this, &drafts](const QString &host, const BoxState &box) {
        for (const auto &session : box.sessions) {
            const QString key = host + '\n' + session.name;
            if (entryHasDraft({host, host.isEmpty() ? box.host : host, key, {}, session, box.ok})) drafts.insert(key);
        }
    };
    collectDrafts({}, fleet.local());
    for (const auto &host : fleet.peerNames()) collectDrafts(host, *fleet.peer(host));
    m_dashboard->setFleet(fleet, drafts);
}
void SessionsWindow::setConnectionError(const QString &error) {
    if (m_connectionError == error) return;
    m_connectionError = error; updateDashboard(); updateConnectionStatus(); if (isVisible()) rebuild();
}

void SessionsWindow::setPollingHosts(const QSet<QString> &hosts)
{
    for (auto it = m_remotePollAges.begin(); it != m_remotePollAges.end();) {
        if (!hosts.contains(it.key())) it = m_remotePollAges.erase(it); else ++it;
    }
    for (const auto &host : hosts) if (!host.isEmpty() && !m_remotePollAges.contains(host)) {
        QElapsedTimer age; age.start(); m_remotePollAges.insert(host, age);
    }
    m_pollingHosts = hosts;
    updateConnectionStatus();
    updateSessionLoading();
}

void SessionsWindow::updateConnectionStatus()
{
    QStringList messages;
    bool canRetry = false;
    const auto add = [&](const QString &host, const QString &name, QString error, bool waiting) {
        const bool polling = m_pollingHosts.contains(host);
        if (waiting) {
            messages << (polling ? tr("%1: connecting…") : tr("%1: waiting for connection")).arg(name);
        } else {
            if (error == "timed out") error = host.isEmpty() ? tr("session update timed out") : tr("SSH request timed out");
            if (error.isEmpty() || error == "offline") error = tr("machine unavailable");
            if (error == "bad_response") error = tr("invalid response from hgs");
            messages << tr("%1: %2 (%3)").arg(name, error.simplified(),
                polling ? tr("retrying…") : tr("automatic retry pending"));
        }
        canRetry |= !polling;
    };
    const auto &local = m_fleet.local();
    if (!m_connectionError.isEmpty()) add({}, local.host.isEmpty() ? tr("This machine") : local.host, m_connectionError, false);
    else if (!local.ok) add({}, local.host.isEmpty() ? tr("This machine") : local.host, local.error, local.error.isEmpty());
    for (const auto &host : m_fleet.peerNames()) {
        const auto *peer = m_fleet.peer(host);
        if (!peer || !peer->ok) add(host, host, peer ? peer->error : QString(), m_fleet.peerPolledAt(host) == 0);
    }
    // WhiteWh fork: each change of the connection line, and its recovery, is logged.
    if (const auto line = messages.join("; "); line != m_connectionStatus->text())
        ForkLog::write("connection", line.isEmpty() ? QStringLiteral("all computers connected") : line);
    m_connectionStatus->setText(messages.join("; "));
    m_connectionStatus->setToolTip(Qt::convertFromPlainText(messages.join('\n')));
    m_connectionRetry->setVisible(!messages.isEmpty());
    m_connectionRetry->setEnabled(canRetry);
}

void SessionsWindow::showProjects(const QString &host)
{
    m_pages->setCurrentIndex(1); m_projectsNav->setChecked(true); m_sessionsNav->setChecked(false); m_machinesNav->setChecked(false);
    for (auto *button : m_filters) button->setChecked(false);
    m_projectsPage->selectHost(host);
}

void SessionsWindow::showSessionList()
{
    m_pages->setCurrentIndex(0); m_projectsNav->setChecked(false); m_sessionsNav->setChecked(true); rebuild();
}

void SessionsWindow::showSession(const QString &host, const QString &name)
{
    m_filter = "all"; m_folderFilterPath.clear(); m_hostFilters.clear(); m_projectFilters.clear(); m_accountFilters.clear(); m_search->clear(); showSessionList();
    const QString key = host + '\n' + name;
    revealSession(key);
    revealRow(key, true);
}

void SessionsWindow::showAttentionSession(const QString &host, const QString &name)
{
    m_detailTabs->setCurrentIndex(0);
    showSession(host, name);
    inspect();
}

void SessionsWindow::populateBatchActions(QMenu *menu)
{
    // clear() only detaches child menus; delete them before populating anew.
    qDeleteAll(menu->findChildren<QMenu *>(QString(), Qt::FindDirectChildrenOnly));
    menu->clear();
    auto addMachine = [&](const QString &host, const BoxState &box, bool online) {
        const QString machine = host.isEmpty() ? box.host : host;
        if (machine.isEmpty()) return;
        auto *group = menu->addMenu(QString(machine).replace('&', "&&"));
        int live = 0, saved = 0;
        bool allIdle = true, nativeControls = false;
        for (const auto &s : box.sessions) {
            if (s.state == "archived") continue;
            if (s.cmd=="opencode" || !s.bulkLifecycleSupported) {nativeControls=true;continue;}
            if (s.state == "running") {
                ++live; allIdle = allIdle && s.resumable && s.activity == "idle" && s.processState != "exited";
            } else if (s.resumable) ++saved;
        }
        if(nativeControls) group->addAction(tr("OpenCode uses individual Pause / Resume controls"))->setEnabled(false);
        auto *pause = group->addAction(tr("Pause all (%1)").arg(live));
        pause->setEnabled(online && !m_pending && m_renameKey.isEmpty() && live > 0 && allIdle);
        if (live && !allIdle) {
            auto *reason = group->addAction(tr("Some sessions are busy or context is not tracked"));
            reason->setEnabled(false);
        }
        if(nativeControls) pause->setToolTip(tr("OpenCode sessions are excluded; use their individual Pause / Resume controls."));
        auto *resume = group->addAction(tr("Resume all in background (%1)").arg(saved));
        if(nativeControls) resume->setToolTip(pause->toolTip());
        resume->setEnabled(online && !m_pending && m_renameKey.isEmpty() && saved > 0);
        group->setEnabled(online);
        connect(pause, &QAction::triggered, this, [this, host, machine, live]() {
            if (m_pending) return;
            m_pending = true; m_restoreKey.clear(); m_client.pauseAll(host, live);
            showNotice(tr("Pausing sessions on %1…").arg(machine)); renderDetails();
        });
        connect(resume, &QAction::triggered, this, [this, host, machine, saved]() {
            if (m_pending || !LocalSessionHost::confirmLaunch(this, m_fleet.local(), host) || m_pending) return;
            m_pending = true; m_restoreKey.clear(); m_client.resumeAll(host, saved);
            showNotice(tr("Resuming sessions on %1…").arg(machine)); renderDetails();
        });
    };
    if (m_fleet.local().localSessions) addMachine({}, m_fleet.local(), m_fleet.local().ok && m_connectionError.isEmpty());
    for (const auto &host : m_fleet.peerNames())
        if (const auto *box = m_fleet.peer(host))
            addMachine(host, *box, box->ok && QDateTime::currentMSecsSinceEpoch()
                - m_fleet.peerPolledAt(host) < FleetState::kPeerStaleMs);
    if (menu->actions().isEmpty()) menu->addAction(tr("Waiting for machines…"))->setEnabled(false);
    menu->addSeparator(); menu->addAction(m_expandAllRowsAction); menu->addAction(m_markAllReadAction);
    menu->addAction(m_clearArchiveAction);
}

void SessionsWindow::showNewSession(const QString &agent, const QString &project, const QString &host, const QString &folder, const QString &path, const QString &account, const QJsonObject &recipe, const QString &taskUuid, const QString &taskParentId, const QString &name)
{
    QString group = m_organization.defaultProject();
    if (const auto *row = m_sessions->currentItem()) group = row->data(SessionRoles::Group).toString();
    else if (const auto *entry = selected()) group = m_organization.groupFor(entry->identity);
    QString launchProject = group;
    if (!project.isEmpty()) for (const auto &p : m_organization.groups()) if (p.id == project || p.name == project) { launchProject=p.id; break; }
    // Only generic creation inherits the selected session. Folder/worktree and
    // account actions already carry an explicit launch target.
    const auto *row=m_sessions->currentItem();
    const bool sessionRow=!row||(!row->data(SessionRoles::Header).toBool()&&row->data(SessionRoles::LaunchId).toString().isEmpty());
    const auto *context=sessionRow&&project.isEmpty()&&folder.isEmpty()&&path.isEmpty()&&account.isNull()?selected():nullptr;
    NewSessionDialog dialog(m_client.executable(), m_fleet, context?context->host:host, agent, launchProject, this, m_usageStore, &m_organization);
    // Task executors retain the reviewed launch/delivery reservation path.
    // Generic creation uses the independent background controller from main.
    dialog.setBackgroundCreation(taskUuid.isEmpty());
    connect(&dialog,&NewSessionDialog::creationRequested,this,[this,&dialog](const QJsonObject &plan){
        m_admittingCreation=true;const auto id=m_creations->submit(plan);m_admittingCreation=false;
        if(id.isEmpty()){dialog.creationAdmissionFailed(m_creations->admissionError());return;}
        const auto predecessor=plan["predecessor"].toString();
        if(!predecessor.isEmpty()&&m_creations->canEdit(predecessor))m_creations->hide(predecessor);
        m_revealCreation=id;selectCreation(id);
        m_organization.setCollapsed(plan["group"].toString(),false);showSessionList();m_listStack->setCurrentIndex(0);renderCreation();
        revealRow("#launch\n"+id,true);
    });
    connect(&dialog,&NewSessionDialog::organizationChanged,this,[this]{saveOrganization();rebuild();});
    dialog.setProjectExplicit(!project.isEmpty());
    dialog.setProjectCatalogSwarm(m_swarm ? m_swarm->snapshot().value("swarm_id").toString() : QString());
    dialog.setGroups(m_organization, launchProject);
    if(context)dialog.prefillSessionFolder(selectedDirectory(),m_details.value("cwd_canonical").toString(context->session.canonicalCwd));
    if(!folder.isEmpty()&&!dialog.selectFolder(folder)){showNotice(tr("This project folder changed. Select it again."),true);return;}
    if(!path.isEmpty())dialog.selectPath(path, false);
    if(!account.isNull())dialog.selectAccount(account);
    if(!recipe.isEmpty())dialog.restoreCreationChoices(recipe);
    if(!name.isEmpty())dialog.setSessionName(name);
    connect(&dialog, &NewSessionDialog::launchRequested, this,
        [this,&dialog,taskUuid,taskParentId,launchProject](const QString &host, const QString &cmd, const QString &target, const QString &name, const QString &account, const QString &group, bool openTerminal, const QString &addFolder, const QString &model, const QString &effort, const QString &permissions) {
            const QString id = QUuid::createUuid().toString(QUuid::WithoutBraces);
            if (!taskUuid.isEmpty()) {
                if (group != launchProject || taskParentId.isEmpty()) { showNotice(tr("Choose the task's project and a confirmed parent session."), true); return; }
                m_taskLaunches[id] = QJsonObject{{"task", taskUuid}, {"project", launchProject}, {"host", host}, {"cmd", cmd}, {"tag", name},
                    {"parent", taskParentId}, {"phase", "waiting"}, {"created", QDateTime::currentSecsSinceEpoch()}};
                if (!saveTaskLaunches()) { m_taskLaunches.remove(id); showNotice(tr("Could not save the task executor launch. Nothing was started."), true); return; }
            }
            m_pendingLaunches.insert(id, QJsonObject{{"host", host}, {"cmd", cmd}, {"tag", name}, {"group", group},
                {"target", target}, {"implicit_inbox", dialog.implicitInbox()}, {"add_folder", !addFolder.isEmpty()}, {"folder", addFolder}, {"show_pending", true}, {"created", QDateTime::currentSecsSinceEpoch()}});
            savePendingLaunches();
            m_organization.setCollapsed(group, false);
            m_filter = "all"; m_folderFilterPath.clear(); m_hostFilters.clear(); m_projectFilters.clear(); m_accountFilters.clear(); m_search->clear(); showSessionList();
            revealRow("#launch\n" + id, false);
            // Refresh soon after terminal creation, while the placeholder already
            // gives feedback. Stop these extra polls once the launch is observed.
            for (int delay : {750, 2000}) QTimer::singleShot(delay, this, [this, id] {
                if (m_pendingLaunches.contains(id)) emit refreshRequested();
            });
            if (!openTerminal || cmd=="dsh" || cmd=="opencode") {
                showNotice(tr("Starting session…"));
                m_client.launchDetachedSession(host,cmd,target,name,account,id,model,effort,permissions);
            } else emit newSessionRequested(host, cmd, target, name, account, id, permissions, cmd == "claude" ? model : QString());
        });
    connect(&dialog, &NewSessionDialog::manageProjectsRequested, this, &SessionsWindow::showProjects);
    connect(&dialog, &NewSessionDialog::projectFoldersRequested, this, [this](const QString &id) { showProjects(); m_projectsPage->selectProject(id); });
    connect(&dialog, &NewSessionDialog::machinesRequested, this, [this] { runShortcut(QStringLiteral("go.machines")); m_machinesPage->selectMachine({}); });
    connect(&dialog, &NewSessionDialog::accountLoginRequested, this, &SessionsWindow::accountLoginRequested);
    // WhiteWh fork: a Claude app session continues as a new detached session.
    connect(&dialog, &NewSessionDialog::claudeAppResumeRequested, this,
        [this](const QString &host, const QString &session, const QString &name, bool force, const QString &folder, const QString &group) {
            const QString id = QUuid::createUuid().toString(QUuid::WithoutBraces);
            m_pendingLaunches.insert(id, QJsonObject{{"host", host}, {"cmd", "claude"}, {"tag", name}, {"group", group},
                {"target", folder}, {"add_folder", false}, {"folder", QString()}, {"show_pending", true}, {"created", QDateTime::currentSecsSinceEpoch()}});
            savePendingLaunches();
            m_organization.setCollapsed(group, false);
            m_filter = "all"; m_folderFilterPath.clear(); m_hostFilters.clear(); m_projectFilters.clear(); m_search->clear(); showSessionList();
            for (int delay : {750, 2000}) QTimer::singleShot(delay, this, [this, id] {
                if (m_pendingLaunches.contains(id)) emit refreshRequested();
            });
            showNotice(tr("Continuing the Claude app session…"));
            m_client.resumeClaudeAppSession(host, session, name, id, force);
        });
    connect(&dialog, &NewSessionDialog::accountSetupRequested, this, [this](const QString &host, const QString &, const QString &id) {
        m_pages->setCurrentIndex(4);
        if (id.isEmpty()) m_accountsPage->showMachine(host); else m_accountsPage->showAccount(host, id);
    });
    dialog.exec();
}

bool SessionsWindow::saveTaskLaunches() {
    // A reservation: stored now, or the launch does not start (Settings.h).
    const bool stored = SettingsJson::storeNow(QStringLiteral("tasks/pendingExecutorLaunches"), QJsonDocument(m_taskLaunches));
    if (m_tasks) m_tasks->setExecutorLaunches(m_taskLaunches);
    return stored;
}

void SessionsWindow::dispatchTaskLaunches() {
    for (auto it = m_taskLaunches.begin(); it != m_taskLaunches.end(); ++it) {
        auto record = it.value().toObject();
        if (record.value("phase") != QLatin1String("waiting") || QUuid(it.key()).isNull()) continue;
        for (const auto &entry : std::as_const(m_entries)) {
            if (!entry.online || entry.host != record.value("host").toString() || entry.session.launchId != it.key()
                || entry.session.cmd != record.value("cmd").toString() || entry.session.name.section('/', 2) != record.value("tag").toString()
                || m_organization.groupFor(entry.identity) != record.value("project").toString()) continue;
            // Reserve before transport. Restart/uncertain delivery cannot authorize a replay.
            record["phase"] = "reserved"; record["session"] = entry.session.name; it.value() = record;
            if (!saveTaskLaunches()) { record["phase"] = "review"; it.value() = record; showNotice(tr("Could not reserve task delivery. Check this task before retrying."), true); break; }
            QStringList args{"send", record.value("task").toString(), "--project", record.value("project").toString(),
                "--session", entry.session.name, "--session-id", QStringLiteral("launch_id:") + entry.session.launchId,
                "--expect-new-session-parent", record.value("parent").toString()};
            if (!entry.host.isEmpty()) args << "--host" << entry.host;
            m_taskLaunchRequests.insert(m_client.requestTask(args), it.key()); break;
        }
    }
}

void SessionsWindow::savePendingLaunches()
{
    auto legacy=m_pendingLaunches;
    for(auto it=legacy.begin();it!=legacy.end();)if(it.value().toObject()["creation_schema"]==1)it=legacy.erase(it);else ++it;
    SettingsJson::store(QStringLiteral("workspace/pendingLaunches"), QJsonDocument(legacy));
}

bool SessionsWindow::takeEndedLaunch(const QString &launchId, QJsonObject *launch)
{
    const auto it = std::find_if(m_endedLaunches.begin(), m_endedLaunches.end(), [&](const auto &ended) { return ended.first == launchId; });
    if (it == m_endedLaunches.end()) return false;
    if (launch) *launch = it->second;
    m_endedLaunches.erase(it); return true;
}

void SessionsWindow::newSessionLaunchFailed(const QString &launchId, const QString &error)
{
    if(!m_creations->forLaunch(launchId).isEmpty()){m_creations->launchFinished(launchId,false,error);return;}
    // A listing that reported the launch ended already showed its reason and last
    // lines. A hidden launch status showed nothing, so its failure still names it.
    if (m_taskLaunches.contains(launchId)) {
        auto record = m_taskLaunches.value(launchId).toObject(); record["phase"] = "review"; record["error"] = error.left(2048);
        m_taskLaunches[launchId] = record; saveTaskLaunches();
    }
    QJsonObject ended;
    if (takeEndedLaunch(launchId, &ended) && ended.value("show_pending").toBool()) return;
    const QString name = (ended.isEmpty() ? m_pendingLaunches.value(launchId).toObject() : ended).value("tag").toString();
    m_pendingLaunches.remove(launchId); savePendingLaunches();
    rebuild();
    showNotice(error.isEmpty() ? tr("Could not start session %1. Check the terminal.").arg(name)
        : tr("Could not start session %1: %2").arg(name, error), true);
}

// A created session is confirmed when its computer's listing shows it. Peers
// are listed once a minute, or only through their event stream, so a session
// not listed yet at the one refresh after its launch waited up to a minute
// (user report 2026-10-28): its computer is read every 2 s until the session
// is placed, for at most 3 minutes.
void SessionsWindow::pollConfirmingCreations()
{
    QSet<QString> hosts;
    for (auto it = m_pendingLaunches.constBegin(); it != m_pendingLaunches.constEnd(); ++it) {
        const auto launch = it.value().toObject();
        const auto state = launch.value("state").toString();
        // "recover": one restored after a restart, which the listing also places.
        if (launch.value("creation_schema").toInt() == 1 && (state == QLatin1String("confirming") || state == QLatin1String("recover")))
            hosts.insert(launch.value("host").toString());
    }
    if (hosts.isEmpty() || m_confirmPollAge.elapsed() > 180000) { m_confirmPoll->stop(); return; }
    for (const auto &host : std::as_const(hosts)) emit hostRefreshRequested(host);
}

void SessionsWindow::placeLaunchedSessions()
{
    bool changed = false;
    QString ended, endedLaunch;
    const qint64 now = QDateTime::currentSecsSinceEpoch();
    for (auto it = m_pendingLaunches.begin(); it != m_pendingLaunches.end();) {
        const auto launch = it.value().toObject();
        // Failed commands never produce this token. Expire abandoned intents,
        // while allowing an SSH password prompt or app restart to take its time.
        const bool creation=launch["creation_schema"]==1;
        const auto expectedLaunch=launch.value("launch_id").toString(it.key());
        bool consumed = QUuid(it.key()).isNull() || (!creation&&now - launch.value("created").toInteger() > 7 * 24 * 60 * 60);
        for (const auto &entry : std::as_const(m_entries)) {
            const auto &session = entry.session;
            if (consumed || !entry.online || session.state == "archived" || entry.host != launch.value("host").toString()
                || session.launchId != expectedLaunch || session.cmd != launch.value("cmd").toString()
                || session.name.section('/', 2) != launch.value("tag").toString()) continue;
            if(creation){
                if(m_selectedCreation==it.key()){m_selectedCreation.clear();m_selectedKey.clear();m_creationSessionToSelect=entry.key;}
                const auto id=it.key();QTimer::singleShot(0,this,[this,id]{m_creations->complete(id);});
            }
            const QString requested = launch.value("group").toString();
            const QString group = m_organization.group(requested) ? requested : m_organization.defaultProject();
            // Only new, strictly marked implicit Inbox intents are passive.
            // Legacy/explicit choices keep their original placement semantics.
            // Native inference may arrive before OR after this fleet listing;
            // neither order turns this local fallback into a shared assignment.
            if (launch.value("implicit_inbox") == QJsonValue(true) && requested == "ungrouped") {
                consumed = true; continue;
            }
            // A generated worktree launches in its target but adds the folder it
            // came from. Launches saved before "folder" existed add their target.
            const auto path=launch.value("add_folder").toBool()?launch.value("folder").toString(launch.value("target").toString()):QString();
            const auto style=MachinePlatform::of(m_fleet.metricsOf(entry.host), entry.host.isEmpty());
            if(MachinePlatform::absolute(path,style))m_organization.addFolder(group,entry.machine,path,{},style);
            m_organization.moveSession(entry.identity, group); m_organization.setCollapsed(group, false); consumed = true;
        }
        // A session that ended before any listing showed it, leaving no record,
        // never appears: its machine reports how the launch ended instead.
        const QString host = launch.value("host").toString();
        const BoxState *box = host.isEmpty() ? &m_fleet.local() : m_fleet.peer(host);
        const auto end = !consumed && box && box->ok ? box->endedLaunches.value(expectedLaunch) : QJsonObject();
        if (!end.isEmpty() && std::none_of(m_entries.cbegin(), m_entries.cend(), [&](const Entry &entry) {
                return entry.host == host && entry.session.launchId == expectedLaunch; })) {
            if(creation){
                const auto id=it.key();const auto code=end["code"];
                const auto reason=(code.isDouble()?tr("Session %1 ended before it appeared (exit code %2).").arg(launch["tag"].toString()).arg(code.toInteger())
                    :tr("Session %1 ended before it appeared.").arg(launch["tag"].toString()))+'\n'+end["output"].toString().left(4000);
                QTimer::singleShot(0,this,[this,id,expectedLaunch,reason]{m_creations->ended(id,expectedLaunch,reason);});++it;continue;
            }
            if (m_taskLaunches.contains(expectedLaunch)) {
                auto record = m_taskLaunches.value(expectedLaunch).toObject(); record["phase"] = "review"; record["error"] = tr("Session ended before task delivery");
                m_taskLaunches[expectedLaunch] = record; saveTaskLaunches();
            }
            consumed = true; m_endedLaunches.append({it.key(), launch});
            // Terminal launches and launches restored after a restart never
            // report a result, so only the newest ended launches are kept.
            constexpr qsizetype EndedLaunchLimit = 16;
            if (m_endedLaunches.size() > EndedLaunchLimit) m_endedLaunches.removeFirst();
            if (m_nativeLaunchToOpen == it.key()) m_nativeLaunchToOpen.clear();
            // A hidden launch status stays hidden.
            if (launch.value("show_pending").toBool()) {
                // Only the last ended notice shows. A launch whose notice it
                // replaces showed nothing, so its later failure still names it.
                for (auto &[id, shown] : m_endedLaunches) if (id == endedLaunch) shown["show_pending"] = false;
                endedLaunch = it.key();
                const QString name = launch.value("tag").toString(); const auto code = end.value("code");
                ended = code.isDouble() ? tr("Session %1 ended before it appeared (exit code %2).").arg(name).arg(code.toInteger())
                    : tr("Session %1 ended before it appeared.").arg(name);
                const QString output = end.value("output").toString().trimmed();
                if (!output.isEmpty()) ended += '\n' + output;
            }
        }
        if (consumed) { it = m_pendingLaunches.erase(it); changed = true; } else ++it;
    }
    if (changed) { saveOrganization(); savePendingLaunches(); }
    dispatchTaskLaunches();
    if (!ended.isEmpty()) showNotice(ended, true);
}

bool SessionsWindow::isTerminating(const Entry &entry) const
{
    const auto it=m_terminating.constFind(entry.key);
    return it!=m_terminating.cend() && it->session.runId==entry.session.runId && it->session.created==entry.session.created;
}

void SessionsWindow::reconcileTerminations()
{
    for(auto it=m_terminating.begin();it!=m_terminating.end();){
        const auto &entry=it.value();
        const auto *box=entry.host.isEmpty()?&m_fleet.local():m_fleet.peer(entry.host);
        const bool online=box&&box->ok&&(entry.host.isEmpty()?m_connectionError.isEmpty():
            QDateTime::currentMSecsSinceEpoch()-m_fleet.peerPolledAt(entry.host)<FleetState::kPeerStaleMs);
        if(!online||!m_terminationFinished.contains(it.key())){++it;continue;}
        const bool remains=std::any_of(box->sessions.cbegin(),box->sessions.cend(),[&](const SessionInfo &s){
            return s.state=="running"&&s.name==entry.session.name&&s.runId==entry.session.runId&&s.created==entry.session.created;
        });
        if(remains){++it;continue;}
        for(const auto &archived:box->sessions){
            if(archived.state!="archived"||archived.name!=entry.session.name||archived.runId!=entry.session.runId||archived.archiveId.isEmpty())continue;
            const auto group=m_organization.groupFor(entry.identity);
            const auto archiveIdentity=m_organization.observe(entry.machine,archived.name,archived.runId,archived.archiveId);
            m_organization.moveSession(archiveIdentity,group);saveOrganization();
        }
        m_terminationFinished.remove(it.key());it=m_terminating.erase(it);
    }
}

QString SessionsWindow::selectedHostTarget() const {
    if (m_hostFilters.size() != 1 || m_hostFilters.contains("@local")) return {};
    return *m_hostFilters.cbegin();
}

const SessionsWindow::Entry *SessionsWindow::selected() const
{
    for (const auto &entry : std::as_const(m_entries)) if (entry.key == m_selectedKey) return &entry;
    // Archive and live catalog reads finish independently. Keep the opened
    // archived view while its exact restore is awaiting the live row/draft.
    for (auto restore = m_restores.cbegin(); restore != m_restores.cend(); ++restore)
        if (restore->source.key == m_selectedKey) return &restore->source;
    return nullptr;
}

bool SessionsWindow::awaitingRestoredInspection(const Entry &entry) const
{
    return m_restoredInspections.contains(entry.key);
}

bool SessionsWindow::archiveNameOccupied(const Entry &archive) const
{
    for (const auto &entry : std::as_const(m_entries))
        if (entry.host == archive.host && entry.session.name == archive.session.name && entry.session.state != "archived")
            return true;
    return false;
}

namespace {
QString childState(const QJsonObject &child) {
    return SessionPresentation::childState(child);
}
QString childStatus(const QJsonObject &child) {
    return SessionPresentation::childStatus(child);
}
using SessionRows::childAction;
using SessionRows::childName;
}
QJsonObject SessionsWindow::childRoster(const Entry &entry) const
{
    return SessionRows::roster(entry, rowSelection());
}
QString SessionsWindow::childKey(const Entry &entry, const QString &id) const
{
    const auto conversation = entry.key == m_selectedKey ? m_details.value("conversation_id").toString(entry.session.conversationId) : entry.session.conversationId;
    return entry.machine + '\n' + entry.session.name + '\n' + conversation + '\n' + id;
}
bool SessionsWindow::showChild(const Entry &entry, const QString &id, const QJsonObject &child, bool countsComplete, bool includeCompleted) const
{
    return SessionRows::showChild(entry, id, child, countsComplete, includeCompleted, rowSelection(),
                                  [this](const QString &key) { return m_subagentComposer->hasDraft(key); });
}
bool SessionsWindow::childNeedsAction(const Entry &entry) const
{
    return SessionRows::childNeedsAction(entry, rowSelection());
}
bool SessionsWindow::entryNeedsAttention(const Entry &entry) const
{
    return SessionRows::needsAttention(entry, rowSelection());
}
bool SessionsWindow::entryHasDraft(const Entry &entry) const
{
    return SessionRows::hasDraft(entry, rowSelection(), [this](const QString &key) { return m_composer->hasDraft(key); },
                                 [this](const QString &key) { return m_subagentComposer->hasDraft(key); });
}

void SessionsWindow::rebuild()
{
    // Reconciles the window's own state with the listing here, on the GUI
    // thread: entries and their organization identities, launched sessions,
    // row preferences, restores, forks and renames. What the rows show is then
    // prepared off the GUI thread (requestRows, SessionsWindowRows.cpp).
    if (m_sessions->dragging()) return;
    m_rebuilding = true;
    // The row to select is chosen when prepared rows are applied; a choice
    // made here (a placed creation, fork, restore or rename) waits for them.
    const auto overrideSelection = [this](const QString &key) { m_rowSelection = key; };
    m_entries.clear();
    auto append = [&](const QString &host, const BoxState &box) {
        if (box.host.isEmpty()) return;
        const bool reachable = box.ok && (host.isEmpty() ? m_connectionError.isEmpty() :
            QDateTime::currentMSecsSinceEpoch() - m_fleet.peerPolledAt(host) < FleetState::kPeerStaleMs);
        for (const auto &stored : box.sessions) {
            auto s=stored;
            const QString key = host + '\n' + s.name + (s.state == "archived" ? '\n' + s.archiveId : QString());
            if(key == m_selectedKey && m_inspectError.isEmpty())s=SessionPresentation::inspected(s,m_details);
            m_entries.append({host, host.isEmpty() ? box.host : host, key, {}, s, reachable});
        }
    };
    QMap<QString,QString> archiveHosts;
    auto withArchive=[&](const QString &host,const BoxState &box){
        const bool scope=m_hostFilters.isEmpty()||m_hostFilters.contains(host.isEmpty()?"@local":host);
        BoxState view=box;
        if(box.archivePaging){
            view.sessions.removeIf([](const SessionInfo&s){return s.state=="archived";});
            if(box.ok&&scope)archiveHosts[host]=host.isEmpty()?box.host:host;
            view.sessions.append(m_archiveBrowser->sessions(host));
            for(const auto &row:m_archiveSearchRows.value(host)){bool duplicate=false;for(const auto &old:view.sessions)if(old.archiveId==row.archiveId){duplicate=true;break;}if(!duplicate)view.sessions.append(row);}
        }
        append(host,view);
    };
    withArchive({},m_fleet.local());
    for(const auto &peer:m_fleet.peerNames())withArchive(peer,*m_fleet.peer(peer));
    // A refreshed listing (refreshFleet) renders these rows again by then.
    const auto now = QDateTime::currentMSecsSinceEpoch();
    m_renderedFreshness = m_fleet.freshness(now);
    m_rowsChangeAt = m_pendingLaunches.isEmpty() ? std::numeric_limits<qint64>::max() : now;
    for (const auto &entry : std::as_const(m_entries)) m_rowsChangeAt = qMin(m_rowsChangeAt, SessionPresentation::textChangesAt(entry.session, now));
    updateWorktreeCleanupOffer();
    m_archiveBrowser->configure(archiveHosts,m_filter=="archived"&&m_pages->currentIndex()==0);
    updateAttentionIndicator();
    std::sort(m_entries.begin(), m_entries.end(), [](const Entry &a, const Entry &b) {
        if (a.session.state == "archived" && b.session.state == "archived" && a.session.archivedAt != b.session.archivedAt)
            return a.session.archivedAt > b.session.archivedAt;
        if ((a.session.state == "archived") != (b.session.state == "archived")) return a.session.state != "archived";
        return a.key < b.key;
    });
    for (auto &entry : m_entries) entry.identity = m_organization.observe(entry.machine, entry.session.name, entry.session.runId, entry.session.archiveId);
    for (auto pending = m_restoredInspections.begin(); pending != m_restoredInspections.end(); ++pending) {
        const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&pending](const Entry &entry) { return entry.key == pending.key(); });
        if (current != m_entries.cend() && !current->session.runId.isEmpty() && !current->session.conversationId.isEmpty())
            pending.value() = {current->session.runId, current->session.conversationId};
    }
    bool rowPreferencesChanged = false;
    for (const auto &entry : std::as_const(m_entries)) rowPreferencesChanged |= m_sessionRowPreferences.observe(entry.host, entry.session);
    if (rowPreferencesChanged) saveSessionRowPreferences();
    const auto creationBeforePlacement=m_selectedCreation;
    placeLaunchedSessions();
    if(!creationBeforePlacement.isEmpty()&&m_selectedCreation.isEmpty()){overrideSelection(m_creationSessionToSelect);m_rowCreationPlaced=true;}
    saveOrganization();
    // Entries, attention and the organization above stay current for other pages.
    // A hidden Sessions list and its details render once, when the page is shown.
    if (m_pages->currentIndex() != 0 && m_forkKey.isEmpty() && m_restores.isEmpty() && m_renameKey.isEmpty()) {
        m_sessionsNav->setChecked(false); m_machinesNav->setChecked(m_pages->currentIndex() == 2);
        m_sessionsStale = true; m_rebuilding = false;
        // Rows requested or still being prepared for the list are dropped:
        // showing the page rebuilds it.
        m_rowRequestQueued = false; m_rowsAgain = false;
        if (m_rowsPending) { ++m_rowGeneration; m_rowsPending = false; }
        return;
    }
    m_sessionsStale = false;
    if (!m_pending && !m_forkKey.isEmpty()) {
        for (const auto &entry : std::as_const(m_entries)) if (entry.key == m_forkKey && entry.session.state == "running") {
            overrideSelection(m_forkKey);
            const auto *group = m_organization.group(m_forkGroup);
            const QString groupId = group ? m_forkGroup : m_organization.defaultProject();
            const QString before = group && group->sessions.contains(m_forkBefore) ? m_forkBefore : QString();
            m_organization.moveSession(entry.identity, groupId, before); m_organization.setCollapsed(groupId, false); saveOrganization();
            // Its terminal opens once the applied rows have selected it (applyRows).
            m_forkTerminalKey = m_forkKey; m_forkKey.clear(); m_forkGroup.clear(); m_forkBefore.clear();
            break;
        }
    }
    for (auto restore = m_restores.begin(); restore != m_restores.end();) {
        if (!restore->succeeded) { ++restore; continue; }
        const auto live = std::find_if(m_entries.cbegin(), m_entries.cend(), [&restore](const Entry &entry) {
            return entry.key == restore.key() && entry.session.state == "running";
        });
        if (live == m_entries.cend()) { ++restore; continue; }
        m_composer->ensureDraftLoaded(restore->source.key);
        if (m_composer->isDraftLoading(restore->source.key)) { ++restore; continue; }
        const auto &entry = *live;
        const auto oldKey = restore->source.key;
        const bool follow = m_restoreKey == restore.key() && m_selectedKey == oldKey;
        if (!restore->conversation.isEmpty() && entry.session.conversationId == restore->conversation) {
            // Only a verified continuation retains old run details. Legacy
            // rows without native IDs use normal navigation and inspection.
            if (follow || m_selectedKey == entry.key) {
                m_restoredInspections.clear();
                m_restoredInspections.insert(entry.key, {entry.session.runId, entry.session.conversationId});
            }
            if (!follow && m_selectedKey == oldKey) {
                // A canceled follow must not be revived by renameDraft's
                // asynchronous editor continuation, even if archive paging
                // still retains the old row. No newer selection is changed.
                m_selectedKey.clear(); overrideSelection({});
                m_details = {}; m_events = {}; m_cursor = 0; m_inspectError.clear(); m_childrenHtml.clear();
                m_activityView->setSessionKey({}); m_composer->setSessionKey({});
            }
            // Loading finishes via draftChanged, which schedules rebuild.
            // Never move an empty placeholder over the retained draft.
            m_composer->renameDraft(oldKey, entry.key);
            // Existing current-run local rows are authoritative; retain the
            // archived overlay rather than replacing or growing that cache.
            if (m_localMessages.contains(oldKey) && m_localMessages.value(entry.key).isEmpty())
                m_localMessages.insert(entry.key, m_localMessages.take(oldKey));
            if (follow) {
                m_activityView->continueSession(oldKey, entry.key);
                m_selectedKey = entry.key; overrideSelection(entry.key);
                m_composer->setSessionKey(entry.key);
                // Keep the visible history, but no old-run controls are
                // enabled before a fresh inspection of the resumed run.
                m_inspectError = tr("Session restored. Refreshing its current state…");
                m_cursor = 0; m_inspectionAge.invalidate(); m_processPollAge.invalidate();
                const auto key = entry.key;
                QTimer::singleShot(0, this, [this, key] { if (m_selectedKey == key) inspect(); });
            }
        } else if (follow) {
            // Missing/different native identity is not a draft migration.
            // The archived draft remains recoverable in Saved drafts.
            if (entry.session.conversationId.isEmpty()) overrideSelection(entry.key);
            else {
                overrideSelection({});
                showNotice(tr("The restored conversation could not be verified. Your archived draft remains in Saved drafts."), true);
            }
        }
        if (m_restoreKey == restore.key()) m_restoreKey.clear();
        restore = m_restores.erase(restore);
    }
    if (!m_pending && !m_renameKey.isEmpty()) {
        for (const auto &entry : std::as_const(m_entries)) if (entry.key == m_renameKey) {
            overrideSelection(m_renameKey); m_renameKey.clear(); break;
        }
    }
    // The account catalog, filters, counts and every row are prepared from a copy.
    m_sessionAccountProfiles = m_accountsPage->catalogProfiles();
    m_sessionAccountDetailsProfiles = m_accountsPage->profiles(true);
    m_rebuilding = false;
    requestRows();
}

void SessionsWindow::updateGitWatch()
{
    auto *streams = EventStreams::instance();
    if (!streams) return;
    QSet<QString> online, seen;
    for (const auto &entry : std::as_const(m_entries)) {
        const auto path = SessionPresentation::gitStatusPath(entry.session);
        if (entry.online && !path.isEmpty()) online.insert(entry.host + '\n' + path);
    }
    QHash<QString, QStringList> paths;
    const auto add = [&](const QString &host, const QString &path) {
        const auto key = host + '\n' + path;
        if (path.isEmpty() || !online.contains(key) || seen.contains(key)) return;
        seen.insert(key); paths[host].append(path);
    };
    if (const auto *entry = selected()) add(entry->host, SessionPresentation::gitStatusPath(entry->session));
    for (int i = 0; i < m_sessions->count(); ++i) {
        const auto *item = m_sessions->item(i);
        if (!item->isHidden()) add(item->data(SessionRoles::GitHost).toString(), item->data(SessionRoles::GitPath).toString());
    }
    for (const auto &entry : std::as_const(m_entries)) add(entry.host, SessionPresentation::gitStatusPath(entry.session));
    streams->watchGit(paths);
}

void SessionsWindow::updateAttentionIndicator()
{
    m_attentionCount = std::count_if(m_entries.cbegin(),m_entries.cend(),[this](const Entry &entry) {
        return entry.session.state != "archived" && !entry.session.muted && entryNeedsAttention(entry);
    });
    m_attentionBadge->setText(m_attentionCount > 99 ? "99+" : QString::number(m_attentionCount));
    setWorkspaceStyle(m_attentionBadge, QString("QLabel{background:%1;color:%2;border-radius:7px;font-size:9px;font-weight:700;}")
        .arg(m_dark ? "#ffda76" : "#a66000",m_dark ? "#30250c" : "#ffffff"));
    m_attentionBadge->setVisible(m_attentionCount > 0);
    const QString hint = m_attentionCount ? tr("Sessions — %1 need attention").arg(m_attentionCount) : tr("Sessions");
    m_sessionsNav->setAccessibleName(hint); m_sessionsNav->setToolTip(ShortcutRegistry::instance().tooltip(hint, "go.sessions"));
    m_brand->setProperty("attentionCount",m_attentionCount);
    m_brand->setAppearance(m_dark ? QColor(Qt::white) : QColor(m_accent), QColor(m_dark ? "#ffda76" : "#a66000"),
        m_attentionCount > 0, Settings::value("workspace/reduceMotion", false).toBool());
}

void SessionsWindow::markAllRepliesRead()
{
    const auto replies = m_fleet.unreadReplies();
    if (replies.isEmpty()) return;
    // Keep the open conversation and its draft in place, even if the attention
    // filter would otherwise remove this session after acknowledgement.
    if (const auto *entry = selected(); entry && entry->session.unreadReply) m_readSelectionKey = entry->key;
    m_readCandidate.clear();
    const int count = m_fleet.markRepliesRead(replies);
    ReadMarkStore::add(m_fleet.takeNewReadMarks());
    if (!count) return;
    rebuild(); updateDashboard(); emit repliesMarkedRead(replies);
    showNotice(count == 1 ? tr("1 reply marked as read.") : tr("%1 replies marked as read.").arg(count));
}

void SessionsWindow::checkViewedReply()
{
    const auto *entry = selected();
    const bool active = isVisible() && isActiveWindow() && !isMinimized() && m_pages->currentIndex() == 0
        && !QApplication::activeModalWidget() && !QApplication::activePopupWidget();
    if (!m_subagentId.isEmpty()) { m_readCandidate.clear(); return; }
    bool latest = false;
    if (active && entry && entry->session.unreadReply && entry->online && m_inspectError.isEmpty()
        && m_details.value("conversation_id").toString() == entry->session.conversationId
        && m_details.value("reply_id").toString() == entry->session.replyId) {
        if (m_activityView->isVisible()) latest = m_activityView->replyVisible(entry->session.replyId);
        else if (m_terminal->isVisible() && m_terminal->isConnected()) {
            const auto *screen = m_terminal->terminalScreen(); const auto *scroll = screen->verticalScrollBar();
            latest = scroll->value() == scroll->maximum() && !screen->screenText().trimmed().isEmpty();
        }
    }
    if (!latest) { m_readCandidate.clear(); return; }
    const QString candidate = entry->key + '\n' + entry->session.conversationId + '\n' + entry->session.replyId;
    if (candidate != m_readCandidate) { m_readCandidate = candidate; m_readDwell.start(); return; }
    if (m_readDwell.elapsed() < 1000) return;
    const auto host = entry->host, name = entry->session.name, conversation = entry->session.conversationId, reply = entry->session.replyId;
    // Keep the opened result in the attention view until the user switches away.
    // Acknowledging it must not replace the conversation under their eyes.
    m_readSelectionKey = entry->key; m_readCandidate.clear();
    if (m_fleet.markReplyRead(host, name, conversation, reply)) {
        ReadMarkStore::add(m_fleet.takeNewReadMarks());
        rebuild(); updateDashboard(); emit replyViewed(host, name, conversation, reply);
    }
}

void SessionsWindow::selectSession()
{
    const auto *item = m_sessions->currentItem();
    if(item&&!item->data(SessionRoles::LaunchId).toString().isEmpty()&&m_creations->owns(item->data(SessionRoles::LaunchId).toString())){
        selectCreation(item->data(SessionRoles::LaunchId).toString());return;
    }
    if (item && (item->data(SessionRoles::Header).toBool() || !item->data(SessionRoles::LaunchId).toString().isEmpty())) return;
    if(item)m_selectedCreation.clear();
    if(item&&item->data(KeyRole).toString()!=m_creationSessionToSelect)m_creationSessionToSelect.clear();
    const QString child = item ? item->data(SessionRoles::ChildId).toString() : QString();
    const QString key = item ? item->data(child.isEmpty() ? KeyRole : SessionRoles::ParentKey).toString() : QString();
    if (key != m_selectedKey) {
        m_selectedTeam.clear();   // WhiteWh fork: a chosen session shows its own team
        m_subagentComposer->setFileReferenceTarget({}, {}, {});
        pushSelectionChanged(key);
        m_readCandidate.clear(); m_readSelectionKey.clear();
        m_selectedKey = key; m_details = {}; m_events = {}; m_cursor = 0; m_inspectError.clear();
        // A pending Team-tab Stop belongs to the session it named.
        m_terminateAfterInspect.clear();
        if (!m_restoredInspections.contains(key)) m_restoredInspections.clear();
        if (m_restoredInspections.contains(key)) m_inspectError = tr("Session restored. Refreshing its current state…");
        rememberRecentSession(key);
        m_processPollAge.invalidate();m_inspectionAge.invalidate();
        m_childrenHtml.clear(); m_activityView->setSessionKey(key); m_composer->setSessionKey(key);
        const auto *entry = selected();
        m_terminal->setSession(m_client.executable(), entry ? entry->host : QString(), entry ? entry->session.name : QString(), {}, entry ? entry->machine : QString());
        if (!child.isEmpty()) openSubagent(child);
        inspect(); renderDetails();
        if (selected() && m_subagentId.isEmpty() && m_detailTabs->currentWidget() == m_terminal) m_terminal->connectSession();
        return;
    }
    if (!child.isEmpty() && child != m_subagentId) openSubagent(child);
    else if (child.isEmpty() && !m_subagentId.isEmpty()) closeSubagent();
    renderDetails();
}

void SessionsWindow::inspect()
{
    if (!m_pending && !m_renameKey.isEmpty()) return;
    const auto *entry = selected();
    if(isVisible() && m_pages->currentIndex()==3)updateDashboardAccounts(true);
    if (!isVisible() || m_pages->currentIndex() != 0 || !entry || !entry->online) return;
    // WhiteWh fork: the Tasks card lists the agent's background jobs from this
    // read, so it is made while the card is on screen even with the Processes
    // tab off, at the background interval (owner, 2026-10-28: with the tab off,
    // which is the default, the card never showed them).
    const bool tasksCard=m_inspector&&m_inspector->isVisible()&&m_inspector->isOpen(InspectorCards::Tasks);
    const int processInterval=m_detailTabs->currentWidget()==m_processes?ProcessSettings::activeMs():ProcessSettings::backgroundMs();
    // The first read of a newly selected session skips processes: reading them
    // takes 1 to 8 s on Windows, and Activity waited for it (owner, 2026-10-28:
    // switching took about 4 s). The next poll reads them.
    const bool processes=!m_details.isEmpty() && (ProcessSettings::enabled()||tasksCard)
        && (!m_processPollAge.isValid() || m_processPollAge.elapsed()>=processInterval);
    // A live event stream brings Activity as it happens (EventStream.h); only
    // the process section is still polled, at its own interval.
    auto *streams = EventStreams::instance();
    // External workstreams expose a cooperative inbox, outside watch-v1's
    // native Activity journal. Keep their scoped inspection on the poll path.
    const bool streamed = entry->session.cmd != "external" && streams && streams->live(entry->host) && entry->session.archiveId.isEmpty()
        && !m_streamEnded.contains(entry->host + '\n' + entry->session.name + '\n' + entry->session.runId);
    if (streams) {
        if (streamed) streams->follow(entry->host, entry->session.name, entry->session.runId, entry->session.conversationId, m_cursor);
        else streams->unfollowAll();
    }
    if (streamed) {
        m_inspectionAge.start();
        if (processes && m_client.requestInspection(entry->host, entry->session.name, m_cursor, entry->session.archiveId, true)) m_processPollAge.start();
    } else if(m_client.requestInspection(entry->host, entry->session.name, m_cursor, entry->session.archiveId,processes)) {
        m_inspectionAge.start();if(processes)m_processPollAge.start();
    }
    refreshAccountUsage();
    if (!m_subagentId.isEmpty()) m_client.requestSubagentInspection(entry->host, entry->session.name, m_subagentId, entry->session.archiveId);
}

void SessionsWindow::renderAccountUsage()
{
    const auto *entry=selected();const bool visible=m_pages->currentIndex()==0 && entry && entry->session.tracked && entry->session.archiveId.isEmpty() && m_subagentId.isEmpty();
    // Hide only when it should go: hiding and showing again would close an open popover on every refresh.
    m_usageStrip->setVisible(visible);if(!visible){m_recovery->setUsageNotice({});m_usageLimit->setActive(false);m_usageRefresh->setRefreshing(false);return;}
    m_accountUsageData=m_usageStore->data(AccountUsageRef::bound(entry->host,entry->session));m_accountUsage->setData(m_accountUsageData);
    QString notice;
    const auto failure=m_details.value("provider_error").toObject();
    if(entry->online && entry->session.cmd=="claude" && m_details.value("run_id").toString()==entry->session.runId
        && m_details.value("conversation_id").toString()==entry->session.conversationId
        && m_details.value("provider_status_at").toDouble()>=entry->session.providerStatusAt
        && (m_details.value("phase")=="error" || m_details.value("attention_id").toString().startsWith("claude-usage-limit:"))
        && (failure.value("error_kind")=="quota" || failure.value("error_kind")=="session_limit"))
        notice=AccountUsage::limitRecoveryNotice(m_accountUsageData,failure.value("at").toDouble());
    m_recovery->setUsageNotice(notice);
    const auto exhausted=AccountUsage::exhausted(m_accountUsageData);const auto warning=exhausted.isEmpty()?notice:exhausted;m_usageWarning->setText(warning);
    m_usageLimit->setTone(exhausted.isEmpty()?ChipTone::Neutral:ChipTone::Danger);
    m_usageLimit->setLabels(exhausted.isEmpty()?tr("Usage available"):AccountUsage::exhaustedSummary(m_accountUsageData),exhausted.isEmpty()?tr("Usage"):tr("Limit"));m_usageLimit->setDetail(warning);
    m_usageLimit->setActive(!warning.isEmpty());
    m_usageRefresh->setRefreshing(m_accountUsageData["refreshing"].toBool());
    m_usageRefresh->setEnabled(entry->online && !m_accountUsageData["refreshing"].toBool());
}
void SessionsWindow::refreshAccountUsage(bool force)
{
    const auto *entry=selected();
    if(entry && entry->online && entry->session.tracked && entry->session.archiveId.isEmpty() && m_subagentId.isEmpty())
        m_usageStore->ensure(AccountUsageRef::bound(entry->host,entry->session),force);
    renderAccountUsage();
}
void SessionsWindow::updateDashboardAccounts(bool request,bool force)
{
    // Showing Overview requests and renders accounts again; skip hidden updates.
    if(!isVisible() || m_pages->currentIndex()!=3)return;
    if(request)m_accountsPage->ensureCatalogs(force);
    QJsonArray rows;
    for(const auto &value:m_accountsPage->profiles()) {
        auto p=value.toObject();const auto ref=AccountUsageRef::profile(p);
        if(request && p["installed"].toBool())m_usageStore->ensure(ref,force);
        rows.append(p);
    }
    m_dashboard->setAccounts(rows);
}

void SessionsWindow::openSubagent(const QString &id)
{
    const auto *entry = selected();
    const auto roster = entry ? childRoster(*entry) : QJsonObject();
    if (!entry || !roster.contains(id)) return;
    if (m_subagentId == id) return;
    m_composer->setFileReferenceTarget({}, {}, {});
    m_subagentDetails = {}; m_subagentComposer->setInputVisible(false);
    m_subagentComposer->setSessionKey(childKey(*entry, id));
    m_subagentComposer->setFileReferenceTarget(childKey(*entry, id), entry->host, entry->machine,
        fileReferenceContext(entry->session, m_details));
    m_subagentId = id; m_subagentConversation = m_details.value("conversation_id").toString(entry->session.conversationId);
    m_subagentCwd = selectedDirectory();
    m_subagentTitle->setText(childName(roster.value(id).toObject(), id));
    m_subagentTitle->setToolTip(id);
    m_subagentHint->setText(tr("Loading subagent activity…"));
    m_subagentView->setSessionKey(m_selectedKey + '\n' + m_subagentConversation + '\n' + id);
    m_subagentView->setActivity({},{});
    m_activityStack->setCurrentIndex(1); m_detailTabs->setCurrentIndex(0);
    m_expandedSessions.insert(entry->key);
    rebuild(); inspect();
}

void SessionsWindow::closeSubagent()
{
    const bool wasOpen = !m_subagentId.isEmpty();
    m_subagentId.clear(); m_subagentConversation.clear(); m_subagentCwd.clear(); m_subagentDetails = {};
    m_subagentComposer->setFileReferenceTarget({}, {}, {});
    m_activityStack->setCurrentIndex(0);
    const QSignalBlocker block(m_sessions);
    for (int i = 0; i < m_sessions->count(); ++i) if (m_sessions->item(i)->data(KeyRole).toString() == m_selectedKey) { m_sessions->setCurrentRow(i); break; }
    if (!m_rebuilding) renderDetails();
    // Drop the temporary visible-row pin after deliberate navigation, outside
    // selection/render callbacks that still hold pointers into m_entries.
    if (wasOpen && !m_rebuilding) QTimer::singleShot(0, this, [this]{ rebuild(); });
}

void SessionsWindow::beginSendEvidence(const QString &key, const QJsonObject &data)
{
    ++m_pendingEvidenceUsers[key];
    if (!data.isEmpty()) m_pendingSendEvidence.insert(key, sendEvidence(data));
    else if (!m_pendingSendEvidence.contains(key)) m_pendingSendEvidence.insert(key, {});
}

void SessionsWindow::finishSendEvidence(const QString &key)
{
    if (--m_pendingEvidenceUsers[key] > 0) return;
    m_pendingEvidenceUsers.remove(key); m_pendingSendEvidence.remove(key);
}

void SessionsWindow::acceptInspection(const QString &host, const QString &name, const QJsonObject &reply, const QString &archiveId)
{
    const SlowEventLog::Scope scope(this, "inspection");
    auto data = reply;
    const QString restoredKey = host + '\n' + name;
    if (archiveId.isEmpty() && m_restoredInspections.contains(restoredKey)) {
        const auto expected = m_restoredInspections.value(restoredKey);
        const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&restoredKey](const Entry &entry) { return entry.key == restoredKey; });
        // A late old-run reply or an unavailable/cached inspection cannot unlock
        // controls. A newer incarnation must also prove its own current IDs.
        if (current == m_entries.cend() || expected.first.isEmpty() || expected.second.isEmpty()
            || current->session.runId != expected.first || current->session.conversationId != expected.second
            || data.value("run_id").toString() != expected.first
            || data.value("conversation_id").toString() != expected.second
            || data.value("inspection_unavailable").toBool()) return;
        m_restoredInspections.remove(restoredKey);
    }
    if (archiveId.isEmpty()) {
        for (const auto &entry : std::as_const(m_entries))
            if (entry.host == host && entry.session.name == name && entry.session.archiveId.isEmpty())
                if (m_pendingSendEvidence.contains(entry.key)) m_pendingSendEvidence.insert(entry.key, sendEvidence(data));
        continueAfterCompact(host, name, data); continueAfterStart(host, name, data);
    }
    if (!m_pending && !m_renameKey.isEmpty()) return;
    const auto *entry = selected(); if (!entry || entry->host != host || entry->session.name != name || entry->session.archiveId != archiveId) return;
    // A reply read before a settings receipt of the same run would undo it: drop
    // its pending change, so the queued automatic apply never comes, or bring back
    // one the receipt applied. Keep the receipt's settings; the next poll reads anew.
    if (m_inspectionBehindSettings.remove(entry->key) && !m_details.isEmpty()
        && m_details.value("run_id") == data.value("run_id") && m_details.value("conversation_id") == data.value("conversation_id"))
        for (const auto *field : {"model", "effort", "pending_model", "pending_effort", "pending_settings_id"}) {
            if (m_details.contains(field)) data[field] = m_details.value(field);
            else data.remove(field);
        }
    // A confirmed clear continues the same Activity timeline, and its global
    // journal cursor stays valid. Keep the earlier events in place.
    if (!m_details.isEmpty() && m_details.value("conversation_id") != data.value("conversation_id")
        && !data.value("cleared_conversations").toArray().contains(m_details.value("conversation_id"))) {
        // The conversation changed under this terminal. Fetch its initial window;
        // a cursor from the previous conversation must never skip the new history.
        m_events = data.value("events").toArray(); m_cursor = 0; m_details = data; m_processPollAge.invalidate();
        reconcileMessages(); renderDetails(); pushInspected(entry->key, m_details); inspect(); return;
    }
    const auto previousRoster = childRoster(*entry);
    const auto inspected=SessionPresentation::inspected(entry->session,data);
    const bool statusChanged=inspected.phase!=entry->session.phase || inspected.activity!=entry->session.activity
        || inspected.providerError!=entry->session.providerError || inspected.recovery!=entry->session.recovery;
    const auto previousProcesses=m_details.value("processes");
    const bool sameRun=m_details.value("run_id")==data.value("run_id") && m_details.value("host_generation")==data.value("host_generation");
    m_details = data; m_inspectError.clear();
    // U35: a message just queued shows until a read lists it or a second read does not.
    if (auto held = m_justHeld.find(entry->key); held != m_justHeld.end()) {
        QSet<QString> listed;
        for (const auto &value : data.value("held_inputs").toArray()) listed.insert(value.toObject().value("request_id").toString());
        for (int i = int(held->size()); i-- > 0;) {
            auto item = (*held)[i].toObject();
            item["reads"] = item.value("reads").toInt() + 1;
            if (listed.contains(item.value("request_id").toString()) || item.value("reads").toInt() >= 2) held->removeAt(i);
            else (*held)[i] = item;
        }
    }
    // #511 N5: a held message hgs could not deliver comes back to this
    // session's composer with why; an uncertain one says Delivery unconfirmed.
    if (auto sent = m_sentHeld.find(entry->key); sent != m_sentHeld.end() && entry->key == m_selectedKey) {
        for (const auto &lost : sent->take(data.value("held_settled").toArray())) {
            m_composer->editor()->setPlainText(HeldSent::restored(m_composer->editor()->toPlainText(), lost.text));
            showNotice(HeldSent::notice(lost), true);
        }
    }
    // WhiteWh fork: the Tasks card's background jobs come from the same read,
    // whether or not the Processes tab is on (it drops the rest).
    if(data.contains("processes")){
        QJsonArray jobs;
        for(const auto &value:data.value("processes").toObject().value("items").toArray())
            if(value.toObject().value("background").toBool()&&jobs.size()<64)jobs.append(value);
        m_backgroundJobs=jobs;
    }else if(!sameRun)m_backgroundJobs={};
    if(!ProcessSettings::enabled())m_details.remove("processes");
    else if(!data.contains("processes") && sameRun && previousProcesses.isObject())m_details["processes"]=previousProcesses;
    else if(!data.contains("processes"))m_processPollAge.invalidate();
    if (data.value("replace_events").toBool()) m_events = data.value("events").toArray();
    else {
        // A conversation switch resets the fetch cursor while retaining its
        // first window. The full refetch must not append those events again.
        QSet<qint64> knownSequences;
        for (const auto &event : m_events) knownSequences.insert(event.toObject().value("seq").toInteger());
        for (const auto &event : data.value("events").toArray()) {
            const auto seq = event.toObject().value("seq").toInteger();
            if (seq <= m_cursor || knownSequences.contains(seq)) continue;
            knownSequences.insert(seq); m_events.append(event);
        }
    }
    while (m_events.size() > 500) m_events.removeFirst();
    m_cursor = data.value("cursor").toInteger(); reconcileMessages(); renderDetails(); applyPendingModelSettings();
    pushInspected(entry->key, m_details);
    // N7: a Team-tab leftover Stop waited for these details; its own terminate
    // flow can now decide the real action and confirm it.
    if (m_terminateAfterInspect == entry->key) {
        m_terminateAfterInspect.clear();
        QTimer::singleShot(0, this, [this, key = entry->key] {
            const auto *current = selected();
            if (current && current->key == key) terminateSession();
        });
    }
    if (statusChanged || previousRoster != childRoster(*entry)) rebuild();
}

namespace {
// Why the host reports a native context action ("clear", "compact")
// unavailable. Older hosts omit the reason: the action keeps its usual text.
QString contextActionReason(const QJsonObject &details, const QString &action)
{
    if (details.value(action + "_context_supported").toBool()) return {};
    return details.value(action + "_context_reason").toString().simplified();
}
// The inspected conversation can be cleared now, or after dismissing a Claude chooser.
bool clearableContext(const QJsonObject &details)
{
    return !details.value("run_id").toString().isEmpty() && !details.value("conversation_id").toString().isEmpty()
        && (details.value("clear_context_supported").toBool() || details.value("clear_context_dismiss").toBool());
}
QString contextReasonSentence(const QString &reason)
{
    return reason.isEmpty() || QStringLiteral(".!?…").contains(reason.back()) ? reason : reason + '.';
}
// A disabled context action shows that reason instead of its usual tooltip.
template<typename Control> void explainContextAction(Control *control, const QString &reason)
{
    if (!control->property("usualToolTip").isValid()) control->setProperty("usualToolTip", control->toolTip());
    // Reported names stay literal, as in the cold-cache line.
    control->setToolTip(reason.isEmpty() ? control->property("usualToolTip").toString()
        : Qt::convertFromPlainText(contextReasonSentence(reason), Qt::WhiteSpaceNormal));
}
}

// WhiteWh fork: a media file the agent names. A preview of a peer's video waits
// for a click: it would download the whole file.
void SessionsWindow::loadMedia(ActivityView *view, const QString &key, const QString &href, bool open)
{
    const auto *entry = selected(); if (!entry) return;
    const bool local = entry->host.isEmpty();
    const auto target = SessionFileReference::parse(href);
    const auto path = SessionFileReference::resolve(target, m_subagentId.isEmpty() ? selectedDirectory() : m_subagentCwd, local ? QDir::homePath() : QString());
    if (path.isEmpty()) { view->setMediaPreview(key, {}, tr("The session folder is unknown")); return; }
    if (local) {
        // #744: the file's state from PathProbe's worker; asked again once it answers.
        const auto file = PathProbe::instance().info(path);
        if (file.state == PathProbe::Info::Checking) {
            view->setMediaPreview(key, {}, tr("Checking the file…"));
            QPointer<ActivityView> target(view);
            auto *once = new QMetaObject::Connection;
            *once = connect(&PathProbe::instance(), &PathProbe::answered, this, [this, target, key, href, open, path, once](const QString &answered) {
                if (answered != path) return;
                disconnect(*once); delete once;
                if (target) loadMedia(target, key, href, open);
            });
            return;
        }
        if (file.state == PathProbe::Info::Unresponsive) { view->setMediaPreview(key, {}, tr("The file does not answer: its disk may be busy")); return; }
        if (!file.file) { view->setMediaPreview(key, {}, tr("Not found on this computer")); return; }
        if (!open && file.size > MediaFiles::PreviewLimit) { view->setMediaPreview(key, {}, tr("Too large for a preview")); return; }
        view->setMediaPreview(key, path);
        if (open) (new MediaViewer(path, QFileInfo(path).fileName() + tr(" — Zerus"), this))->show();
        return;
    }
    if (!entry->online) { view->setMediaPreview(key, {}, tr("%1 is offline").arg(entry->machine)); return; }
    if (!open && MediaFiles::kind(path) == MediaFiles::Kind::Video) { view->setMediaPreview(key, {}, tr("On %1 · click to play").arg(entry->machine)); return; }
    if (open) showNotice(tr("Loading %1 from %2…").arg(QFileInfo(path).fileName(), entry->machine));
    m_mediaDownloads.insert(m_client.downloadFileToCache(entry->host, path), {view, key, open});
}

// WhiteWh fork: the working line shows the plan's current step, else the tool
// in use, else the activity the agent reports, while the selected live session works.
void SessionsWindow::updateWorkingLine(const Entry *entry)
{
    if (!m_workingLine) return;
    const QString phase = m_details.value("phase").toString(), activity = m_details.value("activity").toString();
    const bool working = entry && entry->online && entry->session.state == "running" && entry->session.processState != "exited"
        && m_inspectError.isEmpty() && activity == "busy" && phase != "approval" && phase != "question";
    QString text;
    if (working) {
        const auto main = m_details.value("task_lists").toObject().value("main").toObject();
        if (main.value("run_id").toString() == m_details.value("run_id").toString())
            for (const auto &value : main.value("items").toArray())
                if (value.toObject().value("status") == "in_progress") { text = value.toObject().value("title").toString(); break; }
        if (text.isEmpty()) text = m_details.value("tool_detail").toString();
        if (text.isEmpty()) text = m_details.value("current_tool").toString();
        if (text.isEmpty()) text = m_details.value("activity_detail").toString();
        if (text.isEmpty()) text = m_details.value("activity_summary").toString();
        text = text.simplified();
        if (text.isEmpty()) text = tr("Working");
    }
    m_workingLine->setColors(QColor(m_accent), QColor(m_fg), QColor(m_muted));
    m_workingLine->setWorking(working, text, m_details.value("turn_started").toDouble());
    renderHeld();
}

// U35: what hgs holds for the selected session, and what was just queued there.
// A just-queued message shows until a read lists it or two reads after it do not.
void SessionsWindow::renderHeld()
{
    if (!m_heldBar) return;
    const auto *entry = selected();
    QJsonArray held = entry ? m_details.value("held_inputs").toArray() : QJsonArray();
    QSet<QString> listed;
    for (const auto &value : held) listed.insert(value.toObject().value("request_id").toString());
    if (entry)
        for (const auto &value : m_justHeld.value(m_selectedKey))
            if (!listed.contains(value.toObject().value("request_id").toString())) held.append(value);
    m_heldBar->setColors(QColor(m_fg), QColor(m_muted), QColor(m_border), QColor(m_surface));
    m_heldBar->setHeld(held, entry ? IdentityBadges::providerName(entry->session.cmd) : QString());
    // Messages waiting for an agent that is not working: a Terminal draft may hold them.
    m_terminalDraft->setColors(QColor(m_fg), QColor(m_muted), QColor(m_border), QColor(m_surface));
    m_terminalDraft->setSession(entry ? entry->host : QString(), entry ? entry->session.name : QString(),
                                entry ? m_details.value("run_id").toString() : QString(), entry ? m_details.value("conversation_id").toString() : QString());
    if (!held.isEmpty() && m_details.value("activity").toString() != QLatin1String("busy")) m_terminalDraft->check(true);
}

void SessionsWindow::openSessionTerminal(const QString &host, const QString &name)
{
    showSession(host, name);
    const auto *entry = selected();
    if (!entry || entry->host != host || entry->session.name != name) return;
    if (entry->session.cmd == "dsh") { showNativeUi(*entry); return; }
    m_detailTabs->setCurrentWidget(m_terminal); m_terminal->connectSession();
}

QList<TeamCatalogEditor::Computer> SessionsWindow::teamComputers() const
{
    QList<TeamCatalogEditor::Computer> computers;
    const qint64 now=QDateTime::currentMSecsSinceEpoch();
    for(const auto &alias:m_fleet.peerNames()){
        const auto *box=m_fleet.peer(alias);
        computers.append({alias,alias,box&&box->ok&&now-m_fleet.peerPolledAt(alias)<FleetState::kPeerStaleMs});
    }
    return computers;
}

void SessionsWindow::setKeepWarm(int hours)
{
    const auto *entry=selected(); if(!entry || !m_subagentId.isEmpty() || entry->session.conversationId.isEmpty()) return;
    CacheKeepalive::setHours(entry->host,entry->session,hours);
}

void SessionsWindow::updateTasks(const Entry *entry)
{
    if (!entry || entry->session.state == "archived") {
        m_tasks->setContext({}, {}, {}, {}, {}, {}); m_taskInspector->setContext({}, {}, {}, {}); return;
    }
    // Sessions outside every catalog project have no tasks.
    const QString projectId = m_organization.groupFor(entry->identity);
    const auto *project = m_organization.group(projectId);
    const bool real = project && projectId != QLatin1String("ungrouped");
    QList<TaskSession> sessions;
    for (const auto &other : std::as_const(m_entries)) {
        if (other.session.state == "archived" || m_organization.groupFor(other.identity) != projectId) continue;
        TaskSession session{other.host, other.session.name, status(other.session, other.online), {}, true};
        session.conversation = other.session.conversationId;
        session.id = other.session.launchId.isEmpty() ? (other.session.runId.isEmpty() ? QString() : QStringLiteral("run_id:") + other.session.runId)
                                                     : QStringLiteral("launch_id:") + other.session.launchId;
        session.running = other.online && other.session.state == "running" && other.session.processState != "exited";
        session.working = SessionPresentation::working(other.session, other.online);
        // In progress rows show its state as the session list does.
        const bool attention = other.online && other.session.needsAction(), error = other.online && other.session.phase == "error";
        session.kind = attention ? SessionStatusBadge::Attention : error ? SessionStatusBadge::Error
            : session.working ? (session.status == tr("Background") ? SessionStatusBadge::Background : SessionStatusBadge::Working)
            : session.status == tr("Paused") ? SessionStatusBadge::Paused : SessionStatusBadge::Neutral;
        if (session.kind == SessionStatusBadge::Working && session.status != tr("Compacting")) session.status = tr("Working");
        if (!session.running) session.blocker = tr("The agent is not running. Resume it before sending.");
        else if (other.session.needsAction()) session.blocker = tr("agent is waiting for input, approval, or has an unknown state; open Terminal");
        sessions << session;
    }
    m_tasks->setContext(real ? projectId : QString(), real ? project->name : QString(), entry->host, entry->session.name, entry->key, sessions);
    m_taskInspector->setContext(real ? projectId : QString(), entry->host, entry->session.name, sessions);
}

void SessionsWindow::selectCreation(const QString &id)
{
    if(!m_creations->owns(id)||m_creations->operation(id)["state"]=="bound")return;
    if(m_selectedCreation!=id||!m_selectedKey.isEmpty()){
        if(!m_subagentId.isEmpty())closeSubagent();pushSelectionChanged({});
        m_subagentComposer->setFileReferenceTarget({}, {}, {});m_readCandidate.clear();m_readSelectionKey.clear();
        m_selectedCreation=id;m_selectedKey.clear();m_initialSelectionPending=false;
        m_details={};m_events={};m_cursor=0;m_inspectError.clear();
        m_composer->setSessionKey({});m_activityView->setSessionKey({});m_terminal->setSession(m_client.executable(),{},{});
    }
    renderCreation();
}
// U34: the folder a "the DeepSeek sandbox needs full control of FOLDER (missing
// ...)" creation error names; empty for any other error.
QString SessionsWindow::sandboxFolderOf(const QString &error)
{
    static const QString lead=QStringLiteral("the DeepSeek sandbox needs full control of ");
    const int at=error.indexOf(lead);if(at<0)return {};
    const int start=at+lead.size();const int end=error.indexOf(QStringLiteral(" (missing "),start);
    return end>start?error.mid(start,end-start).trimmed():QString();
}
void SessionsWindow::renderCreation()
{
    const auto operation=m_creations->operation(m_selectedCreation);
    if(operation.isEmpty())return;
    m_detailStack->setCurrentWidget(m_creationPanel);
    const auto scale=ContentScale::factor();const auto appearance=QString::number(m_dark)+':'+QString::number(scale);
    if(m_creationText->property("creationAppearance").toString()!=appearance){
        m_creationText->setProperty("creationAppearance",appearance);
        setWorkspaceStyle(m_creationText,QString("QPlainTextEdit{background:%1;color:%2;border:1px solid %3;border-radius:6px;padding:10px;font-size:%4px;}")
            .arg(m_dark?"#151b22":"#ffffff",m_dark?"#e8edf4":"#1a2733",m_dark?"#34414b":"#d7dfe5").arg(ContentScale::px(13,scale)));
        setWorkspaceStyle(m_creationTitle,QString("QLabel{font-size:%1px;font-weight:600;}").arg(ContentScale::px(18,scale)));
    }
    const auto elapsed=qMax<qint64>(0,QDateTime::currentSecsSinceEpoch()-operation["created"].toInteger());
    m_creationTitle->setText(tr("%1 · %2 · %3:%4").arg(operation["tag"].toString(),SessionCreationController::caption(operation))
        .arg(elapsed/60).arg(elapsed%60,2,10,QLatin1Char('0')));
    const auto text=SessionCreationController::details(operation);
    if(m_creationText->toPlainText()!=text){
        const auto scroll=m_creationText->verticalScrollBar()->value();m_creationText->setPlainText(text);
        m_creationText->verticalScrollBar()->setValue(scroll);
    }
    m_creationContinue->setVisible(SessionCreationController::canResume(operation));
    m_creationEdit->setVisible(m_creations->canEdit(m_selectedCreation));
    {
        const bool fixable=operation["host"].toString().isEmpty()&&!sandboxFolderOf(operation["error"].toString()).isEmpty();
        if(!fixable){m_creationFixAccess->setEnabled(true);m_creationFixAccess->setText(tr("Fix folder rights"));}
        m_creationFixAccess->setVisible(fixable);
    }
    const auto state=operation["state"].toString();
    m_creationCheck->setVisible(state=="unknown"||state=="recover"||state=="confirming");
    m_creationStop->setVisible(!operation["launch_dispatched"].toBool()&&state!="stopped"&&state!="failed"&&state!="storage_error");
    m_creationStop->setEnabled(!operation["stop_requested"].toBool());
    m_creationStop->setText((state=="cloning"||(state=="worktree"&&!operation["worktree_receipts"].toBool()))?tr("Stop after preparation"):tr("Stop creating"));
}

void SessionsWindow::renderDetails()
{
    if(!m_selectedCreation.isEmpty()){renderCreation();return;}
    updateWorktreeCleanupOffer();
    updateSessionLoading();
    m_compactCancel->setVisible(m_compacts.contains(m_selectedKey) || (m_startSend.key == m_selectedKey && !m_startSend.key.isEmpty()));
    const auto *entry = selected(); m_detailStack->setCurrentIndex(entry ? 1 : 0);
    updateTasks(entry);
    if (entry && m_inspectorPanel->isVisible() && m_inspector->block(InspectorCards::Context)->isExpanded()) m_accountsPage->ensureCatalogs();   // the account lines sit in the Context card
    if (entry && awaitingRestoredInspection(*entry) && m_inspectError.isEmpty())
        m_inspectError = tr("Session restored. Refreshing its current state…");
    const auto recoveryDetails=entry && entry->session.state != "archived"
        && m_details.value("provider_status_at").toDouble() >= entry->session.providerStatusAt ? m_details : QJsonObject();
    const auto recoveryState=recoveryDetails.value("recovery").toObject().value("state").toString();
    const bool providerFailure=recoveryDetails.value("phase")=="error" && !recoveryDetails.value("provider_error").toObject().isEmpty();
    auto *recoveryFocus=QApplication::focusWidget();
    if(!providerFailure&&(recoveryState.isEmpty()||recoveryState=="succeeded"||RecoveryUi::resumed(recoveryDetails.value("recovery").toObject()))&&m_recovery->isVisible()&&recoveryFocus
        &&(recoveryFocus==m_recovery||m_recovery->isAncestorOf(recoveryFocus)))m_composer->editor()->setFocus(Qt::OtherFocusReason);
    m_recovery->setState(recoveryDetails,entry && entry->online);
    m_markRead->setActive(entry && entry->session.state!="archived" && entryNeedsAttention(*entry));
    m_markRead->setDetail(entry && entry->session.reviewLater
        ? tr("Marked for later. Mark as read to clear this reminder.")
        : tr("Mark this session's current notification as read. This does not answer a pending question."));
    renderAccountUsage();
    updateWorktrees();
    updateWorkingLine(entry);
    const bool recorded=!entry || !entry->online || entry->session.state=="archived" || entry->session.state=="paused" || entry->session.processState=="exited" || !m_inspectError.isEmpty();
    if(ProcessSettings::enabled())m_processes->setSession(entry?entry->host:QString(),entry?entry->session.name:QString(),entry?entry->session.archiveId:QString(),entry?m_details:QJsonObject(),entry&&entry->online,!recorded);
    const auto processData=m_details.value("processes").toObject();
    const int processCount=recorded?0:qMax(processData.value("live_count").toInt(),processData.value("active_count").toInt());
    m_detailTabs->setTabText(m_detailTabs->indexOf(m_processes),processCount>0?tr("Processes (%1)").arg(processCount):tr("Processes"));
    m_contextUsage->setData(entry?m_details.value("session_usage").toObject():QJsonObject(),recorded);
    m_cacheStatus->setData(entry?m_details:QJsonObject(),recorded);
    const auto cacheWarning=entry?CacheStatus::warning(m_details):QString();m_cacheWarning->setText(cacheWarning);m_cacheWarning->setVisible(!cacheWarning.isEmpty());
    m_cacheDetail->setText(CacheStatus::details(m_details,recorded));m_cacheClear->setVisible(!cacheWarning.isEmpty());
    m_cacheClear->setEnabled(entry && entry->online && !awaitingRestoredInspection(*entry) && !m_clearRequest && m_details.value("clear_context_supported").toBool());
    // An offline host's last inspection no longer explains the action.
    const bool reported=entry && entry->online;
    explainContextAction(m_cacheClear,reported?contextActionReason(m_details,"clear"):QString());
    const bool mainContext=m_subagentId.isEmpty();
    m_detailsClear->setVisible(mainContext);
    const bool canClear=mainContext && entry && entry->online && entry->session.state!="archived"
        && !m_clearRequest && !m_composer->isSending(m_selectedKey) && m_inspectError.isEmpty() && clearableContext(m_details);
    m_detailsClear->setEnabled(canClear);
    // A Claude chooser still allows Dismiss and clear.
    const auto clearReason=reported && !m_details.value("clear_context_dismiss").toBool()?contextActionReason(m_details,"clear"):QString();
    explainContextAction(m_detailsClear,clearReason);explainContextAction(m_clear,clearReason);
    m_detailsClear->setText(m_clearRequest?tr("Clearing…"):tr("Clear session"));
    {const bool warmable=mainContext && entry && !entry->session.conversationId.isEmpty() && entry->session.state!="archived";
     const int hours=warmable?CacheKeepalive::hours(entry->host,entry->session):0;
     const QSignalBlocker checkbox(m_keepWarm),slider(m_keepWarmHours);
     m_keepWarm->setChecked(hours>0);m_keepWarmHours->setValue(hours>0?hours:m_keepWarmHours->value());
     m_keepWarmLimit->setText(tr("%n h",nullptr,m_keepWarmHours->value()));
     m_keepWarm->setEnabled(warmable);m_keepWarmHours->setEnabled(warmable && hours>0);
     m_keepWarmHours->setVisible(hours>0);m_keepWarmLimit->setVisible(hours>0);}
    // A live agent clears natively; a stopped one starts fresh instead.
    const bool stopped=entry && (entry->session.state=="paused" || entry->session.state=="stopped");
    m_clear->setVisible(mainContext && !stopped && !(entry && entry->session.state=="archived"));m_clear->setEnabled(canClear);
    m_clear->setAccessibleName(m_clearRequest?tr("Clearing…"):tr("Clear session"));
    m_freshAction->setVisible(mainContext && stopped);m_freshAction->setEnabled(entry && canStartStopped(*entry));
    m_detailsContext->setText(tr("Context: %1").arg(m_contextUsage->text().isEmpty()?tr("Not reported"):m_contextUsage->text()));m_detailsContext->setToolTip(m_contextUsage->toolTip());
    m_subagentContextUsage->setData(entry?m_subagentDetails.value("session_usage").toObject():QJsonObject(),recorded);
    if (!m_subagentId.isEmpty() && (!entry || (m_subagentConversation != m_details.value("conversation_id").toString(entry->session.conversationId))
        || !childRoster(*entry).contains(m_subagentId))) closeSubagent();
    if (!entry) {
        m_hint->clear(); m_hint->setProperty("active", false); m_hint->hide();
        m_composer->setSessionStatus({}); updateSessionFeedback();
        m_detailsIdValue->setText(tr("Not confirmed")); m_detailsIdValue->setProperty("copyValue", QString());
        m_copySessionId->setEnabled(false); m_copyHeaderId->setEnabled(false);
        m_question->setQuestion({}, {}); m_question->hide();
        m_terminal->setAvailable(false, tr("Select a running session."));
        m_composer->setCooperativeInbox(false);
        m_composer->accountChip()->setSession({}, {}, {}, {});
        m_composer->setAvailability(false, tr("Select a session.")); return;
    }
    m_composer->setFileReferenceTarget(m_subagentId.isEmpty() ? entry->key : QString(), entry->host, entry->machine,
        fileReferenceContext(entry->session, m_details));
    // U25: the composer row's Copy session reference, as the session menu offers it.
    m_composer->setSessionReferenceAvailable(entry->online && !m_details.value("run_id").toString(entry->session.runId).isEmpty());
    auto s = entry->session;
    m_composer->setCooperativeInbox(s.cmd=="external");
    if (m_details.value("tracked").toBool() && m_details.value("last_event_at").toDouble() >= s.lastEventAt
        && m_details.value("provider_status_at").toDouble() >= s.providerStatusAt) {
        s.phase = m_details.value("phase").toString(s.phase); s.activity = m_details.value("activity").toString(s.activity);
        s.processState = m_details.value("process_state").toString(s.processState);
        s.conversationState = m_details.value("conversation_state").toString(s.conversationState);
        s.runtimeState = m_details.value("runtime_state").toString(s.runtimeState);
        s.activitySummary = m_details.value("activity_summary").toString(s.activitySummary);
        if (m_details.value("activity_detail").isString()) {
            s.activityDetail = m_details.value("activity_detail").toString();
            s.activityAnswers = QuestionReply::prepared(m_details, QStringLiteral("activity_detail"));
        }
        s.providerError = m_details.value("provider_error").toObject();
        s.currentTool = m_details.value("current_tool").toString(s.currentTool);
        s.toolDetail = m_details.value("tool_detail").toString(s.toolDetail);
    }
    s.cwd = m_details.value("cwd").toString(s.cwd);
    const QString directory = m_subagentId.isEmpty() ? s.cwd : m_subagentCwd;
    m_shell->setEnabled(entry->online && MachinePlatform::absolute(directory, MachinePlatform::of(m_fleet.metricsOf(entry->host), entry->host.isEmpty())));
    // #744: never a stat here: the owner's tray waited 39 s in isDir for a
    // folder on a disk that was deleting large folders.
    m_folderButtonPath = entry->host.isEmpty() ? directory : QString();
    showFolderState(m_fileManager, nullptr, entry->host.isEmpty(), directory);
    s.gitRoot = m_details.value("git_root").toString(s.gitRoot);
    s.gitBranch = m_details.value("git_branch").toString(s.gitBranch);
    s.gitWorktree = m_details.value("git_worktree").toBool(s.gitWorktree);
    s.gitWorktreeName = m_details.value("git_worktree_name").toString(s.gitWorktreeName);
    s.gitCommonDir = m_details.value("git_common_dir").toString(s.gitCommonDir);
    s.gitDetached = m_details.value("git_detached").toBool(s.gitDetached);
    const bool terminating=isTerminating(*entry);
    const QString state = terminating?tr("Terminating…"):status(s, entry->online);
    const auto *project = m_organization.group(m_organization.groupFor(entry->identity));
    m_fullTitle = project ? project->name + " / " + sessionLabel(s) : displayTitle(s); m_title->setToolTip(sessionTooltip({s.name}, 380).html);
    QString meta = entry->machine + QStringLiteral("  /  ") + s.cmd + QStringLiteral("  /  ") + projectContext(s);
    m_fullMeta = projectContext(s);
    // G7: the services this session runs are named in the header, with their
    // start times and the reminder that they outlive it and are not Background
    // work. They never turn the status into Background.
    if (!s.services.isEmpty()) {
        QStringList lines;
        for (const auto &service : s.services) {
            const QString since = service.since > 0 ? QDateTime::fromSecsSinceEpoch(qint64(service.since)).toString(QStringLiteral("d MMM HH:mm")) : tr("unknown");
            lines << tr("%1 · since %2").arg(service.name, since) + (service.command.isEmpty() ? QString() : QStringLiteral("\n") + service.command);
        }
        lines << tr("These outlive the session and are not Background work.");
        m_fullMeta += QStringLiteral("  ·  ") + (s.services.size() == 1 ? tr("1 service") : tr("%2 services").arg(s.services.size()));
        m_meta->setToolTip(lines.join(QLatin1Char('\n')));    } else m_meta->setToolTip(QString());
    m_providerBadge->setValue(s.cmd); m_machineBadge->setValue(entry->machine);
    {const bool cards=WorkspaceSkin::current().view(QStringLiteral("sessions"))==QLatin1String("cards");
     const QString branch=s.gitDetached?QString():s.gitBranch;
     m_branchText->setText(m_branchText->fontMetrics().elidedText(branch,Qt::ElideMiddle,220));m_branchChip->setToolTip(branch);
     m_branchIcon->setPixmap(workspaceIcon("git-branch",QColor(m_dark?"#9aa3ad":"#627082")).pixmap(11,11));
     m_branchChip->setStyleSheet(QStringLiteral("QFrame#sessionBranchChip{border:1px solid %1;border-radius:5px;} QLabel{color:%2;font-size:11px;background:transparent;border:0;}")
         .arg(m_dark?"#2c323b":"#dce2e8",m_dark?"#c4cad1":"#40505f"));
     m_branchChip->setVisible(cards&&!branch.isEmpty());}
    meta += '\n' + (s.cwd.isEmpty() ? s.name : s.cwd);
    if (s.state == "archived" && s.archivedAt > 0) meta += '\n' + tr("Archived %1").arg(QDateTime::fromSecsSinceEpoch(qint64(s.archivedAt)).toLocalTime().toString("d MMM yyyy HH:mm:ss"));
    if (s.state == "archived") {
        const auto reason = m_details.value("completion_reason").toString();
        if (reason == "clean_exit") meta += '\n' + tr("Session closed normally");
        else if (reason == "user_archived") meta += '\n' + tr("Moved to archive");
    }
    // L4: the header tooltip keeps the metadata, the model and the close reason
    // and adds the services below them; they live with the session and are never
    // Background work.
    if (!s.services.isEmpty()) {
        QStringList lines;
        for (const auto &service : s.services) {
            const QString since = service.since > 0 ? QDateTime::fromSecsSinceEpoch(qint64(service.since)).toString(QStringLiteral("d MMM HH:mm")) : tr("unknown");
            lines << tr("%1 · since %2").arg(service.name, since) + (service.command.isEmpty() ? QString() : QStringLiteral("\n") + service.command);
        }
        lines << tr("These outlive the session and are not Background work.");
        meta += QStringLiteral("\n\nServices\n") + lines.join(QLatin1Char('\n'));
    }
    m_meta->setToolTip(sessionTooltip({meta, m_details.value("model").toString(s.model)}, 420).html); updateHeaderText();
    const bool waiting=entry->online && SessionPresentation::waitingIdle(s);
    m_badge->setText(waiting?SessionPresentation::waitingCaption(s):state); const auto color = waiting
        ? QColor(m_dark ? "#edd096" : "#795913")
        : s.muted && (state==tr("Needs approval") || state==tr("Needs input") || state==tr("Sign in")) ? SessionStatusBadge::mutedTone(m_dark)
        : tone(s.state=="running" && s.processState!="exited" && entry->online && s.phase=="error"?QStringLiteral("Error"):state, m_dark);
    // WhiteWh fork: the cards view outlines the status, as its pills are drawn.
    if (WorkspaceSkin::current().view(QStringLiteral("sessions")) == QLatin1String("cards")) {
        QColor edge = color; edge.setAlphaF(.55);
        setWorkspaceStyle(m_badge, QString("color:%1; background:transparent; border:1px solid %2; border-radius:9px; padding:3px 9px; font-size:11px; font-weight:600;")
                              .arg(color.name(), edge.name(QColor::HexArgb)));
    } else
    setWorkspaceStyle(m_badge, QString("color:%1; background:%2; border-radius:5px; padding:4px 8px; font-size:11px; font-weight:600;")
                          .arg(color.name(), m_dark ? "#2b323d" : "#edf1f5"));
    m_badge->setMinimumWidth(state == "Background" ? m_badge->fontMetrics().horizontalAdvance(state) + 16 : 0);
    // U30 (#508): the header pill shows the card's state, its text and glyph,
    // in the state's tone; its tooltip is the reason (see below).
    m_badgeState = {};
    {
        const auto shown = SessionStates::present(SessionStates::facts(s, entry->online, CacheKeepalive::hours(entry->host, s) * 3600.0),
                                                  double(QDateTime::currentSecsSinceEpoch()));
        const auto kind = shown.key == "deps" ? SessionStatusBadge::Deps : shown.key == "build" ? SessionStatusBadge::Build
                        : shown.key == "cooling" ? SessionStatusBadge::Cooling : shown.key == "cold" ? SessionStatusBadge::Cold
                        : shown.key == "stopped" ? SessionStatusBadge::Stopped : SessionStatusBadge::Neutral;
        const bool needsYou = shown.key == "user" && shown.badge == tr("Needs you") && !s.needsAction();
        if ((kind != SessionStatusBadge::Neutral && !s.needsAction() && s.state != "archived") || needsYou) {
            m_badgeState = shown;
            const QColor tone = needsYou ? QColor(m_dark ? "#f0c77b" : "#885400") : SessionStatusBadge::stateTone(kind, m_dark);
            m_badge->setText(shown.badge);
            // The badge's glyph, drawn as the card draws it.
            if (!shown.icon.isEmpty()) {
                const qreal ratio = devicePixelRatioF();
                QPixmap pixmap(QSize(12, 12) * ratio); pixmap.setDevicePixelRatio(ratio); pixmap.fill(Qt::transparent);
                QPainter painter(&pixmap); SessionStatusBadge::glyph(&painter, QPointF(6, 6), 11, shown.icon, tone); painter.end();
                m_badgeGlyph->setPixmap(pixmap);
            }
            m_badgeGlyph->setVisible(!shown.icon.isEmpty());
            QColor edge = tone; edge.setAlphaF(.5);
            QColor fill = tone; fill.setAlphaF(kind == SessionStatusBadge::Deps || kind == SessionStatusBadge::Cold || needsYou ? .11 : 0);
            setWorkspaceStyle(m_badge, QString("color:%1; background:%3; border:1px solid %2; border-radius:9px; padding:3px 9px 3px %4px; font-size:11px; font-weight:600;")
                                  .arg(tone.name(), edge.name(QColor::HexArgb), fill.name(QColor::HexArgb)).arg(shown.icon.isEmpty() ? 9 : 24));
            m_badge->setProperty("sessionState", shown.key);
        } else { m_badge->setProperty("sessionState", shown.key); m_badgeGlyph->hide(); }
    }
    s.model = m_details.value("model").toString(s.model); s.effort = m_details.value("effort").toString(s.effort);
    const QString modelText = s.model + (s.effort.isEmpty() ? QString() : "  " + s.effort);
    // Details render on every poll; elide again only when the text or font changes.
    if (const auto elision = modelText + '\n' + m_model->font().key(); elision != m_modelElision) {
        m_modelElision = elision; m_model->setText(m_model->fontMetrics().elidedText(modelText, Qt::ElideMiddle, 260)); m_model->setToolTip(modelText);
    }
    m_model->setAccessibleName(modelText);
    m_model->setVisible(!modelText.isEmpty() && WorkspaceSkin::current().view(QStringLiteral("composer")) != QLatin1String("compact"));
    const bool archived = s.state == "archived";
    m_forkAction->setEnabled(!terminating && entry->online && !awaitingRestoredInspection(*entry) && !m_pending && m_forkKey.isEmpty() && !m_settingsRequest && !m_composer->isSending(m_selectedKey)
        && m_details.value("fork_supported").toBool());
    m_forkAction->setToolTip(m_details.value("fork_reason").toString(tr("Create an independent conversation with the same context and working folder")));
    const bool answering = std::any_of(m_pendingAnswers.cbegin(), m_pendingAnswers.cend(), [this](const PendingAnswer &p) { return p.key == m_selectedKey; });
    const bool restoredPending = awaitingRestoredInspection(*entry);
    const bool updating = terminating || m_pending || m_settingsRequest || !m_renameKey.isEmpty() || m_composer->isSending(m_selectedKey) || answering || restoredPending;
    const bool occupied = archived && archiveNameOccupied(*entry);
    const bool terminalAvailable=NativeSessionCapabilities::terminal(s,m_details);
    const bool externalLink=s.cmd=="external"&&!ConversationSource::chatUrl(m_details.value("external_url").toString()).isEmpty();
    m_openAction->setVisible(!archived && (s.cmd=="dsh"||terminalAvailable||externalLink));
    const QString openCaption = externalLink?tr("Open originating chat"):s.cmd == "dsh" ? tr("Open native DeepSeek UI in browser") : m_clipboard ? tr("Copy connect command") : tr("Open externally");
    m_openAction->setText(openCaption); m_openAction->setEnabled(entry->online && !updating && !archived && (s.cmd=="dsh"||terminalAvailable||externalLink));
    const QString pauseCaption = updating ? tr("Updating…") : archived ? tr("Restore session") : s.state == "running" ? tr("Pause session") : tr("Resume session");
    m_changeAction->setText(pauseCaption);
    // Codex defers SessionStart after resume; its verified restored conversation can pause too.
    const bool canPause = s.state == "running" && s.resumable && (s.activity == "idle" || m_details.value("auth_required").toBool()) && s.processState != "exited"
        && (m_details.value("expected_id").toString().isEmpty() || m_details.value("resume_message_can_send").toBool());
    m_changeAction->setEnabled(entry->online && !updating && (archived ? (s.resumable && !s.archiveId.isEmpty() && !occupied) :
        ((canPause && NativeSessionCapabilities::action(s,"pause",m_details)) || (s.state != "running" && s.resumable && NativeSessionCapabilities::action(s,"resume",m_details)))));
    // WhiteWh fork: a paused or stopped session also shows Resume in its header.
    m_restore->setVisible(archived || s.state == "paused" || s.state == "stopped"); m_restore->setEnabled(m_changeAction->isEnabled());
    m_restore->setAccessibleName(pauseCaption); m_restore->setIcon(workspaceIcon("play", QColor(m_accent)));
    m_more->setEnabled(entry->online && !updating && (!archived || !s.archiveId.isEmpty()));
    m_renameAction->setEnabled(NativeSessionCapabilities::action(s,"rename",m_details) && entry->online && !updating && !s.name.section('/', 0, 0).isEmpty() && !s.name.section('/', 1, 1).isEmpty() && (!archived || !s.archiveId.isEmpty()));
    m_archiveAction->setVisible(s.cmd != "dsh" && (s.state == "paused" || s.state == "stopped"));
    m_archiveAction->setEnabled(!updating && NativeSessionCapabilities::action(s,"archive",m_details));
    const auto endOperation=NativeSessionCapabilities::endOperation(s,m_details);
    m_forgetAction->setEnabled(!updating && NativeSessionCapabilities::action(s,NativeSessionCapabilities::capability(s,endOperation),m_details));
    m_forgetAction->setToolTip(NativeSessionCapabilities::forgetsBindingOnly(s,m_details) ? tr("Zerus forgets this session. The conversation stays in OpenCode.") : QString());
    m_forgetAction->setText(archived ? tr("Forget archived session…") : endOperation == "terminate" ? tr("Terminate session…") : tr("Forget saved session…"));
    const auto questionError=m_details.value("pending_questions_error").toString();
    QString hint; bool loadingHint = false;
    if (terminating) hint = tr("Waiting for the session to stop. Its native conversation history is kept.");
    else if (!entry->online) hint = tr("Machine unavailable. Showing the last known state; actions will return when it reconnects.");
    else if (archived && !m_details.value("last_restore_error").toString().isEmpty())
        hint = tr("Last restore failed: %1").arg(m_details.value("last_restore_error").toString());
    else if (!m_inspectError.isEmpty()) hint = m_inspectError;
    else if (!questionError.isEmpty()) hint = tr("Could not load agent questions: %1. Check Terminal.").arg(questionError);
    else if (archived && occupied) hint = tr("Another active or saved session uses this name. Restore becomes available when that name is free.");
    else if (archived && !s.resumable) hint = tr("This session is archived. Its conversation history is currently unavailable, so it cannot be restored.");
    else if (archived) hint = tr("This session was archived. Restore explicitly to continue the same conversation; Resume all never starts archived sessions.");
    else if (!s.tracked && s.activity.isEmpty()) hint = tr("Activity is not tracked for this session. Sessions started with HGS 1.9 report their tools and status here.");
    else if (s.state == "paused") hint = tr("Conversation saved. Resume to continue with the same context.");
    else if (s.state == "stopped") hint = tr("Agent stopped. Its saved conversation can be resumed.");
    else if (s.processState == "exited") hint = tr("The agent process exited, but its terminal session still exists. Open the terminal to inspect it.");
    else if (s.conversationState == "ended" && s.activity != "idle") hint = tr("The last conversation ended, but the agent process is still running. Its current activity is unconfirmed; open the terminal to check.");
    // U19: the record keeps its last subagent note for good; it says something
    // only while that ask waits, and only when someone else answers it.
    else if (state == "Needs approval" && s.subagentConfirm == QLatin1String("asked; the team's lead answers"))
        hint = tr("The agent asks to start subagents. Its team's lead answers.");
    else if (state == "Needs approval" || state == "Needs input") hint = tr("Your agent needs you. Open its terminal to respond.");
    else if (s.activity == "busy") hint = tr("Agent is active. Pause becomes available after the response and subagents finish.");
    else if (SessionPresentation::backgroundIdle(s)) hint = (s.backgroundProcesses == 1
        ? tr("The agent has replied, but a command is still running.") : tr("The agent has replied, but %1 commands are still running.").arg(s.backgroundProcesses))
        + QStringLiteral(" ") + (ProcessSettings::enabled() ? tr("Open Processes to follow their output or stop them.") : tr("Enable Settings > Processes to inspect them."))
        + QStringLiteral(" ") + tr("You can send another message.");
    // WhiteWh fork: before the first answer of a read, say it is loading.
    else if (m_selectedLoading && m_details.isEmpty()) { hint = tr("Loading activity…"); loadingHint = true; }
    else if (s.activity != "idle") hint = tr("Waiting for the agent to confirm its state. Open its terminal if it needs attention.");
    else hint = tr("Ready for your next message. Finishing a response does not mark the task complete.");
    m_hint->setText(hint);
    const auto goal = m_details.contains("goal") ? m_details.value("goal").toObject() : s.goal;
    const bool hasGoal = !goal.value("id").toString().isEmpty();
    m_goal->setVisible(hasGoal);
    const auto goalState = goal.value("status").toString();
    const QString goalColor = goalState == "active" ? (m_dark ? "#c4a6ed" : "#7650a5") :
        (goalState == "blocked" || goalState.endsWith("_limited")) ? (m_dark ? "#efc978" : "#92641b") :
        goalState == "complete" ? m_accent : m_muted;
    if (hasGoal) {
        m_goal->setText(tr("%1    %2    Details →").arg(SessionGoal::status(goal), SessionGoal::elapsed(goal)));
        m_goal->setAccessibleName(tr("%1: %2").arg(SessionGoal::status(goal), goal.value("objective").toString()));
        m_goal->setToolTip(sessionTooltip({goal.value("objective").toString(), SessionGoal::tokens(goal), tr("Goal state is independent of the current response.")}, 380).html);
        setWorkspaceStyle(m_goal, QString("QPushButton { text-align:left; color:%1; background:transparent; border:0; border-left:3px solid %1; border-radius:0; padding:7px 10px; } QPushButton:hover { background:%2; }").arg(goalColor, m_dark ? "#29323d" : "#e9eef4"));
    }
    m_badge->setToolTip(sessionTooltip({hint, m_model->text()}, 380).html);
    // U30 §5: the pill's tooltip is the state and its reason.
    if (!m_badgeState.key.isEmpty()) m_badge->setToolTip(sessionTooltip({m_badgeState.tooltip, m_badgeState.reason == m_badgeState.tooltip ? QString() : m_badgeState.reason}, 380).html);
    m_changeAction->setToolTip(sessionTooltip({pauseCaption, !NativeSessionCapabilities::action(s,archived?"restore":s.state=="running"?"pause":"resume",m_details) ? NativeSessionCapabilities::actionReason(s,archived?"restore":s.state=="running"?"pause":"resume",m_details) : canPause
        ? tr("Close the idle agent and save its conversation. Resume starts it again with the same history.") : hint}, 380).html);
    m_restore->setToolTip(m_changeAction->toolTip());
    const bool contextVisible = !entry->online || !m_inspectError.isEmpty() || !questionError.isEmpty()
        || (archived && (occupied || !m_details.value("last_restore_error").toString().isEmpty()))
        || loadingHint;
    const bool contextError = !entry->online || !m_inspectError.isEmpty() || !questionError.isEmpty()
        || (archived && !m_details.value("last_restore_error").toString().isEmpty());
    m_hint->setProperty("active", contextVisible); m_hint->setProperty("error", contextError); m_hint->hide();
    m_nativeSignIn->setVisible(s.cmd == "dsh" && entry->online && m_details.value("auth_required").toBool() && s.runtimeState == "live");
    setWorkspaceStyle(m_nativeSignIn, QString("QWidget#nativeSignInBanner { background:%1; border:1px solid %2; border-radius:8px; } QLabel { color:%3; background:transparent; border:0; }")
        .arg(m_dark ? "#40371e" : "#fff0ba", m_dark ? "#927140" : "#c6a15f", m_dark ? "#ffda76" : "#392900"));
    // hgs reports a stopped OpenCode service with exactly this reason; inspection
    // never starts it, so the session offers the explicit start.
    const auto serviceReason = m_details.value("inspection_reason").toString();
    const bool serviceStopped = s.cmd == "opencode" && m_details.value("inspection_unavailable").toBool()
        && serviceReason.startsWith(QStringLiteral("OpenCode is not running on "));
    m_openCodeService->setVisible(serviceStopped);
    m_openCodeServiceText->setText(serviceReason + (serviceReason.endsWith('.') ? QString() : QStringLiteral(".")));
    m_openCodeServiceStart->setText(m_openCodeStartRequest && m_openCodeStartHost == entry->host ? tr("Starting OpenCode…") : tr("Start OpenCode on %1").arg(entry->machine));
    m_openCodeServiceStart->setEnabled(serviceStopped && entry->online && !m_openCodeStartRequest);
    setWorkspaceStyle(m_openCodeService, QString("QWidget#openCodeServiceBanner { background:%1; border:1px solid %2; border-radius:8px; } QLabel { color:%3; background:transparent; border:0; }")
        .arg(m_dark ? "#40371e" : "#fff0ba", m_dark ? "#927140" : "#c6a15f", m_dark ? "#ffda76" : "#392900"));
    auto activityDetails = m_details;
    if (!entry->online || !m_inspectError.isEmpty() || s.state != "running") activityDetails["runtime_state"] = "unavailable";
    activityDetails["queue_sending"] = m_queueSendRequest && m_queueSendKey == m_selectedKey;
    // One removal at a time: every Remove waits while one runs.
    if (m_removeQueuedRequest) activityDetails["queued_removal_busy"] = true;
    if (m_removeQueuedRequest && m_removeQueuedKey == m_selectedKey) activityDetails["queued_removal"] = m_removeQueuedMessage;
    if (!entry->online || !m_inspectError.isEmpty()) { auto queue=activityDetails["input_queue"].toObject(); queue["can_send_now"]=false; activityDetails["input_queue"]=queue; }
    m_activityView->setTimeline(activityDetails, m_events, m_localMessages.value(m_selectedKey), s.prompt, s.tracked);
    m_composer->setAttachmentAvailability(s.cmd!="opencode" && m_details.value("attachments_supported").toBool(true),
        m_details.value("attachments_reason").toString(tr("This native adapter does not support file attachments. Your existing files remain saved; remove them to send text.")));
    const QString messageProblem = messageBlockReason(*entry);
    m_composer->setSessionStatus(contextVisible ? hint : QString(), contextError,
        contextError && !messageProblem.isEmpty());
    if (!messageProblem.isEmpty() && canStartStopped(*entry))
        m_composer->setAvailability(true, tr("Session stopped. Send asks whether to resume it or start fresh."));
    else m_composer->setAvailability(messageProblem.isEmpty(), messageProblem);
    const auto phase=m_details.value("phase").toString(entry->session.phase);
    const bool working=QStringList{"working","tool","compacting"}.contains(phase)&&entry->session.state=="running";
    restoreInterruptedPrompt();
    // Claude's suggested next message lasts while the session waits for one.
    const auto suggestion = m_details.value("prompt_suggestion").toObject();
    m_composer->setSuggestion(m_selectedKey, phase == "idle" && entry->online && s.state == "running" ? suggestion.value("text").toString() : QString(),
        suggestion.value("at").toDouble());
    m_composer->setInterruptAvailability(working,entry->online&&m_details.value("interrupt_supported").toBool()&&!m_interruptRequest&&!restoredPending,
        m_interruptRequest?tr("Interrupt is being sent…"):!entry->online?tr("Machine is offline"):
        !m_details.value("interrupt_supported").toBool()?tr("Interrupt is unavailable. Open Terminal to stop this turn."):QString());
    const QString settingsProblem = modelSettingsBlockReason(*entry);
    m_composer->setSettingsTerminalAvailable(s.cmd == "claude" && entry->online && s.state == "running" && !restoredPending);
    m_composer->setOpenCodeContext(OpenCodeModels::catalogSupported(m_details), m_details.value("model_options_truncated").toBool(), entry->online, entry->machine,
        entry->host, m_details.value("cwd").toString(entry->session.cwd));
    m_composer->setModelSettings(m_details.value("settings_model").toString(s.model), s.effort, m_details.value("model_options").toArray(), settingsProblem.isEmpty(),
        settingsProblem.isEmpty() ? m_details.value("settings_change_reason").toString() : settingsProblem,
        m_details.value("pending_model").toString(), m_details.value("pending_effort").toString(),
        m_details.value("settings_apply_when").toString(), s.cmd == "opencode");
    m_composer->setSettingsNotice(m_details.value("settings_change_notice").toString());
    updateAccountChip(*entry, archived || restoredPending);
    renderQuestion(*entry);
    updateSessionFeedback();
    m_terminal->setSession(m_client.executable(), entry->host, s.name, m_details.value("run_id").toString(), entry->machine);
    m_nativeUi->setSession(entry->host, s.cmd == "dsh" ? s.name : QString(), m_details.value("run_id").toString(s.runId), entry->machine, s.cmd == "dsh" && entry->online && !archived && !restoredPending);
    const bool terminalTab = terminalAvailable && m_subagentId.isEmpty();
    // Loads the optional plugin on the first DeepSeek session, not at startup.
    const bool nativeTab = s.cmd == "dsh" && m_subagentId.isEmpty() && NativeBrowserPlugin::factory();
    const int terminalIndex = m_detailTabs->indexOf(m_terminal), nativeIndex = m_detailTabs->indexOf(m_nativeUi);
    if (m_detailTabs->isTabVisible(terminalIndex) != terminalTab) m_detailTabs->setTabVisible(terminalIndex, terminalTab);
    if (m_detailTabs->isTabVisible(nativeIndex) != nativeTab) m_detailTabs->setTabVisible(nativeIndex, nativeTab);
    if (!terminalTab && m_detailTabs->currentIndex() == terminalIndex) m_detailTabs->setCurrentIndex(0);
    if (!nativeTab && m_detailTabs->currentIndex() == nativeIndex) m_detailTabs->setCurrentIndex(0);
    m_terminal->setAvailable(entry->online && s.state == "running" && terminalAvailable && !restoredPending,
        !entry->online ? tr("Machine unavailable. Reconnect when it is online.") :
        restoredPending ? tr("Refresh the restored session before opening Terminal.") :
        !terminalAvailable ? NativeSessionCapabilities::terminalReason(s,m_details) : tr("Resume this session before opening its terminal."));
    // A compact roster keeps many agents visible at once. A finished child
    // turn is Ready, never proof that its overall task or process has ended.
    const QString source = m_details.value("subagent_source").toString(s.subagentSource);
    s.subagentSource = source;
    s.subagentCountsComplete = m_details.value("subagent_counts_complete").toBool(s.subagentCountsComplete);
    s.subagentActiveCount = m_details.value("subagent_active_count").toInt(s.subagentActiveCount);
    s.subagentCount = s.subagentActiveCount;
    s.subagentTotalCount = m_details.value("subagent_total_count").toInt(s.subagentTotalCount);
    const auto agents = m_details.value("subagents").toObject();
    QList<QJsonObject> roster;
    const auto groups = m_details.value("subagent_groups").toObject();
    if (source == "hook_profiles" || source == "mixed_hooks") for (auto it = groups.begin(); it != groups.end(); ++it) {
        auto group = it.value().toObject(); group["id"] = "profile:" + it.key(); group["group"] = true;
        if (!group.contains("label")) group["label"] = it.key();
        roster.append(group);
    }
    for (auto it = agents.begin(); source != "hook_profiles" && it != agents.end(); ++it) {
        auto child = it.value().toObject(); child["id"] = it.key(); roster.append(child);
    }
    if (roster.isEmpty()) for (const auto &value : m_details.value("subagent_previews").toArray(s.subagentPreviews)) roster.append(value.toObject());
    const auto retainedRosterSize = roster.size();
    std::stable_sort(roster.begin(), roster.end(), [](const QJsonObject &a, const QJsonObject &b) {
        const bool activeA = childState(a) == "working" || a.value("active_count").toInt() > 0;
        const bool activeB = childState(b) == "working" || b.value("active_count").toInt() > 0;
        if (activeA != activeB) return activeA;
        return a.value("updated").toDouble() > b.value("updated").toDouble();
    });
    const int rosterWidth = qMax(200, m_children->viewport()->width() - 24);
    const int stateWidth = 58, nameWidth = qMax(110, rosterWidth - stateWidth - 12);
    QFont rosterFont = m_children->font(); rosterFont.setPixelSize(13); const QFontMetrics metrics(rosterFont);
    const auto elide = [&](const QString &text, int width) { return htmlText(metrics.elidedText(text.simplified(), Qt::ElideRight, width)); };
    const bool currentChildren = currentActivity(s, entry->online);
    QString children = QString("<style>body{color:%1;font-size:12px;}p{margin:5px 0 9px;}td{padding:7px 0;}h3{font-size:12px;margin:16px 0 8px;}</style>").arg(m_fg);
    // Owner, 2026-10-28 (#648): "Main agent · Ready", the state word only, and
    // the tool it runs now; the latest prompt never shows, nor a summary that
    // is only a state ("Working: <prompt>").
    children += QString("<p style='margin:2px 0 10px;'><span style='color:%1'>⊙</span> <b>%2</b></p>").arg(m_muted, htmlText(tr("Main agent") + QStringLiteral(" · ") + state));
    {const QString tool = m_details.value("activity_summary").toString();
     QString detail = m_details.value("activity_detail").toString().simplified();
     for (const char *key : {"prompt", "last_prompt"})
         if (!detail.isEmpty() && detail == m_details.value(key).toString().simplified()) detail.clear();
     static const QStringList stateWords{"Working", "Ready", "Needs attention", "Stopped", "Status unavailable"};
     if (currentChildren && m_details.value("activity").toString() == "busy" && !tool.isEmpty() && tool != state && !stateWords.contains(tool))
         children += QString("<p style='color:%1'>%2</p>").arg(m_muted, elide(detail.isEmpty() || detail == tool ? tool : tool + QStringLiteral(": ") + detail, rosterWidth));}
    // The agent's own plan as a checklist under it, its count on the card's
    // header; another owner's list (a subagent's, the session's) keeps its name.
    const auto taskLists = m_details.value("task_lists").toObject();
    QStringList taskOwners = taskLists.keys(); taskOwners.removeAll("main");
    if (taskLists.contains("main")) taskOwners.prepend("main");
    for (const auto &owner : taskOwners) {
        const auto list = taskLists.value(owner).toObject(); const auto items = list.value("items").toArray();
        QString ownerLabel = list.value("source").toString().startsWith("Task") ? tr("Session tasks") : tr("Main agent");
        if (owner.startsWith("agent:")) {
            const QString id = owner.mid(6); const auto agent = agents.value(id).toObject();
            ownerLabel = agent.value("label").toString(agent.value("name").toString(id));
        }
        int done = 0; for (const auto &value : items) if (value.toObject().value("status").toString() == "completed") ++done;
        const bool recorded = !currentChildren || list.value("run_id").toString() != m_details.value("run_id").toString();
        if (owner != taskOwners.first() || ownerLabel != tr("Main agent"))
            children += QString("<p style='margin:12px 0 6px;'><b>%1</b><br><span style='color:%2'>%3%4</span></p>").arg(htmlText(ownerLabel), m_muted,
                tr("%1 of %2 completed").arg(done).arg(items.size()), recorded ? tr(" (Last recorded)") : QString());
        else if (recorded) children += QString("<p style='color:%1;font-size:11px'>%2</p>").arg(m_muted, tr("Last recorded"));
        if (items.isEmpty()) children += QString("<p style='color:%1'>%2</p>").arg(m_muted, tr("The agent cleared this list."));
        for (const auto &value : items) {
            const auto task = value.toObject(); const QString taskState = task.value("status").toString();
            const QString marker = taskState == "completed" ? QStringLiteral("✓") : taskState == "in_progress" ? QStringLiteral("◉") : QStringLiteral("○");
            const QString stateLabel = taskState == "completed" ? tr("Done") : taskState == "in_progress" ? tr("In progress") : taskState == "pending" ? tr("Planned") : tr("Unknown");
            const QString color = taskState == "in_progress" && !recorded ? m_accent : m_muted;
            const QString markColor = taskState == "completed" ? tone("Working", m_dark).name() : color;
            children += QString("<table width='100%' cellspacing='0' cellpadding='0' style='margin:0 0 8px;'><tr><td width='22' valign='top' style='color:%1;padding:1px 0'>%2</td>"
                                "<td style='padding:1px 0'><span style='color:%3'>%4</span><br><span style='font-size:11px;color:%5'>%6%7</span></td></tr></table>")
                .arg(markColor, marker, m_fg, htmlText(task.value("title").toString()), color, stateLabel,
                    task.value("owner").toString().isEmpty() ? QString() : " / " + htmlText(task.value("owner").toString()));
        }
        if (list.value("truncated").toBool()) children += QString("<p style='color:%1'>%2</p>").arg(m_muted, tr("Showing the first 100 tasks."));
    }
    if (taskLists.isEmpty()) children += QString("<p style='color:%1'>%2</p>").arg(m_muted, tr("No task list yet"));
    // Owner, 2026-10-28: the agent's background commands (a build, a test run)
    // are its work too: the running ones, newest first. Claude reports a
    // background command's end only in a later prompt; one with no end seen
    // in the last three hours shows as not confirmed.
    {QStringList jobs; int shown = 0;
     const double nowSeconds = QDateTime::currentSecsSinceEpoch();
     for (const auto &value : std::as_const(m_backgroundJobs)) {
         const auto job = value.toObject();
         if (!job.value("background").toBool() || job.value("source").toString() == "process_tree") continue;
         const QString status = job.value("status").toString();
         const double started = job.value("started_at").toDouble();
         const bool running = status == "running" || status == "starting";
         if (!running && !(status == "unknown" && started > 0 && nowSeconds - started < 3 * 3600)) continue;
         if (++shown > 5) continue;
         QString what = job.value("description").toString();
         if (what.isEmpty()) what = job.value("command").toString();
         const int minutes = started > 0 ? int((nowSeconds - started) / 60) : -1;
         const QString age = minutes < 0 ? QString() : minutes < 60 ? tr("%1 min").arg(minutes) : tr("%1 h %2 min").arg(minutes / 60).arg(minutes % 60);
         jobs << QString("<tr><td width='%1'><font color='%2'>●</font> %3<br><span style='color:%4;font-size:11px'>%5</span></td><td valign='top' align='right' width='%6' style='color:%2;font-size:11px'>%7</td></tr>")
                     .arg(nameWidth).arg(tone(running ? "Working" : "Unknown", m_dark).name(), elide(what, nameWidth - 20), m_muted,
                          htmlText(running ? age : tr("%1 · end not reported").arg(age))).arg(stateWidth).arg(running ? tr("Running") : tr("Unconfirmed"));
     }
     if (!jobs.isEmpty()) {
         children += QString("<h3>%1 <span style='color:%2;font-weight:normal'>%3</span></h3>").arg(tr("Background jobs"), m_muted, QString::number(shown));
         children += "<table width='100%' cellspacing='0' cellpadding='0'>" + jobs.join(QString()) + "</table>";
     }}
    if (hasGoal) {
        children += QString("<h3>%1</h3><p style='color:%2'><b>%3</b></p><p>%4</p><p style='color:%5'>%6</p>")
            .arg(tr("Goal"), goalColor, htmlText(SessionGoal::status(goal)), htmlText(goal.value("objective").toString()),
                m_muted, htmlText(SessionGoal::metrics(goal).join('\n')).replace('\n', "<br>"));
        if (!goal.value("completion_criterion").toString().isEmpty()) children += "<p>" + htmlText(goal.value("completion_criterion").toString()) + "</p>";
        if (!goal.value("reason").toString().isEmpty()) children += "<p>" + htmlText(goal.value("reason").toString()) + "</p>";
        if (!entry->online || s.state != "running" || s.processState == "exited")
            children += QString("<p style='color:%1'>%2</p>").arg(m_muted, tr("The agent is not connected here. Goal progress is not confirmed."));
        children += QString("<p style='font-size:11px;color:%1'>%2</p>").arg(m_muted, tr("Reported by the agent. A finished response does not complete this goal."));
    } else if (m_details.value("goal_source") == "unavailable") {
        children += QString("<h3>%1</h3><p style='color:%2'>%3</p>").arg(tr("Goal"), m_muted, tr("Goal data is unavailable for this conversation or agent version."));
    }
    // U25 part 6: the Tasks card's "N of M completed" for the main agent's list.
    {const auto main = taskLists.value(taskOwners.isEmpty() ? QString() : taskOwners.first()).toObject().value("items").toArray();
     int doneMain = 0; for (const auto &value : main) if (value.toObject().value("status").toString() == "completed") ++doneMain;
     m_inspector->setValue(InspectorCards::Tasks, main.isEmpty() ? QString() : tr("%1 of %2 completed").arg(doneMain).arg(main.size()));}
    // #648: no Subagents heading; the active ones, or one muted line.
    m_children->document()->addResource(QTextDocument::ImageResource, QUrl(QStringLiteral("hgs-icon:users")),
        workspaceIcon(QStringLiteral("users"), QColor(m_muted)).pixmap(14, 14));
    const QString people = QStringLiteral("<img src='hgs-icon:users' width='14' height='14' style='vertical-align:middle'>&nbsp;&nbsp;");
    if (source == "hook_profiles")
        children += QString("<p style='margin-top:12px;'>%1<b>%2</b> <span style='color:%3'>%4</span></p>")
            .arg(people, tr("Profile groups"), m_muted, currentChildren ? childCount(s, true) : tr("Last recorded"));
    else if (!currentChildren && !roster.isEmpty())
        children += QString("<p style='margin-top:12px;color:%1'>%2%3</p>").arg(m_muted, people, tr("Subagents") + QStringLiteral(" · ") + tr("Last recorded"));
    if (source == "hook_profiles") children += QString("<p style='color:%1'>%2</p>").arg(m_muted, tr("Observed runs grouped by profile; individual agent identities are not reported."));
    if (roster.isEmpty()) {
        const bool unavailable = source == "unavailable" || !s.tracked;
        const QString emptyHint = unavailable ? tr("Subagent activity is unavailable for this session.") :
            source == "hook_profiles" && !s.subagentCountsComplete ? tr("Exact subagent counts are unavailable for this run.") :
            tr("No subagent activity yet");
        children += QString("<p style='margin-top:12px;color:%1'>%2%3</p>").arg(m_muted, people, emptyHint);
    } else {
        // Owner, 2026-10-28: finished children fold into a count, unless
        // workspace/inspectorFinishedSubagents is set or the owner
        // opens them here; the list is what works now (a profile's groups
        // keep their rows).
        const bool showFinished = m_showFinishedChildren || Settings::value("workspace/inspectorFinishedSubagents", false).toBool();
        int finished = 0;
        QList<QJsonObject> shownRoster;
        for (const auto &child : roster) {
            const bool working = SessionPresentation::childState(child) == "working" || child.value("active_count").toInt() > 0
                                 || child.value("display_state").toString() == "approval";
            if (!showFinished && currentChildren && !working && !child.value("group").toBool()) { ++finished; continue; }
            shownRoster.append(child);
        }
        const QString folded = finished ? QString(" · %1 · <a href='hgs-children:finished' style='color:%2;text-decoration:none'>%3</a>")
            .arg(finished == 1 ? tr("1 finished") : tr("%1 finished").arg(finished), m_accent, tr("show")) : QString();
        if (shownRoster.isEmpty()) children += QString("<p style='margin-top:12px;color:%1'>%2%3%4</p>").arg(m_muted, people, tr("No subagent activity yet"), folded);
        else if (currentChildren && source != "hook_profiles") children += QString("<p style='margin-top:12px;color:%1'>%2%3%4</p>").arg(m_muted, people, htmlText(childCount(s, true)), folded);
        children += "<table width='100%' cellspacing='0' cellpadding='0'>";
        for (const auto &child : shownRoster) {
            QString name = child.value("label").toString();
            if (name.isEmpty()) name = child.value("name").toString();
            if (name.isEmpty()) name = child.value("id").toString();
            if (name.isEmpty()) name = tr("Unnamed agent");
            const QString childState = SessionPresentation::childState(child);
            QString stateText = childStatus(child);
            if (child.value("group").toBool()) {
                if (s.subagentCountsComplete && child.value("active_count").isDouble() && child.value("total_count").isDouble())
                    stateText = QString("%1/%2").arg(child.value("active_count").toInt()).arg(child.value("total_count").toInt());
                else stateText = tr("Unknown");
            }
            QString detail = child.value("detail").toString();
            if (detail.isEmpty()) detail = child.value("task").toString(child.value("description").toString());
            const auto tool = child.value("current_tool").toString();
            if (!tool.isEmpty()) detail = tool + (detail.isEmpty() ? QString() : QStringLiteral(" / ") + detail);
            if (detail.isEmpty()) detail = tr("No action reported");
            if (!currentChildren) stateText = tr("Recorded");
            const QColor childColor = tone(currentChildren && childState == "working" ? "Working" : stateText, m_dark);
            QString title = elide(name, nameWidth - 28);
            const auto id = child.value("id").toString();
            if (!child.value("group").toBool() && agents.contains(id))
                title = QString("<a href='hgs-agent:%1' style='color:%2;text-decoration:none'>%3 →</a>")
                    .arg(QString::fromLatin1(QUrl::toPercentEncoding(id)), m_fg, title);
            children += QString("<tr><td width='%1'><font color='%2'>●</font> <b>%3</b><br><span style='color:%4;font-size:11px'>%5</span></td><td valign='top' align='right' width='%6' style='color:%2;font-size:11px'>%7</td></tr>")
                .arg(nameWidth).arg(childColor.name(), title, m_muted, elide(detail, nameWidth)).arg(stateWidth).arg(htmlText(stateText));
        }
        children += "</table>";
        if (agents.isEmpty() && s.subagentTotalCount > retainedRosterSize) children += QString("<p style='color:%1'>%2</p>").arg(m_muted, tr("%1 recent records available").arg(retainedRosterSize));
    }
    if (!m_subagentId.isEmpty()) {
        const auto child = childRoster(*entry).value(m_subagentId).toObject();
        m_subagentComposer->setFileReferenceTarget(childKey(*entry, m_subagentId), entry->host, entry->machine,
            fileReferenceContext(entry->session, m_details, m_subagentDetails.value("conversation_id").toString()));
        children = QString("<h3>%1</h3><p>%2</p><p>%3</p><h3>%4</h3>")
            .arg(htmlText(childName(child, m_subagentId)), htmlText(childStatus(child)),
                htmlText(child.value("task").toString(child.value("detail").toString())), tr("Tasks"));
        const auto tasks = taskLists.value("agent:" + m_subagentId).toObject().value("items").toArray();
        if (tasks.isEmpty()) children += "<p>" + tr("No task list reported by this agent.") + "</p>";
        for (const auto &item : tasks) {
            const auto task = item.toObject();
            children += QString("<p>%1 %2</p>").arg(task.value("status") == "completed" ? "✓" : "○", htmlText(task.value("title").toString()));
        }
        m_providerBadge->setValue(m_subagentDetails.value("provider").toString(child.value("provider").toString(s.cmd)));
        m_fullTitle = (project ? project->name + " / " + sessionLabel(s) : displayTitle(s)) + " / " + childName(child, m_subagentId); updateHeaderText();
        m_badge->setText(childStatus(child)); m_badge->setToolTip({});
        m_subagentComposer->setAvailability(!isTerminating(*entry) && entry->online && m_subagentDetails.value("send_supported").toBool(), entry->online ? QString() : tr("Machine unavailable. Your draft stays here."));
        m_goal->hide(); m_model->hide(); m_restore->hide(); m_more->hide();
        m_forkAction->setEnabled(false); m_renameAction->setEnabled(false);
    } else m_more->show();
    updateBrowser(m_children, foldInspectorSections(children), m_childrenHtml);
    const auto conversation = m_details.value("conversation_id").toString(s.conversationId);
    const auto run = m_details.value("run_id").toString(s.runId);
    QString identity = conversation.isEmpty() ? run : conversation;
    QString identityTitle = conversation.isEmpty() && !run.isEmpty() ? tr("Run ID") : tr("Session ID");
    QString copyCaption = conversation.isEmpty() && !run.isEmpty() ? tr("Copy run ID") : tr("Copy session ID");
    if (!m_subagentId.isEmpty()) {
        const auto childConversation = m_subagentDetails.value("conversation_id").toString();
        identity = childConversation.isEmpty() ? m_subagentId : childConversation;
        identityTitle = childConversation.isEmpty() ? tr("Agent ID") : tr("Session ID");
        copyCaption = childConversation.isEmpty() ? tr("Copy agent ID") : tr("Copy session ID");
    }
    m_detailsIdTitle->setText(identityTitle);
    const auto identityText = identity.isEmpty() ? tr("Not confirmed") : identity;
    if (m_detailsIdValue->text() != identityText) m_detailsIdValue->setText(identityText);
    m_detailsIdValue->setProperty("copyValue", identity); m_detailsIdValue->setAccessibleName(identityTitle + ": " + identityText);
    m_copySessionId->setEnabled(!identity.isEmpty());
    m_copySessionId->setToolTip(copyCaption); m_copySessionId->setAccessibleName(copyCaption);
    m_copyHeaderId->setEnabled(!identity.isEmpty()); m_copyHeaderId->setToolTip(copyCaption); m_copyHeaderId->setAccessibleName(copyCaption);
    // U25 part 6: the Environment card shows the session, folder, branch,
    // model and last event; this keeps the usage it reports.
    SessionAccounts::Catalog detailsAccounts;
    detailsAccounts.reset(m_sessionAccountDetailsProfiles);
    detailsAccounts.observe(entry->host, entry->machine, s);
    QString info = QString("<h3>%1</h3><p>%2</p>").arg(tr("Account"), htmlText(detailsAccounts.description(entry->host, s)));
    if (!s.accountId.isEmpty()) info += QString("<h3>%1</h3><p>%2</p>").arg(tr("Account profile"), htmlText(s.accountId));
    if (!s.accountHome.isEmpty()) info += QString("<h3>%1</h3><p>%2</p>").arg(tr("Agent home"), htmlText(s.accountHome));
    updateEnvironmentCard();
    if (!m_subagentId.isEmpty()) info = QString("<h3>%1</h3><p>%2</p><h3>%3</h3><p>%4</p><h3>%5</h3><p>%6</p>")
        .arg(tr("Subagent"), htmlText(m_subagentId), tr("Parent session"), htmlText(s.name), tr("Directory"), htmlText(m_subagentCwd));
    info += SessionUsage::html((m_subagentId.isEmpty() ? m_details : m_subagentDetails).value("session_usage").toObject(), m_muted, !m_subagentId.isEmpty());
    const QStringList infoContext{entry->key, m_details.value("conversation_id").toString(s.conversationId), m_subagentId};
    // Another session or conversation starts the inspector at its top.
    if (m_info->property("context").toStringList() != infoContext) m_inspector->verticalScrollBar()->setValue(0);
    auto previousInfo = m_info->property("inspectorHtml").toString();
    updateBrowser(m_info, foldInspectorSections(info), previousInfo, m_info->property("context").toStringList() == infoContext);
    m_info->setProperty("inspectorHtml", previousInfo); m_info->setProperty("context", infoContext);
}

// WhiteWh fork: the skin's foldable inspector. Each <h3> section of an
// inspector page gets a disclosure link; a folded section keeps only its
// heading. Folded headings are remembered by their text.
QString SessionsWindow::foldInspectorSections(const QString &html) const
{
    if (WorkspaceSkin::current().view(QStringLiteral("inspector")) != QLatin1String("foldable")) return html;
    const QStringList folded = Settings::value(QStringLiteral("workspace/foldedInspector")).toStringList();
    const auto parts = html.split(QStringLiteral("<h3>"));
    QString result = parts.value(0);
    static const QRegularExpression tags(QStringLiteral("<[^>]*>"));
    for (int at = 1; at < parts.size(); ++at) {
        const int end = parts[at].indexOf(QStringLiteral("</h3>"));
        if (end < 0) { result += QStringLiteral("<h3>") + parts[at]; continue; }
        const QString heading = parts[at].left(end), body = parts[at].mid(end + 5);
        const QString key = QString(heading).remove(tags).simplified();
        const bool closed = folded.contains(key);
        // A small capitalized title, as the skin's other panels have.
        result += QStringLiteral("<p style='margin-top:14px;margin-bottom:6px;font-size:11px;font-weight:600;'><a href='hgs-fold:%1' style='color:%2;text-decoration:none'>%3&nbsp;&nbsp;%4</a></p>")
            .arg(QString::fromLatin1(QUrl::toPercentEncoding(key)), m_muted, closed ? QStringLiteral("▸") : QStringLiteral("▾"), key.toUpper().toHtmlEscaped());
        if (!closed) result += body;
    }
    return result;
}

void SessionsWindow::toggleInspectorSection(const QString &key)
{
    auto folded = Settings::value(QStringLiteral("workspace/foldedInspector")).toStringList();
    if (!folded.removeAll(key)) folded << key;
    Settings::set(QStringLiteral("workspace/foldedInspector"), folded);
    m_childrenHtml.clear(); m_info->setProperty("inspectorHtml", QString()); renderDetails();
}

void SessionsWindow::updateLeadPanel()
{
    if (!m_leadPanel) return;
    const auto *entry = selected();
    if (!entry || !m_subagentId.isEmpty()) { m_leadPanel->setContext({}, {}, {}, {}); return; }
    const auto project = m_organization.groupFor(entry->identity);
    const auto nameOf = [this](const Entry &e) { return displayTitle(e.session); };
    SessionLeadPanel::Session lead;
    QList<SessionLeadPanel::Session> subordinates, candidates;
    // #705: the links are read once, not twice for every session (each read
    // went through QSettings: about 90 ms with 400 sessions on every render).
    const auto links = SessionLeads::all();
    const auto leadOf = [&links](const QString &identity) { return identity.isEmpty() ? QString() : links.value(identity).toString(); };
    const auto leadId = leadOf(entry->identity);
    for (const auto &other : std::as_const(m_entries)) {
        if (other.identity == entry->identity) continue;
        if (other.identity == leadId) lead = {other.identity, nameOf(other)};
        const auto otherLead = leadOf(other.identity);
        if (otherLead == entry->identity) subordinates.append({other.identity, nameOf(other)});
        else if (otherLead.isEmpty() && m_organization.groupFor(other.identity) == project)
            candidates.append({other.identity, nameOf(other)});
    }
    if (!leadId.isEmpty() && lead.first.isEmpty()) lead = {leadId, tr("a closed session")};
    m_leadPanel->setContext(entry->identity, lead, subordinates, candidates);
}

// WhiteWh fork: opens the lead with this session's latest reply as its draft.
void SessionsWindow::sendReportToLead()
{
    const auto *entry = selected();
    if (!entry) return;
    const auto leadId = SessionLeads::lead(entry->identity);
    const Entry *lead = nullptr;
    for (const auto &other : std::as_const(m_entries)) if (other.identity == leadId) lead = &other;
    if (!lead) { showNotice(tr("The lead session is not open."), true); return; }
    QString reply;
    for (int at = m_events.size(); at-- > 0 && reply.isEmpty();) {
        const auto event = m_events[at].toObject();
        if (event.value("agent_id").toString().isEmpty() && (event.value("type") == "Stop" || event.value("type") == "AgentMessage"))
            reply = event.value("detail").toString().trimmed();
    }
    const QString text = tr("Report from %1:").arg(displayTitle(entry->session)) + (reply.isEmpty() ? QString() : "\n\n" + reply);
    const QString host = lead->host, name = lead->session.name, key = lead->key;
    showSession(host, name);
    if (!m_composer->offerDraft(key, text)) showNotice(tr("The lead already has a draft. The report was not added."), true);
}

// U25: the selected session's project rules, numbered, for the catalog's
// Prompt order.
QString SessionsWindow::teamProjectRules() const
{
    const auto rules=IronRules::rules(projectOfKey(m_selectedKey));
    QStringList lines;for(int i=0;i<rules.size();++i)lines<<QStringLiteral("%1. %2").arg(i+1).arg(rules[i]);
    return lines.join(QLatin1Char('\n'));
}

QString SessionsWindow::projectOfKey(const QString &key) const
{
    for (const auto &entry : std::as_const(m_entries)) if (entry.key == key) return m_organization.groupFor(entry.identity);
    return {};
}

QString SessionsWindow::modelSettingsBlockReason(const Entry &entry) const
{
    if (!entry.online) return tr("Machine unavailable. Reconnect to change model settings.");
    if (m_settingsRequest) return tr("Updating model settings…");
    if (isTerminating(entry) || m_pending || !m_renameKey.isEmpty() || !m_forkKey.isEmpty() || m_composer->isSending(entry.key))
        return tr("Wait for the session update to finish.");
    for (const auto &answer : m_pendingAnswers) if (answer.key == entry.key) return tr("Wait for the answer to be delivered.");
    if (!m_inspectError.isEmpty()) return tr("Refresh the session before changing model settings.");
    if (!m_details.value("settings_change_supported").toBool()) {
        const QString reason = m_details.value("settings_change_reason").toString();
        return reason.isEmpty() ? tr("Model settings are not available for this session.") : reason;
    }
    return {};
}

// U53: the session's account by its label (inspect's account_label; an older
// hgs names none and shows no chip), the same agent's signed-in accounts on
// that computer, and the reason it cannot change now.
void SessionsWindow::updateAccountChip(const Entry &entry, bool unavailable)
{
    auto *chip = m_composer->accountChip();
    const auto &s = entry.session;
    const auto account = m_details.value("account_id").toString(s.accountId);
    chip->setSession(entry.key, s.cmd, account, unavailable ? QString() : m_details.value("account_label").toString());
    // The window's copy of the accounts page's listing; removed profiles are skipped.
    chip->setOptions(AccountSwitch::options(m_sessionAccountDetailsProfiles, entry.host, s.cmd, AccountSwitch::accountOf(s.cmd, account), m_dark));
    // A team member's account changes only with swap-member (U53, U54).
    const auto *team = m_teams ? m_teams->teamOfSession(entry.host, s.name) : nullptr;
    const bool member = team && QStringList{"planning", "running", "paused"}.contains(team->status);
    chip->setLocked(member ? tr("A member of team %1: its account changes with Swap member in the team's Members.").arg(team->name) : QString());
    chip->setBlocked(accountSwitchBlockReason(entry));
}

QString SessionsWindow::accountSwitchBlockReason(const Entry &entry) const
{
    if (!entry.online) return tr("%1 is offline; change the account when it is back.").arg(entry.machine);
    if (entry.session.state != "running") return tr("The session is not running; resume it to change its account.");
    if (isTerminating(entry) || m_pending || !m_renameKey.isEmpty() || !m_forkKey.isEmpty() || m_composer->isSending(entry.key))
        return tr("Wait for the session update to finish.");
    if (!m_inspectError.isEmpty()) return tr("Refresh the session before changing its account.");
    const auto phase = m_details.value("phase").toString(entry.session.phase);
    if (QStringList{"working", "tool", "compacting"}.contains(phase))
        return tr("The agent is working; change its account once it is idle.");
    if (m_details.value("build").isObject()) return tr("A build Zerus runs for the session is still going; wait for it.");
    return {};
}

void SessionsWindow::changeModelSettings(const QString &key, const QString &model, const QString &effort)
{
    const auto *entry = selected();
    if (!entry || entry->key != key || !modelSettingsBlockReason(*entry).isEmpty()) { renderDetails(); return; }
    m_settingsKey = key; m_settingsRun = m_details.value("run_id").toString();
    m_settingsConversation = m_details.value("conversation_id").toString();
    m_settingsHost = entry->host; m_settingsName = entry->session.name; m_settingsAutomatic = false;
    m_settingsRequest = m_client.requestSettings(entry->host, entry->session.name, model, effort, m_settingsRun, m_settingsConversation, {}, entry->session.cmd == "opencode");
    renderDetails();
}

// OpenCode's grouped model picker in the composer. All models reads the session's own
// folder catalog on its computer; a star changes one favorite there, confirmed by hgs.
void SessionsWindow::connectOpenCodeModels()
{
    connect(m_composer, &MessageComposer::openCodeCatalogRequested, this, [this](const QString &key) {
        const auto *entry = selected();
        if (!entry || entry->key != key || entry->session.cmd != "opencode") return;
        if (!entry->online) { m_composer->setOpenCodeCatalogFailed(key, tr("%1 is offline.").arg(entry->machine)); return; }
        const auto cwd = m_details.value("cwd").toString(entry->session.cwd);
        const auto request = m_client.requestOpenCodeModels(entry->host, cwd);
        m_openCodeCatalogRequests.insert(request, {key, entry->host, cwd, m_composer->openCodeGeneration()}); m_openCodeCatalogLatest.insert(key, request);
    });
    connect(&m_client, &HgsClient::openCodeModelsReady, this, [this](quint64 request, const QString &host, const QString &location, const QJsonObject &catalog) {
        const auto key = openCodeCatalogSession(request, host, location);
        if (!key.isEmpty()) m_composer->setOpenCodeCatalog(key, catalog);
    });
    connect(&m_client, &HgsClient::openCodeModelsFailed, this, [this](quint64 request, const QString &host, const QString &location, const QString &error) {
        const auto key = openCodeCatalogSession(request, host, location);
        if (const auto *entry = selected(); !key.isEmpty() && entry) m_composer->setOpenCodeCatalogFailed(key, OpenCodeModels::errorText(error, entry->machine));
    });
    connect(m_composer, &MessageComposer::openCodeFavoriteRequested, this, [this](const QString &key, const QString &value, bool favorite) {
        const auto *entry = selected();
        if (!entry || entry->key != key || entry->session.cmd != "opencode") return;
        if (!entry->online) { m_composer->setOpenCodeFavorite(key, value, !favorite, tr("%1 is offline.").arg(entry->machine)); return; }
        m_openCodeFavoriteRequests.insert(m_client.requestOpenCodeFavorite(entry->host, value, favorite),
                                          {key, entry->host, value, favorite, m_composer->openCodeGeneration()});
    });
    connect(&m_client, &HgsClient::openCodeFavoriteReady, this, [this](quint64 request, const QString &, const QJsonObject &) {
        if (!m_openCodeFavoriteRequests.contains(request)) return;
        const auto change = m_openCodeFavoriteRequests.take(request);
        // A star from a picker context left since is neither shown nor read again.
        if (change.context != m_composer->openCodeGeneration()) return;
        // The composer ignores it unless that session is still shown; that session reads its details again.
        m_composer->setOpenCodeFavorite(change.key, change.value, change.favorite, {});
        if (const auto *entry = selected(); entry && entry->key == change.key && entry->host == change.host) inspect();
    });
    connect(&m_client, &HgsClient::openCodeFavoriteFailed, this, [this](quint64 request, const QString &, const QString &error) {
        if (!m_openCodeFavoriteRequests.contains(request)) return;
        const auto change = m_openCodeFavoriteRequests.take(request);
        if (change.context != m_composer->openCodeGeneration()) return;
        const auto *entry = selected();
        const auto machine = entry && entry->host == change.host ? entry->machine : change.host;
        m_composer->setOpenCodeFavorite(change.key, change.value, !change.favorite, OpenCodeModels::errorText(error, machine));
    });
}

// The session an `account models` answer may be shown for, or empty. Only the newest
// request issued for a session answers it, and only while that session is selected
// with the computer and folder the request was made for, in the same picker context.
QString SessionsWindow::openCodeCatalogSession(quint64 id, const QString &host, const QString &location)
{
    if (!m_openCodeCatalogRequests.contains(id)) return {};
    const auto request = m_openCodeCatalogRequests.take(id);
    const bool newest = m_openCodeCatalogLatest.value(request.key) == id;
    // With nothing of that session pending, no older answer can come any more.
    if (std::none_of(m_openCodeCatalogRequests.cbegin(), m_openCodeCatalogRequests.cend(),
                     [&](const OpenCodeCatalogRequest &pending) { return pending.key == request.key; }))
        m_openCodeCatalogLatest.remove(request.key);
    const auto *entry = selected();
    if (!newest || request.context != m_composer->openCodeGeneration() || host != request.host || location != request.cwd || !entry || entry->key != request.key
        || entry->host != request.host || m_details.value("cwd").toString(entry->session.cwd) != request.cwd) return {};
    return request.key;
}

QByteArray SessionsWindow::settingsIdentity(const QString &key, const PendingSettings &pending)
{
    return QJsonDocument(QJsonArray{key, pending.run, pending.conversation, pending.id, pending.model, pending.effort}).toJson(QJsonDocument::Compact);
}

// The listing's row for a queued change's run and conversation; null without a
// current listing or after either changed.
const SessionInfo *SessionsWindow::listedSettingsSession(const PendingSettings &pending) const
{
    const auto *box = pending.host.isEmpty() ? &m_fleet.local() : m_fleet.peer(pending.host);
    if (!box || !box->ok || (pending.host.isEmpty() && !m_connectionError.isEmpty())) return nullptr;
    for (const auto &session : box->sessions)
        if (session.name == pending.name && session.runId == pending.run
            && (session.conversationId.isEmpty() || session.conversationId == pending.conversation)) return &session;
    return nullptr;
}

static bool listedReady(const SessionInfo &session)
{
    return session.state == "running" && session.phase == "idle" && session.activity == "idle" && session.processState == "running";
}

// Each pending change gets one automatic attempt. One that dispatch scheduled
// gets one more per Ready transition: after the session left Ready, once the
// listing and, for the selected session, inspect both show it Ready again.
// Never turn a failed native change into a polling loop.
bool SessionsWindow::settingsAttemptAllowed(const QString &key, const PendingSettings &pending, const QByteArray &identity) const
{
    if (!m_settingsAttempts.contains(identity)) return true;
    const auto scheduled = m_scheduledSettings.constFind(key);
    if (scheduled == m_scheduledSettings.cend() || scheduled->identity != identity || !scheduled->rearmed) return false;
    const auto *session = listedSettingsSession(pending);
    if (!session || !listedReady(*session)) return false;
    return key != m_selectedKey || (m_details.value("run_id").toString() == pending.run
        && m_details.value("conversation_id").toString() == pending.conversation && m_details.value("settings_apply_when") == "now");
}

void SessionsWindow::requestAutomaticSettings(const QString &key, const PendingSettings &pending, const QByteArray &identity)
{
    m_settingsKey = key; m_settingsHost = pending.host; m_settingsName = pending.name;
    m_settingsRun = pending.run; m_settingsConversation = pending.conversation; m_settingsAutomatic = true;
    m_settingsAttempts.insert(identity); m_settingsIdentity = identity; m_scheduledSettings.remove(key);
    m_settingsRequest = m_client.requestSettings(pending.host, pending.name, pending.model, pending.effort, pending.run, pending.conversation, pending.id);
    renderDetails();
}

void SessionsWindow::applyPendingModelSettings()
{
    const auto *entry = selected();
    if (!entry) return;
    const QString model = m_details.value("pending_model").toString(), effort = m_details.value("pending_effort").toString();
    if (model.isEmpty() && effort.isEmpty()) { m_queuedSettings.remove(entry->key); m_scheduledSettings.remove(entry->key); return; }
    const QString pendingId = m_details.value("pending_settings_id").toString();
    if (pendingId.isEmpty()) return;
    const QString key = entry->key;
    const PendingSettings pending{entry->host, entry->session.name, m_details.value("run_id").toString(),
        m_details.value("conversation_id").toString(), model, effort, pendingId};
    m_queuedSettings[key] = pending;
    const auto identity = settingsIdentity(key, pending);
    if (m_details.value("settings_apply_when") != "now") {
        // Inspect shows the session not Ready, as while a chooser waits.
        const auto scheduled = m_scheduledSettings.find(key);
        if (scheduled != m_scheduledSettings.end() && scheduled->identity == identity) scheduled->rearmed = true;
        return;
    }
    if (modelSettingsBlockReason(*entry).isEmpty() && settingsAttemptAllowed(key, pending, identity))
        requestAutomaticSettings(key, pending, identity);
}

void SessionsWindow::applyQueuedModelSettings()
{
    // The listing shows whether a session whose automatic apply was scheduled
    // left Ready. A change dropped or replaced meanwhile needs no other attempt.
    for (auto it = m_scheduledSettings.begin(); it != m_scheduledSettings.end();) {
        const auto queued = m_queuedSettings.constFind(it.key());
        if (queued == m_queuedSettings.cend() || settingsIdentity(it.key(), *queued) != it->identity) { it = m_scheduledSettings.erase(it); continue; }
        if (const auto *session = listedSettingsSession(*queued); session && !listedReady(*session)) it->rearmed = true;
        ++it;
    }
    if (m_settingsRequest || m_pending || !m_renameKey.isEmpty() || !m_forkKey.isEmpty()) return;
    for (auto it = m_queuedSettings.cbegin(); it != m_queuedSettings.cend(); ++it) {
        const QString key = it.key(); const auto pending = it.value();
        if (pending.id.isEmpty() || m_terminating.contains(key) || m_composer->isSending(key)) continue;
        bool answering = false;
        for (const auto &answer : m_pendingAnswers) answering |= answer.key == key;
        if (answering) continue;
        const auto *session = listedSettingsSession(pending);
        if (!session || !listedReady(*session)) continue;
        const auto identity = settingsIdentity(key, pending);
        if (!settingsAttemptAllowed(key, pending, identity)) continue;
        requestAutomaticSettings(key, pending, identity); return;
    }
}

QString SessionsWindow::messageBlockReason(const Entry &entry) const
{
    if (entry.key == m_startSend.key) return tr("Waiting for the session to start. This exact message will be sent automatically when ready. Cancel send to edit.");
    if (m_compacts.contains(entry.key)) return tr("Waiting for compaction. This exact message will be sent automatically after it completes. Cancel send to edit.");
    if (entry.key == m_clearKey && m_clearRequest) return tr("Wait for the context reset to finish. Your draft stays here.");
    if (isTerminating(entry)) return tr("The session is terminating. Your draft stays here.");
    if (entry.key == m_settingsKey && m_settingsRequest) return tr("Wait for the model settings change to finish.");
    if (!entry.online) return tr("Machine unavailable. Your draft stays here.");
    if (entry.session.state != "running") return tr("Resume the session to send a message.");
    if (m_pending || !m_renameKey.isEmpty()) return tr("Wait for the session update to finish.");
    if(entry.session.cmd=="external") {
        if(!m_inspectError.isEmpty()||m_details.isEmpty())return tr("Refresh the external inbox before queueing an instruction.");
        if(!ConversationSource::cooperative(m_details)||m_details["run_id"].toString()!=entry.session.runId||m_details["conversation_id"].toString()!=entry.session.conversationId)
            return tr("The external inbox identity or supported protocol is not verified.");
        return {};
    }
    if (!QStringList{"codex", "claude", "kimi", "dsh", "opencode"}.contains(entry.session.cmd)) return tr("Use the Terminal tab to type into this session.");
    if (!m_inspectError.isEmpty()) return tr("Refresh the session before sending.");
    if (m_details.isEmpty()) return tr("Checking the session…");
    if (!m_details.value("tracked").toBool()) return tr("This session has no HGS tracking. Use Terminal to send a message.");
    if (m_details.value("run_id").toString().isEmpty()) return tr("Waiting for the agent to finish starting. Open Terminal to check login or setup.");
    if (m_details.value("runtime_state").toString() != "live" || m_details.value("process_state").toString() != "running")
        return tr("The agent is not running. Resume it before sending.");
    if (entry.session.cmd == "dsh" && m_details.value("auth_required").toBool())
        return tr("Sign in to DeepSeek above. Your draft stays here.");
    if (!m_details.value("error").toString().isEmpty()) return entry.session.cmd=="opencode"?tr("Refresh the native session and check OpenCode configuration on this computer before sending."):tr("Check the agent in the Terminal tab before sending.");
    if (!m_details.value("expected_id").toString().isEmpty()) {
        if (m_details.value("resume_message_can_send").toBool()) return {};
        return m_details.value("resume_message_reason").toString(tr("Waiting for the restored conversation. Open Terminal to check startup."));
    }
    // Codex reports SessionStart only with its first turn. The CLI can verify a
    // fresh supervised run's native prompt before a conversation ID exists.
    if (m_details.value("conversation_id").toString().isEmpty()) {
        if (m_details.value("first_message_can_send").toBool()) return {};
        return m_details.value("first_message_reason").toString(tr("Finish login or setup in Terminal. Sending becomes available when the agent is ready."));
    }
    const QString phase = m_details.value("phase").toString();
    if (phase == "approval" || phase == "input") return entry.session.cmd=="opencode"?tr("Answer the pending native request in Activity before sending."):tr("Respond to the agent in the Terminal tab.");
    if (phase == "interrupted") {
        if (m_details.value("interrupted_message_can_send").toBool()) return {};
        return m_details.value("interrupted_message_reason").toString(tr("Waiting for the agent's input prompt after interruption. Open Terminal to check."));
    }
    if (phase == "error" && m_details.value("provider_error").isObject()) {
        if (m_details.value("error_message_can_send").toBool()) return {};
        return m_details.value("error_message_reason").toString(tr("Check the agent in Terminal before retrying."));
    }
    const QString activity = m_details.value("activity").toString();
    const bool ready = activity == "idle" && phase == "idle";
    const bool working = activity == "busy" && QStringList{"idle", "working", "tool", "compacting"}.contains(phase);
    if (!ready && !working) return tr("Waiting for the agent to confirm its input state.");
    return {};
}

bool SessionsWindow::queuedMessageRemovable(const QString &messageId) const
{
    const auto *entry=selected();
    if(!entry||!entry->online||!m_inspectError.isEmpty()||m_removeQueuedRequest||entry->session.cmd!="opencode"
        ||m_details.value("run_id").toString().isEmpty()||!NativeSessionCapabilities::action(entry->session,"remove_queued",m_details))return false;
    return std::any_of(m_events.cbegin(),m_events.cend(),[&messageId](const QJsonValue &value){
        const auto event=value.toObject();
        return event.value("message_id").toString()==messageId&&event.value("delivery_state")=="not_delivered";
    });
}

// Removing an OpenCode inbox message is destructive: it is confirmed, and sent
// only while the same conversation still reports that message as not delivered.
void SessionsWindow::removeQueuedMessage(const QString &messageId)
{
    if(messageId.isEmpty()||!queuedMessageRemovable(messageId)||findChild<QMessageBox *>("removeQueuedConfirm"))return;
    const auto *entry=selected();
    const auto key=m_selectedKey,host=entry->host,name=entry->session.name;
    const auto run=m_details.value("run_id").toString(),conversation=m_details.value("conversation_id").toString();
    QMessageBox dialog(QMessageBox::Warning,tr("Remove queued message"),
        tr("Remove this queued message? It is still waiting in OpenCode's queue and runs first with your next message unless you remove it."),QMessageBox::NoButton,this);
    dialog.setObjectName("removeQueuedConfirm");dialog.setTextFormat(Qt::PlainText);
    auto *confirm=dialog.addButton(tr("Remove"),QMessageBox::DestructiveRole);
    auto *cancel=dialog.addButton(QMessageBox::Cancel);dialog.setDefaultButton(cancel);dialog.setEscapeButton(cancel);
    dialog.exec();
    if(dialog.clickedButton()!=confirm)return;
    entry=selected();
    if(!entry||m_selectedKey!=key||entry->host!=host||entry->session.name!=name||m_details.value("run_id")!=run
        ||m_details.value("conversation_id")!=conversation||!queuedMessageRemovable(messageId)){
        showNotice(tr("The queued message changed. Check Activity before removing it."),true);return;
    }
    m_removeQueuedKey=key;m_removeQueuedMessage=messageId;m_removeQueuedName=name;
    m_removeQueuedRequest=m_client.requestRemoveQueued(host,name,run,conversation,messageId);
    renderDetails();
}

void SessionsWindow::messageAction(const QString &id,const QString &action)
{
    if(action=="remove_queued"){removeQueuedMessage(id);return;}
    const auto *entry=selected();if(!entry)return;
    auto &messages=m_localMessages[m_selectedKey];
    for(int i=0;i<messages.size();++i) {
        const auto message=messages[i].toObject();
        if(message.value("id").toString()!=id || message.value("status")!="error")continue;
        if(action=="delete"){messages.removeAt(i);renderDetails();return;}
        if(action=="inspect"){
            if(entry->session.cmd=="dsh")showNativeUi(*entry);
            else if(NativeSessionCapabilities::terminal(entry->session,m_details)){m_detailTabs->setCurrentWidget(m_terminal);m_terminal->connectSession();}
            else {inspect();showNotice(tr("Refreshing the native session. Check its provider configuration on %1 if the error remains.").arg(entry->machine));}
            return;
        }
        if(action!="retry" || message.value("uncertain").toBool() || m_composer->isSending(m_selectedKey))return;
        if(message.value("run_id")!=m_details.value("run_id") || message.value("conversation_id")!=m_details.value("conversation_id"))return;
        const auto reason=messageBlockReason(*entry);if(!reason.isEmpty()){showNotice(reason,true);return;}
        const QString key = m_selectedKey;
        if (m_retryReads.contains(key)) return;
        const Entry target = *entry;
        const QString run = message.value("run_id").toString();
        const QString conversation = message.value("conversation_id").toString();
        const auto cacheEvidence = sendEvidence(m_details);
        m_retryReads.insert(key);
        beginSendEvidence(key, cacheEvidence);
        AttachmentFiles::readAsync(message.value("attachments").toArray(), this,
            [this, key, target, message, id, run, conversation, cacheEvidence](const QList<MessageAttachment> &files, const QString &error) {
                m_retryReads.remove(key);
                const auto evidence = m_pendingSendEvidence.value(key, cacheEvidence);
                finishSendEvidence(key);
                if (!error.isEmpty()) { showNotice(error, true); return; }
                const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &value) { return value.key == key; });
                if (current == m_entries.cend() || current->host != target.host || current->session.name != target.session.name
                    || current->session.created != target.session.created || m_composer->isSending(key)
                    || !observedIdentityMatches(current->session, target.session, run, conversation)
                    || !current->online || isTerminating(*current)
                    || evidence.value("run_id").toString() != run || evidence.value("conversation_id").toString() != conversation
                    || evidence.value("runtime_state") != "live" || evidence.value("process_state") != "running"
                    || m_pending || !m_renameKey.isEmpty() || m_startSend.key == key || m_compacts.contains(key)
                    || (key == m_clearKey && m_clearRequest) || (key == m_settingsKey && m_settingsRequest)) return;
                bool retained = false;
                for (const auto &value : m_localMessages.value(key)) if (value.toObject() == message) { retained = true; break; }
                if (!retained) return;
                if (key == m_selectedKey) {
                    if (m_details.value("run_id").toString() != run || m_details.value("conversation_id").toString() != conversation) return;
                    sendMessage(key, message.value("text").toString(), files, id);
                } else {
                    // An accepted warm retry stays with its original session.
                    // A cold retry still needs the foreground consent dialog.
                    if (CacheStatus::expired(cacheEvidence) || CacheStatus::expired(evidence)) return;
                    deliverMessage(key, target.host, target.session.name, run, conversation,
                                   message.value("text").toString(), files, id, {}, false, evidence);
                }
            });
        return;
    }
}

void SessionsWindow::cancelCompactContinuation(const QString &key, const QString &reason)
{
    m_waitingPreparations.remove(key);
    if (!m_compacts.remove(key)) return;
    if (m_compacts.isEmpty() && m_startSend.key.isEmpty()) m_compactTimer.stop();
    m_composer->deliveryFinished(key,false,reason);
}

void SessionsWindow::compactToSend(const QString &key, const QString &host, const QString &name, const QString &run, const QString &conversation,
                                 const QString &text, const QList<MessageAttachment> &attachments, const QString &retryId)
{
    if (m_compacts.contains(key) || !m_composer->draftMatches(key, text, attachments)) return;
    CompactContinuation pending;
    pending.key=key; pending.host=host; pending.name=name; pending.run=run; pending.conversation=conversation;
    pending.text=text; pending.attachments=attachments; pending.retryId=retryId;
    pending.started=QDateTime::currentMSecsSinceEpoch();
    m_compacts.insert(key,pending); m_compactTimer.start();
    const auto serial = ++m_waitingPreparationSerial; m_waitingPreparations.insert(key, serial);
    beginSendEvidence(key, key == m_selectedKey ? m_details : QJsonObject());
    m_composer->prepareWaitingToSendAsync(key,
        tr("Waiting for compaction. This exact message will be sent automatically after it completes. Cancel send to edit."),
        [this, key, host, name, run, conversation, serial](bool okay) {
            const auto evidence = key == m_selectedKey ? sendEvidence(m_details) : m_pendingSendEvidence.value(key);
            finishSendEvidence(key);
            if (m_waitingPreparations.value(key) != serial || !m_compacts.contains(key)) return;
            m_waitingPreparations.remove(key);
            const auto entry = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &value) { return value.key == key; });
            if (!okay || entry == m_entries.cend() || entry->host != host || entry->session.name != name
                || !entry->online || isTerminating(*entry) || m_pending || !m_renameKey.isEmpty()
                || (!evidence.isEmpty() && (evidence.value("run_id").toString() != run || evidence.value("conversation_id").toString() != conversation))
                || (key == m_selectedKey && (m_details.value("run_id").toString() != run || m_details.value("conversation_id").toString() != conversation))) {
                cancelCompactContinuation(key, tr("Compaction was cancelled. Your draft was kept without sending.")); return;
            }
            m_compacts[key].request = m_client.requestCompactContext(host, name, run, conversation);
            if (key == m_selectedKey) renderDetails();
        });
}

void SessionsWindow::pollCompactContinuations()
{
    if (!m_startSend.key.isEmpty()) {
        if (QDateTime::currentMSecsSinceEpoch()-m_startSend.started > 180000)
            cancelStartContinuation(tr("The agent did not become ready. Check Terminal; your draft was kept without sending."));
        else {
            const auto *box = m_startSend.host.isEmpty() ? &m_fleet.local() : m_fleet.peer(m_startSend.host);
            if (box && box->ok && (!m_startSend.host.isEmpty() || m_connectionError.isEmpty()))
                m_client.requestInspection(m_startSend.host,m_startSend.name,m_startSend.key == m_selectedKey ? m_cursor : 0,{},false);
        }
    }
    for (const auto &key : m_compacts.keys()) {
        const auto pending = m_compacts.value(key);
        if (QDateTime::currentMSecsSinceEpoch()-pending.started > 600000) {
            cancelCompactContinuation(key, tr("Compaction was not confirmed. Check Terminal; your draft was kept without sending."));
            continue;
        }
        if (pending.request || pending.commandId.isEmpty()) continue;
        const auto *box = pending.host.isEmpty() ? &m_fleet.local() : m_fleet.peer(pending.host);
        if (!box || !box->ok || (pending.host.isEmpty() && !m_connectionError.isEmpty())) continue;
        // This poll belongs to the accepted send, even when its row is hidden
        // by a filter, another page is open, or the workspace is closed.
        m_client.requestInspection(pending.host, pending.name, key == m_selectedKey ? m_cursor : 0, {}, false);
    }
}

void SessionsWindow::continueAfterCompact(const QString &host, const QString &name, const QJsonObject &data)
{
    for (const auto &key : m_compacts.keys()) {
        if (m_waitingPreparations.contains(key)) continue;
        const auto pending = m_compacts.value(key);
        if (pending.host != host || pending.name != name) continue;
        if (data.value("run_id") != pending.run || data.value("conversation_id") != pending.conversation) {
            cancelCompactContinuation(key, tr("Conversation changed. Your draft was kept without sending.")); return;
        }
        if (QDateTime::currentMSecsSinceEpoch()-pending.started > 600000) {
            cancelCompactContinuation(key, tr("Compaction was not confirmed. Check Terminal; your draft was kept without sending.")); return;
        }
        if (!m_composer->draftMatches(key,pending.text,pending.attachments)) {
            cancelCompactContinuation(key, tr("Draft changed. Send it when you are ready.")); return;
        }
        if (pending.request || pending.commandId.isEmpty()) return;
        const auto result=data.value("compact_context_request").toObject();
        if (result.value("request_id") != pending.commandId) return;
        const auto status=result.value("status").toString();
        if (status=="failed" || status=="cancelled" || status=="unchanged" || status=="uncertain") {
            cancelCompactContinuation(key, status=="unchanged" ? tr("No history could be compacted. Your draft was kept without sending.")
                : tr("Compaction did not complete. Check Terminal; your draft was kept without sending.")); return;
        }
        if (status!="completed" || data.value("phase")!="idle" || data.value("activity")!="idle"
            || data.value("runtime_state")!="live" || data.value("process_state")!="running"
            || !data.value("tracked").toBool() || !data.value("error").toString().isEmpty()
            || !data.value("expected_id").toString().isEmpty() || m_terminating.contains(key)) return;
        // Remove the intent before delivery: duplicate inspection signals must
        // never submit it twice. The CLI also verifies the compaction UUID.
        m_compacts.remove(key);
        if (m_compacts.isEmpty() && m_startSend.key.isEmpty()) m_compactTimer.stop();
        deliverMessage(key,host,name,pending.run,pending.conversation,pending.text,pending.attachments,pending.retryId,pending.commandId,true,data);
        return;
    }
}

// Explicit only: hgs starts the shared service when none is registered and alive.
void SessionsWindow::startOpenCodeService()
{
    const auto *entry = selected();
    if (!entry || !entry->online || m_openCodeStartRequest || entry->session.cmd != "opencode" || !m_openCodeServiceStart->isEnabled()) return;
    m_openCodeStartMachine = entry->machine; m_openCodeStartHost = entry->host;
    m_openCodeStartRequest = m_client.requestAccounts(entry->host, {"start", "native-opencode", "--json"});
    renderDetails();
}

void SessionsWindow::clearContext()
{
    const auto *entry=selected();
    // A Claude chooser hides the native composer: clearing first dismisses it.
    const bool dismiss=!m_details.value("clear_context_supported").toBool();
    if(!entry || !entry->online || m_clearRequest || m_composer->isSending(m_selectedKey)
        || (dismiss && !m_details.value("clear_context_dismiss").toBool()))return;
    if(findChild<QMessageBox *>("clearSessionConfirm"))return;
    const auto key=m_selectedKey,host=entry->host,name=entry->session.name;
    const auto run=m_details.value("run_id").toString(),conversation=m_details.value("conversation_id").toString();
    if(run.isEmpty() || conversation.isEmpty() || !m_inspectError.isEmpty())return;
    const QString text=dismiss
        ? tr("Claude in %1 on %2 is waiting for a choice in Terminal. Dismiss it and clear the conversation context?\n\n"
             "This presses Esc once, as in Terminal: a pending approval is denied and a question stays unanswered. "
             "Then the agent’s native clear command runs. Your unsent draft will be kept.")
        : tr("Clear the conversation context in %1 on %2?\n\nThis runs the agent’s native clear command. Future messages will start without the current conversation context. Your unsent draft will be kept.");
    QMessageBox dialog(QMessageBox::Warning,tr("Clear session?"),text.arg(name,entry->machine),QMessageBox::NoButton,this);
    dialog.setObjectName("clearSessionConfirm");dialog.setTextFormat(Qt::PlainText);
    auto *confirm=dialog.addButton(dismiss?tr("Dismiss and clear"):tr("Clear session"),QMessageBox::DestructiveRole);
    auto *cancel=dialog.addButton(QMessageBox::Cancel);dialog.setDefaultButton(cancel);dialog.setEscapeButton(cancel);
    dialog.exec();
    if(dialog.clickedButton()!=confirm)return;
    entry=selected();
    // U1: the fleet polls while the dialog is open; a changed session finishes
    // this action as failed where the owner sees it instead of doing nothing.
    if(!entry || !entry->online || m_selectedKey!=key || entry->host!=host || entry->session.name!=name
        || m_details.value("run_id")!=run || m_details.value("conversation_id")!=conversation
        || m_clearRequest || m_composer->isSending(key) || !m_inspectError.isEmpty()
        || m_details.value("clear_context_supported").toBool()==dismiss
        || (dismiss && !m_details.value("clear_context_dismiss").toBool())) {
        showNotice(tr("The session changed. Open its context actions again."), true); return;
    }
    m_clearKey=key;
    if(m_detailTabs->currentIndex()==0)m_composer->editor()->setFocus(Qt::OtherFocusReason);
    m_clearRequest=m_client.requestClearContext(host,name,run,conversation,dismiss);
    renderDetails();
}

bool SessionsWindow::canStartStopped(const Entry &entry) const
{
    const auto &s = entry.session;
    // U1: a stopped dsh session gets the same one-click start-and-send as the
    // agents that own a resumable conversation.
    return entry.online && (s.state == "paused" || s.state == "stopped") && s.tracked
        && QStringList{"codex", "claude", "kimi", "dsh"}.contains(s.cmd) && !isTerminating(entry)
        && !m_pending && !m_settingsRequest && m_renameKey.isEmpty() && m_startSend.key.isEmpty()
        && !m_compacts.contains(entry.key) && !m_composer->isSending(entry.key);
}

void SessionsWindow::startFresh()
{
    const auto *entry = selected();
    if (!entry || !canStartStopped(*entry) || findChild<QMessageBox *>("startFreshConfirm")) return;
    const Entry original = *entry;
    QMessageBox dialog(QMessageBox::Warning, tr("Start fresh?"),
        tr("Start %1 on %2 with a new conversation?\n\nThe agent starts in the same folder with the same account and model, without the current conversation context. Earlier messages stay readable in Activity and in the agent’s native history, but Resume will no longer continue them.")
            .arg(original.session.name, original.machine), QMessageBox::NoButton, this);
    dialog.setObjectName("startFreshConfirm"); dialog.setTextFormat(Qt::PlainText);
    auto *confirm = dialog.addButton(tr("Start fresh"), QMessageBox::DestructiveRole);
    auto *cancel = dialog.addButton(QMessageBox::Cancel); dialog.setDefaultButton(cancel); dialog.setEscapeButton(cancel);
    dialog.exec();
    if (dialog.clickedButton() != confirm || !LocalSessionHost::confirmLaunch(this, m_fleet.local(), original.host)) return;
    entry = selected();
    if (!entry || entry->key != original.key || entry->session.runId != original.session.runId
        || entry->session.conversationId != original.session.conversationId || !canStartStopped(*entry)) {
        showNotice(tr("The session changed. Open its actions again."), true); return;
    }
    m_pending = true; m_restoreKey.clear();
    m_client.startFreshSession(entry->host, entry->session.name, entry->session.runId);
    showNotice(tr("Starting %1 with a new conversation…").arg(entry->session.name)); renderDetails();
}

void SessionsWindow::startToSend(const QString &key, const QString &text, const QList<MessageAttachment> &attachments)
{
    const auto *entry = selected();
    if (!entry || entry->key != key || !canStartStopped(*entry) || findChild<QMessageBox *>("startToSendConfirm")) return;
    const Entry original = *entry;
    const QString context = m_contextUsage->text();
    QMessageBox dialog(QMessageBox::Question, tr("Start the session to send?"),
        tr("%1 is stopped. How should it start before your message is sent?").arg(original.session.name), QMessageBox::NoButton, this);
    dialog.setObjectName("startToSendConfirm"); dialog.setTextFormat(Qt::PlainText);
    dialog.setInformativeText(tr("Resume continues the saved conversation, and the message is sent with its full context%1.\n\n"
        "Start fresh begins a new conversation with the same folder, account and model. Earlier messages stay readable in Activity.")
        .arg(context.isEmpty() ? QString() : tr(" (%1)").arg(context)));
    dialog.setInformativeText(dialog.informativeText()+tr("\n\nResume, compact and send summarizes the saved history first. Compaction uses tokens and may omit details."));
    auto *resume = dialog.addButton(tr("Resume and send"), QMessageBox::AcceptRole);
    resume->setObjectName("resumeAndSend"); resume->setEnabled(original.session.resumable);
    auto *compact = dialog.addButton(tr("Resume, compact and send"), QMessageBox::ActionRole);
    compact->setObjectName("resumeCompactAndSend"); compact->setEnabled(original.session.resumable);
    compact->setToolTip(tr("Resume the saved conversation, run native /compact when ready, then send this exact message after successful completion."));
    auto *fresh = dialog.addButton(tr("Start fresh and send"), QMessageBox::ActionRole); fresh->setObjectName("startFreshAndSend");
    auto *cancel = dialog.addButton(QMessageBox::Cancel); dialog.setDefaultButton(cancel); dialog.setEscapeButton(cancel);
    dialog.exec();
    const bool startFresh = dialog.clickedButton() == fresh;
    const bool compactFirst = dialog.clickedButton() == compact;
    if (!startFresh && !compactFirst && dialog.clickedButton() != resume) return;
    if (!LocalSessionHost::confirmLaunch(this, m_fleet.local(), original.host)) return;
    entry = selected();
    if (!entry || entry->key != key || entry->session.runId != original.session.runId
        || entry->session.conversationId != original.session.conversationId || !canStartStopped(*entry)) {
        showNotice(tr("The session changed. Your draft was kept without sending."), true); return;
    }
    if (!m_composer->draftMatches(key, text, attachments)) { showNotice(tr("Draft changed. Send it when you are ready.")); return; }
    m_startSend = {key, entry->host, entry->session.name, entry->session.runId, entry->session.conversationId,
        text, attachments, startFresh, compactFirst, QDateTime::currentMSecsSinceEpoch()};
    m_compactTimer.start();
    const Entry target = *entry;
    const auto serial = ++m_waitingPreparationSerial; m_waitingPreparations.insert(key, serial);
    beginSendEvidence(key, m_details);
    m_composer->prepareWaitingToSendAsync(key, compactFirst
        ? tr("Waiting for the session to start, then compact. This exact message will be sent automatically after compaction. Cancel send to edit.")
        : tr("Waiting for the session to start. This exact message will be sent automatically when ready. Cancel send to edit."),
        [this, key, target, startFresh, serial](bool okay) {
            const auto evidence = key == m_selectedKey ? sendEvidence(m_details) : m_pendingSendEvidence.value(key);
            finishSendEvidence(key);
            if (m_waitingPreparations.value(key) != serial || m_startSend.key != key) return;
            m_waitingPreparations.remove(key);
            const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &value) { return value.key == key; });
            if (!okay || current == m_entries.cend() || current->host != target.host || current->session.name != target.session.name
                || current->session.created != target.session.created || current->session.runId != target.session.runId
                || current->session.conversationId != target.session.conversationId || !current->online || isTerminating(*current)
                || (!evidence.isEmpty() && (evidence.value("run_id").toString() != target.session.runId
                    || evidence.value("conversation_id").toString() != target.session.conversationId))
                || current->session.state == "running" || m_pending || !m_renameKey.isEmpty()) {
                cancelStartContinuation(tr("Starting was cancelled. Your draft was kept without sending.")); return;
            }
            m_pending = true; m_restoreKey.clear();
            if (startFresh) m_client.startFreshSession(target.host, target.session.name, target.session.runId);
            else m_client.resumeSession(target.host, target.session.name);
            renderDetails();
        });
    renderDetails();
}

void SessionsWindow::resumeBehind(const QString &name, const QString &detail)
{
    // U3: the transcript is behind the events; the dialog names both times and
    // offers the lost dialogue (default), accepting it as it is, or cancelling.
    const auto number = [&detail](const char *key) {
        const QRegularExpression re(QStringLiteral("\"%1\"\\s*:\\s*([0-9.]+)").arg(QLatin1String(key)));
        const auto match = re.match(detail);
        return match.hasMatch() ? match.captured(1).toDouble() : 0.0;
    };
    const double transcriptAt = number("transcript_at"), eventsAt = number("events_at");
    const auto when = [](double seconds) {
        return seconds > 0 ? QDateTime::fromSecsSinceEpoch(qint64(seconds)).toString(QStringLiteral("d MMM HH:mm"))
                           : tr("an unknown time");
    };
    const QRegularExpression missingRe(QStringLiteral("\"missing\"\\s*:\\s*([0-9]+)"));
    const auto missingMatch = missingRe.match(detail);
    const QString missing = missingMatch.hasMatch() ? tr(" (%1 entries missing)").arg(missingMatch.captured(1)) : QString();
    // The refusal may be plain text ("stops 14400 s before the events Zerus
    // recorded") without the JSON times; the lag still names the gap.
    const QRegularExpression lagRe(QStringLiteral("stops\\s+(\\d+)\\s*s\\s+before"));
    const auto lagMatch = lagRe.match(detail);
    const qint64 lag = lagMatch.hasMatch() ? lagMatch.captured(1).toLongLong() : 0;
    QString lagText;
    if (lag > 0) {
        const qint64 minutes = lag / 60;
        lagText = minutes >= 60 ? tr("%1 h").arg(minutes / 60) : tr("%1 min").arg(qMax<qint64>(1, minutes));
    }
    QString behindText;
    if (transcriptAt > 0 && eventsAt > 0)
        behindText = tr("Claude's transcript stops at %1, but Zerus recorded the conversation up to %2%3.\n\n"
                  "Resuming as it is restores the older conversation and leaves that recorded dialogue out.")
            .arg(when(transcriptAt), when(eventsAt), missing);
    else if (lag > 0)
        behindText = tr("Claude's transcript stops %1 before the events Zerus recorded%2.\n\n"
                  "Resuming as it is restores the older conversation and leaves that recorded dialogue out.")
            .arg(lagText, missing);
    else
        behindText = tr("Claude's transcript is behind the events Zerus recorded%1.\n\n"
                  "Resuming as it is restores the older conversation and leaves that recorded dialogue out.").arg(missing);
    // The dialog is modeless: a refused resume must not block the window (or the
    // tests) in a nested loop; its answer runs in the finished handler.
    auto *dialog = new QMessageBox(QMessageBox::Warning, tr("The transcript is behind"), behindText, QMessageBox::NoButton, this);
    dialog->setObjectName("resumeBehindConfirm"); dialog->setTextFormat(Qt::PlainText);
    auto *lost = dialog->addButton(tr("Resume with the lost dialogue"), QMessageBox::AcceptRole);
    lost->setObjectName("resumeLostDialogue");
    auto *asIs = dialog->addButton(tr("Resume as it is"), QMessageBox::ActionRole);
    asIs->setObjectName("resumeAsIs");
    auto *cancel = dialog->addButton(QMessageBox::Cancel);
    dialog->setDefaultButton(lost); dialog->setEscapeButton(cancel);
    connect(dialog, &QDialog::finished, this, [this, dialog, name] {
        const auto entry = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &e) { return e.session.name == name; });
        const QString host = entry == m_entries.cend() ? QString() : entry->host;
        const auto *clicked = dialog->clickedButton();
        if (clicked && clicked->objectName() == "resumeLostDialogue") { m_pending = true; m_client.resumeSession(host, name, true); }
        else if (clicked && clicked->objectName() == "resumeAsIs") { m_pending = true; m_client.resumeSession(host, name, false, true); }
        dialog->deleteLater();
    });
    dialog->open();
}

void SessionsWindow::cancelStartContinuation(const QString &reason)
{
    if (m_startSend.key.isEmpty()) return;
    const auto key = m_startSend.key; m_startSend = {}; m_waitingPreparations.remove(key);
    if (m_compacts.isEmpty()) m_compactTimer.stop();
    m_composer->deliveryFinished(key, false, reason);
}

// Send the waiting draft once the new run accepts input. A resume must keep
// its conversation; a fresh start must never reopen the replaced one.
void SessionsWindow::continueAfterStart(const QString &host, const QString &name, const QJsonObject &data)
{
    if (m_waitingPreparations.contains(m_startSend.key)) return;
    if (m_startSend.key.isEmpty() || m_startSend.host != host || m_startSend.name != name) return;
    if (!m_composer->draftMatches(m_startSend.key, m_startSend.text, m_startSend.attachments)) {
        cancelStartContinuation(tr("Draft changed. Send it when you are ready.")); return;
    }
    if (QDateTime::currentMSecsSinceEpoch() - m_startSend.started > 180000) {
        cancelStartContinuation(tr("The agent did not become ready. Check Terminal; your draft was kept without sending.")); return;
    }
    const auto run = data.value("run_id").toString(), conversation = data.value("conversation_id").toString();
    if (m_pending || run.isEmpty() || run == m_startSend.run) return;
    const bool sameConversation = conversation == m_startSend.conversation;
    if (m_startSend.fresh ? sameConversation : !conversation.isEmpty() && !sameConversation) {
        cancelStartContinuation(tr("The agent opened an unexpected conversation. Your draft was kept without sending.")); return;
    }
    if (!data.value("tracked").toBool() || data.value("runtime_state")!="live" || data.value("process_state")!="running"
        || !data.value("error").toString().isEmpty() || data.value("phase")!="idle" || data.value("activity")!="idle"
        || m_terminating.contains(m_startSend.key)
        || (!data.value("expected_id").toString().isEmpty() && !data.value("resume_message_can_send").toBool())
        || (conversation.isEmpty() && !data.value("first_message_can_send").toBool())) return;
    // A ready inspection can precede the catalog's paused -> running update.
    // Keep the continuation queued until both name the new incarnation;
    // otherwise the async send guard correctly rejects the still-paused row
    // after we have already discarded the continuation.
    const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &entry) {
        return entry.key == m_startSend.key && entry.host == host && entry.session.name == name;
    });
    if (current == m_entries.cend() || !current->online || current->session.state != "running"
        || current->session.runId != run || current->session.conversationId != conversation) return;
    const auto pending = m_startSend;
    if (pending.compact && (conversation.isEmpty() || !data.value("compact_context_supported").toBool())) return;
    m_startSend = {};
    if (pending.compact) compactToSend(pending.key,pending.host,pending.name,run,conversation,pending.text,pending.attachments);
    else deliverMessage(pending.key,pending.host,pending.name,run,conversation,pending.text,pending.attachments,{},{},true,data);
    if (m_compacts.isEmpty()) m_compactTimer.stop();
}

void SessionsWindow::sendMessage(const QString &key, const QString &text, const QList<MessageAttachment> &attachments, const QString &retryId, const QString &compactionId, bool contextChosen)
{
    const auto *entry = selected();
    if (!entry || key != m_selectedKey || m_composer->isSending(key)) return;
    if (retryId.isEmpty() && compactionId.isEmpty() && !contextChosen && canStartStopped(*entry)) { startToSend(key, text, attachments); return; }
    const auto reason = messageBlockReason(*entry);
    if (!reason.isEmpty()) { m_composer->deliveryFinished(key, false, reason); return; }
    QString runId = m_details.value("run_id").toString();
    QString conversationId = m_details.value("conversation_id").toString();
    bool fullContextConsent = contextChosen || !compactionId.isEmpty();
    // Resume and send already chose the full saved context.
    if(compactionId.isEmpty() && !contextChosen && CacheStatus::expired(m_details)) {
        const auto host=entry->host,name=entry->session.name;
        QMessageBox dialog(QMessageBox::Warning,tr("Continue with a cold cache?"),CacheStatus::warning(m_details)+"\n\n"+
            tr("The next message may process the entire conversation again, increasing token usage, cost or subscription usage. Send with the full history, compact it into a summary before sending, or clear it and keep your draft. Compaction also uses tokens and may omit details."),QMessageBox::NoButton,this);
        dialog.setObjectName("coldCacheConfirm");auto *proceed=dialog.addButton(tr("Send with full context"),QMessageBox::AcceptRole);
        auto *compact=dialog.addButton(tr("Compact and continue"),QMessageBox::ActionRole);
        compact->setEnabled(m_details.value("compact_context_supported").toBool() && !m_compacts.contains(key));
        compact->setToolTip(tr("Run native /compact, then send this draft only after successful completion."));
        auto *clear=dialog.addButton(tr("Clear context"),QMessageBox::ActionRole);
        clear->setEnabled(m_details.value("clear_context_supported").toBool() && !m_clearRequest);
        // Name what the host reports blocking each unavailable action, such as
        // a subagent or tool that is still working.
        const auto compactReason=contextActionReason(m_details,"compact"),clearReason=contextActionReason(m_details,"clear");
        explainContextAction(compact,compactReason);explainContextAction(clear,clearReason);
        QStringList unavailable;
        if(!compactReason.isEmpty() && compactReason==clearReason)
            unavailable<<tr("Compact and continue and Clear context are unavailable. %1").arg(contextReasonSentence(compactReason));
        else {
            if(!compactReason.isEmpty())unavailable<<tr("Compact and continue is unavailable. %1").arg(contextReasonSentence(compactReason));
            if(!clearReason.isEmpty())unavailable<<tr("Clear context is unavailable. %1").arg(contextReasonSentence(clearReason));
        }
        // Reported names stay literal: the format also applies to this line.
        if(!unavailable.isEmpty()){dialog.setInformativeText(unavailable.join('\n'));dialog.setTextFormat(Qt::PlainText);}
        auto *cancel=dialog.addButton(QMessageBox::Cancel);dialog.setDefaultButton(cancel);dialog.setEscapeButton(cancel);dialog.exec();
        if(dialog.clickedButton()!=proceed && dialog.clickedButton()!=clear && dialog.clickedButton()!=compact)return;
        // The fleet polls while the modal runs. Re-read the selection and the
        // identity; a session that was still starting may gain the ids the
        // dialog opened with empty, and that is the same session: send with the
        // new identity. Anything else finishes the delivery as failed where the
        // owner sees it, never as a silent no-op (U1).
        entry=selected();
        if(!entry||m_selectedKey!=key||entry->host!=host||entry->session.name!=name){
            m_composer->deliveryFinished(key,false,tr("The session changed while the dialog was open. Your draft was kept without sending."));
            return;
        }
        const QString freshRun=m_details.value("run_id").toString(), freshConversation=m_details.value("conversation_id").toString();
        if(freshRun!=runId||freshConversation!=conversationId){
            const bool filledRun=runId.isEmpty()&&!freshRun.isEmpty();
            const bool filledConversation=conversationId.isEmpty()&&!freshConversation.isEmpty();
            if((freshRun!=runId&&!filledRun)||(freshConversation!=conversationId&&!filledConversation)){
                m_composer->deliveryFinished(key,false,tr("The session's identity changed while the dialog was open. Your draft was kept without sending; refresh and send again."));
                return;
            }
            runId=freshRun; conversationId=freshConversation;
        }
        if(dialog.clickedButton()==clear){clearContext();return;}
        if(dialog.clickedButton()==compact) {
            if (!m_composer->draftMatches(key, text, attachments)) { showNotice(tr("Draft changed. Send it when you are ready.")); return; }
            compactToSend(key,host,name,runId,conversationId,text,attachments,retryId);
            renderDetails(); return;
        }
        fullContextConsent = true;
        const auto currentProblem=messageBlockReason(*entry);
        if(!currentProblem.isEmpty()){m_composer->deliveryFinished(key,false,currentProblem);showNotice(currentProblem,true);return;}
    }
    deliverMessage(key,entry->host,entry->session.name,runId,conversationId,text,attachments,retryId,compactionId,fullContextConsent);
}

void SessionsWindow::deliverMessage(const QString &key, const QString &host, const QString &name, const QString &runId, const QString &conversationId,
                                   const QString &text, const QList<MessageAttachment> &attachments, const QString &retryId, const QString &compactionId, bool fullContextConsent, const QJsonObject &inspectionEvidence)
{
    if (m_composer->isSending(key)) return;
    const auto original = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &entry) { return entry.key == key && entry.host == host && entry.session.name == name; });
    if (original == m_entries.cend()) return;
    const Entry target = *original;
    QJsonObject retryMessage;
    if (!retryId.isEmpty()) {
        for (const auto &value : m_localMessages.value(key)) {
            const auto message = value.toObject();
            if (message.value("id").toString() == retryId && message.value("status") == "error" && !message.value("uncertain").toBool()) {
                retryMessage = message; break;
            }
        }
        if (retryMessage.isEmpty()) return;
    }
    const QJsonObject cacheEvidence = !inspectionEvidence.isEmpty() ? sendEvidence(inspectionEvidence)
        : key == m_selectedKey ? sendEvidence(m_details) : QJsonObject();
    beginSendEvidence(key, cacheEvidence);
    const bool preserve = !retryId.isEmpty() && !m_composer->draftMatches(key, text, attachments);
    m_composer->prepareSendingAsync(key, preserve, [this, target, key, host, name, runId, conversationId, text, attachments, retryId, retryMessage, compactionId, fullContextConsent, cacheEvidence](bool okay) {
        if (!okay) { finishSendEvidence(key); return; }
        const auto stillValid = [this, target, key, host, name, runId, conversationId, retryId, retryMessage, fullContextConsent, cacheEvidence] {
            const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &entry) { return entry.key == key; });
            const auto evidence = m_pendingSendEvidence.value(key, cacheEvidence);
            if (!fullContextConsent && (CacheStatus::expired(cacheEvidence) || CacheStatus::expired(evidence))) return false;
            if (current == m_entries.cend() || current->host != host || current->session.name != name
                || current->session.created != target.session.created
                || !observedIdentityMatches(current->session, target.session, runId, conversationId)
                || !current->online || isTerminating(*current)) return false;
            if (!evidence.isEmpty() && (evidence.value("run_id").toString() != runId
                || evidence.value("conversation_id").toString() != conversationId
                || (target.session.cmd=="external" ? !ConversationSource::cooperative(evidence)
                    : evidence.value("runtime_state") != "live" || evidence.value("process_state") != "running"))) return false;
            if (m_pending || !m_renameKey.isEmpty() || (key == m_clearKey && m_clearRequest)
                || (key == m_settingsKey && m_settingsRequest)) return false;
            if (!retryId.isEmpty()) {
                bool retained = false;
                for (const auto &value : m_localMessages.value(key)) if (value.toObject() == retryMessage) { retained = true; break; }
                if (!retained) return false;
            }
            if (key == m_selectedKey && (m_details.value("run_id").toString() != runId
                || m_details.value("conversation_id").toString() != conversationId || !messageBlockReason(*current).isEmpty()
                || (CacheStatus::expired(m_details) && !fullContextConsent))) return false;
            return true;
        };
        if (!stillValid()) { finishSendEvidence(key); m_composer->deliveryFinished(key, false, tr("The session changed. Your draft was kept without sending.")); return; }
        AttachmentFiles::prepareAsync(attachments, this,
            [this, stillValid, key, host, name, runId, conversationId, text, attachments, retryId, compactionId](const QJsonArray &files, const QString &error) {
                const bool valid = stillValid(); finishSendEvidence(key);
                if (!error.isEmpty() || !valid || !m_composer->isSending(key)) {
                    m_composer->deliveryFinished(key, false, error.isEmpty() ? tr("The session changed. Your draft was kept without sending.") : error); return;
                }
                // WhiteWh fork: the project's iron rules travel ahead of the text; the
                // draft, retries and the local copy keep the text alone.
                // U35: an hgs that holds messages keeps this one until the agent takes the previous.
                const bool hold = key == m_selectedKey && m_details.value("input_hold_supported").toBool();
                const quint64 request = m_client.requestSendMessage(host, name, IronRules::wrap(IronRules::rules(projectOfKey(key)), text), attachments, runId, conversationId, {}, compactionId, hold);
                m_pendingMessages.insert(request, {key, runId, conversationId});
                if (m_push && m_push->key == key && !m_push->request && m_push->text == text) {
                    m_push->request = request; m_push->run = runId; m_push->conversation = conversationId;
                }
                auto &messages = m_localMessages[key];
                for(int i=messages.size();i-- >0;) {
                    const auto previous=messages[i].toObject();
                    if(previous.value("status")=="error" && (previous.value("id").toString()==retryId
                        || (previous.value("text").toString()==text && previous.value("attachments").toArray()==files)))messages.removeAt(i);
                }
                messages.append(QJsonObject{{"id", QString::number(request)}, {"text", text}, {"status", "sending"},
                    {"attachments", files}, {"run_id", runId}, {"conversation_id", conversationId},
                    {"submitted_at", QDateTime::currentMSecsSinceEpoch() / 1000.0}});
                while (messages.size() > 50) messages.removeFirst();
                if (key == m_selectedKey) { renderDetails(); m_activityView->jumpToLatest(); }
            });
    });
}

void SessionsWindow::finishMessage(quint64 request, bool ok, const QJsonObject &receipt, const QString &error, bool uncertain)
{
    if (m_childMessages.contains(request)) {
        const auto key = m_childMessages.take(request);
        m_subagentComposer->deliveryFinished(key, ok, error, uncertain);
        inspect(); emit refreshRequested(); return;
    }
    if (!m_pendingMessages.contains(request)) return;
    const auto pending = m_pendingMessages.take(request);
    const QString detail = uncertain ? tr("Delivery is not confirmed. Check Terminal before retrying. %1").arg(error) : error;
    m_composer->deliveryFinished(pending.key, ok, detail, uncertain);
    pushSendFinished(request, ok, uncertain);
    if (!ok && !uncertain && pending.key == m_selectedKey && error.contains(QStringLiteral("has a draft"))) m_terminalDraft->check();
    auto &messages = m_localMessages[pending.key];
    // U35: a held message shows in the queued line, not as a sent card.
    if (ok && receipt.value("status") == "queued") {
        // #511 N5: the text the owner wrote, without the iron rules sent ahead of it.
        QString written;
        for (qsizetype i = 0; i < messages.size(); ++i)
            if (messages.at(i).toObject().value("id").toString() == QString::number(request)) { written = messages.at(i).toObject().value("text").toString(); messages.removeAt(i); break; }
        m_justHeld[pending.key].append(QJsonObject{{"request_id", receipt.value("request_id")}, {"text", receipt.value("text")}});
        m_sentHeld[pending.key].add(receipt.value("request_id").toString(), written.isEmpty() ? receipt.value("text").toString() : written);
        if (pending.key == m_selectedKey) { renderHeld(); renderDetails(); inspect(); }
        emit refreshRequested();
        return;
    }
    for (qsizetype i = 0; i < messages.size(); ++i) {
        auto message = messages.at(i).toObject();
        if (message.value("id").toString() != QString::number(request)) continue;
        message["status"] = ok ? "sent" : "error"; message["error"] = detail;
        message["uncertain"] = !ok && uncertain;
        if (ok) {
            message["message_id"]=receipt.value("request_id");
            auto files=message.value("attachments").toArray();
            for(int j=0;j<files.size();++j){auto file=files[j].toObject();file["request_id"]=receipt.value("request_id");file["index"]=j;files[j]=file;}
            message["attachments"]=files;
            message["submitted_text"] = receipt.value("submitted_text");
            if (receipt.contains("submitted_at")) message["submitted_at"] = receipt.value("submitted_at");
        }
        messages[i] = message; break;
    }
    if (pending.key == m_selectedKey) { reconcileMessages(); renderDetails(); inspect(); }
    emit refreshRequested();
}

void SessionsWindow::reconcileMessages()
{
    auto &messages = m_localMessages[m_selectedKey];
    const QString runId = m_details.value("run_id").toString();
    const QString conversationId = m_details.value("conversation_id").toString();
    for (qsizetype i = messages.size(); i-- > 0;) {
        const auto message = messages.at(i).toObject();
        if (message.value("status").toString() == "sending") continue;
        if ((!runId.isEmpty() && message.value("run_id").toString() != runId)
            || (!conversationId.isEmpty() && message.value("conversation_id").toString() != conversationId)) {
            messages.removeAt(i); continue;
        }
        if(message.value("source")=="hgs_voice") {
            for(const auto &value:m_details.value("voice_messages").toArray())
                if(value.toObject().value("message_id")==message.value("message_id")){messages.removeAt(i);break;}
            continue;
        }
        const bool failed=message.value("status")=="error";
        if (!failed && message.value("status") != "sent") continue;
        bool saved=false;
        for(const auto &value:m_details.value("attachment_messages").toArray()) {
            if(!message.value("message_id").toString().isEmpty() && value.toObject().value("message_id")==message.value("message_id")){saved=true;break;}
        }
        if(saved){messages.removeAt(i);continue;}
        // Keep the local file card until the durable attachment metadata arrives.
        if(!message.value("attachments").toArray().isEmpty())continue;
        const QString submitted = message.value("submitted_text").toString(message.value("text").toString()).trimmed();
        if (submitted.isEmpty()) continue;
        for (const auto &value : m_events) {
            const auto event = value.toObject();
            if (!QStringList{"UserPromptSubmit", "UserPromptQueued", "TurnStarted"}.contains(event.value("type").toString())
                || !event.value("agent_id").toString().isEmpty()
                || event.value("at").toDouble() < message.value("submitted_at").toDouble() - 2) continue;
            const QString detail = event.value("detail").toString().trimmed();
            if (!detail.isEmpty() && (failed ? detail==submitted : detail.startsWith(submitted.left(1000)))) {
                if(failed && !m_composer->isSending(m_selectedKey) && m_composer->draftMatches(m_selectedKey,message.value("text").toString(),{}))
                    m_composer->deliveryFinished(m_selectedKey,true);
                messages.removeAt(i); break;
            }
        }
    }
}

void SessionsWindow::renderQuestion(const Entry &entry, int navigation)
{
    m_question->setNativeUi(entry.session.cmd=="dsh");
    m_question->setTerminalAvailable(entry.session.cmd=="dsh"||NativeSessionCapabilities::terminal(entry.session,m_details),NativeSessionCapabilities::terminalReason(entry.session,m_details));
    QJsonObject question;
    QList<QJsonObject> optional;
    int pendingQuestions=0;QSet<QString> seen;
    for (const auto &value : m_details.value("pending_questions").toArray()) {
        const auto candidate = value.toObject();
        const auto id=candidate.value("question_id").toString();
        // A card without questions still explains itself when it cannot be answered
        // here (hgs empties an oversized OpenCode form).
        if(id.isEmpty() || seen.contains(id) || (candidate.value("questions").toArray().isEmpty() && candidate.value("can_answer").toBool(true)))continue;
        seen.insert(id);
        if(m_question->hasSubmittedAnswer(m_selectedKey,candidate))continue;
        pendingQuestions+=qMax<qsizetype>(1,candidate.value("questions").toArray().size());
        if(candidate.value("optional").toBool())optional.append(candidate);
        else if(question.isEmpty() || (!question.value("can_answer").toBool() && candidate.value("can_answer").toBool()))question=candidate;
    }
    std::stable_sort(optional.begin(),optional.end(),[](const QJsonObject &a,const QJsonObject &b) {
        return a.value("created_at").toDouble()>b.value("created_at").toDouble();
    });
    const QString scope=QString::fromUtf8(QJsonDocument(QJsonArray{m_selectedKey,m_details.value("run_id"),m_details.value("conversation_id")}).toJson(QJsonDocument::Compact));
    const QString chosen=m_questionSelections.value(scope);
    int index=0;
    if(!chosen.isEmpty()) {
        const auto found=std::find_if(optional.cbegin(),optional.cend(),[&](const QJsonObject &item){return item.value("question_id").toString()==chosen;});
        if(found==optional.cend())m_questionSelections.remove(scope);
        else index=int(found-optional.cbegin());
    }
    // Required confirmations always take priority. Optional requests can be
    // answered in any order; polling must preserve explicit queue navigation.
    if(question.isEmpty() && !optional.isEmpty()) {
        if(navigation) {
            index=qBound(0,index+navigation,int(optional.size())-1);
            m_questionSelections.insert(scope,optional[index].value("question_id").toString());
        }
        question=optional[index];
    }
    const bool available = !question.isEmpty();
    m_nativeUi->setResponsePending(entry.session.cmd == "dsh" && available);
    auto *focus = QApplication::focusWidget();
    const bool returnToComposer = !available && m_question->isVisible() && focus
        && (focus == m_question || m_question->isAncestorOf(focus));
    // Show and focus the replacement before clearing the focused question's
    // children. Background completion must not interrupt navigation elsewhere.
    m_composer->setInputVisible(!available || question.value("optional").toBool());
    if (returnToComposer) m_composer->editor()->setFocus(Qt::OtherFocusReason);
    m_question->setQuestion(m_selectedKey, question, pendingQuestions);
    m_question->setQueueNavigation(index,int(optional.size()));
    m_question->setVisible(available);
    QString reason;
    if (!entry.online) reason = tr("Machine unavailable. Your answers stay here.");
    else if (entry.session.state != "running") reason = tr("This session is not running.");
    else if (isTerminating(entry) || m_pending || !m_renameKey.isEmpty()) reason = tr("Wait for the session update to finish.");
    else if (!m_inspectError.isEmpty()) reason = tr("Refresh the session before answering.");
    else if (question.value("run_id") != m_details.value("run_id")
             || question.value("conversation_id") != m_details.value("conversation_id"))
        reason = tr("Refresh the pending question before answering.");
    else if (m_details.value("runtime_state").toString() != "live"
             || m_details.value("process_state").toString() != "running")
        reason = tr("The agent is no longer running.");
    m_question->setAvailability(reason.isEmpty(), reason);
    if (available && reason.isEmpty() && question.value("can_answer").toBool())
        m_hint->hide();
}

void SessionsWindow::answerQuestion(const QString &key, const QString &questionId, const QJsonArray &answers)
{
    // Signals can refer to QuestionCard members which a nested modal event loop
    // replaces. Consent and transport must use immutable copies instead.
    const QString targetKey = key, targetQuestionId = questionId;
    const QJsonArray requestedAnswers = answers;
    if (m_confirmingQuestionCache) return;
    const bool skipping = !requestedAnswers.isEmpty() && requestedAnswers.first().toObject().value("skip").toBool();
    const auto actionableQuestion = [&](bool reportChange = false) -> QJsonObject {
        const auto *entry = selected();
        if (!entry || targetKey != m_selectedKey || !entry->online || entry->session.state != "running"
            || isTerminating(*entry) || m_pending || !m_renameKey.isEmpty() || !m_inspectError.isEmpty()
            || m_details.value("runtime_state").toString() != "live"
            || m_details.value("process_state").toString() != "running") return {};
        for (const auto &pending : m_pendingAnswers) if (pending.key == targetKey) return {};
        QJsonObject question;
        for (const auto &value : m_details.value("pending_questions").toArray())
            if (value.toObject().value("question_id").toString() == targetQuestionId) { question = value.toObject(); break; }
        if (m_question->hasSubmittedAnswer(targetKey, question)) return {};
        if (question.isEmpty() || !(skipping ? question.value("can_skip").toBool() : question.value("can_answer").toBool())
            || question.value("run_id") != m_details.value("run_id")
            || question.value("conversation_id") != m_details.value("conversation_id")) {
            if (reportChange) m_question->setError(targetKey, targetQuestionId, tr("This question changed. Refresh before answering."));
            return {};
        }
        return question;
    };
    const auto consentIdentity = [](const QJsonObject &question) {
        QJsonObject identity;
        for (const auto *field : {"question_id", "question_hash", "run_id", "conversation_id", "questions",
                "source", "tool", "tool_name", "tool_call_id", "native_question_id", "agent_id",
                "answer_transport", "approval", "approval_choices", "trust_request", "optional"}) {
            const auto name = QString::fromLatin1(field);
            if (question.contains(name)) identity.insert(name, question.value(name));
        }
        return identity;
    };
    const auto question = actionableQuestion(true);
    if (question.isEmpty()) return;
    // This reviewed native panel already asks the user how to handle its cold
    // cache. Deliver the chosen row, without asking to resume with full context.
    const bool nativeCacheChoice = question.value("source") == "kimi_cache_hint"
        && question.value("answer_transport") == "kimi_tui";
    bool fullContextConsent = false;
    if (!nativeCacheChoice && CacheStatus::expired(m_details)) {
        const Entry original = *selected();
        const auto run = m_details.value("run_id"), conversation = m_details.value("conversation_id");
        const auto draft = m_question->submissionSnapshot(targetKey, targetQuestionId);
        if (draft.isEmpty() || draft.value("sending").toBool() || draft.value("submitted").toBool()
            || draft.value("answered").toBool() || draft.value("uncertain").toBool()
            || !draft.value("storage_error").toString().isEmpty()) return;
        QScopedValueRollback<bool> confirming(m_confirmingQuestionCache, true);
        QMessageBox box(QMessageBox::Warning, tr("Submit with a cold cache?"),
            CacheStatus::warning(m_details) + "\n\n" + tr("Submitting this answer may resume the agent with the entire conversation, increasing token usage, cost or subscription usage. Compaction or clearing can invalidate the pending question. Cancel keeps your answers."),
            QMessageBox::NoButton, this);
        box.setObjectName("questionColdCacheConfirm");
        auto *proceed = box.addButton(tr("Submit with full context"), QMessageBox::AcceptRole);
        auto *cancel = box.addButton(QMessageBox::Cancel);
        box.setDefaultButton(cancel); box.setEscapeButton(cancel);
        box.exec();
        if (box.clickedButton() != proceed) return;
        const auto *current = selected();
        if (!current || current->key != original.key || current->host != original.host
            || current->session.name != original.session.name || current->session.created != original.session.created
            || current->session.runId != original.session.runId || current->session.conversationId != original.session.conversationId
            || m_details.value("run_id") != run || m_details.value("conversation_id") != conversation
            || consentIdentity(actionableQuestion()) != consentIdentity(question)
            || m_question->submissionSnapshot(targetKey, targetQuestionId) != draft) {
            showNotice(tr("The question or your answers changed. Your draft was kept; review it before submitting again."), true);
            return;
        }
        fullContextConsent = true;
    }
    const auto *entry = selected();
    if (!entry) return;
    const Entry target = *entry;
    const auto pinnedIdentity = consentIdentity(question);
    const QJsonObject cacheEvidence = sendEvidence(m_details);
    beginSendEvidence(targetKey, cacheEvidence);
    m_question->prepareSendingAsync(targetKey, targetQuestionId,
        [this, target, targetKey, targetQuestionId, question, requestedAnswers, pinnedIdentity, skipping, consentIdentity, nativeCacheChoice, fullContextConsent, cacheEvidence](bool okay) {
            if (!okay) { finishSendEvidence(targetKey); return; }
            const auto current = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &value) { return value.key == targetKey; });
            const auto evidence = targetKey == m_selectedKey ? sendEvidence(m_details) : m_pendingSendEvidence.value(targetKey, cacheEvidence);
            finishSendEvidence(targetKey);
            QJsonObject refreshed;
            for (const auto &value : evidence.value("pending_questions").toArray())
                if (value.toObject().value("question_id").toString() == targetQuestionId) { refreshed = value.toObject(); break; }
            if (current == m_entries.cend() || current->host != target.host || current->session.name != target.session.name
                || current->session.created != target.session.created
                || !observedIdentityMatches(current->session, target.session, question.value("run_id").toString(), question.value("conversation_id").toString())
                || !current->online || current->session.state != "running" || isTerminating(*current)
                || m_pending || !m_renameKey.isEmpty() || (targetKey == m_selectedKey && !m_inspectError.isEmpty())
                || evidence.value("runtime_state") != "live" || evidence.value("process_state") != "running"
                || evidence.value("run_id") != question.value("run_id") || evidence.value("conversation_id") != question.value("conversation_id")
                || consentIdentity(refreshed) != pinnedIdentity
                || (!nativeCacheChoice && !fullContextConsent && (CacheStatus::expired(cacheEvidence) || CacheStatus::expired(evidence)))
                || !(skipping ? refreshed.value("can_skip").toBool() : refreshed.value("can_answer").toBool())
                || m_question->hasSubmittedAnswer(targetKey, refreshed)) {
                m_question->setError(targetKey, targetQuestionId, tr("The question changed. Your answers were kept without submitting.")); return;
            }
            const auto request = m_client.requestAnswerQuestion(target.host, target.session.name, question, requestedAnswers);
            m_pendingAnswers.insert(request, {targetKey, targetQuestionId, question.value("question_hash").toString()});
            renderDetails();
        });
}

void SessionsWindow::finishQuestion(quint64 request, bool ok, const QString &error, bool uncertain, bool submitted)
{
    if (!m_pendingAnswers.contains(request)) return;
    const auto pending = m_pendingAnswers.take(request);
    if (ok && submitted) m_question->setSubmitted(pending.key, pending.questionId);
    else if (ok) m_question->setAnswered(pending.key, pending.questionId);
    else m_question->setError(pending.key, pending.questionId, error, uncertain);
    if (m_selectedKey == pending.key) { renderDetails(); inspect(); }
    emit refreshRequested();
}

void SessionsWindow::openExternalFile(const QString &path, bool otherMachine)
{
    switch (ExternalFile::open(path, otherMachine ? ExternalFile::Origin::OtherMachine : ExternalFile::Origin::ThisComputer)) {
    case ExternalFile::Outcome::Opened: return;
    case ExternalFile::Outcome::Revealed: showNotice(ExternalFile::revealedNotice(path)); return;
    case ExternalFile::Outcome::RevealFailed: showNotice(ExternalFile::revealFailedNotice(path), true); return;
    case ExternalFile::Outcome::OpenFailed:
        showNotice(tr("Could not open %1 with its default application. You can copy the path.").arg(path), true); return;
    }
}

void SessionsWindow::showNotice(const QString &message, bool error, bool transient)
{
    m_notice->show();
    m_noticeDismiss->show();
    m_notice->setText(message.isEmpty() ? tr("Operation failed. Open the terminal for details.") : message);
    ForkLog::write(error ? "error" : "notice", m_notice->text());
    m_notice->setToolTip(Qt::convertFromPlainText(m_notice->text()));
    m_notice->setProperty("error", error);
    m_notice->style()->unpolish(m_notice); m_notice->style()->polish(m_notice); m_notice->update();
    const int revision=m_notice->property("noticeRevision").toInt()+1;
    m_notice->setProperty("noticeRevision",revision);
    updateSessionFeedback();
    if (!error || transient) QTimer::singleShot(m_noticeMs, this, [this,revision] {
        if (m_notice->property("noticeRevision").toInt()==revision) clearNotice();
    });
}

void SessionsWindow::clearNotice()
{
    m_notice->setProperty("noticeRevision", m_notice->property("noticeRevision").toInt() + 1);
    m_notice->clear(); m_notice->setToolTip({}); m_notice->hide(); m_noticeDismiss->hide();
    updateSessionFeedback();
}

bool SessionsWindow::selectSessionEntry(const Entry &original)
{
    const auto found = std::find_if(m_entries.cbegin(), m_entries.cend(), [&original](const Entry &entry) { return entry.key == original.key; });
    if (found == m_entries.cend() || (original.session.created > 0 && found->session.created > 0 && original.session.created != found->session.created)) {
        showNotice(tr("This session is no longer available. Refresh the list and open its actions again."), true); return false;
    }
    for (int row = 0; row < m_sessions->count(); ++row) if (m_sessions->item(row)->data(KeyRole).toString() == original.key) {
        m_sessions->setCurrentRow(row); renderDetails(); return true;
    }
    showNotice(tr("This session is no longer in the current view. Open its actions again."), true); return false;
}

QString SessionsWindow::selectedDirectory() const
{
    const auto *entry = selected(); if (!entry) return {};
    return m_details.value("cwd").toString(entry->session.cwd);
}

void SessionsWindow::openFolderShell()
{
    const auto *entry = selected();
    if (!entry || !entry->online) return;
    const auto directory = m_subagentId.isEmpty() ? selectedDirectory() : m_subagentCwd;
    if (MachinePlatform::absolute(directory, MachinePlatform::of(m_fleet.metricsOf(entry->host), entry->host.isEmpty())))
        emit folderShellRequested(entry->host, directory);
}

void SessionsWindow::openSessionFolder()
{
    const auto *entry = selected();
    if (!entry || !entry->host.isEmpty()) return;
    openLocalFolder(m_subagentId.isEmpty() ? selectedDirectory() : m_subagentCwd);
}

void SessionsWindow::openLocalFolder(const QString &directory)
{
    if (!QFileInfo(directory).isAbsolute()) { showNotice(tr("Session folder is unavailable."), true); return; }
    // #744: checked by PathProbe's worker, bounded; the window keeps answering.
    PathProbe::instance().check(directory, this, [this, directory](const PathProbe::Info &folder) {
        if (folder.state == PathProbe::Info::Unresponsive) { showNotice(tr("The session folder does not answer: its disk may be busy."), true); return; }
        if (!folder.dir) { showNotice(tr("Session folder is unavailable."), true); return; }
        if (!QDesktopServices::openUrl(QUrl::fromLocalFile(directory)))
            showNotice(tr("Could not open the session folder in the file manager."), true);
    });
}

SessionsWindow::FolderState SessionsWindow::folderState(bool local, const QString &directory) const
{
    if (!local) return {false, false, tr("Available only for sessions on this computer")};
    if (directory.isEmpty() || !QFileInfo(directory).isAbsolute()) return {false, false, tr("Session folder is unavailable")};
    const auto folder = PathProbe::instance().info(directory);
    if (folder.state == PathProbe::Info::Checking) return {false, true, tr("Checking the session folder…")};
    if (folder.state == PathProbe::Info::Unresponsive) return {false, false, tr("The session folder does not answer: its disk may be busy")};
    if (!folder.dir) return {false, false, tr("Session folder is unavailable")};
    return {true, false, tr("Open session folder in file manager\n%1").arg(directory)};
}

void SessionsWindow::showFolderState(QPushButton *button, QAction *action, bool local, const QString &directory)
{
    const auto state = folderState(local, directory);
    if (button) { button->setEnabled(state.open); button->setToolTip(state.hint); button->setProperty("checking", state.checking); }
    if (action) {
        action->setEnabled(state.open); action->setToolTip(state.hint);
        action->setText(state.checking ? tr("Open folder (checking…)") : tr("Open folder"));
    }
}

void SessionsWindow::showSessionMenu(const QString &key, const QPoint &position)
{
    if (selectionEntries().size() > 1) { showSelectionMenu(position); return; }
    const auto found = std::find_if(m_entries.cbegin(), m_entries.cend(), [&key](const Entry &entry) { return entry.key == key; });
    if (found == m_entries.cend()) return;
    const Entry original = *found;
    const bool archived = original.session.state == "archived";
    const auto &target = original.session;
    const bool selectedTarget = original.key == m_selectedKey && m_subagentId.isEmpty();
    const bool idle = !isTerminating(original) && !m_pending && !m_settingsRequest && m_renameKey.isEmpty()
        && !m_composer->isSending(original.key) && std::none_of(m_pendingAnswers.cbegin(), m_pendingAnswers.cend(), [&original](const PendingAnswer &p) { return p.key == original.key; });
    const bool available = original.online && idle;
    const QString directory = selectedTarget ? selectedDirectory() : target.cwd;
    const bool canPause = target.state == "running" && target.resumable && target.activity == "idle" && target.processState != "exited";
    const bool canChange = available && (archived ? target.resumable && !target.archiveId.isEmpty() && !archiveNameOccupied(original)
        : canPause || (target.state != "running" && target.resumable));
    const auto add = [this, original](const QString &text, const char *name, SessionMenuAction kind, bool enabled, const QString &hint = QString()) {
        auto *action = m_sessionMenu->addAction(text); action->setObjectName(QLatin1String(name)); action->setEnabled(enabled);
        if (!hint.isEmpty()) action->setToolTip(hint);
        connect(action, &QAction::triggered, this, [this, original, kind]() { runSessionMenuAction(original, kind); });
        return action;
    };
    qDeleteAll(m_sessionMenu->findChildren<QMenu *>(QString(), Qt::FindDirectChildrenOnly));
    m_sessionMenu->clear();
    const bool rowCollapsed = m_sessionRowPreferences.collapsed(original.host, target);
    add(rowCollapsed ? tr("Expand session row") : tr("Compact session row"), "contextSessionRowDensity",
        rowCollapsed ? SessionMenuAction::ExpandRow : SessionMenuAction::CollapseRow, true);
    m_sessionMenu->addAction(m_expandAllRowsAction);
    m_sessionMenu->addSeparator();
    if (!archived) add(original.session.cmd == "dsh" ? tr("Open native DeepSeek UI") : tr("Open terminal"), "contextOpenTerminal", SessionMenuAction::OpenTerminal, available && (target.cmd=="dsh"||NativeSessionCapabilities::terminal(target,selectedTarget?m_details:QJsonObject())),
        NativeSessionCapabilities::terminal(target,selectedTarget?m_details:QJsonObject())||target.cmd=="dsh"?QString():NativeSessionCapabilities::terminalReason(target,selectedTarget?m_details:QJsonObject()));
    if (!archived && SessionPresentation::backgroundIdle(original.session))
        add(tr("Show running commands"), "contextOpenProcesses", SessionMenuAction::OpenProcesses, original.online && ProcessSettings::enabled(),
            ProcessSettings::enabled() ? tr("Follow this session's background work in Processes") : tr("Enable Settings > Processes to inspect running commands"));
    // #744: the folder's state from PathProbe; the item says checking until it answers.
    m_menuFolderAction = add(tr("Open folder"), "contextOpenFolder", SessionMenuAction::OpenFolder, false);
    m_menuFolderPath = original.host.isEmpty() ? directory : QString();
    showFolderState(nullptr, m_menuFolderAction, original.host.isEmpty(), directory);
    add(tr("Open shell in session folder"), "contextFolderShell", SessionMenuAction::FolderShell,
        original.online && MachinePlatform::absolute(directory, MachinePlatform::of(m_fleet.metricsOf(original.host), original.host.isEmpty())));
    add(archived ? tr("Copy restore command") : tr("Copy connect command"), "contextCopyCommand", SessionMenuAction::CopyCommand,
        (archived || NativeSessionCapabilities::terminal(target)) && (!archived || !original.session.archiveId.isEmpty()));
    if (!archived) {
        m_sessionMenu->addSeparator();
        const bool pending = std::any_of(m_pendingNotifications.cbegin(), m_pendingNotifications.cend(), [&](const PendingNotifications &p) {
            return p.entry.host == original.host && p.entry.session.conversationId == target.conversationId;
        });
        QString muteReason = target.muted ? tr("Allow future notifications for this session on all devices") : tr("Mute notifications for this session on all devices");
        const bool notifications = original.online && (!original.host.isEmpty() || m_connectionError.isEmpty()) && target.sharedNotifications
            && target.notificationPreferencesError.isEmpty() && SessionNotifications::identity(target.runId,128) && SessionNotifications::identity(target.conversationId,256)
            && !pending && m_pendingNotifications.size() < 16;
        if (!original.online || (original.host.isEmpty() && !m_connectionError.isEmpty())) muteReason = tr("Connect to this session's host to change shared notifications.");
        else if (!target.sharedNotifications) muteReason = tr("Update hgs on this session's host to support shared notification preferences.");
        else if (!SessionNotifications::identity(target.runId,128) || !SessionNotifications::identity(target.conversationId,256)) muteReason = tr("Refresh this session's identity before changing shared notifications.");
        else if (!target.notificationPreferencesError.isEmpty()) muteReason = tr("Conflicting shared notification preferences. Refresh the session.");
        else if (pending || m_pendingNotifications.size() >= 16) muteReason = tr("A shared notification change is pending. Wait for its result.");
        auto *mute = add(target.muted ? tr("Unmute notifications") : tr("Mute notifications"), "contextToggleMute", SessionMenuAction::ToggleMute, notifications, muteReason);
        mute->setIcon(workspaceIcon(target.muted ? "bell" : "bell-off", SessionStatusBadge::mutedTone(m_dark)));
        // WhiteWh fork: the hours stay as set in Details; on from here is one hour.
        auto *warm = add(tr("Keep cache warm"), "contextKeepCacheWarm", SessionMenuAction::ToggleKeepWarm, !original.session.conversationId.isEmpty(),
            tr("Send the idle agent a one-word service message before its prompt cache expires"));
        warm->setCheckable(true); warm->setChecked(CacheKeepalive::hours(original.host, original.session) > 0);
        if (entryNeedsAttention(original))
            add(tr("Mark as read"), "contextMarkRead", SessionMenuAction::MarkRead, true);
        else
            add(tr("Mark needs attention"), "contextReviewLater", SessionMenuAction::ReviewLater, true,
                tr("Keep a reminder to review this session later"));
    }
    m_sessionMenu->addSeparator();
    add(archived ? tr("Restore session") : original.session.state == "running" ? tr("Pause session") : tr("Resume session"),
        "contextChangeSession", SessionMenuAction::ChangeState, selectedTarget ? m_changeAction->isEnabled() : canChange && NativeSessionCapabilities::action(target,archived?"restore":target.state=="running"?"pause":"resume"));
    // A live session clears where a stopped one starts fresh.
    QPointer<QAction> clear;
    if (target.state == "paused" || target.state == "stopped")
        add(tr("Start fresh…"), "contextStartFresh", SessionMenuAction::StartFresh, canStartStopped(original),
            tr("Start with a new conversation and the same folder, account and model"));
    else if (!archived)
        clear = add(tr("Clear session…"), "contextClearSession", SessionMenuAction::Clear, selectedTarget && m_clear->isEnabled(),
            selectedTarget ? m_clear->toolTip() : QString());
    add(tr("Rename…"), "contextRenameSession", SessionMenuAction::Rename, available && NativeSessionCapabilities::action(target,"rename") && !target.name.section('/', 1, 1).isEmpty() && (!archived || !target.archiveId.isEmpty()));
    auto *fork = add(tr("Fork session…"), "contextForkSession", SessionMenuAction::Fork, selectedTarget && m_forkAction->isEnabled(), m_forkAction->toolTip());
    if (!selectedTarget && available) {
        fork->setToolTip(tr("Checking whether this session can be forked…"));
        if (clear) clear->setToolTip(tr("Checking whether this session can be cleared…"));
        connect(&m_client, &HgsClient::inspectionReady, fork, [this, fork, clear, original](const QString &host, const QString &name, const QJsonObject &data, const QString &archive) {
            if (host != original.host || name != original.session.name || archive != original.session.archiveId) return;
            if ((!original.session.runId.isEmpty() && data["run_id"] != original.session.runId)
                || (!original.session.conversationId.isEmpty() && data["conversation_id"] != original.session.conversationId)) return;
            fork->setProperty("details", data); fork->setEnabled(data["fork_supported"].toBool());
            fork->setToolTip(data["fork_reason"].toString());
            if (!clear) return;
            // Selecting the session adopts only details that name its listed conversation.
            clear->setProperty("details", data); clear->setProperty("usualToolTip", QString());
            clear->setEnabled(!m_clearRequest && clearableContext(data)
                && data["run_id"] == original.session.runId && data["conversation_id"] == original.session.conversationId);
            explainContextAction(clear.data(), data["clear_context_dismiss"].toBool() ? QString() : contextActionReason(data, "clear"));
        });
        m_client.requestInspection(original.host, target.name, 0, target.archiveId,false);
    }
    if (original.session.state == "paused" || original.session.state == "stopped")
        add(tr("Move to archive"), "contextArchiveSession", SessionMenuAction::Archive, available && NativeSessionCapabilities::action(target,"archive"),NativeSessionCapabilities::actionReason(target,"archive"));
    auto *groups = m_sessionMenu->addMenu(tr("Move to project")); groups->setObjectName("moveSessionToGroup");
    for (const auto &group : m_organization.groups()) {
        auto *action = groups->addAction(QString(group.name).replace('&', "&&")); action->setCheckable(true);
        action->setChecked(m_organization.groupFor(original.identity) == group.id); action->setData(group.id);
        connect(action, &QAction::triggered, this, [this, identity = original.identity, id = group.id]() {
            m_organization.moveSession(identity, id); saveOrganization(); rebuild();
        });
    }
    groups->addSeparator();
    connect(groups->addAction(tr("New project…")), &QAction::triggered, this, [this, identity = original.identity]() { createGroup(identity); });
    m_sessionMenu->addSeparator();
    add(tr("Copy session name"), "contextCopyName", SessionMenuAction::CopyName, true);
    add(tr("Copy session reference"), "contextCopyReference", SessionMenuAction::CopyReference, original.online && !target.runId.isEmpty());
    add(tr("Copy folder path"), "contextCopyFolder", SessionMenuAction::CopyFolder, !directory.isEmpty());
    m_sessionMenu->addSeparator();
    const auto endOperation=NativeSessionCapabilities::endOperation(target);
    add(archived ? tr("Forget archived session…") : endOperation == "terminate" ? tr("Terminate session…") : tr("Forget saved session…"),
        "contextTerminateSession", SessionMenuAction::Terminate, available && NativeSessionCapabilities::action(target,NativeSessionCapabilities::capability(target,endOperation)) && (!archived || !target.archiveId.isEmpty()),NativeSessionCapabilities::actionReason(target,NativeSessionCapabilities::capability(target,endOperation)));
    m_sessionMenu->popup(position);
}

void SessionsWindow::copySessionReference(const Entry &entry, const QString &agent)
{
    const bool selectedTarget=entry.key==m_selectedKey;
    const auto run=selectedTarget?m_details.value("run_id").toString(entry.session.runId):entry.session.runId;
    const auto conversation=selectedTarget?m_details.value("conversation_id").toString(entry.session.conversationId):entry.session.conversationId;
    m_client.requestSessionReference(entry.host,entry.session.name,run,conversation,entry.session.archiveId,agent);
}

void SessionsWindow::runSessionMenuAction(const Entry &original, SessionMenuAction action)
{
    const auto found = std::find_if(m_entries.cbegin(), m_entries.cend(), [&original](const Entry &e) { return e.key == original.key; });
    if (found == m_entries.cend() || (original.session.created > 0 && found->session.created != original.session.created)
        || found->session.conversationId != original.session.conversationId) {
        showNotice(tr("This session is no longer available. Open its actions again."), true); return;
    }
    const Entry current = *found;
    const auto directory = current.key == m_selectedKey && m_subagentId.isEmpty() ? selectedDirectory() : current.session.cwd;
    switch (action) {
    case SessionMenuAction::ToggleMute: {
        if (!current.online || (current.host.isEmpty() && !m_connectionError.isEmpty()) || !current.session.sharedNotifications
            || !current.session.notificationPreferencesError.isEmpty() || current.session.runId != original.session.runId
            || !SessionNotifications::identity(current.session.runId,128) || !SessionNotifications::identity(current.session.conversationId,256)
            || current.session.notificationPreferences != original.session.notificationPreferences || m_pendingNotifications.size() >= 16
            || std::any_of(m_pendingNotifications.cbegin(), m_pendingNotifications.cend(), [&](const PendingNotifications &p) {
                return p.entry.host == current.host && p.entry.session.conversationId == current.session.conversationId;
            })) return;
        // A member muted by its team first records the mute it has, then unmutes.
        const bool team = current.session.teamMuted;
        const QJsonObject payload{{"run_id", current.session.runId}, {"conversation_id", current.session.conversationId},
            {"muted", team || !current.session.muted}, {"expected_revision", current.session.notificationPreferences.value("revision")}};
        const auto request = m_client.requestNotifications(current.host, current.session.name, payload);
        m_pendingNotifications.insert(request, {current, team}); return;
    }
    case SessionMenuAction::ToggleKeepWarm:
        CacheKeepalive::setHours(current.host, current.session, CacheKeepalive::hours(current.host, current.session) > 0 ? 0 : CacheKeepalive::DefaultHours);
        if (current.key == m_selectedKey) renderDetails();
        return;
    case SessionMenuAction::CollapseRow:
    case SessionMenuAction::ExpandRow:
        setSessionRowCollapsed(current, action == SessionMenuAction::CollapseRow); return;
    case SessionMenuAction::MarkRead:
    case SessionMenuAction::ReviewLater: {
        const bool changed = action == SessionMenuAction::MarkRead ? m_fleet.markSessionRead(original.host, original.session)
            : m_fleet.setReviewLater(original.host, original.session, !original.session.reviewLater);
        if (!changed) {
            showNotice(action == SessionMenuAction::MarkRead
                ? tr("A new reply or question arrived. Open the menu again to mark it as read.")
                : tr("This session is no longer available. Open its actions again."), true);
            return;
        }
        ReadMarkStore::add(m_fleet.takeNewReadMarks());
        const auto marks = m_fleet.attentionMarks(), read = m_fleet.readReplies();
        SettingsJson::store(QStringLiteral("attention/sessionMarks"), QJsonDocument(marks));
        SettingsJson::store(QStringLiteral("attention/readReplies"), QJsonDocument(read));
        if (current.key == m_selectedKey) m_readSelectionKey = current.key;
        rebuild(); updateDashboard(); emit attentionMarksChanged(marks, read); return;
    }
    case SessionMenuAction::OpenFolder:
        if (current.host.isEmpty()) openLocalFolder(directory); return;
    case SessionMenuAction::FolderShell:
        if (current.online && MachinePlatform::absolute(directory, MachinePlatform::of(m_fleet.metricsOf(current.host), current.host.isEmpty())))
            emit folderShellRequested(current.host, directory);
        return;
    case SessionMenuAction::CopyName: emit copyTextRequested(current.session.name); return;
    case SessionMenuAction::CopyReference: copySessionReference(current); return;
    case SessionMenuAction::CopyFolder: if (!directory.isEmpty()) emit copyTextRequested(directory); return;
    case SessionMenuAction::CopyCommand:
        if (current.session.state != "archived" || !current.session.archiveId.isEmpty())
            emit copySessionCommandRequested(current.host, current.session.name, current.session.archiveId);
        return;
    default: break;
    }
    if (!selectSessionEntry(original)) return;
    const auto *entry = selected(); if (!entry) return;
    if (awaitingRestoredInspection(*entry) && action != SessionMenuAction::OpenProcesses) {
        showNotice(tr("Refresh the restored session before using its native controls."), true); return;
    }
    if (action == SessionMenuAction::Fork || action == SessionMenuAction::Clear) {
        const auto *source = m_sessionMenu->findChild<QAction *>(action == SessionMenuAction::Fork ? "contextForkSession" : "contextClearSession");
        const auto data = source ? source->property("details").toJsonObject() : QJsonObject();
        if (!data.isEmpty() && data["run_id"].toString() == entry->session.runId && data["conversation_id"].toString() == entry->session.conversationId) {
            m_details = data; renderDetails();
        }
    }
    switch (action) {
    case SessionMenuAction::OpenProcesses:
        if (entry->online && ProcessSettings::enabled()) m_detailTabs->setCurrentWidget(m_processes);
        return;
    case SessionMenuAction::OpenTerminal:
        if (entry->session.cmd == "dsh") { showNativeUi(*entry); return; }
        if (!entry->online || !m_openAction->isEnabled() || entry->session.state == "archived" || !NativeSessionCapabilities::terminal(entry->session,m_details)) return;
        // This terminal opens also in clipboard mode, and `hgs a` resumes a saved session.
        if (entry->session.state != "running") {
            if (!confirmLocalStart(*entry)) return;
            entry = selected();
            if (!entry->online || !m_openAction->isEnabled() || !NativeSessionCapabilities::terminal(entry->session,m_details)) return;
        }
        emit terminalOpenRequested(entry->host, entry->session.name,
            entry->host.isEmpty() && entry->session.attached && !entry->session.clients.isEmpty() ? entry->session.clients.first() : QString());
        return;
    case SessionMenuAction::ChangeState:
    case SessionMenuAction::Terminate:
        if ((original.session.state == "running") != (entry->session.state == "running")) {
            showNotice(tr("The session state changed. Open its actions again."), true); return;
        }
        if (action == SessionMenuAction::ChangeState) changeSession(); else terminateSession();
        return;
    case SessionMenuAction::Clear:
        // A session that stopped meanwhile offers Start fresh instead.
        if (entry->session.state != original.session.state) {
            showNotice(tr("The session state changed. Open its actions again."), true); return;
        }
        clearContext(); return;
    case SessionMenuAction::Rename: renameSession(); return;
    case SessionMenuAction::StartFresh: startFresh(); return;
    case SessionMenuAction::Fork: forkSession(); return;
    case SessionMenuAction::Archive: archiveSession(); return;
    default: return;
    }
}

void SessionsWindow::jumpToLatestActivity(QListWidgetItem *item)
{
    if (!item || item->data(SessionRoles::Header).toBool() || !item->data(SessionRoles::LaunchId).toString().isEmpty()) return;
    m_detailTabs->setCurrentIndex(0);
    (m_subagentId.isEmpty() ? m_activityView : m_subagentView)->jumpToLatest();
}

void SessionsWindow::showNativeUi(const Entry &entry)
{
    if (awaitingRestoredInspection(entry)) return;
    if (NativeBrowserPlugin::factory()) { m_detailTabs->setCurrentWidget(m_nativeUi); return; }
    if (entry.online) m_client.openNativeUi(entry.host, entry.session.name);
}

void SessionsWindow::copySessionIdentity()
{
    const auto value = m_detailsIdValue->property("copyValue").toString();
    if (!selected() || value.isEmpty()) return;
    emit copyTextRequested(value); showNotice(tr("%1 copied").arg(m_detailsIdTitle->text()));
}

void SessionsWindow::openSession()
{
    const auto *e = selected(); if (!e || !e->online || !m_openAction->isEnabled() || e->session.state == "archived" || awaitingRestoredInspection(*e)) return;
    if(e->session.cmd=="external") {
        const auto url=ConversationSource::chatUrl(m_details.value("external_url").toString());
        if(url.isEmpty()||m_details.value("run_id").toString()!=e->session.runId||m_details.value("conversation_id").toString()!=e->session.conversationId){showNotice(tr("Refresh the exact external conversation before opening it."),true);return;}
        if(!QDesktopServices::openUrl(url))showNotice(tr("Could not open the conversation in the default browser."),true);
        return;
    }
    if (e->session.cmd == "dsh") { m_client.openNativeUi(e->host,e->session.name); return; }
    // `hgs a` resumes a saved session; a copied command starts nothing.
    if (!m_clipboard && e->session.state != "running") {
        if (!confirmLocalStart(*e)) return;
        e = selected(); if (!e->online || !m_openAction->isEnabled()) return;
    }
    emit sessionActivated(e->host, e->session.name,
        e->host.isEmpty() && e->session.attached && !e->session.clients.isEmpty() ? e->session.clients.first() : QString());
}

bool SessionsWindow::confirmLocalStart(const Entry original)
{
    // The modal loop may rebuild m_entries. Keep the confirmed identity by
    // value, so the comparison below never reads a retired or replaced row.
    if (!LocalSessionHost::confirmLaunch(this, m_fleet.local(), original.host)) return false;
    const auto *e = selected();
    if (e && e->key == original.key && e->session.state == original.session.state && e->session.runId == original.session.runId
        && e->session.conversationId == original.session.conversationId && e->session.archiveId == original.session.archiveId) return true;
    showNotice(tr("The session changed. Open its actions again."), true); return false;
}

void SessionsWindow::changeSession()
{
    const auto *e = selected(); if (!e || !e->online || m_pending || !m_changeAction->isEnabled()) return;
    if (e->session.state != "running") {
        if (!confirmLocalStart(*e)) return;
        e = selected(); if (!e->online || m_pending || !m_changeAction->isEnabled()) return;
    }
    const QString restoreKey = e->host + '\n' + e->session.name;
    if (e->session.state == "archived" && (!m_restoreWriteKey.isEmpty() || m_restores.contains(restoreKey)
        || m_restores.size() + m_restoredInspections.size() >= 64)) {
        showNotice(tr("Finish or refresh the earlier restores before restoring another session."), true); return;
    }
    m_pending = true; m_restoreKey.clear();
    if (e->session.state == "archived") {
        m_restoreKey = restoreKey; m_restoreWriteKey = restoreKey;
        m_restores.insert(restoreKey, {*e, m_details.value("conversation_id").toString(e->session.conversationId), false});
        m_client.restoreArchive(e->host, e->session.name, e->session.archiveId);
    } else if (e->session.state == "running") m_client.pauseSession(e->host, e->session.name);
    else m_client.resumeSession(e->host, e->session.name);
    showNotice(tr("Updating session on %1…").arg(e->machine)); renderDetails();
}

void SessionsWindow::archiveSession()
{
    const auto *e = selected();
    if (!e || !e->online || m_pending || !m_renameKey.isEmpty() || (e->session.state != "paused" && e->session.state != "stopped")) return;
    const QString host = e->host, name = e->session.name;
    m_removeTerminatedWorktree=false;
    ++m_worktreeCleanupRevision; m_worktreeCleanupOffered = false; m_worktreeCleanup->hide();
    m_archivedWorktreeRun = e->session.runId;
    const bool inspected = m_details.value("git_worktree").toBool() && !m_details.value("git_root").toString().isEmpty();
    m_archivedWorktree = {host,name,inspected ? m_details.value("git_root").toString() : e->session.gitWorktree ? e->session.gitRoot : QString(),
        inspected ? m_details.value("git_common_dir").toString() : e->session.gitWorktree ? e->session.gitCommonDir : QString()};
    m_pending = true; m_restoreKey.clear(); m_client.archiveSession(host, name);
    showNotice(tr("Moving session to archive…")); renderDetails();
}

void SessionsWindow::offerWorktreeCleanup()
{
    if (m_archivedWorktree.size() != 4 || m_archivedWorktree[2].isEmpty()) return;
    m_worktreeCleanupOffered = true; m_worktreeCleanupUnconfirmed = false;
    m_worktreeCleanupReason = tr("Review its files before deciding whether to remove it.");
    ++m_worktreeCleanupRevision; updateWorktreeCleanupOffer();
}

void SessionsWindow::updateWorktreeCleanupOffer()
{
    if (!m_worktreeCleanup) return;
    if (!m_worktreeCleanupOffered || m_archivedWorktree.size() != 4 || m_archivedWorktree[2].isEmpty()) {
        m_worktreeCleanup->hide(); return;
    }
    const auto host = m_archivedWorktree[0], name = m_archivedWorktree[1], path = m_archivedWorktree[2];
    const auto inUse = [&](const SessionInfo &session) {
        if (session.state == "archived" || session.gitRoot != path) return false;
        // The successful command ended this exact run; its cached list row can
        // outlive the acknowledgement. Any other occupant keeps the folder in use.
        return session.name != name || m_archivedWorktreeRun.isEmpty() || session.runId != m_archivedWorktreeRun;
    };
    const auto *box = host.isEmpty() ? &m_fleet.local() : m_fleet.peer(host);
    if (box) for (const auto &session : box->sessions) if (inUse(session)) { m_worktreeCleanup->hide(); return; }
    for (const auto &entry : std::as_const(m_entries))
        if (entry.host == host && inUse(entry.session)) { m_worktreeCleanup->hide(); return; }
    // acceptInspection binds details to the selected host/name. Only its exact
    // current tracked run/conversation can add newer checkout occupancy here.
    const auto *current = selected();
    if (current && current->host == host && current->session.state != "archived"
        && !current->session.runId.isEmpty() && m_details.value("tracked").toBool()
        && m_details.value("run_id").toString() == current->session.runId
        && (current->session.conversationId.isEmpty()
            || m_details.value("conversation_id").toString() == current->session.conversationId)
        && m_details.value("git_root").toString() == path) {
        auto inspected = current->session; inspected.gitRoot = path;
        if (inUse(inspected)) { m_worktreeCleanup->hide(); return; }
    }
    const auto machine = host.isEmpty() ? m_fleet.local().host : host;
    const auto platform = MachinePlatform::of(m_fleet.metricsOf(host), host.isEmpty());
    const auto folder = MachinePlatform::leaf(path, platform);
    const auto shortFolder = m_worktreeCleanup->fontMetrics().elidedText(folder, Qt::ElideMiddle, 150);
    const auto label = (m_worktreeCleanupUnconfirmed ? tr("Cleanup unconfirmed: %1") : tr("Folder kept: %1")).arg(shortFolder);
    m_worktreeCleanup->setText(QString(label).replace('&', "&&"));
    const auto context = tr("Worktree cleanup for %1 on %2.\nFolder: %3\n%4")
        .arg(name, machine, MachinePlatform::native(path, platform), m_worktreeCleanupReason);
    m_worktreeCleanup->setToolTip(context);
    m_worktreeCleanup->setAccessibleName(tr("Review retained folder for %1 on %2").arg(name, machine));
    m_worktreeCleanup->setAccessibleDescription(context);
    m_worktreeCleanup->show();
}

void SessionsWindow::cleanUpArchivedWorktree()
{
    if (m_archivedWorktree.size() != 4) return;
    const auto offer = m_archivedWorktree; m_archivedWorktree.clear(); m_worktreeCleanupOffered = false; ++m_worktreeCleanupRevision; m_worktreeCleanup->hide();
    // The common directory still answers after the worktree folder is gone.
    WorktreeBatchCleanupDialog dialog(&m_client, {QJsonObject{{"host",offer[0]}, {"machine",offer[0].isEmpty() ? m_fleet.local().host : offer[0]},
        {"common_dir",offer[3].isEmpty() ? offer[2] : offer[3]}, {"cleanup_reason", m_worktreeCleanupReason},
        {"row",QJsonObject{{"path",offer[2]}, {"kind","linked"}}}}}, this);
    dialog.exec();
    if (dialog.changed()) { emit refreshRequested(); updateWorktrees(); }
}

void SessionsWindow::reviewTerminatedWorktrees(const QList<QStringList> &offers)
{
    QList<QJsonObject> entries;
    for(const auto &offer:offers) {
        if(offer.size()<4 || offer[2].isEmpty() || offer[3].isEmpty())continue;
        auto entry=QJsonObject{{"host",offer[0]},{"machine",offer[0].isEmpty()?m_fleet.local().host:offer[0]},
            {"common_dir",offer[3]},{"exit_cleanup",true},{"expected_owner_id",offer.value(4)},{"row",QJsonObject{{"path",offer[2]},{"kind","linked"}}}};
        const auto outcome=m_terminationCleanupResults.take(offer[0]+'\n'+offer[1]+'\n'+offer[2]);
        if(!outcome.isEmpty() && (outcome["common_dir"]!=offer[3] || outcome["status"]!="kept")) {
            entry["cleanup_status"]=outcome["common_dir"]==offer[3] && outcome["status"]=="removed"?QStringLiteral("removed"):QStringLiteral("unconfirmed");
            entry["cleanup_detail"]=outcome["reason"];
        }
        entries<<entry;
    }
    if(entries.isEmpty())return;
    m_terminationCleanup->add(entries);
}

void SessionsWindow::forkSession()
{
    const auto *entry = selected(); if (!entry || !m_forkAction->isEnabled() || m_pending) return;
    const Entry original = *entry; const QString conversation = m_details.value("conversation_id").toString();
    const QString run = m_details.value("run_id").toString(original.session.runId);
    const QString prefix = original.session.name.section('/', 0, 1) + '/';
    const auto occupied = [&](const QString &tag) {
        return std::any_of(m_entries.cbegin(), m_entries.cend(), [&](const Entry &e) {
            return e.host == original.host && e.session.name == prefix + tag;
        });
    };
    QString base = original.session.tag.isEmpty() ? QStringLiteral("fork") : original.session.tag + "-fork";
    QString tag = base; for (int i = 2; occupied(tag); ++i) tag = base + '-' + QString::number(i);
    QDialog dialog(this); dialog.setObjectName("forkSessionDialog"); dialog.setWindowTitle(tr("Fork session")); dialog.setMinimumWidth(440);
    auto *layout = new QVBoxLayout(&dialog); layout->setContentsMargins(24,22,24,22); layout->setSpacing(12);
    auto *heading = label(tr("Fork session"), "heading"); layout->addWidget(heading);
    auto *description = label(tr("Create an independent conversation from this saved context.\nWorking folder: %1").arg(selectedDirectory()), "muted");
    description->setWordWrap(true); layout->addWidget(description);
    auto *name = new QLineEdit(tag); name->setObjectName("forkSessionName"); name->setAccessibleName(tr("New session name")); layout->addWidget(name);
    auto *error = label({}, "hint"); error->setWordWrap(true); layout->addWidget(error);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel); auto *create = buttons->button(QDialogButtonBox::Ok); create->setText(tr("Fork session")); layout->addWidget(buttons);
    const auto validate = [&] { QString problem = SessionTag::problem(name->text()); if (problem.isEmpty() && occupied(name->text())) problem = tr("This session name is already in use."); error->setText(problem); create->setEnabled(problem.isEmpty()); };
    connect(name, &QLineEdit::textChanged, &dialog, validate); connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, [&] {
        if (LocalSessionHost::confirmLaunch(&dialog, m_fleet.local(), original.host)) dialog.accept();
    });
    validate(); name->selectAll(); name->setFocus();
    if (dialog.exec() != QDialog::Accepted || !SessionTag::problem(name->text()).isEmpty() || occupied(name->text())) return;
    // U1: the fleet polls while the dialog is open; say why nothing happens.
    if (!selectSessionEntry(original) || !m_forkAction->isEnabled()) {
        showNotice(tr("The session changed. Open its actions again."), true); return;
    }
    m_forkKey = original.host + '\n' + prefix + name->text(); m_forkGroup = m_organization.groupFor(original.identity);
    if (const auto *group = m_organization.group(m_forkGroup)) {
        const int index = group->sessions.indexOf(original.identity); if (index >= 0 && index + 1 < group->sessions.size()) m_forkBefore = group->sessions[index + 1];
    }
    m_pending = true; m_restoreKey.clear(); m_renameKey.clear();
    showNotice(tr("Forking session on %1…").arg(original.machine)); renderDetails();
    m_client.forkSession(original.host, original.session.name, name->text(), original.session.archiveId, run, conversation);
}

void SessionsWindow::renameSession()
{
    const auto *entry = selected();
    if (!entry || !entry->online || m_pending || !m_renameAction->isEnabled()) return;
    // Fleet polling continues while the dialog is open, so keep value copies.
    const Entry original = *entry;
    const bool archived = original.session.state == "archived";
    const QString prefix = original.session.name.section('/', 0, 1) + '/';
    const QString oldTag = original.session.name.section('/', 2);
    QDialog dialog(this); dialog.setObjectName("renameSessionDialog"); dialog.setWindowTitle(tr("Rename session"));
    dialog.setMinimumWidth(430); dialog.setModal(true);
    auto *layout = new QVBoxLayout(&dialog); layout->setContentsMargins(24, 22, 24, 22); layout->setSpacing(14);
    auto *heading = label(tr("Rename session"), "renameHeading");
    heading->setStyleSheet("font-size:20px; font-weight:600"); layout->addWidget(heading);
    auto *context = label(original.machine + QStringLiteral("  /  ") + prefix.chopped(1), "muted");
    context->setWordWrap(true); layout->addWidget(context);
    auto *caption = label(tr("Session name"), "renameCaption"); layout->addWidget(caption);
    auto *name = new QLineEdit(oldTag, &dialog); name->setObjectName("renameSessionName");
    name->setAccessibleName(tr("Session name")); name->setPlaceholderText(tr("e.g. release planning"));
    caption->setBuddy(name); layout->addWidget(name);
    auto *preview = label({}, "renameFullName"); preview->setWordWrap(true);
    preview->setStyleSheet(QString("color:%1; font-size:12px").arg(m_muted)); layout->addWidget(preview);
    auto *error = label({}, "renameError"); error->setWordWrap(true);
    error->setStyleSheet(QString("color:%1").arg(m_dark ? "#ff9298" : "#b52b3d")); layout->addWidget(error);
    // Typing changes content, never the form's geometry. Keep two measured lines
    // for both the full-name preview and validation, including the empty state.
    for (auto *message : {preview, error}) {
        message->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Fixed); message->ensurePolished();
        message->setFixedHeight(2 * message->fontMetrics().lineSpacing());
    }
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Save | QDialogButtonBox::Cancel, &dialog);
    auto *save = buttons->button(QDialogButtonBox::Save); save->setText(tr("Rename")); save->setObjectName("primary");
    save->style()->unpolish(save); save->style()->polish(save);
    save->setDefault(true); layout->addWidget(buttons);
    const auto problem = [this, original, archived, prefix](const QString &tag) {
        const QString invalid = SessionTag::problem(tag); if (!invalid.isEmpty()) return invalid;
        if (!archived) for (const auto &other : std::as_const(m_entries))
            if (other.host == original.host && other.key != original.key && other.session.state != "archived" && other.session.name == prefix + tag)
                return tr("This name is already in use on %1.").arg(original.machine);
        return QString();
    };
    const auto validate = [&]() {
        const QString invalid = problem(name->text());
        error->setText(invalid); error->setToolTip(invalid);
        preview->setText(prefix + name->text()); preview->setToolTip(preview->text());
        save->setEnabled(invalid.isEmpty() && name->text() != oldTag);
    };
    connect(name, &QLineEdit::textChanged, &dialog, validate);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    validate(); name->selectAll(); name->setFocus();
    if (dialog.exec() != QDialog::Accepted || name->text() == oldTag || !problem(name->text()).isEmpty()) return;
    // U1: the fleet polls while the dialog is open; say why nothing happens.
    if (!selectSessionEntry(original) || !selected()->online || m_pending || !m_renameAction->isEnabled()) {
        showNotice(tr("The session changed. Open its actions again."), true); return;
    }
    const QString newName = prefix + name->text();
    m_pending = true; m_restoreKey.clear(); m_renameArchived = archived;
    m_renameKey = original.host + '\n' + newName + (archived ? '\n' + original.session.archiveId : QString());
    showNotice(tr("Renaming session on %1…").arg(original.machine)); renderDetails();
    m_client.renameSession(original.host, original.session.name, newName, original.session.archiveId);
}

void SessionsWindow::stopTeamSession(const QString &host, const QString &name)
{
    // N7: the Team tab names a session, not the action. Select it, wait for the
    // inspection its decision needs, and let terminateSession show its own
    // confirmation (stop and archive, terminate without an archive, or forget
    // the binding). Reselecting clears m_details, so a decision made now would
    // be made on nothing.
    m_terminateAfterInspect.clear();
    const auto target = std::find_if(m_entries.cbegin(), m_entries.cend(), [&](const Entry &entry) {
        return entry.host == host && entry.session.name == name;
    });
    if (target == m_entries.cend()) {
        showNotice(tr("%1 is not listed here; open it to stop it").arg(name), true);
        return;
    }
    showSession(host, name);
    const auto *entry = selected();
    if (!entry || entry->host != host || entry->session.name != name) {
        showNotice(tr("%1 is not listed here; open it to stop it").arg(name), true);
        return;
    }
    m_terminateAfterInspect = entry->key;
    if (!m_details.isEmpty()) {   // this selection's details are already loaded
        m_terminateAfterInspect.clear();
        QTimer::singleShot(0, this, [this] { terminateSession(); });
        return;
    }
    inspect();
}

void SessionsWindow::terminateSession()
{
    const auto *e = selected(); if (!e || !e->online || isTerminating(*e) || m_pending || !m_renameKey.isEmpty() || m_forgetBindingRequest
        || !NativeSessionCapabilities::action(e->session,NativeSessionCapabilities::capability(e->session,NativeSessionCapabilities::endOperation(e->session,m_details)),m_details)) return;
    const Entry original = *e;
    const QString host = e->host, name = e->session.name, archiveId = e->session.archiveId;
    const bool archived = e->session.state == "archived";
    // OpenCode keeps its conversation; with a known identity only the binding goes.
    const auto run = m_details.value("run_id").toString(e->session.runId), conversation = m_details.value("conversation_id").toString(e->session.conversationId);
    const bool bindingOnly = !archived && NativeSessionCapabilities::forgetsBindingOnly(e->session,m_details);
    const bool scopedForget = bindingOnly && !run.isEmpty();
    const bool live = e->session.state == "running" && !bindingOnly;
    const bool keepArchive = live && e->session.tracked && QStringList{"codex","claude","kimi"}.contains(e->session.cmd);
    QMessageBox confirm(QMessageBox::Warning, live ? tr("Terminate session?") : archived ? tr("Forget archived session?") : tr("Forget saved session?"),
        live ? (keepArchive ? tr("Stop %1 and move its recorded conversation to Archive? Running processes will stop; the conversation history is kept.").arg(name) : tr("Terminate %1? Its running processes will stop. No HGS archive is available for this session; any native conversation history is kept.").arg(name))
             : archived ? tr("Remove this archived instance of %1 from HGS? Other sessions with this name and the agent’s native conversation history are kept.").arg(name)
             : bindingOnly ? tr("Forget %1 in Zerus? The conversation stays in OpenCode; only Zerus forgets it.").arg(name)
             : tr("Remove %1 from HGS? The agent’s native conversation history is kept.").arg(name), QMessageBox::Yes | QMessageBox::Cancel, this);
    confirm.setObjectName("terminateSessionConfirm"); confirm.setTextFormat(Qt::PlainText);
    confirm.setInformativeText(tr("Computer: %1").arg(original.machine));
    QCheckBox *removeWorktree=nullptr;
    const bool exactInspection=m_details.value("run_id").toString()==run
        && m_details.value("conversation_id").toString()==conversation;
    const bool linked=original.session.gitWorktree || (exactInspection && m_details.value("git_worktree").toBool());
    if(live && keepArchive && linked) {
        removeWorktree=new QCheckBox(tr("Remove the working folder after stopping"),&confirm);
        removeWorktree->setObjectName("removeTerminatedWorktree");
        removeWorktree->setChecked(Settings::value("workspace/removeWorktreeAfterTermination",false).toBool());
        removeWorktree->setToolTip(tr("Cleanup runs in the background. Unsaved work, unknown local files and folders in use are kept for review."));
        confirm.setCheckBox(removeWorktree);
        confirm.setInformativeText(tr("Computer: %1\nThe conversation and branch are kept. Folders that need review stay on disk.").arg(original.machine));
    }
    confirm.button(QMessageBox::Yes)->setText(live ? (keepArchive ? tr("Stop and archive") : tr("Terminate session"))
        : archived ? tr("Forget archived session") : tr("Forget saved session"));
    if(removeWorktree){
        const auto update=[&confirm](bool checked){confirm.button(QMessageBox::Yes)->setText(checked?tr("Stop and remove folder"):tr("Stop and archive"));};
        connect(removeWorktree,&QCheckBox::toggled,&confirm,update);update(removeWorktree->isChecked());
    }
    confirm.setDefaultButton(QMessageBox::Cancel); confirm.setEscapeButton(QMessageBox::Cancel);
    if (confirm.exec() != QMessageBox::Yes) return;
    // U1: the fleet polls while the dialog is open; a lost selection says so.
    if (!selectSessionEntry(original)) { showNotice(tr("The session changed. Open its actions again."), true); return; }
    const auto *current = selected();
    if (!current || !current->online || isTerminating(*current) || m_pending || !m_renameKey.isEmpty() || m_forgetBindingRequest) return;
    const bool currentBindingOnly = !archived && NativeSessionCapabilities::forgetsBindingOnly(current->session,m_details);
    if ((current->session.state == "running" && !currentBindingOnly) != live || currentBindingOnly != bindingOnly) {
        showNotice(tr("The session state changed. Open its actions again."), true); return;
    }
    if (scopedForget) {
        if (m_details.value("run_id").toString(current->session.runId) != run || m_details.value("conversation_id").toString(current->session.conversationId) != conversation) {
            showNotice(tr("The session state changed. Open its actions again."), true); return;
        }
        m_pending = true; m_restoreKey.clear(); m_forgetBindingName = name;
        m_forgetBindingRequest = m_client.requestForgetBinding(host, name, run, conversation);
        renderDetails(); return;
    }
    m_removeTerminatedWorktree=removeWorktree && removeWorktree->isChecked();
    m_archivedWorktreeOwnerId=original.session.worktreeOwnerId;
    if(removeWorktree)Settings::set("workspace/removeWorktreeAfterTermination",removeWorktree->isChecked());
    m_pending = true; m_restoreKey.clear();
    if(live){
        ++m_worktreeCleanupRevision; m_worktreeCleanupOffered = false; m_worktreeCleanup->hide();
        m_archivedWorktreeRun = original.session.runId;
        const bool inspected=exactInspection && m_details.value("git_worktree").toBool() && !m_details.value("git_root").toString().isEmpty();
        m_archivedWorktree = keepArchive ? QStringList{host,name,inspected ? m_details.value("git_root").toString() : original.session.gitWorktree ? original.session.gitRoot : QString(),
            inspected ? m_details.value("git_common_dir").toString() : original.session.gitWorktree ? original.session.gitCommonDir : QString()} : QStringList{};
        m_terminationFinished.remove(original.key);m_terminating.insert(original.key,original);m_terminationCommand=original.key;
        showNotice(tr("Terminating %1…").arg(name));rebuild();
    }
    if (archived) m_client.forgetArchive(host, name, archiveId);
    else if(keepArchive)m_client.terminateSession(host,name,original.session.runId);
    else m_client.killSession(host, name);
    renderDetails();
}

void SessionsWindow::placeRailControls(bool inTitle)
{
    // A collapsed list restores before the status bar exists; the theme places
    // the controls once the window is complete.
    if (inTitle == m_railInTitle || !m_railLayout || !m_statusLayout || !m_titleBar) return;
    m_railInTitle = inTitle;
    const QList<QPushButton *> leading{m_railSessionsToggle, m_railNewSession, m_sessionsNav, m_projectsNav, m_machinesNav, m_accountsNav, m_teamDesignerNav, m_teamWaitingNav, m_modelServersNav};
    const QList<QPushButton *> trailing{m_windowPin, m_settingsNav};
    QList<QPushButton *> all{m_brand}; all << leading << trailing;
    // Moving a widget hides it; each keeps whether it was shown.
    QHash<QWidget *, bool> hidden;
    for (auto *button : all) {
        if (!button->property("railSize").isValid()) { button->setProperty("railSize", button->maximumSize()); button->setProperty("railIcon", button->iconSize()); }
        hidden.insert(button, button->isHidden()); m_railLayout->removeWidget(button);
    }
    m_railLayout->removeWidget(m_versionButton); m_statusLayout->removeWidget(m_versionButton);
    if (inTitle) {
        // Compact controls: the mark leads the bar, navigation follows the name,
        // and the pin and Settings stand before the window buttons.
        m_titleBar->setMarkVisible(false);
        m_titleBar->markSlot()->addWidget(m_brand); m_brand->setFixedSize(30, 30);
        for (auto *button : leading) { m_titleBar->leading()->addWidget(button); button->setFixedSize(30, 28); button->setIconSize(QSize(17, 17)); }
        for (auto *button : trailing) { m_titleBar->trailing()->addWidget(button); button->setFixedSize(30, 28); button->setIconSize(QSize(17, 17)); }
        // U25: the prototype's title-bar gear is 19 px.
        m_settingsNav->setIconSize(QSize(19, 19));
        m_attentionBadge->setGeometry(16, 0, 14, 13);
        m_statusLayout->addWidget(m_versionButton); m_versionButton->setFixedSize(40, 22);
    } else {
        m_titleBar->setMarkVisible(true);
        // Back to the rail: the mark, its spacing, then the navigation; the pin and
        // Settings after the stretch, the version last.
        m_railLayout->insertWidget(0, m_brand);
        for (int at = 0; at < leading.size(); ++at) m_railLayout->insertWidget(2 + at, leading[at]);
        for (int at = 0; at < trailing.size(); ++at) m_railLayout->insertWidget(3 + leading.size() + at, trailing[at]);
        m_railLayout->addWidget(m_versionButton); m_versionButton->setFixedSize(42, 24);
        for (auto *button : all) { button->setFixedSize(button->property("railSize").toSize()); button->setIconSize(button->property("railIcon").toSize()); }
        m_attentionBadge->setGeometry(24, 3, 17, 16);
    }
    for (auto *button : all) button->setVisible(!hidden.value(button));
    m_versionButton->show();
    m_sidebar->setVisible(!inTitle);
}

bool SessionsWindow::nativeEvent(const QByteArray &eventType, void *message, qintptr *result)
{
    if (m_framelessMain && FramelessWindow::nativeEvent(this, m_titleBar, message, result)) return true;
    return QWidget::nativeEvent(eventType, message, result);
}

// WhiteWh fork: a skin with "window":{"main":"frameless"} replaces the system
// title bar with its own (Windows only; other systems keep theirs).
void SessionsWindow::applyWindowFrame()
{
#ifdef Q_OS_WIN
    const bool frameless = WorkspaceSkin::current().framelessMain();
#else
    const bool frameless = false;
#endif
    m_titleBar->setVisible(frameless);
    placeRailControls(frameless);
    m_titleBar->setShortcut(ShortcutRegistry::instance().keys(QStringLiteral("go.anywhere")).value(0).toString(QKeySequence::NativeText));
    m_titleBar->setMark(windowIcon().pixmap(18, 18));
    if (frameless == m_framelessMain) return;
    m_framelessMain = frameless;
    const bool shown = isVisible(); const auto geometry = saveGeometry();
    // Changing the flag recreates the native window, which then shows again.
    setWindowFlag(Qt::FramelessWindowHint, frameless);
    if (shown) { restoreGeometry(geometry); show(); }
    if (frameless && testAttribute(Qt::WA_WState_Created)) FramelessWindow::apply(this);
}

void SessionsWindow::showEvent(QShowEvent *event)
{
    QWidget::showEvent(event);
    if (m_framelessMain) FramelessWindow::apply(this); rebuild(); if (m_dashboardStale) updateDashboard();
    m_timer.start(); m_readTimer.start(); emit refreshRequested(); inspect(); m_tasks->setPolling(true);
}
// U25 part 6 (spec 6): links from outside (Open team panel, N iron rules,
// the git chips, the context and cache indicators) open the inspector and
// unfold the card they name.
void SessionsWindow::openInspectorCard(int card)
{
    if (!m_inspector) return;
    setInspectorVisible(true);
    m_inspector->open(InspectorCards::Card(card));
}

void SessionsWindow::updateEnvironmentCard()
{
    if (!m_inspector) return;
    const auto *entry = selected();
    const auto set = [this](const char *row, const QString &text) { if (auto *value = m_environment.value(QString::fromLatin1(row))) value->setValue(text); };
    // The Team card: the team's status and its open questions on the header.
    QString teamValue;
    if (entry && m_teams) if (const auto *team = m_teams->teamOfSession(entry->host, entry->session.name)) {
        teamValue = team->status.isEmpty() ? QString() : team->status.left(1).toUpper() + team->status.mid(1);
        const int answers = m_inspector->property("teamAnswers").toInt();
        if (answers > 0) teamValue += tr(" · ?%1").arg(answers);
    }
    m_inspector->setValue(InspectorCards::Team, teamValue);
    if (!entry) {
        for (auto *value : m_environment) value->setValue(QString());
        updateContextCard(); updateChangesCard(); return;
    }
    const auto &s = entry->session;
    set("machine", entry->machine + (entry->host.isEmpty() ? tr(" · this computer") : QString()));
    if (auto *key = m_inspector->body(InspectorCards::Environment)->parentWidget()->findChild<QLabel *>("inspectorEnvKey"))
        key->setText(entry->online ? tr("machine") : tr("offline"));
    const auto cwd = m_subagentId.isEmpty() ? m_details.value("cwd").toString(s.cwd) : m_subagentCwd;
    const auto groupId = m_organization.groupFor(entry->identity);
    const auto *group = m_organization.group(groupId);
    const QString project = group ? group->name : QString();
    set("folder", cwd.isEmpty() ? tr("Not reported") : (project.isEmpty() ? cwd : project + QStringLiteral(" · ") + cwd));
    const auto git = m_client.gitStatusSnapshot(entry->host, cwd);
    const QString branch = git.value("branch").toString();
    set("branch", !branch.isEmpty() ? branch : !s.gitRoot.isEmpty() ? projectContext(s) : tr("No Git branch"));
    set("model", m_model->text().isEmpty() ? tr("Not reported") : m_providerBadge->text().isEmpty() ? m_model->text()
                                                                  : m_providerBadge->text() + QStringLiteral(" · ") + m_model->text());
    const double at = m_details.value("last_event_at").toDouble();
    set("last event", at > 0 ? QDateTime::fromSecsSinceEpoch(qint64(at)).toLocalTime().toString("d MMM HH:mm") : tr("No events yet"));
    if (!cwd.isEmpty()) m_client.requestGitStatus(entry->host, cwd);
    updateContextCard(); updateChangesCard();
}

void SessionsWindow::updateContextCard()
{
    if (!m_contextUsed) return;
    const auto *entry = selected();
    const auto usage = (m_subagentId.isEmpty() ? m_details : m_subagentDetails).value("session_usage").toObject().value("context").toObject();
    const bool known = entry && usage.value("used").isDouble();
    const double used = usage.value("used").toDouble(), limit = usage.value("limit").toDouble();
    const auto compact = [](double n) { return n >= 1000 ? QString::number(qRound(n / 1000.0)) + QStringLiteral("k") : QString::number(qRound(n)); };
    const auto &skin = WorkspaceSkin::active();
    const auto token = [&skin, this](const char *name) { return WorkspaceSkin::color(skin, m_dark, QString::fromLatin1(name)); };
    if (!known) {
        m_contextUsed->setText(tr("Context not reported"));
        m_contextTokens->clear(); m_contextLeft->clear();
        m_contextBar->setParts({}, token("border"));
        m_inspector->setValue(InspectorCards::Context, QString());
    } else {
        const bool capacity = limit > 0;
        const int percent = capacity ? qRound(100.0 * used / limit) : -1;
        m_contextUsed->setText(capacity ? tr("%1% used").arg(percent) : tr("%1 used").arg(compact(used)));
        m_contextTokens->setText(capacity ? QStringLiteral("%1 / %2").arg(compact(used), compact(limit)) : compact(used));
        m_contextLeft->setText(capacity ? tr("%1 left in the window").arg(compact(qMax(0.0, limit - used))) : QString());
        m_contextBar->setParts(capacity ? QVector<ContextBar::Part>{{used / limit, token("accent")}} : QVector<ContextBar::Part>{}, token("border"));
        m_inspector->setValue(InspectorCards::Context, capacity ? QStringLiteral("%1%").arg(percent) : QString(), true);
    }
    // The category rows only when an agent reports them (none does today).
    const auto categories = usage.value("categories").toArray();
    auto *rows = m_contextRows->layout();
    while (auto *item = rows->takeAt(0)) { delete item->widget(); delete item; }
    m_contextRows->setVisible(!categories.isEmpty());
}

void SessionsWindow::updateChangesCard()
{
    if (!m_changesList) return;
    const auto *entry = selected();
    auto *list = m_changesList->layout();
    while (auto *item = list->takeAt(0)) { delete item->widget(); delete item; }
    const auto cwd = entry ? m_details.value("cwd").toString(entry->session.cwd) : QString();
    const auto git = entry ? m_client.gitStatusSnapshot(entry->host, cwd) : QJsonObject();
    const auto changes = git.value("changes").toObject();
    const bool known = git.value("state").toString() == "ok";
    const auto &skin = WorkspaceSkin::active();
    const auto token = [&skin, this](const char *name) { return WorkspaceSkin::color(skin, m_dark, QString::fromLatin1(name)).name(); };
    for (const auto &value : changes.value("files").toArray()) {
        const auto file = value.toObject();
        auto *row = new QWidget; row->setObjectName("inspectorChangeRow");
        auto *h = new QHBoxLayout(row); h->setContentsMargins(0, 0, 0, 0); h->setSpacing(6);
        auto *path = new InspectorValue(Qt::ElideMiddle); path->setObjectName("inspectorChangePath"); path->setValue(file.value("path").toString());
        auto *added = new QLabel(file.value("untracked").toBool() ? tr("new") : file.value("added").isDouble() ? QStringLiteral("+%1").arg(file.value("added").toInt()) : QStringLiteral("bin"));
        auto *deleted = new QLabel(file.value("deleted").isDouble() ? QStringLiteral("−%1").arg(file.value("deleted").toInt()) : QString());
        added->setObjectName("inspectorChangeAdded"); deleted->setObjectName("inspectorChangeDeleted");
        added->setStyleSheet(QStringLiteral("color:%1;").arg(token("accent"))); deleted->setStyleSheet(QStringLiteral("color:%1;").arg(token("danger")));
        h->addWidget(path, 1); h->addWidget(added); h->addWidget(deleted);
        list->addWidget(row);
    }
    if (changes.value("truncated").toBool()) {
        const int total = changes.value("tracked_files").toInt() + changes.value("untracked_files").toInt();
        list->addWidget(new QLabel(tr("%1 more files").arg(total - changes.value("files").toArray().size())));
    }
    const bool clean = known && git.value("changed_files").toInt() == 0;
    m_changesEmpty->setText(!entry ? QString() : clean ? tr("Working tree is clean.") : known ? QString() : tr("Git status not reported for this folder."));
    m_changesEmpty->setVisible(!m_changesEmpty->text().isEmpty());
    m_commitChanges->setEnabled(entry && !clean && known && m_subagentId.isEmpty());
    m_openDiff->setEnabled(entry && !clean && known);
    m_inspector->setValue(InspectorCards::Changes, changes.isEmpty() ? QString()
        : QStringLiteral("+%1 −%2").arg(changes.value("added").toInt()).arg(changes.value("deleted").toInt()));
}

void SessionsWindow::showDiff(const QJsonObject &data, const QString &error)
{
    if (!error.isEmpty() || data.value("state").toString() != "ok") {
        showNotice(data.value("state") == "too_large" ? tr("The diff is larger than 1 MiB; open it in Terminal.")
                                                      : tr("The diff could not be read: %1").arg(error.isEmpty() ? data.value("state").toString() : error), true);
        return;
    }
    auto *dialog = new QDialog(this);
    dialog->setObjectName("inspectorDiffDialog");
    dialog->setAttribute(Qt::WA_DeleteOnClose);
    dialog->setWindowTitle(tr("Diff · %1").arg(data.value("path").toString()));
    dialog->resize(820, 600);
    auto *v = new QVBoxLayout(dialog);
    auto *text = new QPlainTextEdit(data.value("diff").toString());
    text->setObjectName("inspectorDiffText"); text->setReadOnly(true); text->setLineWrapMode(QPlainTextEdit::NoWrap);
    text->setFont(QFontDatabase::systemFont(QFontDatabase::FixedFont));
    v->addWidget(text, 1);
    if (data.value("truncated").toBool())
        v->addWidget(new QLabel(tr("Showing the first 256 KiB of %1 KiB.").arg(data.value("bytes").toInt() / 1024)));
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Close);
    connect(buttons, &QDialogButtonBox::rejected, dialog, &QDialog::reject);
    v->addWidget(buttons);
    dialog->show();
}

bool SessionsWindow::eventFilter(QObject *watched, QEvent *event)
{
    if (m_workSplitter && watched == m_workSplitter->handle(1) && event->type() == QEvent::MouseButtonDblClick
        && static_cast<QMouseEvent *>(event)->button() == Qt::LeftButton) {
        m_inspectorWidth = DefaultInspectorWidth;
        Settings::set("workspace/inspectorWidth", m_inspectorWidth);
        applyInspectorWidth(); return true;
    }
    if (watched == m_workSplitter && event->type() == QEvent::Resize && !m_inspectorWidthPending) {
        m_inspectorWidthPending = true;
        QTimer::singleShot(0, this, [this] { m_inspectorWidthPending = false; applyInspectorWidth(); });
    }

    if (m_children && watched == m_children->viewport() && event->type() == QEvent::Resize)
        QTimer::singleShot(0, this, &SessionsWindow::renderDetails);
    if ((watched == m_meta || watched == m_title) && (event->type() == QEvent::Resize || event->type() == QEvent::FontChange)) updateHeaderText();
    if (watched == m_sessions && event->type() == QEvent::KeyPress) {
        const auto *key = static_cast<QKeyEvent *>(event);
        if (key->key() == Qt::Key_Menu || (key->key() == Qt::Key_F10 && key->modifiers().testFlag(Qt::ShiftModifier))) {
            if (const auto *item = m_sessions->currentItem())
                showSessionMenu(item->data(KeyRole).toString(), m_sessions->viewport()->mapToGlobal(m_sessions->visualItemRect(item).center()));
            return true;
        }
        if (key->key() == Qt::Key_Return || key->key() == Qt::Key_Enter) {
            jumpToLatestActivity(m_sessions->currentItem()); return true;
        }
    }
    return QWidget::eventFilter(watched, event);
}
void SessionsWindow::hideEvent(QHideEvent *event) { m_readTimer.stop(); m_readCandidate.clear(); m_timer.stop(); m_tasks->setPolling(false); m_terminal->disconnectSession(); QWidget::hideEvent(event); }
void SessionsWindow::closeEvent(QCloseEvent *event)
{
    Settings::set("sessions/geometry", saveGeometry()); Settings::set("workspace/splitter", m_splitter->saveState());
    Settings::set("workspace/sessionsWidth", m_sessionDock->dockedWidth());
    QWidget::closeEvent(event);
}
void SessionsWindow::changeEvent(QEvent *event)
{
    QWidget::changeEvent(event);
    if (event->type() == QEvent::WindowStateChange && m_brand) m_brand->syncAnimation();
    if (event->type() == QEvent::WindowStateChange && m_titleBar) m_titleBar->setMaximized(isMaximized());
    // A runtime light/dark switch of the system reaches a window only as PaletteChange:
    // QWidget::event() consumes ApplicationPaletteChange. The window's own stylesheet
    // also sends PaletteChange, so re-theme later and only when Follow system appearance
    // now picks the other theme.
    if (event->type() == QEvent::PaletteChange && m_activityView)
        QTimer::singleShot(0, this, [this] {
            if (Settings::value("workspace/theme", "system").toString() != "system") return;
            if ((qApp->palette().color(QPalette::Window).lightness() < 128) != m_dark) applyTheme();
        });
}

void SessionsWindow::updateWorktrees()
{
    if(!m_worktrees||!m_inspectorPanel||!m_inspector)return;
    const auto *entry=selected();
    m_worktrees->setContext(entry?entry->host:QString(),entry?(m_subagentId.isEmpty()?m_details.value("cwd").toString(entry->session.cwd):m_subagentCwd):QString(),m_fleet);
    // WhiteWh fork: SocratiCode is an optional extension; off, its panel has no folder and hides.
    if(m_socratiCode){const bool on=entry&&Extensions::enabled(QStringLiteral("socraticode"));m_socratiCode->setContext(on?entry->host:QString(),on?m_details.value("cwd").toString(entry->session.cwd):QString());
        // The SocratiCode card exists only while the extension is on.
        m_inspector->card(InspectorCards::SocratiCode)->setVisible(Extensions::enabled(QStringLiteral("socraticode")));}
    if(m_ironRules){
        const auto project=entry?m_organization.groupFor(entry->identity):QString();
        const auto *group=m_organization.group(project);
        m_ironRules->setProject(project,group?group->name:project);
        m_composer->setIronRulesCount(m_ironRules->rules().size());
    }
    updateLeadPanel();
    updateTeamPanel();
}
// WhiteWh fork: the Team tab follows the selected session's team, unless a
// team header in the list was chosen.
void SessionsWindow::updateProposalCards()
{
    // U57: the lead's own session shows its team's open proposals.
    if(!m_proposalCards||!m_teams)return;
    const auto *entry=selected();
    const auto *team=entry?m_teams->teamOfSession(entry->host,entry->session.name):nullptr;
    const auto *member=team?team->memberOfSession(entry->host,entry->session.name):nullptr;
    const bool lead=member&&m_subagentId.isEmpty()&&(member->lead||member->id==team->lead);
    // The watched copy is the newer one when it is this team.
    if(lead)if(const auto *watched=m_teams->watched();watched&&watched->id==team->id)team=watched;
    m_proposalCards->setTeam(lead?team:nullptr);
}

void SessionsWindow::updateTeamPanel()
{
    const SlowEventLog::Scope scope(this,"team panel");
    if(!m_teamPanel||!m_teams)return;
    updateProposalCards();
    if(m_teamStrip){
        const auto *entry=selected();
        const auto *found=entry?m_teams->teamOfSession(entry->host,entry->session.name):nullptr;
        const auto *member=found?found->memberOfSession(entry->host,entry->session.name):nullptr;
        m_teamStrip->setVisible(member);
        if(member){
            QString text=tr("Team <b>%1</b> · %2").arg(found->name.toHtmlEscaped(),found->roleOf(member->id).toHtmlEscaped());
            for(const auto &task:found->tasks){
                const auto *attempt=task.last();
                const bool current=task.assignee==member->id&&!task.finished()&&task.status!="pending"&&task.status!="draft";
                if(current||(member->lead&&task.status=="submitted")){
                    text+=tr(" · Task: <a href='task:%1' style='color:%2;'><b>#%1 %3</b></a>").arg(task.id).arg(m_accent,task.title.toHtmlEscaped());
                    Q_UNUSED(attempt);break;
                }
            }
            m_teamStripText->setText(text);
            QColor fill(m_accent);fill.setAlphaF(m_dark?.10:.07);
            m_teamStrip->setStyleSheet(QStringLiteral("QFrame#teamStrip{background:%1;border:1px solid %2;border-radius:8px;} QPushButton#teamStripOpen{color:%3;border:none;background:transparent;}")
                .arg(fill.name(QColor::HexArgb),[this]{QColor edge(m_accent);edge.setAlphaF(.45);return edge.name(QColor::HexArgb);}(),m_accent));
        }
    }
    // The Team designer's session shows the catalog draft it proposes.
    {const auto *entry=selected();
     const bool designer=m_selectedTeam.isEmpty()&&entry&&Teams::isDesignerSession(entry->host,entry->session.name);
     m_teamPanel->showDesigner(designer);
     // N12 (#382): no team's header (pill, Pause, Stop) stays on the designer.
     if(designer){
         if(m_teamWorkspace)m_teamWorkspace->setHeaderShown(false);
         if(m_teamHeader){m_teamHeader=false;updateHeaderText();}
         setNewTeamTab(false);return;
     }}
    QString team=m_selectedTeam;
    if(team.isEmpty()||!m_teams->team(team)){
        m_selectedTeam.clear();
        const auto *entry=selected();
        const auto *found=entry?m_teams->teamOfSession(entry->host,entry->session.name):nullptr;
        team=found?found->id:QString();
    }
    // F5: a draft is not a team yet; its lead's New team tab shows it.
    const auto *teamRecord=team.isEmpty()?nullptr:m_teams->team(team);
    const QString draft=teamRecord&&teamRecord->draft()?team:QString();
    if(!draft.isEmpty())team.clear();
    m_teamPanel->showTeam(team);
    m_teamPanel->refresh();   // #706: only when what it shows changed
    if(m_teamWorkspace){
        QList<TeamWorkspace::Computer> computers;
        const qint64 now=QDateTime::currentMSecsSinceEpoch();
        for(const auto &alias:m_fleet.peerNames()){
            const auto *box=m_fleet.peer(alias);
            computers.append({alias,alias,box&&box->ok&&now-m_fleet.peerPolledAt(alias)<FleetState::kPeerStaleMs});
        }
        m_teamWorkspace->setComputers(computers);
        // U25 (spec 3): only the team's lead session carries it in its header.
        {const auto *entry=selected();
         const auto *record=team.isEmpty()?nullptr:m_teams->team(team);
         const auto *member=record&&entry?record->memberOfSession(entry->host,entry->session.name):nullptr;
         const bool lead=member&&member->lead&&record->status!="archived"&&entry->session.state!="archived";
         m_teamWorkspace->setHeaderShown(lead);
         if(lead!=m_teamHeader){m_teamHeader=lead;updateHeaderText();}}
        m_teamWorkspace->showTeam(team);
        const int index=m_detailTabs->indexOf(m_teamWorkspace);
        if(team.isEmpty()&&index>=0){if(m_detailTabs->currentWidget()==m_teamWorkspace)m_detailTabs->setCurrentIndex(0);m_detailTabs->removeTab(index);m_teamWorkspace->hide();}
        else if(!team.isEmpty()&&index<0)m_detailTabs->addTab(m_teamWorkspace,workspaceIcon("team",QColor(m_dark?"#c4cad1":"#40505f")),tr("Team"));
    }
    // New team while the selected local session has no team; it leads the team.
    const auto *entry=selected();
    setNewTeamTab(m_teams->available()&&team.isEmpty()&&(m_selectedTeam.isEmpty()||!draft.isEmpty())&&entry&&entry->host.isEmpty()&&entry->online&&entry->session.state!="archived");
    if(m_teamNewForm&&!draft.isEmpty()&&entry&&m_teamNewForm->leadSession()==entry->session.name)m_teamNewForm->openDraft(draft);
    if(!team.isEmpty()&&team==m_teamOpenAfterCreate){m_teamOpenAfterCreate.clear();m_detailTabs->setCurrentWidget(m_teamWorkspace);}
    if(m_teamDesignerPage&&m_teamDesignerPage->isVisible())m_teamDesignerPage->refresh();
    if(m_teamNewForm&&entry&&m_teamNewForm->leadSession()==entry->session.name){
        Teams::Member self;self.session=entry->session.name;m_teamNewForm->setCoordinatorBusy(teamLive(self).working);
    }
}
void SessionsWindow::setNewTeamTab(bool shown)
{
    if(!m_teamNewTab)return;
    const int index=m_detailTabs->indexOf(m_teamNewTab);
    if(!shown&&index>=0){if(m_detailTabs->currentWidget()==m_teamNewTab)m_detailTabs->setCurrentIndex(0);m_detailTabs->removeTab(index);m_teamNewTab->hide();}
    else if(shown&&index<0)m_detailTabs->addTab(m_teamNewTab,workspaceIcon("add",QColor(m_dark?"#c4cad1":"#40505f")),tr("New team"));
    else if(shown&&m_detailTabs->currentWidget()==m_teamNewTab)ensureTeamNewForm();
}
Teams::Live SessionsWindow::teamLive(const Teams::Member &member) const
{
    Teams::Live live;
    for(const auto &entry:std::as_const(m_entries))if(entry.host==member.host&&entry.session.name==member.session&&entry.session.state!="archived"){
        const auto &s=entry.session;
        live.known=true;live.offline=!entry.online;
        live.attention=entry.online&&s.needsAction();
        live.working=SessionPresentation::working(s,entry.online);
        live.attentionText=s.activitySummary.isEmpty()?s.toolDetail:s.activitySummary;
        return live;
    }
    return live;
}
TeamNewDialog *SessionsWindow::teamNewForm(QWidget *parent)
{
    QList<TeamNewDialog::Computer> computers{{QString(),tr("this computer"),true}};
    const qint64 now=QDateTime::currentMSecsSinceEpoch();
    for(const auto &alias:m_fleet.peerNames()){
        const auto *box=m_fleet.peer(alias);
        computers.append({alias,alias,box&&box->ok&&now-m_fleet.peerPolledAt(alias)<FleetState::kPeerStaleMs});
    }
    QList<TeamNewDialog::Session> sessions;
    // F4: the Team designer is offered to no team.
    for(const auto &e:std::as_const(m_entries))if(e.session.state!="archived"&&e.online&&!Teams::isDesignerSession(e.host,e.session.name))sessions.append({e.host,e.session.name,e.session.cmd});
    const auto *entry=selected();
    QString folder,project;
    if(entry){if(entry->host.isEmpty())folder=entry->session.cwd;project=m_organization.groupFor(entry->identity);
        if(const auto *group=m_organization.group(project))project=group->name;}
    return new TeamNewDialog(m_teams,&m_client,computers,sessions,folder,project,parent);
}
void SessionsWindow::ensureTeamNewForm()
{
    const auto *entry=selected();
    if(!m_teams||!m_teamNewTab||!entry||!entry->host.isEmpty())return;
    if(m_teamNewForm&&m_teamNewForm->leadSession()==entry->session.name)return;
    if(!m_teams->catalogReady()){
        // The form fills its templates and roles from the catalog.
        if(m_teamNewWaiting)return;
        m_teamNewWaiting=true;
        auto *once=new QMetaObject::Connection;
        *once=connect(m_teams,&TeamController::catalogChanged,this,[this,once]{disconnect(*once);delete once;m_teamNewWaiting=false;ensureTeamNewForm();});
        m_teams->refreshCatalog();return;
    }
    if(m_teamNewForm){m_teamNewForm->hide();m_teamNewForm->deleteLater();}
    QList<TeamNewTab::Computer> computers{{QString(),tr("this computer"),true}};
    const qint64 now=QDateTime::currentMSecsSinceEpoch();
    for(const auto &alias:m_fleet.peerNames()){
        const auto *box=m_fleet.peer(alias);
        computers.append({alias,alias,box&&box->ok&&now-m_fleet.peerPolledAt(alias)<FleetState::kPeerStaleMs});
    }
    Teams::Member self;self.session=entry->session.name;
    const auto live=teamLive(self);
    const TeamNewTab::Coordinator coordinator{entry->session.name,entry->session.cmd,entry->session.model,entry->session.accountId,
        entry->session.cwd,entry->machine,SessionPresentation::working(entry->session,entry->online)?tr("Working"):tr("Ready"),live.working};
    QString project=m_organization.groupFor(entry->identity);
    if(const auto *group=m_organization.group(project))project=group->name;
    m_teamNewForm=new TeamNewTab(m_teams,&m_client,computers,coordinator,project,m_teamNewTab);
    m_teamNewForm->setDark(m_dark);
    m_teamNewForm->askDesigner=[this]{showTeamDesigner();};
    // A discarded draft leaves the session without a team: the tab starts over.
    m_teamNewForm->discarded=[this](const QString &){m_teams->refresh();updateTeamPanel();};
    m_teamNewForm->created=[this](const QString &team){
        m_teamOpenAfterCreate=team;m_teams->refresh();
        // A session whose team ends gets a fresh form.
        m_teamNewForm->deleteLater();m_teamNewForm=nullptr;
        // The list may carry the team already; a refresh that finds nothing new
        // reports nothing, so open its Team tab now if it is there.
        updateTeamPanel();
    };
    m_teamNewTab->layout()->addWidget(m_teamNewForm);
    m_teamNewForm->show();
    // F5: the session leads a draft: the tab shows it again.
    if(const auto *team=m_teams->teamOfSession(entry->host,entry->session.name);team&&team->draft())m_teamNewForm->openDraft(team->id);
}
void SessionsWindow::showNewTeam(const QString &host, const QString &name)
{
    if(!m_teams)return;
    if(m_teamNewTab&&m_detailTabs->indexOf(m_teamNewTab)>=0){
        m_pages->setCurrentIndex(0);m_detailTabs->setCurrentWidget(m_teamNewTab);
        if(!name.isEmpty())showNotice(tr("%1 is offered as the lead in the form above.").arg(name));
        return;
    }
    if(!m_teams->catalogReady()){
        auto *once=new QMetaObject::Connection;
        *once=connect(m_teams,&TeamController::catalogChanged,this,[this,once,host,name]{disconnect(*once);delete once;showNewTeam(host,name);});
        m_teams->refreshCatalog();return;
    }
    std::unique_ptr<TeamNewDialog> dialog(teamNewForm(this));
    // F7: "Take into a new team" names the session it came from; the dialog's
    // only existing-session slot is the lead, so it is preselected there.
    if(!name.isEmpty())dialog->preselectExistingLead(host,name);
    if(dialog->exec()!=QDialog::Accepted)return;
    m_teams->refresh();
    showTeam(dialog->createdTeam());
}
void SessionsWindow::openTeamTask(int task)
{
    const auto *entry=selected();
    const auto *team=entry&&m_teams?m_teams->teamOfSession(entry->host,entry->session.name):nullptr;
    if(!team)return;
    showTeam(team->id);
    m_teamPanel->openTask(task);
}
void SessionsWindow::showTeamAttention(const QString &token)
{
    const auto team=TeamAttentionNotices::teamOf(token);
    m_pendingTeamAttention.clear();
    if(team.isEmpty()||!m_teams)return;
    // N32: a notification raised before this window existed can be clicked
    // before its first team list: open the team once it is read.
    if(!m_teams->team(team)){m_pendingTeamAttention=token;return;}
    showTeam(team);
    if(m_teamPanel&&m_teamPanel->openWorkspace)m_teamPanel->openWorkspace();
}
void SessionsWindow::showTeam(const QString &team)
{
    m_selectedTeam=team;
    if(!m_teamPanel)return;
    openInspectorCard(InspectorCards::Team);
    updateTeamPanel();
}
void SessionsWindow::filterFolder(const QString &host,const QString &path,bool checkout)
{
    m_projectFilters.clear(); m_accountFilters.clear(); m_folderFilterHost=host;m_folderFilterPath=path;m_folderFilterCheckout=checkout;
    if(path.isEmpty()){m_filter="all";m_hostFilters.clear();m_search->clear();}
    showSessionList();
}
void SessionsWindow::openRelatedSession(const QString &host,const QString &name,const QString &archive)
{
    m_folderFilterPath.clear();m_filter=archive.isEmpty()?"all":"archived";m_hostFilters.clear(); m_projectFilters.clear(); m_accountFilters.clear();m_search->clear();showSessionList();
    const auto key=host+'\n'+name+(archive.isEmpty()?QString():'\n'+archive);revealSession(key);
    revealRow(key,true);
}
// WhiteWh fork: agent teams. The Team designer mode shows the catalog and the
// designer session, which proposes catalog changes; its draft waits for Apply.
QString SessionsWindow::teamDesignerSession() const
{
    // One designer conversation, outside the projects: hgs starts it in its own folder.
    for(const auto &e:std::as_const(m_entries))if(e.session.state=="running"&&Teams::isDesignerSession(e.host,e.session.name))return e.session.name;
    return {};
}
void SessionsWindow::showTeamDesigner()
{
    if(!m_teams)return;
    if(!m_teamDesignerPage){
        // Made on first use: the page reads the catalog while it shows.
        m_teamDesignerPage=new TeamDesignerPage(m_teams);m_teamDesignerPage->setDark(m_dark);
        m_teamDesignerPage->designerSession=[this]{return teamDesignerSession();};
        m_teamDesignerPage->designerLive=[this]{Teams::Member member;member.session=teamDesignerSession();return teamLive(member);};
        m_teamDesignerPage->startDesigner=[this](const QString &folder,const QString &goal,const QJsonObject &agent){startTeamDesigner(folder,goal,agent);};
        m_teamDesignerPage->openDesigner=[this](const QString &name){showSession({},name);openInspectorCard(InspectorCards::Team);};
        m_teamDesignerPage->openTerminal=[this](const QString &host,const QString &name){openSessionTerminal(host,name);};
        m_teamDesignerPage->confirm=[this](const QString &name,const QString &title,const QString &text,const QString &action){return m_teamPanel->confirm(name,title,text,action);};
        m_teamDesignerPage->editor()->projectRules=[this]{return teamProjectRules();};
        m_teamDesignerPageIndex=m_pages->addWidget(m_teamDesignerPage);
    }
    // The designer follows the project of the session selected when the mode opens.
    if(const auto *entry=selected();entry&&entry->host.isEmpty()){
        m_teamDesignerFolder=entry->session.cwd;m_teamDesignerProject=entry->session.name.section('/',1,1);
    }
    m_teamDesignerPage->setContext(m_teamDesignerFolder,m_teamDesignerProject);
    m_pages->setCurrentWidget(m_teamDesignerPage);
    m_teamDesignerPage->refresh();
}
// U60 (#656): the Model servers tab, made on first use; it reads the servers
// in the background only while it shows.
void SessionsWindow::showModelServers()
{
    if(!m_modelServers)return;
    if(!m_modelServersPage){
        m_modelServersPage=new ModelServersPage(m_modelServers);m_modelServersPage->setDark(m_dark);
        m_modelServersPage->machines=[this]{return m_fleet.peerNames();};
        m_modelServersPage->folders=[this]{
            // This computer's project folders, the selected session's first.
            QStringList folders;
            if(const auto *entry=selected();entry&&entry->host.isEmpty()&&!entry->session.cwd.isEmpty())folders<<entry->session.cwd;
            for(const auto &group:m_organization.groups())for(const auto &folder:group.folders)
                if(folder.machine==m_fleet.local().host&&!folders.contains(folder.path))folders<<folder.path;
            return folders;
        };
        m_modelServersPage->editServers=[this](const QString &server){
            m_pages->setCurrentIndex(5);m_settingsPage->openSection(m_modelServerSettingsSection);
            if(server.isEmpty()&&m_modelServers->servers().isEmpty())m_modelServerSettings->addServer();else m_modelServerSettings->showServer(server);
        };
        m_modelServersPage->startSession=[this](const QString &agent,const QString &folder,const QString &tag,const QString &server,const QString &model){
            const auto launch=QUuid::createUuid().toString(QUuid::WithoutBraces);
            showNotice(tr("Starting %1 on %2…").arg(agent,server));
            m_client.launchModelServerSession(agent,QDir::toNativeSeparators(folder),tag,server,model,launch);
        };
        m_modelServersPageIndex=m_pages->addWidget(m_modelServersPage);
    }
    m_pages->setCurrentWidget(m_modelServersPage);
    m_modelServersNav->setChecked(true);
}
// U50: the waiting list of every team; a row opens its team's Team tab, a
// draft phase unfolded.
void SessionsWindow::showTeamWaiting()
{
    if(!m_teams)return;
    if(!m_teamWaitingPage){
        m_teamWaitingPage=new TeamWaitingPage(m_teams);m_teamWaitingPage->setDark(m_dark);
        m_teamWaitingPage->openTeam=[this](const TeamWaitingPage::Item &item){
            m_pages->setCurrentIndex(0);showTeam(item.team);
            if(m_teamPanel&&m_teamPanel->openWorkspace)m_teamPanel->openWorkspace();
            if(item.phase>0&&item.kind=="phase_draft"&&m_teamWorkspace)m_teamWorkspace->showPhase(item.phase);
        };
        m_teamWaitingPage->answered=[this](const TeamWaitingPage::Item &item){m_waitingFeed->answered(item);};
        m_teamWaitingPage->countChanged=[this](int total){
            m_teamWaitingNav->setToolTip(total>0?tr("Waiting for you: %1").arg(total):tr("Waiting for you"));
        };
        m_teamWaitingPageIndex=m_pages->addWidget(m_teamWaitingPage);
    }
    m_pages->setCurrentWidget(m_teamWaitingPage);
    m_teamWaitingNav->setChecked(true);
}
// U55: a banner's button opens the team at its item: a draft phase unfolded,
// a task's card; the owner decides there with the usual confirmations.
void SessionsWindow::openWaitingItem(const QString &team,const QString &kind,int phase,int task)
{
    if(!m_teams||team.isEmpty())return;
    m_pages->setCurrentIndex(0);showTeam(team);
    if(m_teamPanel&&m_teamPanel->openWorkspace)m_teamPanel->openWorkspace();
    if(phase>0&&kind=="phase_draft"&&m_teamWorkspace)m_teamWorkspace->showPhase(phase);
    else if(task>0&&m_teamPanel)m_teamPanel->openTask(task);
}
void SessionsWindow::startTeamDesigner(const QString &folder,const QString &goal,const QJsonObject &agent)
{
    if(!m_teams)return;
    showNotice(tr("Starting the Team designer…"));
    QJsonObject input=agent;input.insert("folder",QDir::toNativeSeparators(folder));input.insert("goal",goal);
    m_teams->call(QStringLiteral("catalog designer"),input,
        [this](bool ok,const QJsonObject &result,const QString &error){
            if(!ok){showNotice(tr("The Team designer did not start: %1").arg(error),true);if(m_teamDesignerPage){m_teamDesignerPage->startFailed(error);m_teamDesignerPage->refresh();}return;}
            const auto name=result.value("name").toString();
            showNotice(tr("Team designer started%1").arg(name.isEmpty()?QString():QStringLiteral(": ")+name));
            // The page shows its conversation once the listing has it.
            auto *tries=new int(0);auto *timer=new QTimer(this);timer->setInterval(1000);
            connect(timer,&QTimer::timeout,this,[this,timer,tries]{
                if(!teamDesignerSession().isEmpty()||++*tries>30){if(m_teamDesignerPage)m_teamDesignerPage->refresh();timer->stop();timer->deleteLater();delete tries;}
            });
            timer->start();
        });
}
