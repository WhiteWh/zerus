#pragma once
// WhiteWh fork, #713: the fixture library of the agent-team UI suites
// (test_teamsui_*.cpp). Each suite tests one area of the team surfaces and
// shares this file: the fake hgs, the teams, catalogs and profiles they read,
// the shot helpers and TeamsUiFixture, which installs the fake once per suite
// and guards each test against a modal left open. A helper only one suite
// uses lives in that suite.
//
// The suites were one file, test_teamsui.cpp: agent teams in the desktop,
// M3–M6 (design/agent-teams S3, S4, S6, S7, S10, S11): the task card, planning
// edits, New team, the catalog editor, the Team designer's draft and the team
// workspace. Each sends one `hgs team` request per change, recorded by a fake
// CLI.
#include "Wait.h"
#include "Settings.h"
#include <QPointer>
#include <QScrollBar>
#include <QtTest>
#include <QPainter>
#include <memory>
#include <tuple>
#include <QStandardItemModel>
#include <algorithm>
#include <QStyle>
#include <QStyleFactory>
#include <QStyleOption>
#include <QApplication>
#include <QCheckBox>
#include <QComboBox>
#include <QDialog>
#include <QDialogButtonBox>
#include <QJsonArray>
#include <QImage>
#include <QJsonDocument>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMenu>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QRadioButton>
#include <QScrollArea>
#include <QSpinBox>
#include <QSplitter>
#include <QTabWidget>
#include <QTemporaryDir>
#include <QProcess>
#include <QTextBlock>
#include <QTextBrowser>
#include <QTimer>
#include <QUuid>
#include <QToolButton>
#include <QVBoxLayout>
#include <QClipboard>
#include "TeamController.h"
#include <QPainter>
#include <QUuid>
#include "TeamModel.h"
#include "TeamPanel.h"
#include "TeamWidgets.h"
#include <QScopeGuard>
#include <QElapsedTimer>
#include <QTableWidget>
#include <QInputDialog>
#include "FakeCli.h"
#include "DialogTheme.h"
using namespace Teams;

// #713: screenshots are opt-in. Without ZERUS_TEAM_SHOTS a test takes no
// shot and skips the light half it would switch to only for one.
inline bool shotsWanted() { return !qEnvironmentVariable("ZERUS_TEAM_SHOTS").isEmpty(); }

namespace {
QJsonObject team(const QString &status = "running")
{
    auto t = QJsonDocument::fromJson(R"({
 "id":"team-1","name":"Checkout redesign","status":"running","project":"Checkout","folder":"/work/checkout","lead":"lead",
 "revision":12,"created":1000,"updated":5000,"settings":{},
 "members":[
  {"id":"lead","role":"Architect","agent":"claude","model":"Opus","state":"active","session":{"host":"","name":"claude/checkout/lead"}},
  {"id":"backend","role":"Backend","agent":"codex","model":"gpt-5.5-codex","state":"active","session":{"host":"","name":"codex/checkout/backend"}},
  {"id":"tester","role":"Tester","agent":"kimi","model":"kimi-k2","computer":"msi-old","state":"active","session":{"host":"msi-old","name":"kimi/checkout/tester"}}],
 "tasks":[
  {"id":1,"title":"Requirements","assignee":"lead","status":"accepted","attempts":[{"n":1,"id":"a1","member":"lead","state":"reported","report":{"text":"Done","at":1100}}]},
  {"id":2,"title":"Address form UI","assignee":"backend","status":"submitted","deps":[{"task":1}],"criteria":["Validation messages","Keyboard flow"],
   "description":"New address form.","attempts":[{"n":1,"id":"a2","member":"backend","state":"reported","sent_at":1200,"report":{"text":"Form ready in src/forms","at":1300}}]},
  {"id":3,"title":"Saved cards API","assignee":"backend","status":"in_progress","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a3","member":"backend","state":"rejected"},{"n":2,"id":"a3b","member":"backend","state":"delivered","sent_at":1400}]},
  {"id":4,"title":"Unit tests","assignee":"tester","status":"assigned","deps":[{"task":3,"kind":"soft"}],
   "attempts":[{"n":1,"id":"a4","member":"tester","state":"unconfirmed","sent_at":1500}]},
  {"id":5,"title":"Code review","assignee":"backend","status":"pending","deps":[{"task":2},{"task":4}]}],
 "layers":{"1":0,"2":1,"3":1,"4":2,"5":3},"cycle":[],"ready":[],"validation":{"errors":[],"warnings":[]}})").object();
    if (status == "planning") {
        t["status"] = "planning";
        auto members = t["members"].toArray();
        for (int i = 1; i < members.size(); ++i) { auto m = members[i].toObject(); m.remove("session"); m["state"] = "not_created"; members[i] = m; }
        t["members"] = members;
        auto tasks = t["tasks"].toArray();
        for (int i = 0; i < tasks.size(); ++i) { auto k = tasks[i].toObject(); k["status"] = "draft"; k.remove("attempts"); tasks[i] = k; }
        auto extra = QJsonObject{{"id", 6}, {"title", "E2E tests"}, {"status", "draft"}};
        tasks.append(extra);
        t["tasks"] = tasks;
        t["validation"] = QJsonObject{{"errors", QJsonArray{QJsonObject{{"kind", "assignee"}, {"task", 6}, {"text", "#6 E2E tests: no assignee"}}}}, {"warnings", QJsonArray()}};
        auto layers = t["layers"].toObject(); layers["6"] = 0; t["layers"] = layers;
    }
    return t;
}

// Opens the member menu of `member` and hands the active menu to `inspect`.
void inspectMemberMenu(QWidget *root, const QString &member, const std::function<void(QMenu *)> &inspect)
{
    QToolButton *more = nullptr;
    for (auto *card : root->findChildren<QFrame *>("teamWorkspaceMember"))
        if (card->property("teamMember") == member && card->isVisibleTo(root)) more = card->findChild<QToolButton *>("teamWorkspaceMemberMenu");
    QVERIFY(more);
    bool seen = false;
    std::function<void(int)> look = [&](int tries) {
        QTimer::singleShot(10, qApp, [&, tries] {
            auto *menu = qobject_cast<QMenu *>(QApplication::activePopupWidget());
            if (!menu) { if (tries > 0) look(TestWait::busy() ? tries : tries - 1); return; }
            seen = true;
            inspect(menu);
            menu->hide();
        });
    };
    // Retries count only while nothing is in flight: under a loaded machine
    // the popup can take seconds to show.
    look(1000);
    more->click();
    WAIT_VERIFY(seen);
}

// U25 part 5 (spec 5.4): the prototype's checkout - phase 1 done, phase 2
// Build running (a question on #2, #3 at work, a long edge #3 -> #6 past #5),
// phase 3 a draft that takes edge edits.
QJsonObject graphTeam()
{
    auto t = QJsonDocument::fromJson(R"({
 "id":"team-1","name":"Checkout v2","status":"running","project":"Checkout","folder":"/work/checkout","lead":"lead",
 "revision":21,"created":1000,"updated":5000,"settings":{"approval":"review","phase_approval":"review"},
 "members":[
  {"id":"lead","role":"Lead","agent":"claude","model":"claude-opus-4-6","state":"active","session":{"host":"","name":"claude/checkout/lead"}},
  {"id":"backend","role":"Backend","agent":"dsh","model":"deepseek-v4-pro","state":"active","session":{"host":"","name":"dsh/checkout/backend"}},
  {"id":"frontend","role":"Frontend","agent":"codex","model":"gpt-6.1-sol","state":"active","session":{"host":"","name":"codex/checkout/frontend"}},
  {"id":"tester","role":"Tester","agent":"kimi","model":"kimi-k2.5","state":"active","session":{"host":"","name":"kimi/checkout/tester"}}],
 "phases":[
  {"n":2,"order":2,"title":"Build","state":"approved","tasks":[2,3,4,5,6],"added_by":"Lead","added_at":1500},
  {"n":3,"order":3,"title":"Verify & ship","state":"draft","tasks":[7,8,9],"added_by":"Lead","added_at":1700}],
 "tasks":[
  {"id":1,"phase":1,"title":"Requirements and acceptance","assignee":"lead","status":"accepted"},
  {"id":2,"phase":2,"title":"Saved cards API","assignee":"backend","status":"in_progress","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a2","member":"backend","state":"delivered","sent_at":1600,"questions":[{"id":"q1","text":"Keep the old field?","at":1650}]}]},
  {"id":3,"phase":2,"title":"Card form UI","assignee":"frontend","status":"in_progress","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a3","member":"frontend","state":"delivered","sent_at":1600,"started_at":1610}]},
  {"id":4,"phase":2,"title":"Address validation","assignee":"frontend","status":"submitted","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a4","member":"frontend","state":"reported","report":{"text":"Done","at":1700}}]},
  {"id":5,"phase":2,"title":"Contract tests","assignee":"tester","status":"pending","deps":[{"task":2},{"task":3}]},
  {"id":6,"phase":2,"title":"E2E checkout flow","assignee":"tester","status":"pending","deps":[{"task":3},{"task":4},{"task":5}]},
  {"id":7,"phase":3,"title":"Release notes","assignee":"lead","status":"draft","deps":[{"task":8},{"task":9}]},
  {"id":8,"phase":3,"title":"Rate limits on /v1/cards","assignee":"backend","status":"draft","deps":[{"task":2}]},
  {"id":9,"phase":3,"title":"Load test checkout","assignee":"tester","status":"draft","deps":[{"task":6}]}],
 "layers":{"1":0,"2":1,"3":1,"4":1,"5":2,"6":3,"7":5,"8":2,"9":4},"cycle":[],"ready":[],
 "validation":{"errors":[],"warnings":[]}})").object();
    return t;
}

