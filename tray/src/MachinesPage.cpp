#include "Settings.h"
#include "SettingsJson.h"
#include "MachinesPage.h"
#include <QScopedValueRollback>
#include "HgsClient.h"
#include "PhonePairing.h"
#include "MachineAppearance.h"
#include "LocalSessionHost.h"
#include "MachinePlatform.h"
#include "WorkspaceIcons.h"
#include "WorkspacePageHeader.h"
#include "IdentityBadge.h"
#include "WorkspaceList.h"
#include "WorkspaceColorDialog.h"
#include "PageCards.h"
#include "WorkspaceSkin.h"
#include <QStyle>
#include <QStyledItemDelegate>
#include <QCheckBox>
#include <QComboBox>
#include <QSignalBlocker>
#include <QDateTime>
#include <QDialog>
#include <QDialogButtonBox>
#include <QFileDialog>
#include <QFormLayout>
#include <QHBoxLayout>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMessageBox>
#include <QPainter>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QRegularExpression>
#include <QScrollArea>
#include <QShowEvent>
#include <QSpinBox>
#include <QSysInfo>
#include <QTimer>
#include <QVBoxLayout>

namespace {
// #647 (prototype, Machines): a row is the monitor icon with its status dot
// (green connected, red not connected, grey off), the machine's label and a
// line under it; the chosen row has an accent tint and edge.
enum MachineRole { DirtyRole = Qt::UserRole + 1, StateRole };
class MachineDelegate final : public QStyledItemDelegate {
public:
    using QStyledItemDelegate::QStyledItemDelegate;
    QSize sizeHint(const QStyleOptionViewItem &, const QModelIndex &) const override { return {220, 52}; }
    void paint(QPainter *p, const QStyleOptionViewItem &o, const QModelIndex &index) const override {
        p->save(); p->setClipRect(o.rect); p->setRenderHint(QPainter::Antialiasing);
        const bool dark = o.widget->property("hgsDark").toBool();
        const bool selected = o.state & QStyle::State_Selected, hovered = o.state & QStyle::State_MouseOver;
        const QRectF frame = QRectF(o.rect).adjusted(2.5, 2.5, -2.5, -2.5);
        QColor accent = PageCards::token(dark, "accent");
        if (selected) {
            QColor fill = accent; fill.setAlphaF(dark ? .13 : .10); QColor edge = accent; edge.setAlphaF(.55);
            p->setPen(QPen(edge, 1)); p->setBrush(fill); p->drawRoundedRect(frame, 8, 8);
        } else if (hovered) { p->setPen(Qt::NoPen); p->setBrush(PageCards::token(dark, "hover")); p->drawRoundedRect(frame, 8, 8); }
        const QString text = index.data(Qt::DisplayRole).toString(), machine = text.section('\n', 0, 0);
        const QRect r = o.rect.adjusted(12, 7, -12, -7);
        // The monitor in its tile, the state dot on the tile's corner.
        const QRectF tile(r.x(), r.y() + (r.height() - 22) / 2.0, 22, 22);
        p->setPen(QPen(PageCards::token(dark, "border"), 1)); p->setBrush(PageCards::token(dark, "surface")); p->drawRoundedRect(tile, 6, 6);
        const QColor iconColor = PageCards::token(dark, "text2");
        workspaceIcon("machines", iconColor).paint(p, tile.adjusted(4, 4, -4, -4).toRect());
        const QString state = index.data(StateRole).toString();
        if (!state.isEmpty()) {
            const QColor dot = state == "ok" ? PageCards::token(dark, "accent") : state == "off" ? PageCards::token(dark, "danger")
                             : state == "new" ? PageCards::token(dark, "warn") : PageCards::token(dark, "dim");
            p->setPen(QPen(PageCards::token(dark, "page"), 1.5)); p->setBrush(dot);
            p->drawEllipse(QRectF(tile.right() - 5.5, tile.bottom() - 5.5, 7.5, 7.5));
        }
        const int left = r.x() + 32, width = r.right() - left;
        const int badge = IdentityBadges::width(IdentityBadges::Machine, machine, o.font, width);
        IdentityBadges::paint(p, QRect(left, r.y() + 1, badge, 18), IdentityBadges::Machine, machine, dark);
        // U25 (spec 8): the amber dot of a machine with unsaved edits.
        if (index.data(DirtyRole).toBool()) {
            p->setPen(Qt::NoPen); p->setBrush(PageCards::token(dark, "warn"));
            p->drawEllipse(QRectF(left + badge + 7, r.y() + 6.5, 7, 7));
        }
        QFont font = o.font; font.setPixelSize(11); p->setFont(font); p->setPen(PageCards::token(dark, "muted"));
        p->drawText(QRect(left, r.y() + 21, width, 16), Qt::AlignVCenter, QFontMetrics(font).elidedText(text.section('\n', 1), Qt::ElideRight, width));
        p->restore();
    }
};

// The header's machine label: the label's own colors at the header's size.
class MachineTitle final : public QLabel {
public:
    MachineTitle() { setObjectName("machineTitle"); setTextFormat(Qt::PlainText); setFixedHeight(26); setSizePolicy(QSizePolicy::Maximum, QSizePolicy::Fixed); }
    void setMachine(const QString &value, bool dark) { m_dark = dark; setText(value); setAccessibleName(value); setToolTip(value); updateGeometry(); update(); }
    QSize sizeHint() const override { QFont f = font(); f.setPixelSize(14); f.setWeight(QFont::DemiBold); return {qMin(320, QFontMetrics(f).horizontalAdvance(text()) + 20), 26}; }
    QSize minimumSizeHint() const override { return {qMin(sizeHint().width(), 60), 26}; }
protected:
    void paintEvent(QPaintEvent *) override {
        if (text().isEmpty()) return;
        QPainter p(this); p.setRenderHint(QPainter::Antialiasing);
        const QColor color = MachineAppearance::color(text()); const bool bright = MachineAppearance::vivid(text());
        p.setPen(Qt::NoPen); p.setBrush(MachineAppearance::backgroundColor(color, m_dark, bright)); p.drawRoundedRect(QRectF(rect()), 5, 5);
        QFont f = font(); f.setPixelSize(14); f.setWeight(QFont::DemiBold); p.setFont(f);
        p.setPen(MachineAppearance::textColor(color, m_dark, bright));
        p.drawText(rect().adjusted(10, 0, -10, 0), Qt::AlignVCenter | Qt::AlignLeft, QFontMetrics(f).elidedText(text(), Qt::ElideMiddle, width() - 20));
    }
private:
    bool m_dark = false;
};

// "Windows 11 · x64": the system as the GUI or a check found it.
QString archName(QString arch)
{
    arch = arch.trimmed();
    if (arch == "x86_64" || arch == "amd64" || arch == "AMD64") return QStringLiteral("x64");
    if (arch == "aarch64") return QStringLiteral("arm64");
    return arch;
}
QString osName(const QString &os)
{
    if (os == "Darwin" || os == "macos") return QStringLiteral("macOS");
    if (os == "Windows_NT" || os == "windows") return QStringLiteral("Windows");
    if (os == "linux") return QStringLiteral("Linux");
    return os;
}
}

MachinesPage::MachinesPage(const QString &path, QWidget *parent) : QWidget(parent), m_client(new HgsClient(path, this))
{
    setObjectName("machinesPage");
    SettingsJson::warm(this, {QStringLiteral("machines/otherTransport/*")});
    // #647 (prototype, Machines): the machine list is a column of its own with
    // its title, Refresh, Add machine and the summary; the chosen machine's
    // cards scroll beside it.
    auto *layout = new QHBoxLayout(this); layout->setContentsMargins(0, 0, 0, 0); layout->setSpacing(0);
    m_sidebar = new QFrame; m_sidebar->setObjectName("machinesSidebar"); m_sidebar->setFixedWidth(292);
    auto *side = new QVBoxLayout(m_sidebar); side->setContentsMargins(8, 14, 8, 10); side->setSpacing(6);
    auto *sideHead = new QHBoxLayout; sideHead->setContentsMargins(4, 0, 4, 0); sideHead->setSpacing(8);
    auto *title = new QLabel(tr("Machines")); title->setObjectName("machinesHeading"); sideHead->addWidget(title, 1);
    m_refresh = new QPushButton; m_refresh->setObjectName("refreshMachineProfiles"); m_refresh->setToolTip(tr("Refresh machines")); m_refresh->setAccessibleName(tr("Refresh machines"));
    m_refresh->setFixedSize(26, 26); sideHead->addWidget(m_refresh);
    m_add = new QPushButton(tr("Add machine")); m_add->setObjectName("addMachine"); m_add->setFixedHeight(26); sideHead->addWidget(m_add);
    side->addLayout(sideHead);
    m_summary = new QLabel(tr("SSH connections and HGS setup")); m_summary->setObjectName("muted"); m_summary->setWordWrap(true);
    m_summary->setContentsMargins(4, 0, 4, 6); side->addWidget(m_summary);
    // #650 L4: HGS_PEERS decides which machines are monitored; edits here do
    // not change that list, and the page says so where it is seen.
    m_peersNote = new QLabel(tr("HGS_PEERS in the environment overrides the list of monitored machines; edits here do not change it."));
    m_peersNote->setObjectName("machinesPeersOverride"); m_peersNote->setWordWrap(true); m_peersNote->setContentsMargins(4, 0, 4, 6);
    m_peersNote->hide(); side->addWidget(m_peersNote);
    m_list = new WorkspaceList; m_list->setObjectName("machineProfiles"); m_list->setAccessibleName(tr("Machines")); side->addWidget(m_list, 1);
    m_list->setItemDelegate(new MachineDelegate(m_list)); m_list->setMouseTracking(true); m_list->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_list->setStyleSheet("QListWidget#machineProfiles { background:transparent; border:0; outline:0; } QListWidget#machineProfiles::item { padding:0; border:0; }");
    layout->addWidget(m_sidebar);
    auto *editorWidget = new QWidget; editorWidget->setObjectName("machineEditor");
    auto *editor = new QVBoxLayout(editorWidget); editor->setContentsMargins(22, 14, 22, 18); editor->setSpacing(10);
    // U25 (spec 8), #647: the machine's label, its system ("Windows 11 · x64")
    // and its sessions and accounts on the right; its state under them.
    auto *head = new QHBoxLayout; head->setSpacing(8);
    auto *titleChip = new MachineTitle; m_title = titleChip; head->addWidget(m_title);
    m_titleNote = new QLabel; m_titleNote->setObjectName("machineSystem"); m_titleNote->setTextFormat(Qt::PlainText); head->addWidget(m_titleNote); head->addStretch();
    m_sessions = new QPushButton(tr("View sessions")); m_sessions->setObjectName("machineSessions"); head->addWidget(m_sessions);
    m_accounts = new QPushButton(tr("Agent accounts…")); m_accounts->setObjectName("machineAccounts"); head->addWidget(m_accounts);
    editor->addLayout(head);
    m_stateLine = new QLabel; m_stateLine->setObjectName("machineState"); m_stateLine->setTextFormat(Qt::PlainText); m_stateLine->setWordWrap(true); editor->addWidget(m_stateLine);
    m_localHost = new QLabel; m_localHost->setObjectName("localSessionHost"); m_localHost->setWordWrap(true); m_localHost->setTextFormat(Qt::PlainText); m_localHost->setTextInteractionFlags(Qt::TextSelectableByMouse); m_localHost->hide(); editor->addWidget(m_localHost);
    m_hint = new QLabel; m_hint->setObjectName("machineHint"); m_hint->setWordWrap(true); m_hint->setTextFormat(Qt::PlainText); m_hint->hide(); editor->addWidget(m_hint);
    // The Label card: seven colors (and a custom one), Soft / Bright,
    // Automatic, the preview tag, and the effective connection under them.
    QBoxLayout *labelBox = nullptr; auto *labelCard = PageCards::card(QStringLiteral("machineLabelCard"), &labelBox);
    auto *appearance = new QHBoxLayout; appearance->setSpacing(6);
    auto *labelTitle = new QLabel(tr("Label")); labelTitle->setStyleSheet(QStringLiteral("font-weight:600;")); appearance->addWidget(labelTitle);
    for (const char *hex : {"#3f9a7a", "#4a86d0", "#9b7be0", "#d08a3a", "#c95f86", "#8a96a3", "#5aa7e8"}) {
        auto *swatch = new QPushButton; swatch->setObjectName("machineSwatch"); swatch->setProperty("color", QString::fromLatin1(hex));
        swatch->setCheckable(true); swatch->setFixedSize(18, 18); swatch->setCursor(Qt::PointingHandCursor); swatch->setToolTip(tr("Label color"));
        swatch->setAccessibleName(tr("Label color %1").arg(QString::fromLatin1(hex)));
        appearance->addWidget(swatch); m_swatches << swatch;
        connect(swatch, &QPushButton::clicked, this, [this, swatch] {
            const auto machine = appearanceMachine(); if (m_new || machine.isEmpty()) return;
            MachineAppearance::setColor(machine, QColor(swatch->property("color").toString())); updateAppearance(); renderRows(); emit appearanceChanged();
        });
    }
    m_color = new QPushButton(QStringLiteral("+")); m_color->setObjectName("machineColor"); m_color->setToolTip(tr("Custom label color…"));
    m_color->setAccessibleName(tr("Custom label color…")); m_color->setFixedSize(22, 22); appearance->addWidget(m_color); appearance->addSpacing(4);
    // The segment drives the style; the combo stays as its model.
    m_colorStyle = new QComboBox(labelCard); m_colorStyle->setObjectName("machineLabelStyle"); m_colorStyle->setAccessibleName(tr("Label style"));
    m_colorStyle->addItem(tr("Soft"), false); m_colorStyle->addItem(tr("Bright"), true); m_colorStyle->hide();
    m_styleSoft = new QPushButton(tr("Soft")); m_styleSoft->setObjectName("machineLabelSoft"); PageCards::segment(m_styleSoft, "first");
    m_styleBright = new QPushButton(tr("Bright")); m_styleBright->setObjectName("machineLabelBright"); PageCards::segment(m_styleBright, "last");
    auto *segment = new QHBoxLayout; segment->setSpacing(0); segment->addWidget(m_styleSoft); segment->addWidget(m_styleBright); appearance->addLayout(segment);
    connect(m_styleSoft, &QPushButton::clicked, this, [this] { m_colorStyle->setCurrentIndex(0); updateAppearance(); });
    connect(m_styleBright, &QPushButton::clicked, this, [this] { m_colorStyle->setCurrentIndex(1); updateAppearance(); });
    m_colorReset = new QPushButton(tr("Automatic")); m_colorReset->setObjectName("resetMachineColor"); m_colorReset->setToolTip(tr("Use the default color for this machine")); m_colorReset->setFlat(true); appearance->addWidget(m_colorReset);
    auto *previewTitle = new QLabel(tr("Preview")); previewTitle->setObjectName("muted"); appearance->addWidget(previewTitle);
    m_colorPreview = new IdentityBadge(IdentityBadges::Machine); m_colorPreview->setObjectName("machineLabelPreview"); appearance->addWidget(m_colorPreview);
    m_colorHint = new QLabel(tr("Saved on this device")); m_colorHint->setObjectName("muted"); appearance->addWidget(m_colorHint); appearance->addStretch();
    labelBox->addLayout(appearance);
    m_effective = new QLabel; m_effective->setObjectName("effectiveMachineConnection"); m_effective->setProperty("pageMono", true); m_effective->setWordWrap(true); m_effective->setTextFormat(Qt::PlainText); m_effective->setTextInteractionFlags(Qt::TextSelectableByMouse); labelBox->addWidget(m_effective);
    // #650 L4: where the alias comes from, as a line under its connection.
    m_sourceNote = new QLabel; m_sourceNote->setObjectName("machineSourceNote"); m_sourceNote->setWordWrap(true); m_sourceNote->setTextFormat(Qt::PlainText);
    m_sourceNote->hide(); labelBox->addWidget(m_sourceNote);
    // #647: this computer is reached without SSH, and the card says so.
    m_localLine = new QLabel(tr("Local machine — no SSH connection")); m_localLine->setObjectName("machineLocalConnection"); m_localLine->setProperty("pageMono", true);
    m_localLine->setTextFormat(Qt::PlainText); m_localLine->hide(); labelBox->addWidget(m_localLine);
    editor->addWidget(labelCard);
    // #647: This machine: how its sessions run, its session host test and,
    // on a Windows control console, the offer to enable local sessions.
    QBoxLayout *localBox = nullptr; m_localCard = PageCards::card(QStringLiteral("machineLocalCard"), &localBox);
    auto *localTitle = new QLabel(tr("This machine")); localTitle->setObjectName("machineLocalTitle"); localTitle->setStyleSheet(QStringLiteral("font-weight:600;"));
    localBox->addWidget(localTitle);
    m_origin = new QLabel; m_origin->setObjectName("muted"); m_origin->setWordWrap(true); m_origin->setTextFormat(Qt::PlainText); localBox->addWidget(m_origin);
    auto *localActions = new QHBoxLayout; localActions->setSpacing(8);
    m_testHost = new QPushButton(tr("Test session host")); m_testHost->setObjectName("testSessionHost");
    m_testHost->setToolTip(tr("Ask the session host on this computer whether it answers"));
    m_enableLocal = new QPushButton(tr("Enable local sessions")); m_enableLocal->setObjectName("enableLocalSessions"); m_enableLocal->hide();
    m_enableLocal->setToolTip(tr("Run agents on this computer too. Zerus stages the HGS CLI and a session host in your user profile."));
    localActions->addWidget(m_testHost); localActions->addWidget(m_enableLocal); localActions->addStretch(); localBox->addLayout(localActions);
    editor->addWidget(m_localCard);
    // The connection card: the editable form, its actions and the footer.
    QBoxLayout *connectionBox = nullptr; auto *connectionCard = m_connectionCard = PageCards::card(QStringLiteral("machineConnectionCard"), &connectionBox);
    auto *form = m_form = new QFormLayout; form->setHorizontalSpacing(12); form->setVerticalSpacing(8); form->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);
    form->setLabelAlignment(Qt::AlignRight | Qt::AlignVCenter);
    auto *aliasRow = new QHBoxLayout; aliasRow->setSpacing(8);
    m_alias = new QLineEdit; m_alias->setObjectName("machineAlias"); m_alias->setPlaceholderText(tr("e.g. studio-mac")); aliasRow->addWidget(m_alias, 1);
    m_aliasError = new QLabel(tr("This name already exists")); m_aliasError->setObjectName("machineAliasError"); m_aliasError->setProperty("pageError", true); m_aliasError->hide(); aliasRow->addWidget(m_aliasError);
    form->addRow(tr("Name / SSH alias"), aliasRow);
    m_transport = new QComboBox; m_transport->setObjectName("machineTransport");
    m_transport->addItem(tr("SSH"), "ssh"); m_transport->addItem(tr("Local WSL (Windows)"), "wsl-local");
    form->addRow(tr("Connection type"), m_transport);
    m_wslDistribution = new QLineEdit; m_wslDistribution->setObjectName("machineWslDistribution");
    form->addRow(tr("WSL distribution"), m_wslDistribution);
    m_wslUser = new QLineEdit; m_wslUser->setObjectName("machineWslUser");
    form->addRow(tr("WSL user"), m_wslUser);
    m_wslHgs = new QLineEdit; m_wslHgs->setObjectName("machineWslHgs");
    m_wslHgs->setPlaceholderText(tr("Absolute Linux path to this fork's hgs"));
    form->addRow(tr("Linux HGS executable"), m_wslHgs);
    m_hostname = new QLineEdit; m_hostname->setObjectName("machineHostname"); m_hostname->setPlaceholderText(tr("Address or existing SSH alias")); form->addRow(tr("Hostname / IP"), m_hostname);
    m_user = new QLineEdit; m_user->setObjectName("machineUser"); m_user->setPlaceholderText(tr("Use SSH configuration")); form->addRow(tr("SSH user"), m_user);
    m_port = new QSpinBox; m_port->setObjectName("machinePort"); m_port->setRange(0, 65535); m_port->setSpecialValueText(tr("SSH configuration (usually 22)")); m_port->setMaximumWidth(120); form->addRow(tr("Port"), m_port);
    auto *keyRow = new QHBoxLayout; m_key = new QLineEdit; m_key->setObjectName("machineKey"); m_key->setPlaceholderText(tr("Use SSH agent / configuration")); keyRow->addWidget(m_key, 1);
    m_browse = new QPushButton(tr("Browse…")); m_browse->setObjectName("browseMachineKey"); keyRow->addWidget(m_browse); m_keyRow = new QWidget; keyRow->setContentsMargins(0,0,0,0); m_keyRow->setLayout(keyRow); form->addRow(tr("Private key"), m_keyRow);
    m_enabled = new QCheckBox(tr("Show sessions and include in background polling")); m_enabled->setObjectName("machineEnabled"); form->addRow(QString(), m_enabled);
    // U62 (#733): the owner makes the Machine a test runner of one platform:
    // the team's suites of that platform run there over SSH, never here.
    m_testRunner = new QComboBox; m_testRunner->setObjectName("machineTestRunner");
    m_testRunner->addItem(tr("Not a test runner"), QString());
    m_testRunner->addItem(tr("Linux"), "linux"); m_testRunner->addItem(tr("macOS"), "macos");
    m_testRunner->addItem(tr("Windows"), "windows"); m_testRunner->addItem(tr("Android"), "android");
    m_testRunner->setToolTip(tr("Test suites of this platform run on this Machine over SSH, each on the exact commit, isolated"));
    form->addRow(tr("Test runner"), m_testRunner);
    m_testRunnerHint = new QLabel; m_testRunnerHint->setObjectName("machineTestRunnerHint"); m_testRunnerHint->setWordWrap(true);
    m_testRunnerHint->setProperty("pageHint", true);
    form->addRow(QString(), m_testRunnerHint);
    connectionBox->addLayout(form);
    // Save (only with changes), Discard changes / Discard draft, then the
    // actions that need a saved connection.
    auto *actions = new QHBoxLayout; actions->setSpacing(8);
    m_save = new QPushButton(tr("Save connection")); m_save->setObjectName("saveMachine"); PageCards::primary(m_save);
    m_discardDraft=new QPushButton(tr("Discard draft"));m_discardDraft->setObjectName("discardNewMachine"); PageCards::danger(m_discardDraft);
    m_check = new QPushButton(tr("Test connection")); m_check->setObjectName("checkMachine");
    m_ssh = new QPushButton(tr("Open SSH terminal")); m_ssh->setObjectName("sshMachine");
    m_install = new QPushButton(tr("Install / update HGS…")); m_install->setObjectName("installMachine");
    auto *divider = new QWidget; divider->setObjectName("machineActionsDivider"); divider->setFixedSize(1, 20);
    actions->addWidget(m_save); actions->addWidget(m_discardDraft); actions->addWidget(divider); actions->addWidget(m_check); actions->addWidget(m_ssh); actions->addWidget(m_install); actions->addStretch();
    connectionBox->addLayout(actions);
    m_unsaved = new QLabel(tr("Unsaved changes — test, SSH and install are off until you save or discard."));
    m_unsaved->setObjectName("machineUnsavedNotice"); m_unsaved->setProperty("pageWarn", true); m_unsaved->setWordWrap(true); m_unsaved->hide(); connectionBox->addWidget(m_unsaved);
    m_status = new QLabel; m_status->setObjectName("machineNotice"); m_status->setTextFormat(Qt::PlainText); m_status->setWordWrap(true); m_status->setTextInteractionFlags(Qt::TextSelectableByMouse); editor->addWidget(m_status);
    m_log = new QPlainTextEdit; m_log->setObjectName("machineSetupLog"); m_log->setReadOnly(true); m_log->setMaximumBlockCount(1500); m_log->hide(); editor->addWidget(m_log, 1);
    auto *footer = new QHBoxLayout;
    m_reset = new QPushButton(tr("Reset SSH overrides")); m_reset->setObjectName("resetMachine"); m_reset->setFlat(true); footer->addWidget(m_reset);
    footer->addStretch();
    m_remove = new QPushButton(tr("Remove from Zerus")); m_remove->setObjectName("removeMachine"); PageCards::danger(m_remove); footer->addWidget(m_remove);
    connectionBox->addLayout(footer);
    editor->insertWidget(editor->indexOf(labelCard) + 1, connectionCard);
    // Phones of every workspace this computer reaches, here or through a peer.
    QBoxLayout *phonesBox = nullptr; auto *phonesCard = PageCards::card(QStringLiteral("machinePhonesCard"), &phonesBox);
    m_phones = new PhonePairing::Panel(path); phonesBox->addWidget(m_phones); editor->addWidget(phonesCard);
    editor->addStretch();
    auto *scroll = new QScrollArea; scroll->setObjectName("machineEditorScroll"); scroll->setWidgetResizable(true); scroll->setFrameShape(QFrame::NoFrame); scroll->setWidget(editorWidget);
    scroll->setAutoFillBackground(false); scroll->viewport()->setAutoFillBackground(false); editorWidget->setAutoFillBackground(false);
    layout->addWidget(scroll, 1);
    connect(m_color, &QPushButton::clicked, this, [this] {
        const auto machine = appearanceMachine(); if (m_new || machine.isEmpty()) return;
        WorkspaceColorDialog dialog(MachineAppearance::color(machine), this);
        dialog.setWindowTitle(tr("Label color for %1").arg(machine));
        if (dialog.exec() != QDialog::Accepted) return;
        const auto color = dialog.selectedColor();
        if (!color.isValid()) return;
        MachineAppearance::setColor(machine, color); updateAppearance(); renderRows(); emit appearanceChanged();
    });
    connect(m_colorStyle, &QComboBox::currentIndexChanged, this, [this] {
        const auto machine = appearanceMachine(); if (machine.isEmpty()) return;
        MachineAppearance::setVivid(machine, m_colorStyle->currentData().toBool()); updateAppearance(); renderRows(); emit appearanceChanged();
    });
    connect(m_colorReset, &QPushButton::clicked, this, [this] {
        const auto machine = appearanceMachine(); if (m_new || machine.isEmpty()) return;
        MachineAppearance::resetColor(machine); updateAppearance(); renderRows(); emit appearanceChanged();
    });
    const auto discardDraft=[this]{m_new=false;m_selected="@local";renderRows();loadSelection();};
    // Discard draft drops a new machine; Discard changes reloads a saved one.
    // Both confirm unsaved edits ("Unsaved connection").
    connect(m_discardDraft,&QPushButton::clicked,this,[this,discardDraft]{
        if(!discardChanges())return;
        if(m_new)discardDraft(); else { renderRows(); loadSelection(); }
    });
    connect(m_refresh, &QPushButton::clicked, this, [this,discardDraft] {
        if(!discardChanges())return;if(m_new)discardDraft();reload();
    });
    connect(m_add, &QPushButton::clicked, this, [this] {
        if(m_new){m_alias->setFocus();return;}
        if (!discardChanges()) return;
        m_new = true; m_selected="@new"; renderRows();loadSelection(); m_alias->setFocus();
    });
    connect(m_list, &QListWidget::currentRowChanged, this, [this](int row) {
        if (m_loading || row < 0) return;
        const auto next = m_list->item(row)->data(Qt::UserRole).toString();
        if(next==(m_new?QStringLiteral("@new"):m_selected))return;
        if (discardChanges()) { m_selected = next; m_new = false; renderRows();loadSelection(); }
        else renderRows();
        const auto key=m_selected;const bool draft=m_new;
        // The original mouse press still applies its selection after this
        // returns, also after a nested dialog, at the row now under its old
        // position: a discarded draft can scroll the list. Restore the
        // highlight once it finishes.
        QTimer::singleShot(0,this,[this,key,draft]{
            if(m_selected!=key||m_new!=draft)return;
            const QSignalBlocker blocked(m_list);
            m_list->setCurrentRow(m_list->currentRow(),QItemSelectionModel::ClearAndSelect);
        });
    });
    for (auto *field : {m_alias, m_hostname, m_user, m_key, m_wslDistribution, m_wslUser, m_wslHgs}) connect(field, &QLineEdit::textChanged, this, &MachinesPage::changed);
    connect(m_transport, &QComboBox::currentIndexChanged, this, &MachinesPage::changed);
    connect(m_port, &QSpinBox::valueChanged, this, &MachinesPage::changed); connect(m_enabled, &QCheckBox::toggled, this, &MachinesPage::changed);
    connect(m_testRunner, &QComboBox::currentIndexChanged, this, &MachinesPage::changed);
    connect(m_browse, &QPushButton::clicked, this, [this] {
        if (!m_key->isEnabled()) return;
        const auto file = QFileDialog::getOpenFileName(this, tr("Choose SSH private key")); if (!file.isEmpty()) m_key->setText(file);
    });
    connect(m_save, &QPushButton::clicked, this, &MachinesPage::save); connect(m_check, &QPushButton::clicked, this, &MachinesPage::check);
    connect(m_ssh, &QPushButton::clicked, this, [this] { emit sshTerminalRequested(alias()); });
    connect(m_install, &QPushButton::clicked, this, &MachinesPage::install);
    connect(m_enableLocal, &QPushButton::clicked, this, &MachinesPage::enableLocalSessions);
    connect(m_testHost, &QPushButton::clicked, this, [this] {
        if (m_hostRequest || m_hostTest || m_installing) return;
        report(tr("Testing the session host…")); m_hostTest = m_client->requestSessionHost(QStringLiteral("status")); updateControls();
    });
    connect(m_sessions, &QPushButton::clicked, this, [this] { emit viewSessionsRequested(m_selected == "@local" ? QString() : alias()); });
    connect(m_accounts, &QPushButton::clicked, this, [this] { emit accountsRequested(m_selected == "@local" ? QString() : alias()); });
    for (auto *button : {m_remove, m_reset}) connect(button, &QPushButton::clicked, this, [this, button] {
        const bool remove = button == m_remove;
        const auto target = alias(), selection = m_selected, revision = m_data.value("revision").toString();
        if (remove) {
            QMessageBox confirm(QMessageBox::Question, tr("Remove machine?"),
                tr("Remove %1 from this Zerus? Its agents keep running and its files stay unchanged.").arg(target), QMessageBox::Cancel, this);
            confirm.setObjectName("removeMachineConfirm"); confirm.setTextFormat(Qt::PlainText);
            auto *proceed = confirm.addButton(tr("Remove machine"), QMessageBox::DestructiveRole);
            confirm.setDefaultButton(QMessageBox::Cancel); confirm.setEscapeButton(QMessageBox::Cancel);
            confirm.exec(); if (confirm.clickedButton() != proceed) return;
            if (m_request || alias() != target || m_selected != selection || m_data.value("revision").toString() != revision) {
                report(tr("The connection changed. Review it before removing."), true); return;
            }
        }
        m_operation = remove ? "remove" : "reset";
        // A removed machine leaves no kept overrides behind for a later one of its name.
        if (remove) Settings::remove(otherTransportKey(target));
        m_request = m_client->requestMachines({m_operation, target, "--revision", revision}); updateControls();
    });
    connect(m_client, &HgsClient::machinesReady, this, [this](quint64 request, const QJsonObject &data) {
        if (request == m_resolveRequest) {
            m_resolveRequest = 0;
            if (!m_new && m_selected == m_resolveAlias && data.value("alias").toString() == m_resolveAlias) {
                m_effectiveData = data.value("effective").toObject(); showEffectiveConnection(m_effectiveData);
            }
            return;
        }
        if (request != m_request) return;
        const QString operation = m_operation; m_request = 0;
        if (operation == "ls") {
            m_data = data;
            if (!m_pendingSelection.isEmpty()) { m_selected = m_pendingSelection; m_pendingSelection.clear(); m_new = false; }
            renderRows(); loadSelection();
        }
        else if (operation == "check") {
            // #647: the header names the system, HGS and tmux a check found.
            if (const auto checked = data.value("alias").toString(); !checked.isEmpty() && data.value("ssh_ok").toBool(data.value("ok").toBool())) m_checks.insert(checked, data);
            if (data.value("transport").toString() == "wsl-local") {
                report(data.value("ok").toBool()
                    ? tr("Connected to WSL %1 as %2: %3. OpenCode policy: existing-only.").arg(data.value("distribution").toString(), data.value("user").toString(), data.value("hgs").toString())
                    : data.value("error").toString(), !data.value("ok").toBool());
            }
            else if (data.value("ok").toBool()) report(tr("Connected: %1 %2, %3, %4").arg(data.value("os").toString(), data.value("arch").toString(), data.value("hgs").toString(), data.value("tmux").toString()));
            else if (data.value("ssh_ok").toBool()) {
                QStringList missing;
                if (data.value("hgs").toString().isEmpty()) missing << tr("HGS");
                if (data.value("tmux").toString().isEmpty()) missing << tr("tmux");
                if (data.value("cargo").toString().isEmpty()) missing << tr("Rust");
                report(data.value("os").toString().isEmpty() ? data.value("error").toString()
                    : tr("SSH connected: %1 %2. Missing: %3. Install the newest stable Rust and tmux in the SSH terminal if needed, then use Install / update HGS.").arg(data.value("os").toString(), data.value("arch").toString(), missing.isEmpty() ? tr("a working HGS installation") : missing.join(", ")), true);
            }
            else report(tr("SSH connection failed: %1\nOpen SSH terminal to check credentials or approve the host key, then test again.").arg(data.value("error").toString()), true);
        } else { m_dirty = false; m_new = false; emit machinesChanged(); reload(); }
        updateControls();
    });
    connect(m_client, &HgsClient::machinesFailed, this, [this](quint64 request, const QString &error) {
        if (request == m_resolveRequest) {
            m_resolveRequest = 0;
            if (!m_new && m_selected == m_resolveAlias) m_effective->setText(tr("Could not read effective SSH settings: %1\nYour connection overrides are shown below.").arg(error));
        } else if (request == m_request) { m_request = 0; report(error, true); updateControls(); }
    });
    connect(m_client, &HgsClient::sessionHostReady, this, [this](quint64 request, const QJsonObject &data) {
        if (request && request == m_hostTest) {
            // #647: Test session host reports what the host answered now.
            m_hostTest = 0;
            const auto host = data.value("host").toObject();
            const bool running = host.value("state").toString() == "running";
            QString text = tr("Session host: %1").arg(hostFacts(host));
            if (running && host.value("sessions").isDouble()) text += tr(", %1 sessions").arg(host.value("sessions").toInteger());
            report(text, !running); updateControls();
            return;
        }
        if (request != m_hostRequest) return;
        m_hostRequest = 0; m_installing = false;
        const auto cli = data.value("cli").toObject().value("path").toString();
        // This GUI keeps its CLI until it restarts (spec D2).
        m_localNotice = cli.isEmpty() ? tr("Local sessions are enabled on this computer. Zerus runs the stable HGS CLI from its next start.")
                                      : tr("Local sessions are enabled on this computer. Zerus runs %1 from its next start.").arg(cli);
        report(m_localNotice); updateControls(); emit machinesChanged();
    });
    connect(m_client, &HgsClient::sessionHostFailed, this, [this](quint64 request, const QString &error) {
        if (request && request == m_hostTest) { m_hostTest = 0; report(tr("The session host did not answer: %1").arg(error), true); updateControls(); return; }
        if (request != m_hostRequest) return;
        m_hostRequest = 0; m_installing = false; report(error, true); updateControls();
    });
    connect(m_client, &HgsClient::machineSetupOutput, this, [this](const QString &text) {
        auto cursor = m_log->textCursor(); cursor.movePosition(QTextCursor::End); cursor.insertText(text); m_log->setTextCursor(cursor); m_log->ensureCursorVisible();
    });
    connect(m_client, &HgsClient::machineSetupFinished, this, [this](bool ok, const QString &error) {
        m_installing = false; updateControls();
        if (ok) { report(tr("HGS installed. Checking connection…")); emit machinesChanged(); check(); }
        else report(error, true);
    });
    setTheme(false); loadSelection();
}
QString MachinesPage::alias() const { return m_alias->text().trimmed(); }
bool MachinesPage::aliasExists() const {
    if (alias() == m_fleet.local().host || alias() == m_data.value("local").toString()) return true;
    for (const auto &value : m_data.value("machines").toArray())
        if (value.toObject().value("alias").toString() == alias()) return true;
    return false;
}
QJsonObject MachinesPage::connection() const {
    if (m_transport->currentData().toString() == "wsl-local") {
        return {{"transport","wsl-local"},{"enabled",m_enabled->isChecked()},{"test_runner",m_testRunner->currentData().toString()},
            {"wsl", QJsonObject{{"distribution",m_wslDistribution->text().trimmed()},
                {"user",m_wslUser->text().trimmed()},{"hgs_path",m_wslHgs->text()}}}};
    }
    return {{"hostname", m_hostname->text().trimmed()}, {"user", m_user->text().trimmed()}, {"port", m_port->value() ? QJsonValue(m_port->value()) : QJsonValue(QJsonValue::Null)}, {"identity_file", m_key->text().trimmed()}, {"enabled", m_enabled->isChecked()},
            {"test_runner", m_testRunner->currentData().toString()}};
}
void MachinesPage::report(const QString &text, bool error) { m_status->setText(text); m_status->setStyleSheet(error ? "color:#d37c69;" : QString()); m_status->setVisible(!text.isEmpty()); }
void MachinesPage::changed() { if (!m_loading) { m_dirty = true; if (!m_effectiveData.isEmpty()) showEffectiveConnection(m_effectiveData); updateControls(); } }