// A running team with two phases appended: phase 1 is finished (it starts
// collapsed), phase 2 has a barrier, phase 3 waits as a draft.
QJsonObject phasedTeam()
{
    auto t = team();
    t["settings"] = QJsonObject{{"all_hard", false}, {"approval", "review"}, {"phase_approval", "review"}};
    t["waiting_for_lead"] = false;
    t["pending_phase"] = 3;
    t["phases"] = QJsonArray{
        QJsonObject{{"n", 2}, {"title", "QA fixes"}, {"barrier", true}, {"approval", "review"}, {"state", "approved"},
                    {"tasks", QJsonArray{3, 4}}, {"added_by", "Lead"}, {"added_at", 1500.0},
                    {"decided_by", "You"}, {"decided_at", 1600.0}},
        QJsonObject{{"n", 3}, {"title", "Polish"}, {"barrier", false}, {"approval", "review"}, {"state", "draft"},
                    {"tasks", QJsonArray{5}}, {"added_by", "Lead"}, {"added_at", 1700.0}}};
    t["tasks"] = QJsonArray{
        QJsonObject{{"id", 1}, {"phase", 1}, {"title", "Requirements"}, {"assignee", "lead"}, {"status", "accepted"},
                    {"attempts", QJsonArray{QJsonObject{{"n", 1}, {"id", "a1"}, {"member", "lead"}, {"state", "reported"}}}}},
        QJsonObject{{"id", 2}, {"phase", 1}, {"title", "Checkout design"}, {"assignee", "backend"}, {"status", "accepted"}},
        QJsonObject{{"id", 3}, {"phase", 2}, {"title", "Address form UI"}, {"assignee", "backend"}, {"status", "accepted"},
                    {"deps", QJsonArray{QJsonObject{{"task", 2}}}},
                    {"attempts", QJsonArray{QJsonObject{{"n", 1}, {"id", "a3"}, {"member", "backend"}, {"state", "reported"}}}}},
        QJsonObject{{"id", 4}, {"phase", 2}, {"title", "Unit tests"}, {"assignee", "tester"}, {"status", "in_progress"},
                    {"deps", QJsonArray{QJsonObject{{"task", 3}, {"kind", "soft"}}}},
                    {"attempts", QJsonArray{QJsonObject{{"n", 1}, {"id", "a4"}, {"member", "tester"}, {"state", "delivered"}, {"sent_at", 1600.0}}}}},
        QJsonObject{{"id", 5}, {"phase", 3}, {"title", "Code review"}, {"assignee", "backend"}, {"status", "draft"},
                    {"deps", QJsonArray{QJsonObject{{"task", 4}}}}}};
    t["layers"] = QJsonObject{{"1", 0}, {"2", 0}, {"3", 0}, {"4", 1}, {"5", 2}};
    t["ready"] = QJsonArray{4};
    t["validation"] = QJsonObject{{"errors", QJsonArray()}, {"warnings", QJsonArray()}};
    return t;
}

// The delivery states of the spec (A1-A6): one task per attempt phase, a
// question, a held task, a replaced report and a captured one.
QJsonObject deliveryTeam()
{
    return QJsonDocument::fromJson(R"({
 "id":"team-2","name":"Delivery","status":"running","project":"Checkout","folder":"/work/delivery","lead":"lead",
 "revision":3,"created":1000,"updated":5000,"settings":{},
 "members":[
  {"id":"lead","role":"Architect","agent":"claude","state":"active","session":{"host":"","name":"claude/delivery/lead"}},
  {"id":"backend","role":"Backend","agent":"codex","state":"active","session":{"host":"","name":"codex/delivery/backend"}},
  {"id":"tester","role":"Tester","agent":"kimi","state":"active","session":{"host":"","name":"kimi/delivery/tester"}}],
 "tasks":[
  {"id":11,"title":"Pending hand-off","assignee":"backend","status":"assigned",
   "attempts":[{"n":1,"id":"s1","member":"backend","state":"delivering","sent_at":1700}]},
  {"id":12,"title":"In the terminal","assignee":"backend","status":"assigned",
   "attempts":[{"n":1,"id":"s2","member":"backend","state":"delivered","sent_at":1700,"delivered_at":1701}]},
  {"id":13,"title":"Late start","assignee":"backend","status":"in_progress",
   "attempts":[{"n":1,"id":"s3","member":"backend","state":"delivered","delivered_at":1700,"start_warned":true}]},
  {"id":14,"title":"Building","assignee":"backend","status":"in_progress",
   "attempts":[{"n":2,"id":"s4","member":"backend","state":"delivered","delivered_at":1700,"started_at":1710}]},
  {"id":15,"title":"Asks the lead","assignee":"tester","status":"in_progress",
   "attempts":[{"n":1,"id":"s5","member":"tester","state":"delivered","delivered_at":1700,"started_at":1710,
    "questions":[{"id":"q1","text":"Must I keep the old field?","at":1720,"answer":"","answered_at":null,"by":""}]}]},
  {"id":16,"title":"Held","assignee":"tester","status":"pending",
   "hold":{"by":"Lead","at":1730,"reason":"the member is busy"},
   "attempts":[{"n":1,"id":"s6","member":"tester","state":"rejected"}]},
  {"id":17,"title":"Reported","assignee":"tester","status":"submitted",
   "attempts":[{"n":1,"id":"s7","member":"tester","state":"reported","delivered_at":1700,"started_at":1701,
    "report":{"text":"Done: the tests pass","at":1740,"captured":false,
              "replaced":[{"text":"Building and running the tests...","at":1735}]}}]},
  {"id":18,"title":"No report","assignee":"lead","status":"in_progress",
   "attempts":[{"n":1,"id":"s8","member":"lead","state":"delivered","delivered_at":1700,"turn_ended_at":1750,"no_report":true}]},
  {"id":19,"title":"Stopped","assignee":"lead","status":"in_progress",
   "attempts":[{"n":1,"id":"s9","member":"lead","state":"delivered","delivered_at":1700,"started_at":1701,"phase":"stopped"}]},
  {"id":20,"title":"Rejected","assignee":"backend","status":"rejected",
   "attempts":[{"n":1,"id":"s10","member":"backend","state":"rejected","phase":null}]},
  {"id":21,"title":"Accepted work","assignee":"backend","status":"accepted",
   "attempts":[{"n":1,"id":"s12","member":"backend","state":"reported","phase":null,
                "report":{"text":"Done long ago","at":1750}}]},
  {"id":22,"title":"Captured","assignee":"backend","status":"submitted",
   "attempts":[{"n":1,"id":"s11","member":"backend","state":"reported","report":{"text":"Captured reply","at":1760,"captured":true}}]},
  {"id":23,"title":"Park me","assignee":"backend","status":"pending"},
  {"id":24,"title":"Retried once","assignee":"backend","status":"pending",
   "attempts":[{"n":1,"id":"s13","member":"backend","state":"revoked","phase":null},
               {"n":2,"id":"s14","member":"backend","state":"refused","phase":null}]}],
 "layers":{"11":0,"12":1,"13":2,"14":2,"15":2,"16":2,"17":2,"18":2,"19":2,"20":2,"21":2,"22":2,"23":2,"24":2},
 "cycle":[],"ready":[],"validation":{"errors":[],"warnings":[]}})").object();
}

QJsonObject catalog()
{
    return QJsonDocument::fromJson(R"({"builtin":false,
 "catalog":{"version":3,
  "roles":[{"id":"lead","name":"Lead","agent":"claude","prompt":"You lead."},
           {"id":"backend","name":"Backend","agent":"codex","folder":"worktree","prompt":"Build the API."},
           {"id":"tester","name":"Tester","agent":"kimi","model":"kimi-k2","folder":"worktree","prompt":"Test it."}],
  "templates":[{"id":"feature","name":"Feature delivery","lead_protocol":"Plan in waves of three","roster":[{"role":"backend","count":1},{"role":"tester","count":1}],
                "tasks":[{"title":"Plan","role":"lead","deps":[]},{"title":"Build","role":"backend","deps":[0]},{"title":"Test","role":"tester","deps":[1]}],
                "planning":"template","approval":"review","phase_approval":"auto","max_members":6}],
  "routing":[]},
 "draft":{"base":3,"proposals":[
  {"id":"p1","kind":"role","action":"change","target":"tester","title":"Tester: Kimi → Claude Sonnet","reason":"Deliveries to msi-old fail",
   "value":{"id":"tester","name":"Tester","agent":"claude","model":"Sonnet","folder":"worktree","prompt":"Test it."},"state":"proposed"},
  {"id":"p2","kind":"role","action":"add","target":"security","title":"New role: Security","reason":"Payments code",
   "value":{"id":"security","name":"Security","agent":"claude","prompt":"Look for secrets."},"state":"kept"}]}})").object();
}

Live live(const Member &member)
{
    Live state; state.known = true;
    if (member.id == "tester") state.offline = false;
    return state;
}

QList<QJsonObject> calls(const QString &script, const QString &operation)
{
    QList<QJsonObject> found;
    QFile file(script + ".calls");
    if (!file.open(QIODevice::ReadOnly)) return found;
    for (const auto &line : file.readAll().split('\n')) {
        const auto call = QJsonDocument::fromJson(line).object();
        QStringList args; for (const auto &a : call["args"].toArray()) args << a.toString();
        if (args.mid(1, operation.count(' ') + 1).join(' ') == operation) found << call["input"].toObject();
    }
    return found;
}

void shot(QWidget *widget, const QString &name)
{
    const auto dir = qEnvironmentVariable("ZERUS_TEAM_SHOTS");
    if (dir.isEmpty()) return;
    TestWait::settle();
    const QString path = QDir(dir).filePath(name + ".png");
    if (!widget->grab().save(path)) qWarning() << "screenshot not saved:" << path;
}

// U25: a phase's scroll in the Team tab, an action in its header, its graph,
// and every task node the open phase graphs show (sorted).
QFrame *phaseScrollOf(QWidget *root, int phase)
{
    for (auto *frame : root->findChildren<QFrame *>("teamPhaseScroll")) if (frame->property("phase").toInt() == phase) return frame;
    return nullptr;
}
bool phaseOpen(QWidget *root, int phase)
{
    auto *frame = phaseScrollOf(root, phase);
    return frame && frame->findChild<QWidget *>("teamPhaseBody");
}
void togglePhase(QWidget *root, int phase)
{
    if (auto *frame = phaseScrollOf(root, phase)) if (auto *toggle = frame->findChild<QToolButton *>("teamPhaseToggle")) toggle->click();
}
TeamUi::TaskView *phaseGraphOf(QWidget *root, int phase)
{
    auto *frame = phaseScrollOf(root, phase);
    return frame ? dynamic_cast<TeamUi::TaskView *>(frame->findChild<QWidget *>("teamPhaseGraph")) : nullptr;
}
QList<int> phaseNodeIds(QWidget *root)
{
    QList<int> ids;
    for (auto *w : root->findChildren<QWidget *>("teamPhaseGraph"))
        if (auto *view = dynamic_cast<TeamUi::TaskView *>(w)) for (const auto &node : view->nodes) ids.append(node.id);
    std::sort(ids.begin(), ids.end());
    return ids;
}

// A point in a TaskView's widget coordinates: its painted rects are in graph
// coordinates, which the view scales by its zoom.
QPoint viewPoint(const TeamUi::TaskView *view, const QRectF &rect)
{
    return QPoint(int(std::round(rect.center().x() * view->zoom)), int(std::round(rect.center().y() * view->zoom)));
}

// Arms the owner's confirmation on the next modal dialog: `cancel` clicks the
// reject (Cancel) button, otherwise the destructive action button. The dialog
// runs its own event loop inside the click, so a zero timer lands inside it; the
// retry covers a timer that fires before the dialog is up.
// Clicks `point` on `view` and reports whether a confirmation opened; one that
// opens is cancelled, so a regression fails the check instead of hanging.
bool clickAsks(QWidget *view, const QPoint &point)
{
    bool asked = false;
    QTimer watch; watch.setInterval(5);
    QObject::connect(&watch, &QTimer::timeout, [&asked] {
        if (auto *box = qobject_cast<QMessageBox *>(QApplication::activeModalWidget())) { asked = true; box->reject(); }
    });
    watch.start();
    QTest::mouseClick(view, Qt::LeftButton, {}, point);
    TestWait::settle();
    return asked;
}

// Picks the action named `name` from the next popup menu (QMenu::exec blocks).
void chooseMenuAction(const QString &name, int tries = 300)
{
    QTimer::singleShot(10, qApp, [name, tries] {
        auto *menu = qobject_cast<QMenu *>(QApplication::activePopupWidget());
        if (!menu) { if (tries > 0) chooseMenuAction(name, TestWait::busy() ? tries : tries - 1); return; }
        QAction *found = nullptr;
        for (auto *action : menu->actions()) if (action->objectName() == name) found = action;
        menu->hide();
        if (found && found->isEnabled()) found->trigger();
    });
}

// #344: a running team with what the lead proposes (F8), a swap's old session
// (H6), a member at a usage limit (H1) and two members on one DeepSeek host (H3),
// shaped like `hgs team status` (spec 4, 5, 6.1, 6.3).
QJsonObject proposalsTeam(double resetsAt)
{
    auto t = team();
    t["settings"] = QJsonObject{{"approval", "review"}, {"report_timeout_seconds", 120}, {"resume_after_limits", false}};
    auto members = t["members"].toArray();
    auto backend = members[1].toObject();
    backend["agent"] = "dsh";
    backend["limited"] = QJsonObject{{"at", resetsAt - 3600}, {"resets_at", resetsAt}, {"detail", "Usage limit reached \u00b7 resets 5pm"}};
    backend["host_shared"] = true;
    members[1] = backend;
    auto tester = members[2].toObject();
    tester["agent"] = "dsh";
    tester["host_shared"] = true;
    members[2] = tester;
    t["members"] = members;
    t["hosts"] = QJsonArray{QJsonObject{{"agent", "dsh"}, {"pid", 4242}, {"started", 1000.0}, {"members", QJsonArray{"backend", "tester"}}}};
    t["proposals"] = QJsonArray{
        QJsonObject{{"id", "p1"}, {"kind", "add-member"}, {"by", "Lead"}, {"at", 2000.0}, {"state", "open"}, {"reason", "The UI needs a designer"},
                    {"member", QJsonObject{{"role", "Designer"}, {"agent", "claude"}, {"model", "sonnet"}}}, {"hash", "h1"}},
        QJsonObject{{"id", "p2"}, {"kind", "swap-member"}, {"by", "Lead"}, {"at", 2100.0}, {"state", "open"}, {"reason", "Backend hit its limit"},
                    {"member", QJsonObject{{"member", "backend"}, {"agent", "codex"}, {"model", "gpt-5.5"}}}, {"hash", "h2"}},
        QJsonObject{{"id", "p3"}, {"kind", "remove-member"}, {"by", "Lead"}, {"at", 2200.0}, {"state", "open"}, {"reason", "Testing is done"},
                    {"member", "tester"}, {"hash", "h3"}},
        QJsonObject{{"id", "p4"}, {"kind", "next-goal"}, {"by", "Lead"}, {"at", 1900.0}, {"state", "rejected"}, {"reason", "Later"},
                    {"goal", "Saved cards"}}};
    t["swapped"] = QJsonArray{QJsonObject{{"member", "Backend"}, {"old", "dsh/checkout/backend-old"}, {"at", 1800.0}, {"by", "You"}}};
    return t;
}