bool MachinesPage::discardChanges() {
    if (m_installing || m_request || m_discardPrompt) return false;
    if (!m_dirty) return true;
    const QScopedValueRollback<bool> prompt(m_discardPrompt,true);
    QMessageBox dialog(QMessageBox::Question,tr("Unsaved connection"),tr("Discard the unsaved changes to this connection?"),QMessageBox::Discard|QMessageBox::Cancel,this);
    dialog.setObjectName("unsavedMachineDialog");dialog.setDefaultButton(QMessageBox::Cancel);dialog.setEscapeButton(QMessageBox::Cancel);
    if(dialog.exec()!=QMessageBox::Discard)return false;
    m_dirty = false; return true;
}
void MachinesPage::reload() { if (m_request || m_installing || m_dirty) return; m_operation = "ls"; m_request = m_client->requestMachines(); updateControls(); }
void MachinesPage::selectMachine(const QString &host) {
    const QString target = host.isEmpty() || host == "@local" || host == m_fleet.local().host || host == m_data.value("local").toString() ? QStringLiteral("@local") : host;
    if (target == m_selected && !m_new) return;
    if (m_request) {
        if (m_operation == "ls" && !m_dirty && !m_installing) m_pendingSelection = target;
        return;
    }
    if (!discardChanges()) return;
    if (m_data.isEmpty() && target != "@local") { m_pendingSelection = target; reload(); return; }
    m_selected = target; m_new = false; renderRows(); loadSelection();
}
void MachinesPage::showEvent(QShowEvent *event)
{
    QWidget::showEvent(event);
    if (m_rowsStale) { renderRows(); updateAppearance(); }
    reload();
}
void MachinesPage::setTheme(bool dark) {
    m_dark = dark;
    // #647: the prototype's compact controls on the machine's cards.
    const auto c = [dark](const char *name) { return PageCards::token(dark, name).name(QColor::HexArgb); };
    QColor primaryOff = PageCards::token(dark, "accent"); primaryOff.setAlphaF(.38);
    QColor primaryTextOff = PageCards::token(dark, "accentText"); primaryTextOff.setAlphaF(.75);
    QColor dangerOff = PageCards::token(dark, "danger"); dangerOff.setAlphaF(.5);
    const QString editor = QStringLiteral(
        "QWidget#machineEditor QPushButton{background:%1;color:%2;border:1px solid %3;border-radius:6px;padding:3px 10px;font-size:12px;min-height:18px;}"
        "QWidget#machineEditor QPushButton:hover{background:%4;}"
        "QWidget#machineEditor QPushButton:disabled{color:%5;}"
        "QWidget#machineEditor QPushButton[pagePrimary=\"true\"]{background:%6;border-color:%6;color:%7;font-weight:600;}"
        "QWidget#machineEditor QPushButton[pagePrimary=\"true\"]:hover{background:%8;border-color:%8;}"
        "QWidget#machineEditor QPushButton[pagePrimary=\"true\"]:disabled{background:%9;border-color:transparent;color:%10;}"
        "QWidget#machineEditor QPushButton[pageDanger=\"true\"]{color:%11;}"
        "QWidget#machineEditor QPushButton[pageDanger=\"true\"]:disabled{color:%12;}"
        "QWidget#machineEditor QPushButton[pageSegment=\"true\"]{border-radius:0;padding:3px 10px;font-size:11.5px;}"
        "QWidget#machineEditor QPushButton[pageSegment=\"true\"][segmentEdge=\"first\"]{border-top-left-radius:7px;border-bottom-left-radius:7px;}"
        "QWidget#machineEditor QPushButton[pageSegment=\"true\"][segmentEdge=\"last\"]{border-top-right-radius:7px;border-bottom-right-radius:7px;}"
        "QWidget#machineEditor QPushButton[pageSegment=\"true\"]:checked{background:%13;color:%6;font-weight:600;}"
        "QWidget#machineEditor QPushButton#resetMachineColor,QWidget#machineEditor QPushButton#resetMachine{background:transparent;border:0;font-weight:600;}"
        "QWidget#machineEditor QPushButton#resetMachineColor:hover,QWidget#machineEditor QPushButton#resetMachine:hover{color:%6;}"
        "QWidget#machineEditor QLineEdit,QWidget#machineEditor QSpinBox,QWidget#machineEditor QComboBox{min-height:20px;padding:2px 8px;font-size:12.5px;}")
        .arg(c("surface"), c("fg"), c("border"), c("hover"), c("dim"), c("accent"), c("accentText"), c("accentHover"), primaryOff.name(QColor::HexArgb))
        .arg(primaryTextOff.name(QColor::HexArgb), c("danger"), dangerOff.name(QColor::HexArgb), c("segOn"));
    setStyleSheet(PageCards::sheet(dark) + editor);
    // The swatches: their color, a 2 px ring in the text color when chosen.
    for (auto *swatch : std::as_const(m_swatches))
        swatch->setStyleSheet(QStringLiteral("QPushButton{background:%1;border:2px solid transparent;border-radius:5px;padding:0;}"
                                             "QPushButton:checked{border-color:%2;} QPushButton:disabled{background:%1;}")
                                  .arg(swatch->property("color").toString(), PageCards::token(dark, "fg").name()));
    m_colorPreview->setTheme(dark);
    if (auto *divider = findChild<QWidget *>("machineActionsDivider"))
        divider->setStyleSheet(QStringLiteral("background:%1;").arg(PageCards::token(dark, "border").name()));
    m_color->setStyleSheet(QStringLiteral("QPushButton{padding:0;border-radius:5px;font-weight:600;}"));
    m_list->setProperty("hgsDark", dark); m_list->viewport()->update();
    // #647: the list column has its own edge; its title, the icon Refresh and
    // a small Add machine sit on one line; the header's notes are muted.
    const auto token = [dark](const char *name) { return PageCards::token(dark, name).name(QColor::HexArgb); };
    m_sidebar->setStyleSheet(QStringLiteral("QFrame#machinesSidebar{background:transparent;border:0;border-right:1px solid %1;}"
                                            "QLabel#machinesHeading{font-size:17px;font-weight:700;color:%2;}").arg(token("border"), token("fg")));
    const QString small = QStringLiteral("QPushButton{background:%1;color:%2;border:1px solid %3;border-radius:7px;padding:0 9px;font-size:12px;}"
                                         "QPushButton:hover{background:%4;} QPushButton:disabled{color:%5;}")
                              .arg(token("surface"), token("fg"), token("border"), token("hover"), token("dim"));
    m_refresh->setStyleSheet(small + QStringLiteral("QPushButton{padding:0;}")); m_refresh->setIcon(workspaceIcon("refresh", PageCards::token(dark, "text2"))); m_refresh->setIconSize(QSize(15, 15));
    m_add->setStyleSheet(small); m_add->setIcon(workspaceIcon("add", PageCards::token(dark, "text2"))); m_add->setIconSize(QSize(13, 13));
    m_refresh->setCursor(Qt::PointingHandCursor); m_add->setCursor(Qt::PointingHandCursor);
    for (auto *note : {m_titleNote, m_stateLine, m_localHost, m_hint, m_sourceNote}) note->setStyleSheet(QStringLiteral("color:%1;font-size:12px;").arg(token("muted")));
    m_peersNote->setStyleSheet(QStringLiteral("color:%1;font-size:12px;").arg(token("warn")));
    m_phones->setTheme(dark);
    updateHeader();
}
void MachinesPage::setFleet(const FleetState &fleet) {
    // The origin text also names Enable local sessions while it is offered.
    const bool console = fleet.local().localSessions != m_fleet.local().localSessions
        || LocalSessionHost::canEnable(fleet.local()) != LocalSessionHost::canEnable(m_fleet.local());
    // Windows: its platform and session host decide what this computer offers.
    const bool host = fleet.local().localHost != m_fleet.local().localHost
        || MachinePlatform::of(fleet.local().metrics, true) != MachinePlatform::of(m_fleet.local().metrics, true);
    m_fleet = fleet;
    // A hidden page renders its rows when shown.
    if (isVisible()) { renderRows(); updateAppearance(); } else m_rowsStale = true;
    if (console && !m_loading) { if (m_selected == "@local" && !m_new && !m_dirty) loadSelection(); else updateControls(); }
    else if (host && !m_loading) updateControls();
}
void MachinesPage::renderRows() {
    m_rowsStale = false;
    const QSignalBlocker blocked(m_list);const QScopedValueRollback<bool> loading(m_loading,true);
    // A control console is listed for its color, but it is no machine of the
    // fleet: the counts match Overview, which leaves it out.
    const bool console = !m_fleet.local().localSessions;
    m_list->clear(); int online = m_fleet.local().ok && !console ? 1 : 0;
    const auto localName = m_fleet.local().host.isEmpty() ? m_data.value("local").toString() : m_fleet.local().host;
    // #647: this machine's line counts its sessions, as the prototype's does.
    const auto localSessions = m_fleet.local().sessions.size();
    const QString localState = !m_fleet.local().localSessions ? tr("This computer: control console")
        : localSessions == 1 ? tr("This machine · 1 session") : tr("This machine · %1 sessions").arg(localSessions);
    auto *local = new QListWidgetItem(MachineAppearance::icon(localName), localName + '\n' + localState, m_list); local->setData(Qt::UserRole, "@local");
    local->setData(StateRole, m_fleet.local().ok || m_data.isEmpty() ? QStringLiteral("ok") : QStringLiteral("off"));
    for (const auto &value : m_data.value("machines").toArray()) {
        const auto row = value.toObject(); const auto name = row.value("alias").toString(); const auto *box = m_fleet.peer(name);
        const bool live = box && box->ok && QDateTime::currentMSecsSinceEpoch() - m_fleet.peerPolledAt(name) < FleetState::kPeerStaleMs;
        const bool enabled = row.value("enabled").toBool();
        if (live) ++online;
        const QString state = !enabled ? tr("Disabled") : !live ? tr("Not connected")
            : box->sessions.size() == 1 ? tr("Connected, 1 session") : tr("Connected, %1 sessions").arg(box->sessions.size());
        auto *item = new QListWidgetItem(MachineAppearance::icon(name), name + '\n' + state, m_list); item->setData(Qt::UserRole, name);
        item->setData(StateRole, !enabled ? QStringLiteral("disabled") : live ? QStringLiteral("ok") : QStringLiteral("off"));
    }
    const int savedCount=m_list->count()-(console?1:0);
    if(m_new){auto *draft=new QListWidgetItem(tr("New machine\nUnsaved connection"),m_list);draft->setData(Qt::UserRole,"@new");draft->setData(StateRole,QStringLiteral("new"));draft->setToolTip(tr("This connection is a draft. Save it to add the machine to Zerus."));}
    const auto selected=m_new?QStringLiteral("@new"):m_selected;
    for (int i = 0; i < m_list->count(); ++i) if (m_list->item(i)->data(Qt::UserRole).toString() == selected) m_list->setCurrentRow(i);
    m_peersNote->setVisible(m_data.value("environment_override").toBool());
    m_summary->setText(tr("SSH connections and HGS setup · %1").arg(savedCount == 1 ? tr("%1 connected, 1 machine").arg(online) : tr("%1 connected, %2 machines").arg(online).arg(savedCount)));
    updateHeader();
    // N20: a fleet poll, an appearance click or a cancelled switch rebuilds
    // the rows; the edits are still unsaved.
    markUnsaved();
}
void MachinesPage::markUnsaved() {
    for (int i = 0; i < m_list->count(); ++i) {
        auto *item = m_list->item(i);
        const bool dot = !m_new && m_dirty && item->data(Qt::UserRole).toString() == m_selected;
        if (item->data(Qt::UserRole + 1).toBool() != dot) item->setData(Qt::UserRole + 1, dot);
    }
}
void MachinesPage::loadSelection() {
    m_loading = true; QJsonObject row;
    for (const auto &v : m_data.value("machines").toArray()) if (v.toObject().value("alias").toString() == m_selected) row = v.toObject();
    if (!m_new && m_selected != "@local" && row.isEmpty()) { m_selected = "@local"; renderRows(); }
    const bool local = m_selected == "@local"; const auto c = row.value("connection").toObject();
    const auto localName = m_fleet.local().host.isEmpty() ? m_data.value("local").toString() : m_fleet.local().host;
    m_alias->setText(m_new?QString():local ? localName : m_selected); m_hostname->setText(c.value("hostname").toString()); m_user->setText(c.value("user").toString());
    m_transport->setCurrentIndex(c.value("transport").toString() == "wsl-local" ? 1 : 0);
    const auto wsl = c.value("wsl").toObject();
    m_wslDistribution->setText(wsl.value("distribution").toString());
    m_wslUser->setText(wsl.value("user").toString());
    m_wslHgs->setText(wsl.value("hgs_path").toString());
    m_port->setValue(c.value("port").toInt()); m_key->setText(c.value("identity_file").toString()); m_enabled->setChecked(c.value("enabled").toBool(true));
    m_testRunner->setCurrentIndex(qMax(0, m_testRunner->findData(c.value("test_runner").toString())));
    // #650 N2: hgs keeps one connection type per machine. The other type's
    // overrides, kept on this device when the machine was saved, come back
    // into their (hidden) fields, so switching the type back restores them.
    if (!local && !m_new) {
        // #709 L8: through the overlay, so a save still queued behind the lock reads back.
        const auto other = SettingsJson::object(otherTransportKey(m_selected));
        if (m_transport->currentIndex() == 1) {
            m_hostname->setText(other.value("hostname").toString()); m_user->setText(other.value("user").toString());
            m_port->setValue(other.value("port").toInt()); m_key->setText(other.value("identity_file").toString());
        } else {
            const auto kept = other.value("wsl").toObject();
            m_wslDistribution->setText(kept.value("distribution").toString()); m_wslUser->setText(kept.value("user").toString());
            m_wslHgs->setText(kept.value("hgs_path").toString());
        }
    }
    // A control-only computer (hgs.exe on Windows) runs no agents of its own.
    // #647: this computer's explanation is in its card; a draft's in a line
    // under the header; a saved machine's on its effective connection.
    const bool windowsLocal = MachinePlatform::of(m_fleet.local().metrics, true) == MachinePlatform::Style::Windows;
    m_origin->setText(m_fleet.local().localSessions
        ? (windowsLocal ? tr("Local sessions run through the Zerus session host. No SSH settings are needed.")
                        : tr("This device runs Zerus. Add a remote machine to manage its agents over SSH."))
        : tr("This computer is a control console: agents run on the remote machines you add here, over SSH.")
            + (LocalSessionHost::canEnable(m_fleet.local()) ? tr(" Enable local sessions to run them on this computer too.") : QString()));
    m_hint->setText(m_new ? tr("Fill in the SSH alias or the host, then save.") : QString());
    m_hint->setVisible(m_new);
    QString explain = (row.value("source").toString() == "config" ? tr("This alias comes from HGS configuration. ") : QString()) +
        tr("SSH supplies inherited values from your OpenSSH configuration and defaults. Fields below are Zerus overrides; blank fields keep the inherited value.") + (m_data.value("environment_override").toBool() ? tr(" HGS_PEERS in the environment overrides the list of monitored machines.") : QString());
    if (!local && m_transport->currentData().toString() == "wsl-local")
        explain = tr("Local WSL uses the explicit distribution, user and Linux HGS executable. OpenCode must already be running; Zerus never starts or replaces it in this mode.");
    m_effective->setToolTip(local || m_new ? QString() : explain);
    m_alias->setToolTip(local || m_new ? QString() : explain);
    // #650 L4: the alias's source in the page's text, not only its tooltip.
    const bool fromConfig = !local && !m_new && row.value("source").toString() == "config";
    m_sourceNote->setText(fromConfig ? tr("This alias comes from HGS configuration; the fields below are this device's overrides.") : QString());
    m_sourceNote->setVisible(fromConfig);
    m_dirty = false; m_loading = false; m_log->hide(); report(local ? m_localNotice : QString()); updateControls(); resolveConnection();
}
QString MachinesPage::appearanceMachine() const {
    if (m_new) return {};
    return m_selected == "@local" ? (m_fleet.local().host.isEmpty() ? m_data.value("local").toString() : m_fleet.local().host) : m_selected;
}
void MachinesPage::updateAppearance() {
    const auto machine = appearanceMachine(); const bool available = !machine.isEmpty();
    m_color->setEnabled(available); m_colorReset->setEnabled(available); m_colorStyle->setEnabled(available);
    const QSignalBlocker blocked(m_colorStyle); m_colorStyle->setCurrentIndex(MachineAppearance::vivid(machine) ? 1 : 0);
    m_colorPreview->setValue(available ? machine : tr("New machine"));
    // The chosen swatch and style.
    const QColor chosen = available ? MachineAppearance::color(machine) : QColor();
    for (auto *swatch : std::as_const(m_swatches)) {
        swatch->setEnabled(available);
        swatch->setChecked(available && QColor(swatch->property("color").toString()) == chosen);
    }
    const bool bright = available && MachineAppearance::vivid(machine);
    m_styleSoft->setEnabled(available); m_styleBright->setEnabled(available);
    m_styleSoft->setChecked(!bright); m_styleBright->setChecked(bright);
    m_colorHint->setText(available ? tr("Saved on this device") : tr("Save the machine to choose its color"));
}
void MachinesPage::resolveConnection() {
    // Resolution is independent of editing and may finish after the selection
    // changes. Ignore old request IDs; never populate edit values from SSH.
    m_resolveRequest = 0; m_resolveAlias.clear(); m_effectiveData = {};
    m_hostname->setPlaceholderText(tr("Address or existing SSH alias")); m_user->setPlaceholderText(tr("Use SSH configuration"));
    m_port->setSpecialValueText(tr("SSH configuration (usually 22)")); m_key->setPlaceholderText(tr("Use SSH agent / configuration"));
    m_effective->setVisible(!m_new && m_selected != "@local");
    if (m_new || m_selected == "@local") return;
    m_effective->setText(m_transport->currentData().toString() == "wsl-local" ? tr("Reading WSL connection…") : tr("Reading effective SSH connection…")); m_resolveAlias = m_selected;
    m_resolveRequest = m_client->requestMachines({"resolve", m_selected});
}
void MachinesPage::showEffectiveConnection(const QJsonObject &data) {
    if (data.value("transport").toString() == "wsl-local") {
        m_effective->setText(tr("WSL: %1, user %2\nHGS: %3\nOpenCode: existing-only")
            .arg(data.value("distribution").toString(), data.value("user").toString(), data.value("hgs_path").toString())
            + (m_dirty ? tr("\nUnsaved edits are not included. Save to refresh these values.") : QString()));
        return;
    }
    QStringList keys; for (const auto &key : data.value("identity_files").toArray()) keys << key.toString();
    // #647: the prototype's one line, "…, port 22 · key ~/.ssh/id_ed25519".
    QStringList details{tr("%1: %2@%3, port %4").arg(m_dirty ? tr("Saved connection") : tr("Effective connection"), data.value("user").toString(), data.value("hostname").toString(), data.value("port").toString())
                        + (keys.isEmpty() ? QString() : (keys.size() == 1 ? tr(" · key %1") : tr(" · keys %1")).arg(keys.join(", ")))};
    if (!data.value("proxy_jump").toString().isEmpty()) details << tr("Jump host: %1").arg(data.value("proxy_jump").toString());
    else if (data.value("proxy_command").toBool()) details << tr("Proxy command configured in OpenSSH");
    if (m_dirty) details << tr("Unsaved edits are not included. Save to refresh these values.");
    m_effective->setText(details.join('\n'));
    // Placeholders describe inheritance without turning it into a saved override.
    m_hostname->setPlaceholderText(tr("Inherited: %1").arg(data.value("hostname").toString()));
    m_user->setPlaceholderText(tr("Inherited: %1").arg(data.value("user").toString()));
    m_port->setSpecialValueText(tr("Inherited: %1").arg(data.value("port").toString()));
    if (!keys.isEmpty()) m_key->setPlaceholderText(tr("Inherited: %1").arg(keys.first()));
}
void MachinesPage::updateControls() {
    const bool local = m_selected == "@local", busy = m_request || m_installing;
    const bool validAlias = QRegularExpression("^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,79}$").match(alias()).hasMatch();
    const bool duplicate = m_new && aliasExists();
    const bool wsl = m_transport->currentData().toString() == "wsl-local";
    m_alias->setReadOnly(!m_new); m_alias->setEnabled(!busy && !local);
    for (auto *field : {m_hostname, m_user, m_key}) field->setEnabled(!busy && !local);
    m_port->setEnabled(!busy && !local); m_enabled->setEnabled(!busy && !local); m_browse->setEnabled(!busy && !local);
    // #733: this computer runs its own platform's suites; a peer runs one class.
    m_testRunner->setEnabled(!busy && !local);
    const QString runner = m_testRunner->currentData().toString();
    m_testRunnerHint->setText(runner.isEmpty() ? tr("The team's suites run on this computer, or wait for a test runner of their platform.")
                              : !m_enabled->isChecked() ? tr("Turned off: a disabled Machine runs no suites; their %1 suites wait.").arg(m_testRunner->currentText())
                              : tr("%1 suites go to this Machine over SSH; when it cannot be reached they wait in the Tests view with the reason.").arg(m_testRunner->currentText()));
    const QRegularExpression wslName("^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,127}$");
    const bool validWsl = wslName.match(m_wslDistribution->text().trimmed()).hasMatch()
        && wslName.match(m_wslUser->text().trimmed()).hasMatch() && m_wslHgs->text().startsWith('/');
    m_transport->setEnabled(!busy && !local);
    for (auto *field : {m_wslDistribution, m_wslUser, m_wslHgs}) field->setEnabled(!busy && !local && wsl);
    m_save->setEnabled(!busy && !local && validAlias && !duplicate && (!wsl || validWsl) && (m_dirty || m_new));
    m_save->setToolTip(duplicate ? tr("This name already exists. Select the machine in the list to edit it.") : QString());
    // U25 (spec 8): a duplicate name is a red field with its reason.
    if (m_alias->property("pageError").toBool() != duplicate) {
        m_alias->setProperty("pageError", duplicate); m_alias->style()->unpolish(m_alias); m_alias->style()->polish(m_alias);
    }
    // Discard changes / Discard draft, while there is something to discard.
    m_discardDraft->setText(m_new ? tr("Discard draft") : tr("Discard changes"));
    m_discardDraft->setEnabled(!busy && (m_new || m_dirty));
    const bool unsaved = !local && (m_new || m_dirty);
    m_unsaved->setVisible(unsaved);
    for (auto *button : {m_check, m_ssh, m_install, m_reset, m_remove}) {
        button->setEnabled(!busy && !local && !m_new && !m_dirty);
        button->setToolTip(unsaved ? tr("Save or discard the connection first") : QString());
    }
    // The amber dot beside a machine with unsaved edits; the refresh icon
    // turns while the list loads.
    markUnsaved();
    PageCards::spin(m_refresh, m_request && m_operation == "ls");
    bool hasInheritedOverride = false;
    for (const auto &value : m_data.value("machines").toArray()) {
        const auto row = value.toObject();
        if (row.value("alias").toString() == m_selected)
            hasInheritedOverride = row.value("inherited").toBool() && row.value("source").toString() == "local";
    }
    m_reset->setEnabled(m_reset->isEnabled() && hasInheritedOverride);
    if (!unsaved) m_reset->setToolTip(tr("Restore the SSH settings of a machine from the shared configuration."));
    if (wsl) { m_install->setEnabled(false); m_reset->setEnabled(false); }
    if (!unsaved) m_install->setToolTip(wsl ? tr("Install this fork's Linux HGS explicitly in the selected distribution; no agent is installed or restarted here.") : QString());
    m_ssh->setText(wsl ? tr("Open WSL terminal") : tr("Open SSH terminal"));
    m_sessions->setEnabled(!busy && !m_new); m_accounts->setEnabled(!busy && !m_new); m_add->setEnabled(!busy); m_list->setEnabled(!busy); m_refresh->setEnabled(!busy);
    // A control console has no sessions, accounts or SSH connection of its own, and
    // no Windows computer is reached over SSH, so its page has no connection either.
    const bool console = local && !m_new && !m_fleet.local().localSessions;
    const bool windows = local && !m_new && MachinePlatform::of(m_fleet.local().metrics, true) == MachinePlatform::Style::Windows;
    m_sessions->setVisible(!console); m_accounts->setVisible(!console);
    for (int row = 0; row < m_form->rowCount(); ++row) m_form->setRowVisible(row, !console && !windows);
    for (QWidget *field : QList<QWidget *>{m_wslDistribution, m_wslUser, m_wslHgs})
        m_form->setRowVisible(field, !console && !windows && wsl);
    for (QWidget *field : QList<QWidget *>{m_hostname, m_user, m_keyRow, m_port})
        m_form->setRowVisible(field, !console && !windows && !wsl);
    // #650 N2: every remote machine, saved or not, can change its connection
    // type here (and back); this computer has none.
    m_form->setRowVisible(m_transport, !console && !windows && !local);
    // #733: a peer is a test runner; this computer serves its own platform.
    for (QWidget *field : QList<QWidget *>{m_testRunner, m_testRunnerHint}) m_form->setRowVisible(field, !console && !windows && !local);
    for (auto *button : {m_save, m_check, m_ssh, m_install, m_reset, m_remove}) button->setVisible(!console && !windows);
    m_discardDraft->setVisible(!console && !windows && (m_new || !local));
    if (console || windows) m_unsaved->hide();
    // After the form rows are shown: the error belongs to a duplicate only.
    m_aliasError->setVisible(duplicate && !console && !windows);
    // Windows (spec D16): this computer shows its session host, and a control
    // console whose CLI reports one can enable local sessions.
    const auto &host = m_fleet.local().localHost;
    const bool windowsHost = windows && !host.isEmpty();
    m_enableLocal->setVisible(local && !m_new && LocalSessionHost::canEnable(m_fleet.local())); m_enableLocal->setEnabled(!busy);
    m_localHost->setVisible(windowsHost);
    if (windowsHost) m_localHost->setText(tr("Session host: %1").arg(hostFacts(host)));
    // #647: This machine has its own card; the SSH card shows only where it
    // has a field, so no empty card stays under the label.
    m_localCard->setVisible(local && !m_new);
    m_testHost->setVisible(windowsHost && m_fleet.local().localSessions);
    m_testHost->setEnabled(!busy && !m_hostTest);
    m_connectionCard->setVisible(!console && !windows);
    m_localLine->setVisible(local && !m_new);
    updateHeader();
    updateAppearance();
}

QString MachinesPage::hostFacts(const QJsonObject &host) const
{
    const auto state = host.value("state").toString();
    QStringList facts{state == "not-installed" ? tr("not installed") : state == "stopped" ? tr("stopped")
                      : state == "running" ? tr("running") : state == "unavailable" ? tr("unavailable") : state};
    if (host.value("pid").isDouble()) facts << tr("PID %1").arg(host.value("pid").toInteger());
    if (!host.value("conpty").toString().isEmpty()) facts << tr("%1 ConPTY").arg(host.value("conpty").toString());
    if (!host.value("integrity").toString().isEmpty()) facts << tr("%1 integrity").arg(host.value("integrity").toString());
    if (host.value("windows_session").isDouble()) facts << tr("Windows session %1").arg(host.value("windows_session").toInteger());
    const auto error = host.value("error").toString();
    return facts.join(", ") + (error.isEmpty() ? QString() : ": " + error);
}

// #647: the label, its system and its state, as the prototype's header has them.
void MachinesPage::updateHeader()
{
    const bool local = m_selected == "@local" && !m_new;
    const auto localName = m_fleet.local().host.isEmpty() ? m_data.value("local").toString() : m_fleet.local().host;
    const QString name = m_new ? tr("New machine") : local ? localName : m_selected;
    static_cast<MachineTitle *>(m_title)->setMachine(name, m_dark);
    QString system;
    QString arch;
    if (local) {
        // #650 L5: read once; on Linux each QSysInfo call reads /etc/os-release,
        // and this runs on every edit.
        static const QString localSystem = QSysInfo::productType() == QLatin1String("windows") ? QStringLiteral("Windows %1").arg(QSysInfo::productVersion())
                                         : QSysInfo::productType() == QLatin1String("macos") ? QStringLiteral("macOS %1").arg(QSysInfo::productVersion())
                                         : QSysInfo::prettyProductName();
        static const QString localArch = archName(QSysInfo::currentCpuArchitecture());
        system = localSystem;
        arch = localArch;
    } else if (!m_new) {
        const auto checked = m_checks.value(m_selected);
        system = osName(checked.value("os").toString());
        if (system.isEmpty()) system = osName(m_fleet.metricsOf(m_selected).value("platform").toString());
        arch = archName(checked.value("arch").toString());
    }
    m_titleNote->setText(arch.isEmpty() || system.isEmpty() ? system : system + QStringLiteral(" · ") + arch);
    m_titleNote->setVisible(!m_titleNote->text().isEmpty());
    // A remote machine: whether it answers, its HGS and tmux.
    QString state;
    if (m_new) state = tr("Unsaved connection");
    else if (!local) {
        const auto *peer = m_fleet.peer(m_selected);
        const bool live = peer && peer->ok && QDateTime::currentMSecsSinceEpoch() - m_fleet.peerPolledAt(m_selected) < FleetState::kPeerStaleMs;
        QStringList parts{live ? tr("Connected") : tr("Not connected")};
        if (live && !peer->hgsVersion.isEmpty()) parts << tr("hgs %1").arg(peer->hgsVersion);
        const QString tmux = m_checks.value(m_selected).value("tmux").toString();
        if (live && !tmux.isEmpty()) parts << tmux;
        state = parts.join(QStringLiteral(" · "));
    }
    m_stateLine->setText(state);
    m_stateLine->setVisible(!state.isEmpty());
}
QString MachinesPage::otherTransportKey(const QString &machine)
{
    return QStringLiteral("machines/otherTransport/") + machine;
}
void MachinesPage::rememberOtherTransport() {
    // #650 N2: hgs refuses SSH overrides on a WSL profile and the other way
    // round, so the type not saved keeps its overrides on this device.
    QJsonObject other;
    if (m_transport->currentData().toString() == "wsl-local") {
        if (!m_hostname->text().trimmed().isEmpty()) other["hostname"] = m_hostname->text().trimmed();
        if (!m_user->text().trimmed().isEmpty()) other["user"] = m_user->text().trimmed();
        if (m_port->value()) other["port"] = m_port->value();
        if (!m_key->text().trimmed().isEmpty()) other["identity_file"] = m_key->text().trimmed();
    } else if (!m_wslDistribution->text().trimmed().isEmpty() || !m_wslUser->text().trimmed().isEmpty() || !m_wslHgs->text().isEmpty()) {
        other["wsl"] = QJsonObject{{"distribution", m_wslDistribution->text().trimmed()}, {"user", m_wslUser->text().trimmed()}, {"hgs_path", m_wslHgs->text()}};
    }
    // #694: a GUI-thread write never waits on the settings file's lock.
    if (other.isEmpty()) Settings::remove(otherTransportKey(alias()));
    else SettingsJson::store(otherTransportKey(alias()), QJsonDocument(other));
}
void MachinesPage::save() {
    if (m_request || m_installing || (m_new && aliasExists())) return;
    rememberOtherTransport();
    m_selected = alias(); m_operation = "set";
    QStringList arguments{"set", alias(), "--json", QString::fromUtf8(QJsonDocument(connection()).toJson(QJsonDocument::Compact)), "--revision", m_data.value("revision").toString()};
    if (m_new) arguments << "--create";
    m_request = m_client->requestMachines(arguments); updateControls();
}
void MachinesPage::check() { m_operation = "check"; report(m_transport->currentData().toString() == "wsl-local" ? tr("Testing WSL and checking HGS…") : tr("Testing SSH and checking HGS…")); m_request = m_client->requestMachines({"check", alias()}); updateControls(); }
void MachinesPage::enableLocalSessions() {
    if (m_request || m_installing) return;
    QMessageBox confirm(QMessageBox::Question, tr("Enable local sessions?"),
        tr("Run agents on this computer too? Zerus stages the HGS CLI and a session host in your user profile and enables local sessions. "
           "To disable local sessions again, run hgs session-host uninstall."), QMessageBox::Cancel, this);
    confirm.setObjectName("enableLocalSessionsConfirm"); confirm.setTextFormat(Qt::PlainText);
    auto *proceed = confirm.addButton(tr("Enable local sessions"), QMessageBox::AcceptRole);
    confirm.setDefaultButton(QMessageBox::Cancel); confirm.setEscapeButton(QMessageBox::Cancel);
    confirm.exec(); if (confirm.clickedButton() != proceed || m_request || m_installing) return;
    m_installing = true; report(tr("Enabling local sessions on this computer…")); updateControls();
    m_hostRequest = m_client->requestSessionHost("install");
}
void MachinesPage::install() {
    QDialog dialog(this); dialog.setObjectName("machineSetupDialog"); dialog.setWindowTitle(tr("Install HGS on %1").arg(alias())); dialog.resize(520, 280);
    auto *layout = new QVBoxLayout(&dialog);
    auto *description = new QLabel(tr("Build and install the HGS CLI on %1 using SSH. The newest stable Rust and tmux must already be installed there. Existing sessions and configuration are preserved.").arg(alias())); description->setWordWrap(true); layout->addWidget(description);
    auto *source = new QLineEdit; source->setObjectName("machineSetupSource"); source->setPlaceholderText(tr("Default: ~/.local/src/hgs on this device")); layout->addWidget(source);
    auto *browse = new QPushButton(tr("Choose HGS source folder…")); layout->addWidget(browse);
    connect(browse, &QPushButton::clicked, &dialog, [&] { const auto dir = QFileDialog::getExistingDirectory(&dialog, tr("HGS source folder")); if (!dir.isEmpty()) source->setText(dir); });
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel); buttons->button(QDialogButtonBox::Ok)->setText(tr("Install HGS")); layout->addWidget(buttons);
    connect(buttons, &QDialogButtonBox::accepted, &dialog, &QDialog::accept); connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    if (dialog.exec() != QDialog::Accepted) return;
    m_installing = true; m_log->clear(); m_log->show(); report(tr("Installing on %1… Keep Zerus running until setup finishes.").arg(alias())); updateControls(); m_client->setupMachine(alias(), source->text().trimmed());
}