void armConfirm(TeamPanel *panel, bool cancel, int tries = 300)
{
    QTimer::singleShot(0, panel, [panel, cancel, tries] {
        auto *box = qobject_cast<QMessageBox *>(QApplication::activeModalWidget());
        if (!box) {
            if (tries > 0) QTimer::singleShot(10, panel, [panel, cancel, tries] { armConfirm(panel, cancel, TestWait::busy() ? tries : tries - 1); });
            return;
        }
        for (auto *button : box->buttons())
            if (cancel ? box->buttonRole(button) == QMessageBox::RejectRole
                       : box->buttonRole(button) == QMessageBox::DestructiveRole) { button->click(); return; }
        for (auto *button : box->buttons())
            if (!button->text().contains(QStringLiteral("Cancel"))) { button->click(); return; }
    });
}

// Answers the next confirmation after recording its object name and text: Cancel
// clicks the reject button, otherwise the destructive action. F12/N40 uses it
// to check what the owner is warned about.
void armConfirmRecord(TeamPanel *panel, bool cancel, QString *name, QString *text, int delay = 0, int tries = 300)
{
    QTimer::singleShot(delay, panel, [panel, cancel, name, text, tries] {
        auto *box = qobject_cast<QMessageBox *>(QApplication::activeModalWidget());
        if (!box) {
            if (tries > 0) armConfirmRecord(panel, cancel, name, text, 10, TestWait::busy() ? tries : tries - 1);
            return;
        }
        if (name) *name = box->objectName();
        if (text) *text = box->text();
        for (auto *button : box->buttons())
            if (cancel ? box->buttonRole(button) == QMessageBox::RejectRole
                       : box->buttonRole(button) == QMessageBox::DestructiveRole) { button->click(); return; }
        box->reject();
    });
}

// Answers the next confirmation after running `during` while it is open, as a
// poll arriving under the dialog does.
void armConfirmAfter(TeamPanel *panel, std::function<void()> during, int tries = 300)
{
    QTimer::singleShot(0, panel, [panel, during, tries] {
        auto *box = qobject_cast<QMessageBox *>(QApplication::activeModalWidget());
        if (!box) {
            if (tries > 0) QTimer::singleShot(10, panel, [panel, during, tries] { armConfirmAfter(panel, during, TestWait::busy() ? tries : tries - 1); });
            return;
        }
        during();
        for (auto *button : box->buttons())
            if (box->buttonRole(button) == QMessageBox::DestructiveRole) { button->click(); return; }
    });
}

void darkPalette()
{
    if (qEnvironmentVariable("ZERUS_TEAM_SHOTS").isEmpty()) return;
    QApplication::setStyle("Fusion");
    QPalette p;
    p.setColor(QPalette::Window, QColor("#15191f")); p.setColor(QPalette::WindowText, QColor("#e6e8eb"));
    p.setColor(QPalette::Base, QColor("#1b1f26")); p.setColor(QPalette::Button, QColor("#232830"));
    p.setColor(QPalette::ButtonText, QColor("#e6e8eb")); p.setColor(QPalette::Text, QColor("#e6e8eb"));
    p.setColor(QPalette::PlaceholderText, QColor("#8b98a8"));
    QApplication::setPalette(p);
}

// The light half of the shots; no effect without ZERUS_TEAM_SHOTS, like darkPalette().
void lightPalette()
{
    if (qEnvironmentVariable("ZERUS_TEAM_SHOTS").isEmpty()) return;
    QApplication::setStyle("Fusion");
    QPalette p;
    p.setColor(QPalette::Window, QColor("#f5f7f9")); p.setColor(QPalette::WindowText, QColor("#1a2733"));
    p.setColor(QPalette::Base, QColor("#ffffff")); p.setColor(QPalette::Button, QColor("#edf2f5"));
    p.setColor(QPalette::ButtonText, QColor("#1a2733")); p.setColor(QPalette::Text, QColor("#1a2733"));
    p.setColor(QPalette::PlaceholderText, QColor("#627082"));
    QApplication::setPalette(p);
}

// True when the grab is really light: the average pixel lightness is well above
// the dark theme's. A "light" shot of a dark widget is a lie, so it fails.
bool looksLight(QWidget *widget)
{
    const auto image = widget->grab().toImage();
    if (image.isNull()) return false;
    qint64 sum = 0; int count = 0;
    for (int y = 0; y < image.height(); y += 5)
        for (int x = 0; x < image.width(); x += 5) { sum += image.pixelColor(x, y).lightness(); ++count; }
    return count > 0 && sum / count > 150;
}

// The light half of a shot, asserted to be light before it is saved.
void lightShot(QWidget *widget, const QString &name)
{
    if (!shotsWanted()) return;
    lightPalette();
    TestWait::settle();
    QVERIFY2(looksLight(widget), qPrintable(QStringLiteral("the light shot is dark: ") + name));
    shot(widget, name);
}
// U25: the prototype's team (shots 10-19): Analysis done, Build running with a
// question, Verify & ship a draft with the lead's note.
QJsonObject prototypeTeam(const QString &third = QStringLiteral("draft"))
{
    auto t = QJsonDocument::fromJson(R"({
 "id":"team-1","name":"Checkout","status":"running","project":"Orbit platform","folder":"/work/orbit","lead":"lead",
 "revision":20,"created":1000,"updated":5000,"settings":{},"pending_phase":3,
 "members":[
  {"id":"lead","role":"Lead","agent":"claude","model":"claude-opus-4-6","state":"active","session":{"host":"","name":"claude/orbit/checkout"}},
  {"id":"backend","role":"Backend","agent":"dsh","model":"deepseek-v4-pro","state":"active","session":{"host":"","name":"dsh/orbit/checkout-api"}},
  {"id":"frontend","role":"Frontend","agent":"codex","model":"gpt-6.1-sol","state":"active","session":{"host":"","name":"codex/orbit/checkout-ui"}},
  {"id":"tester","role":"Tester","agent":"kimi","model":"kimi-k2.5","state":"active","session":{"host":"","name":"kimi/orbit/checkout-tests"}}],
 "phases":[
  {"n":1,"title":"Analysis","state":"approved","tasks":[1],"added_by":"Lead","added_at":1000},
  {"n":2,"title":"Build","state":"approved","tasks":[2,3,4,5,6],"added_by":"Lead","added_at":1000},
  {"n":3,"title":"Verify & ship","state":"draft","tasks":[8,9,7],"added_by":"Lead","added_at":1700,
   "note":"Lead proposed the next phase while Build is running: rate limits, a load test and the release notes."}],
 "tasks":[
  {"id":1,"phase":1,"title":"Requirements and acceptance","assignee":"lead","status":"accepted"},
  {"id":2,"phase":2,"title":"Saved cards API","assignee":"backend","status":"in_progress","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a2","member":"backend","state":"delivered","sent_at":1400}]},
  {"id":3,"phase":2,"title":"Card form UI","assignee":"frontend","status":"in_progress","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a3","member":"frontend","state":"delivered","sent_at":1400}]},
  {"id":4,"phase":2,"title":"Address validation","assignee":"frontend","status":"submitted","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a4","member":"frontend","state":"reported","sent_at":1300,"report":{"text":"Done","at":1500}}]},
  {"id":5,"phase":2,"title":"Contract tests","assignee":"tester","status":"pending","deps":[{"task":2}]},
  {"id":6,"phase":2,"title":"E2E checkout flow","assignee":"tester","status":"pending","deps":[{"task":3},{"task":5}]},
  {"id":8,"phase":3,"title":"Rate limits on /v1/cards","assignee":"backend","status":"draft","deps":[{"task":2}]},
  {"id":9,"phase":3,"title":"Load test checkout","assignee":"tester","status":"draft","deps":[{"task":6}]},
  {"id":7,"phase":3,"title":"Release notes","assignee":"lead","status":"draft","deps":[{"task":8},{"task":9}]}],
 "layers":{"1":0,"2":1,"3":1,"4":1,"5":2,"6":3,"8":2,"9":4,"7":5},"cycle":[],"ready":[],
 "validation":{"errors":[],"warnings":[]}})").object();
    auto phases = t["phases"].toArray();
    auto last = phases[2].toObject();
    if (third == "rejected") {
        last["state"] = "rejected";
        last["rejection"] = QJsonObject{{"comment", "Run the load test before the rate limits; release notes last."}, {"by", "owner"}, {"at", 1800}};
        t["pending_phase"] = QJsonValue();
    } else if (third == "queued") {
        last["state"] = "queued";
        last["waiting"] = QJsonObject{{"text", "Starts after phase 2"}, {"phase", 2}};
        auto tasks = t["tasks"].toArray();
        for (int i = 0; i < tasks.size(); ++i) { auto k = tasks[i].toObject(); if (k["status"] == "draft") k["status"] = "pending"; tasks[i] = k; }
        t["tasks"] = tasks; t["pending_phase"] = QJsonValue();
    }
    phases[2] = last; t["phases"] = phases;
    // The store's revision moves with every decision.
    t["revision"] = third == "rejected" ? 21 : third == "queued" ? 22 : 20;
    return t;
}

Live prototypeLive(const Member &member)
{
    Live state; state.known = true;
    if (member.id == "backend") { state.attention = true; state.attentionText = QStringLiteral("Approval: Run command: cargo sqlx migrate run"); }
    return state;
}
// U36: the mock-up's Orbit checkout team (design/agent-teams/pipelines) in
// phase 2 with every stage state, shaped like `hgs team status` with #531's
// pipeline_run and #532's stage queue and folders. Times count from 1800000000;
// the tests set the views' clock to 1800004000.
QJsonObject pipelineTeam()
{
    return QJsonDocument::fromJson(R"JSON({
 "id":"team-1","name":"Checkout","status":"running","project":"Orbit platform","folder":"/work/orbit","lead":"lead",
 "revision":30,"created":1000,"updated":5000,"settings":{},
 "members":[
  {"id":"lead","role":"Lead","agent":"claude","model":"claude-opus-4-6","state":"active","session":{"host":"","name":"claude/orbit/checkout"}},
  {"id":"backend","role":"Backend","agent":"dsh","model":"deepseek-v4-pro","state":"active","directory":"/work/orbit-team/backend","session":{"host":"","name":"dsh/orbit/api"}},
  {"id":"frontend","role":"Frontend","agent":"codex","model":"gpt-6.1-sol","state":"active","directory":"/work/orbit-team/frontend","session":{"host":"","name":"codex/orbit/ui"}},
  {"id":"reviewer","role":"Reviewer","agent":"claude","model":"claude-sonnet-4-6","state":"active","session":{"host":"","name":"claude/orbit/review"},
   "stage_queue":[{"task":3,"stage":"review","round":1,"state":"delivered","since":1800003000},{"task":10,"stage":"review","round":1,"state":"waiting","since":1800003760}]},
  {"id":"reviewer2","role":"Reviewer 2","agent":"codex","model":"gpt-5.5-codex","state":"active","session":{"host":"","name":"codex/orbit/review2"},
   "stage_queue":[{"task":11,"stage":"review","round":2,"state":"delivered","since":1800003500},{"task":10,"stage":"review","round":1,"state":"waiting","since":1800003760}]},
  {"id":"tester","role":"Tester","agent":"kimi","model":"kimi-k2.5","computer":"build-01","state":"active","session":{"host":"","name":"kimi/orbit/tests"},
   "stage_queue":[{"task":4,"stage":"test","round":1,"state":"delivered","since":1800003280}]},
  {"id":"tester2","role":"Tester 2","agent":"kimi","model":"kimi-k2.5","state":"active","session":{"host":"","name":"kimi/orbit/tests2"},
   "stage_queue":[{"task":17,"stage":"test","round":1,"state":"delivered","since":1800003600}]},
  {"id":"tester3","role":"Tester 3","agent":"opencode","model":"qwen3-coder","state":"active","session":{"host":"","name":"opencode/orbit/tests3"},
   "stage_queue":[{"task":18,"stage":"test","round":1,"state":"delivered","since":1800003900}]},
  {"id":"secretary","role":"Secretary","agent":"claude","model":"claude-haiku-4-5","state":"active","session":{"host":"","name":"claude/orbit/docs"}}],
 "phases":[
  {"n":1,"title":"Analysis","state":"approved","tasks":[1],"added_by":"Lead","added_at":1000},
  {"n":2,"title":"Build","state":"approved","tasks":[9,6,2,8,5,16,3,11,10,4,17,18,12,13],"added_by":"Lead","added_at":1000}],
 "tasks":[
  {"id":1,"phase":1,"title":"Requirements and acceptance","assignee":"lead","status":"accepted"},
  {"id":9,"phase":2,"title":"Card form keyboard order","assignee":"frontend","status":"pending","deps":[{"task":1}],
   "waits":{"for":"member","on":["Frontend"],"unblock":"folder held by the test of #4"}},
  {"id":6,"phase":2,"title":"E2E checkout flow","assignee":"tester","status":"pending","deps":[{"task":3},{"task":5}]},
  {"id":2,"phase":2,"title":"Saved cards API","assignee":"backend","status":"in_progress","deps":[{"task":1}],"pipeline":"code",
   "attempts":[{"n":1,"id":"a2","member":"backend","state":"rejected","sent_at":1800000400,"report":{"text":"Cards API with cursor paging","at":1800001000}},
               {"n":2,"id":"a2b","member":"backend","state":"delivered","sent_at":1800002000,"started_at":1800002010}],
   "pipeline_run":{"id":"code","name":"Code: review, test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":2,
    "stages":[{"kind":"review","role":"reviewer"},{"kind":"test","role":"tester"}],
    "runs":[{"id":"r2a","stage":0,"kind":"review","round":1,"attempt":1,"member":"reviewer","state":"returned","by":"Reviewer","at":1800001000,"delivered_at":1800001100,"ended_at":1800001900,
             "text":"Three findings","findings":["major: cursor ids leak the row id","minor: no test for an empty page","nit: rename next_cursor"]}]}},
  {"id":8,"phase":2,"title":"Rate limits on /v1/cards","assignee":"backend","status":"pending","deps":[{"task":2}],"pipeline":"code"},
  {"id":5,"phase":2,"title":"Contract tests","assignee":"tester","status":"submitted","deps":[{"task":2,"kind":"soft"}],
   "attempts":[{"n":1,"id":"a5","member":"tester","state":"rejected","report":{"text":"Contract suite","at":1800000500}},
               {"n":2,"id":"a5b","member":"tester","state":"rejected","report":{"text":"Fixed the fixtures","at":1800001500}},
               {"n":3,"id":"a5c","member":"tester","state":"reported","report":{"text":"Sandbox port fixed","at":1800002500}}],
   "pipeline_run":{"id":"code","name":"Code: review, test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":3,
    "stages":[{"kind":"test","role":"tester2"}],
    "runs":[{"id":"r5a","stage":0,"kind":"test","round":1,"attempt":1,"member":"tester2","state":"returned","by":"Tester 2","at":1800000600,"ended_at":1800000900,"findings":["e2e flake","port in use","fixture drift"]},
            {"id":"r5b","stage":0,"kind":"test","round":2,"attempt":2,"member":"tester2","state":"returned","by":"Tester 2","at":1800001600,"ended_at":1800001900,"findings":["port in use","sandbox timeout"]},
            {"id":"r5c","stage":0,"kind":"test","round":3,"attempt":3,"member":"tester2","state":"returned","by":"Tester 2","at":1800002600,"ended_at":1800002900,"findings":["sandbox timeout","teardown leaks"]}]}},
  {"id":16,"phase":2,"title":"Postal code table","assignee":"backend","status":"submitted","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a16","member":"backend","state":"reported","report":{"text":"Table loaded","at":1800002000}}],
   "pipeline_run":{"id":"check-review","name":"Review, lead","max_rounds":3,"folder":"hold","accept":"lead","round":1,
    "stages":[{"kind":"review","role":"reviewer"}],
    "runs":[{"id":"r16","stage":0,"kind":"review","round":1,"attempt":1,"state":"skipped","by":"Lead","at":1800002050,"ended_at":1800002100,"text":"Generated data, no logic"}]}},
  {"id":3,"phase":2,"title":"Card form UI","assignee":"frontend","status":"in_review","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a3","member":"frontend","state":"reported","sent_at":1800001000,"report":{"text":"Card form with masks","at":1800002900}}],
   "pipeline_run":{"id":"code","name":"Code: review, test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":1,
    "stages":[{"kind":"review","role":"reviewer"},{"kind":"test","role":"tester"}],
    "runs":[{"id":"r3","stage":0,"kind":"review","round":1,"attempt":1,"member":"reviewer","state":"delivered","at":1800002950,"delivered_at":1800003000,"folder":"/work/orbit-team/frontend"}]}},
  {"id":11,"phase":2,"title":"Ledger cursor index","assignee":"backend","status":"in_review","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a11","member":"backend","state":"rejected","report":{"text":"Index added","at":1800001500}},
               {"n":2,"id":"a11b","member":"backend","state":"reported","report":{"text":"Index on (ledger, cursor)","at":1800003400}}],
   "pipeline_run":{"id":"code","name":"Code: review, test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":2,
    "stages":[{"kind":"review","role":"reviewer"},{"kind":"test","role":"tester"}],
    "runs":[{"id":"r11a","stage":0,"kind":"review","round":1,"attempt":1,"member":"reviewer2","state":"returned","by":"Reviewer 2","at":1800001600,"ended_at":1800002000,"findings":["missing down migration"]},
            {"id":"r11b","stage":0,"kind":"review","round":2,"attempt":2,"member":"reviewer2","state":"delivered","at":1800003450,"delivered_at":1800003500,"folder":"/work/orbit-team/backend"}]}},
  {"id":10,"phase":2,"title":"Saved cards API docs","assignee":"secretary","status":"in_review","deps":[{"task":1}],"stage_candidates":["reviewer","reviewer2"],
   "attempts":[{"n":1,"id":"a10","member":"secretary","state":"reported","report":{"text":"Docs page","at":1800003700}}],
   "pipeline_run":{"id":"docs","name":"Docs: review, auto","max_rounds":2,"folder":"hold","accept":"auto","round":1,
    "stages":[{"kind":"review","role":"reviewer"}],
    "runs":[{"id":"r10","stage":0,"kind":"review","round":1,"attempt":1,"state":"waiting","at":1800003760}]}},
  {"id":4,"phase":2,"title":"Address validation","assignee":"frontend","status":"in_test","deps":[{"task":1}],
   "criteria":["Postal codes are checked per country","Street, city, postal code keep keyboard order","The error shows under the field on blur"],
   "attempts":[{"n":1,"id":"a4","member":"frontend","state":"rejected","sent_at":1800000840,"report":{"text":"Reported: postal codes per country, message under the field. 14 tests passed.","at":1800002760}},
               {"n":2,"id":"a4b","member":"frontend","state":"reported","sent_at":1800003120,"report":{"text":"Fixed all 3; normalization in normalizePostal(). 17 tests passed.","at":1800004200}}],
   "pipeline_run":{"id":"code","name":"Code: review, test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":2,
    "stages":[{"kind":"review","role":"reviewer"},{"kind":"test","role":"tester"}],
    "runs":[{"id":"r4a","stage":0,"kind":"review","round":1,"attempt":1,"member":"reviewer","state":"returned","by":"Reviewer","at":1800002800,"delivered_at":1800002850,"ended_at":1800003480,
             "text":"3 findings, sent to Frontend as attempt 2",
             "findings":["MAJOR: Canadian codes pass in lower case but are stored unnormalized. web/checkout/address/validate.ts:41",
                         "MINOR: Criterion 3: the error is announced only on submit, not on blur. AddressForm.tsx:88",
                         "NIT: Test name repeats \"should\". validate.test.ts:12"]},
            {"id":"r4b","stage":0,"kind":"review","round":2,"attempt":2,"member":"reviewer","state":"passed","by":"Reviewer","at":1800004210,"delivered_at":1800004220,"ended_at":1800004020,
             "text":"The three findings are fixed; criteria 1-3 met."},
            {"id":"r4c","stage":1,"kind":"test","round":2,"attempt":2,"member":"tester","state":"delivered","at":1800003270,"delivered_at":1800003280,
             "folder":"/work/orbit-team/frontend","holds":true,"changed":[{"commit":"e41b2a0","files":2},{"commit":"a3f9c1e","files":2}]}]}},
  {"id":17,"phase":2,"title":"Shipping copy","assignee":"secretary","status":"in_test","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a17","member":"secretary","state":"reported","report":{"text":"Copy updated","at":1800003500}}],
   "pipeline_run":{"id":"copy","name":"Copy: test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":1,
    "stages":[{"kind":"test","role":"tester"}],
    "runs":[{"id":"r17","stage":0,"kind":"test","round":1,"attempt":1,"member":"tester2","state":"delivered","at":1800003590,"delivered_at":1800003600,"folder":"/work/orbit","holds":false,
             "changed":[{"path":"copy/shipping.md","size":10},{"path":"copy/fees.md","size":10},{"path":"copy/eta.md","size":10},{"path":"copy/old.md","deleted":true}]}]}},
  {"id":18,"phase":2,"title":"Ledger export","assignee":"backend","status":"in_test","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a18","member":"backend","state":"reported","report":{"text":"CSV export","at":1800003000}}],
   "pipeline_run":{"id":"code-detached","name":"Code, detached test","max_rounds":3,"folder":"checkout","accept":"lead","round":1,
    "stages":[{"kind":"test","role":"tester"}],
    "runs":[{"id":"r18","stage":0,"kind":"test","round":1,"attempt":1,"member":"tester3","state":"delivered","at":1800003890,"delivered_at":1800003900,
             "folder":"/tmp/zerus-checkout-18","checkout":"/tmp/zerus-checkout-18","holds":false,"changed":[{"commit":"9c0ffee","files":3}]}]}},
  {"id":12,"phase":2,"title":"Provider token vault","assignee":"backend","status":"accepted","deps":[{"task":1}],"decided_by":"lead",
   "attempts":[{"n":1,"id":"a12","member":"backend","state":"reported","report":{"text":"Vault","at":1800001000}}],
   "pipeline_run":{"id":"code","name":"Code: review, test, lead","max_rounds":3,"folder":"hold","accept":"lead","round":1,
    "stages":[{"kind":"review","role":"reviewer"},{"kind":"test","role":"tester"}],
    "runs":[{"id":"r12a","stage":0,"kind":"review","round":1,"attempt":1,"member":"reviewer","state":"passed","at":1800001100,"ended_at":1800001300},
            {"id":"r12b","stage":1,"kind":"test","round":1,"attempt":1,"member":"tester","state":"passed","at":1800001300,"ended_at":1800001700}]}},
  {"id":13,"phase":2,"title":"Checkout copy review","assignee":"secretary","status":"accepted","deps":[{"task":1}],
   "attempts":[{"n":1,"id":"a13","member":"secretary","state":"reported","report":{"text":"Copy","at":1800001000}}],
   "pipeline_run":{"id":"docs","name":"Docs: review, auto","max_rounds":2,"folder":"hold","accept":"auto","round":1,
    "stages":[{"kind":"review","role":"reviewer"}],
    "runs":[{"id":"r13","stage":0,"kind":"review","round":1,"attempt":1,"member":"reviewer","state":"passed","at":1800001100,"ended_at":1800001300}]}}],
 "layers":{"1":0,"9":1,"6":2,"2":1,"8":2,"5":2,"16":1,"3":1,"11":1,"10":1,"4":1,"17":1,"18":1,"12":1,"13":1},"cycle":[],"ready":[],
 "validation":{"errors":[],"warnings":[]}})JSON").object();
}

double stageNow() { return 1800004000; }

// Every chip of the open phase's Board: task id -> the chip texts.
QMap<int, QStringList> boardChips(QWidget *root)
{
    QMap<int, QStringList> chips;
    for (auto *line : root->findChildren<QWidget *>("teamStageLine")) {
        if (!line->isVisibleTo(root)) continue;
        QStringList texts;
        for (auto *chip : line->findChildren<QWidget *>("teamStageChip")) texts << chip->property("text").toString();
        chips[line->property("task").toInt()] = texts;
    }
    return chips;
}

QFrame *boardCard(QWidget *root, int task)
{
    for (auto *card : root->findChildren<QFrame *>("teamBoardCard")) if (card->property("task").toInt() == task) return card;
    return nullptr;
}

QString columnOf(QWidget *root, int task)
{
    for (auto *col : root->findChildren<QFrame *>("teamBoardColumn"))
        for (auto *card : col->findChildren<QFrame *>("teamBoardCard")) if (card->property("task").toInt() == task) return col->property("column").toString();
    return {};
}
}

// U54: the team the account move tests use. The lead and Backend run on Work,
// the Reviewer on the default account, QA is a Codex member, the Designer runs
// on another computer and the Tester is a Kimi member there.
QJsonObject moveTeam()
{
    auto t = team();
    const auto member = [](const char *id, const char *role, const char *agent, const char *session, const char *account, const char *host = "") {
        QJsonObject m{{"id", id}, {"role", role}, {"agent", agent}, {"state", "active"}, {"account", ""}, {"session_account", account},
                      {"session", QJsonObject{{"host", host}, {"name", session}, {"run_id", QStringLiteral("run-") + id}}}};
        if (*host) m["computer"] = host;
        return m;
    };
    t["members"] = QJsonArray{member("lead", "Architect", "claude", "claude/checkout/lead", "work"),
                              member("backend", "Backend", "claude", "claude/checkout/backend", "work"),
                              member("tester", "Tester", "kimi", "kimi/checkout/tester", "native-kimi", "msi-old"),
                              member("reviewer", "Reviewer", "claude", "claude/checkout/reviewer", "native-claude"),
                              member("qa", "QA", "codex", "codex/checkout/qa", "native-codex"),
                              member("designer", "Designer", "claude", "claude/checkout/designer", "work", "gb10")};
    return t;
}

QJsonObject withMember(QJsonObject team, const QString &id, const std::function<void(QJsonObject &)> &change)
{
    auto members = team["members"].toArray();
    for (int i = 0; i < members.size(); ++i) {
        auto m = members[i].toObject();
        if (m["id"].toString() == id) { change(m); members[i] = m; }
    }
    team["members"] = members;
    return team;
}

const char *moveAccounts = R"({"profiles":[
 {"id":"native-claude","provider":"claude","label":"Default account","account_status":{"status":"ok"}},
 {"id":"work","provider":"claude","label":"Work","account_status":{"status":"ok"}},
 {"id":"second","provider":"claude","label":"Second","account_status":{"status":"ok"}},
 {"id":"old","provider":"claude","label":"Old","account_status":{"status":"signed_out"}},
 {"id":"native-codex","provider":"codex","label":"Default account","account_status":{"status":"ok"}}]})";

QFrame *memberCard(QWidget *root, const QString &id)
{
    for (auto *card : root->findChildren<QFrame *>("teamWorkspaceMember"))
        if (card->property("teamMember") == id && card->isVisibleTo(root)) return card;
    return nullptr;
}
namespace {
// #706: a small running team: Backend works on #1, the lead waits on #2.
QJsonObject liveTeam()
{
    return QJsonDocument::fromJson(R"JSON({
 "id":"team-1","name":"Live","status":"running","project":"Orbit platform","folder":"/work/orbit","lead":"lead",
 "revision":1,"created":1000,"updated":5000,"settings":{},"layers":{},"cycle":[],"ready":[],"validation":{},
 "members":[
  {"id":"lead","role":"Lead","agent":"claude","model":"claude-opus-4-6","state":"active","session":{"host":"","name":"claude/orbit/live"}},
  {"id":"backend","role":"Backend","agent":"codex","model":"gpt-6.1-sol","state":"active","session":{"host":"","name":"codex/orbit/api"}}],
 "phases":[{"n":1,"title":"Build","state":"approved","tasks":[1,2],"added_by":"Lead","added_at":1000}],
 "tasks":[
  {"id":1,"phase":1,"title":"Saved cards API","assignee":"backend","status":"in_progress",
   "attempts":[{"n":1,"id":"a1","member":"backend","state":"delivered","sent_at":1800002000,"started_at":1800002010}]},
  {"id":2,"phase":1,"title":"Release notes","assignee":"lead","status":"pending","deps":[{"task":1}]}]})JSON").object();
}
}

// The suites' common state: the fake hgs in a temporary folder and the
// status it answers, and the guard for modals a test leaves open.
class TeamsUiFixture {
protected:
    void setUpCase();
    void setUp();
    void tearDown();

    QString script() const { return m_dir.filePath("hgs"); }
    void setTeam(const QJsonObject &value) { QVERIFY(FakeCli::replace(script() + ".status", QJsonDocument(value).toJson(QJsonDocument::Compact))); }
    QTemporaryDir m_dir;
    std::unique_ptr<TestWait::ModalGuard> m_modals;
};


inline void TeamsUiFixture::setUpCase()
{
    WorkspaceSkin::preload();
    QVERIFY(m_dir.isValid());

    TeamController::setPollInterval(60000);
    darkPalette();
    QFile c(script() + ".catalog"); QVERIFY(c.open(QIODevice::WriteOnly)); c.write(QJsonDocument(catalog()).toJson(QJsonDocument::Compact)); c.close();
    QFile f(script() + ".feed"); QVERIFY(f.open(QIODevice::WriteOnly)); f.write(R"({"items":[{"seq":2,"at":1300,"kind":"reported","detail":{"by":"Backend","task":2,"attempt":1}}],"next_cursor":null})"); f.close();
    QVERIFY(FakeCli::install(script(), R"PY(#!/usr/bin/env python3
import json,sys
a=sys.argv[1:]
if a and a[0].startswith('@'):a=a[1:]
body=json.load(sys.stdin) if a[:1]==['team'] else {}
open(__file__+'.calls','a').write(json.dumps({'args':a,'input':body})+'\n')
import os
open(__file__+'.env','a').write(json.dumps({'args':a,'markers':sorted(k for k in os.environ if k in ('HGS_SESSION','HGS_RUN_ID','DSH_SESSION_ID','HGS_LAUNCH_ID','HGS_FRESH'))})+'\n')
if a[:2]==['team','list']:
 t=json.load(open(__file__+'.status'))
 for key in ('layers','cycle','ready','validation'):t.pop(key)
 print(json.dumps({'items':[t]}))
elif a[:2]==['team','status']:
 import os,time
 # A test holds the status read back while `.statushold` exists (30 s at most).
 for _ in range(300):
  if not os.path.exists(__file__+'.statushold'):break
  time.sleep(.1)
 sys.stdout.write(open(__file__+'.status').read())
elif a[:2]==['team','events']:sys.stdout.write(open(__file__+'.feed').read())
elif a[:2]==['team','profile']:
 # U38: Heat (#666) keeps the pages by metric in `.profile`, a cursor naming
 # its page; Profile (#665) keeps one answer there, or `.compare` for a compare.
 import os
 if os.path.exists(__file__+'.profilerefuse'):print(open(__file__+'.profilerefuse').read(),file=sys.stderr);sys.exit(1)
 name=__file__+('.compare' if 'compare' in body else '.profile')
 data=json.load(open(name)) if os.path.exists(name) else {'summary':{}}
 if 'compare' not in body and isinstance(data.get('time'),list) and isinstance(data.get('tokens'),list):
  pages=data['tokens' if body.get('tokens') else 'time']
  c=body.get('after_tokens' if body.get('tokens') else 'after_time')
  data=pages[c['page'] if c else 0]
 print(json.dumps(data))
elif a[:2]==['team','waiting']:
 # U50: pages `.waiting` as hgs does: newest first, ties by id, after the cursor.
 import os
 if os.path.exists(__file__+'.waitingrefuse'):print(open(__file__+'.waitingrefuse').read(),file=sys.stderr);sys.exit(1)
 items=json.load(open(__file__+'.waiting')) if os.path.exists(__file__+'.waiting') else []
 items.sort(key=lambda i:(-i['since'],i['id']))
 c=body.get('before');start=0
 if c:start=next((n for n,i in enumerate(items) if i['since']<c['since'] or (i['since']==c['since'] and i['id']>c['id'])),len(items))
 page=items[start:start+min(body.get('limit',50),200)]
 nxt={'since':page[-1]['since'],'id':page[-1]['id']} if page and start+len(page)<len(items) else None
 print(json.dumps({'items':page,'next':nxt,'total':len(items)}))
elif a[:3]==['team','catalog','get']:sys.stdout.write(open(__file__+'.catalog').read())
elif a[:2]==['team','add-to-phase']:
 # U29: the first call warns and keeps the addition pending; the second adds it.
 if body.get('decline'):print(json.dumps({'status':'declined','pending':body.get('confirm'),'found':True,'phase':body.get('phase'),'tasks':[7]}))
 elif body.get('confirm'):print(json.dumps({'status':'added','phase':body.get('phase'),'tasks':[7]}))
 else:print(json.dumps({'status':'warning','phase':body.get('phase'),'pending':'pending-1','tasks':[7],
  'warning':'Adding tasks to running phase %d changes its scope after you approved it. The load balance of the lanes shifts and members may sit idle: members do not work ahead and later phases are not approved.'%body.get('phase')}))
elif a[:3]==['team','catalog','decide'] or a[:3]==['team','catalog','apply']:
 # U15: decisions and Apply change the draft as hgs does; `.refuse` refuses one.
 import os
 if a[2]=='decide' and os.path.exists(__file__+'.refuse'):print(open(__file__+'.refuse').read(),file=sys.stderr);sys.exit(1)
 if a[2]=='apply' and os.path.exists(__file__+'.refuseapply'):print(open(__file__+'.refuseapply').read(),file=sys.stderr);sys.exit(1)
 c=json.load(open(__file__+'.catalog'));draft=c['draft']
 if a[2]=='decide':
  for p in draft['proposals']:
   if p['id']==body.get('proposal'):p['state']={'keep':'kept','reject':'rejected','undo':'proposed'}[body.get('decision')]
  out={'status':'decided','proposal':body.get('proposal')}
 else:
  kept=[p for p in draft['proposals'] if p['state']=='kept']
  for p in kept:
   roles=[r for r in c['catalog']['roles'] if r['id']!=p['target']]
   c['catalog']['roles']=roles+([p['value']] if p['action']!='remove' else [])
  c['catalog']['version']+=1;c['draft']={'base':0,'proposals':[]}
  out={'status':'applied','version':c['catalog']['version'],'changes':len(kept)}
 open(__file__+'.catalog.new','w').write(json.dumps(c))
 import time
 for _ in range(100):
  try:os.replace(__file__+'.catalog.new',__file__+'.catalog');break
  except PermissionError:time.sleep(.05)   # Windows: a read holds the file
 print(json.dumps(out))
elif a[:2]==['team','spans']:
 # U38: `.spans` in pages after `after`, `limit` at a time.
 import os
 tl=json.load(open(__file__+'.spans')) if os.path.exists(__file__+'.spans') else []
 rest=[t for t in tl if t['task']>body.get('after',0)];page=rest[:body.get('limit',50)]
 print(json.dumps({'team':body.get('team'),'tasks':page,'next_cursor':page[-1]['task'] if len(rest)>len(page) else None,'events':{'read':0,'truncated':False}}))
elif a[:2]==['team','test']:
 # #719: `.testqueue` and `.testresults` hold the database's rows; a forced
 # submit joins the queue; `.testrefuse` refuses every call.
 import os,time
 if os.path.exists(__file__+'.testrefuse'):print(open(__file__+'.testrefuse').read(),file=sys.stderr);sys.exit(1)
 queue=json.load(open(__file__+'.testqueue')) if os.path.exists(__file__+'.testqueue') else []
 act=body.get('action')
 if act=='queue':print(json.dumps({'status':'ok','items':queue}))
 elif act=='results':
  rows=json.load(open(__file__+'.testresults')) if os.path.exists(__file__+'.testresults') else []
  rows=[r for r in rows if (body.get('task') is None or r.get('task')==body['task']) and (body.get('from') is None or r['at']>=body['from'])
        and (body.get('before') is None or r['at']<body['before'])]
  rows.sort(key=lambda r:-r['at']);page=rows[:min(body.get('limit',20),100)]
  print(json.dumps({'status':'ok','items':page,'next_cursor':page[-1]['at'] if page else None}))
 elif act=='submit':
  item={'id':'forced-%d'%len(queue),'team':body.get('team'),'suite':body.get('suite'),'task':None,'targets':[],'asked_by':body.get('asked_by',''),
        'forced':bool(body.get('forced')),'at':time.time(),'seconds':30,'budget_minutes':10,'resources':'small','state':'queued','started':None}
  json.dump(queue+[item],open(__file__+'.testqueue','w'))
  print(json.dumps({'status':'queued','suite':body.get('suite'),'request':item}))
 else:print('hgs: Unknown test action',file=sys.stderr);sys.exit(1)
elif a[:2]==['team','builds']:
 # #756: `.builds` holds the build broker's answer.
 import os
 print(open(__file__+'.builds').read() if os.path.exists(__file__+'.builds') else json.dumps({'enabled':False,'folders':[],'members':{}}))
elif a[:2]==['team','decide-proposal']:
 # U57: the receipt names the stored decision; `.proposalrefuse` refuses it,
 # `.proposalstatus` names an earlier decision (a replay).
 import os
 if os.path.exists(__file__+'.proposalrefuse'):print(open(__file__+'.proposalrefuse').read(),file=sys.stderr);sys.exit(1)
 status=open(__file__+'.proposalstatus').read() if os.path.exists(__file__+'.proposalstatus') else ('accepted' if body.get('accept') else 'rejected')
 print(json.dumps({'status':status,'proposal':body.get('proposal')}))
elif a[:2]==['team','approve-phase'] and body.get('phase')==9:
 print('hgs: Phase 9 is not waiting for approval',file=sys.stderr);sys.exit(1)
elif a[:2]==['team','intake-key']:
 # U51: a new key is in the receipt once; a revoke names the id.
 if body.get('revoke'):print(json.dumps({'status':'revoked','id':body['revoke']}))
 else:print(json.dumps({'status':'created','id':'i2','label':body.get('label'),'key':'zik_0123456789abcdef'}))
elif a[:2]==['team','create']:print(json.dumps({'status':'created','team':'team-new','request_id':body.get('request_id')}))
elif a[:2]==['team','next-goal']:print(json.dumps({'status':'planned','team':'team-2','previous':body.get('team')}))
elif a[:2]==['team','draft']:
 import os
 act=body.get('action')
 if act=='start':print(json.dumps({'team':'draft-1','revision':1}))
 elif act=='update':
  rev=int(open(__file__+'.draftrev').read()) if os.path.exists(__file__+'.draftrev') else body.get('revision')
  if body.get('revision')!=rev:print('hgs: Draft revision is %d now; read it again'%rev,file=sys.stderr);sys.exit(1)
  if os.path.exists(__file__+'.draftrev'):open(__file__+'.draftrev','w').write(str(rev+1))
  print(json.dumps({'status':'updated','draft_revision':rev+1,'revision':rev+40,'unseen':[]}))
 elif act=='seen':print(json.dumps({'status':'seen','unseen':[f for f in ['members','goal'] if f not in body.get('fields',[])]}))
 elif act=='create':print(json.dumps({'status':'planning','team':body.get('team')}))
 elif act=='discard':print(json.dumps({'status':'stopped','stop_lead':body.get('stop_lead',False)}))
 else:print(json.dumps({'status':'ok'}))
elif a[:2]==['team','swap-member'] and body.get('from_account'):
 # U54: a bulk move waits while `.swaphold` exists, then answers `.swaprefuse` or `.swapreceipt`.
 import os,time
 for _ in range(300):
  if not os.path.exists(__file__+'.swaphold'):break
  time.sleep(.1)
 if os.path.exists(__file__+'.swaprefuse'):print(open(__file__+'.swaprefuse').read(),file=sys.stderr);sys.exit(1)
 sys.stdout.write(open(__file__+'.swapreceipt').read())
elif a[:2]==['account','ls'] and os.path.exists(__file__+'.accounts'):sys.stdout.write(open(__file__+'.accounts').read())
elif a[:2]==['account','ls']:print(json.dumps({'profiles':[{'id':'native-claude','provider':'claude','label':'Default','account_status':{'status':'ok'}},
 {'id':'work','provider':'claude','label':'Claude work','account_status':{'status':'signed_out'}}]}))
elif a[:2]==['account','model-settings']:
 # #738: `.models` holds the catalogs by account when a test sets them.
 import os
 models=json.load(open(__file__+'.models')) if os.path.exists(__file__+'.models') else {'native-claude':[{'id':'opus'},{'id':'sonnet'},{'id':'haiku'}],
         'native-kimi':[{'id':'kimi-code/kimi-for-coding'},{'id':'kimi-code/k3.1-pro'}]}
 print(json.dumps({'account_id':a[2],'models':models.get(a[2],[])}))
else:print(json.dumps({'status':'ok','version':4,'request_id':body.get('request_id')}))
)PY"));
}

inline void TeamsUiFixture::setUp()
{
    // #729: a change the last test queued is written before this one starts.
    Settings::flush();
    m_modals = std::make_unique<TestWait::ModalGuard>();
    QFile::remove(script() + ".calls");
    QFile::remove(script() + ".draftrev");
    QFile::remove(script() + ".statushold");
    for (const char *suffix : {".swaphold", ".swaprefuse", ".swapreceipt", ".accounts", ".models"}) QFile::remove(script() + QLatin1String(suffix));
    QFile::remove(script() + ".refuse");
    QFile::remove(script() + ".refuseapply");
    QFile::remove(script() + ".proposalrefuse");
    for (const char *suffix : {".profile", ".profilerefuse", ".spans", ".compare"}) QFile::remove(script() + QLatin1String(suffix));
    QFile::remove(script() + ".proposalstatus");
    QFile::remove(script() + ".waiting");
    QFile::remove(script() + ".waitingrefuse");
    for (const char *suffix : {".testqueue", ".testresults", ".testrefuse", ".builds"}) QFile::remove(script() + QLatin1String(suffix));
    // Decisions and Apply change the fake catalog: each test starts from the fixture.
    QVERIFY(FakeCli::replace(script() + ".catalog", QJsonDocument(catalog()).toJson(QJsonDocument::Compact)));
    setTeam(team());
}

// #525: a dialog left open fails the test instead of hanging the suite.
inline void TeamsUiFixture::tearDown()
{
    const QString closed = m_modals ? m_modals->closed() : QString();
    m_modals.reset();
    if (!closed.isEmpty()) QFAIL(qPrintable(QStringLiteral("the dialog %1 stayed open unanswered and was closed").arg(closed)));
}
