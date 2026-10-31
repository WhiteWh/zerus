#include "TeamModel.h"
#include "TeamAccountMove.h"
#include <QCoreApplication>
#include <QDateTime>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonValue>
#include <QRegularExpression>
#include <QSet>
#include <algorithm>
#include <functional>

namespace Teams {
namespace {
QString tr(const char *text) { return QCoreApplication::translate("Teams", text); }

QString str(const QJsonObject &object, const char *key) { return object.value(QLatin1String(key)).toString(); }

bool openAttempt(const QString &state) { return state == "delivering" || state == "delivered" || state == "unconfirmed"; }

const Attempt *openOf(const Task &task)
{
    for (int i = task.attempts.size() - 1; i >= 0; --i)
        if (openAttempt(task.attempts[i].state)) return &task.attempts[i];
    return nullptr;
}

// Layers by longest path from the roots; DFS back edges are ignored, so a
// cycle does not reshuffle the rest (src/state/teams/plan.rs).
QMap<int, int> computeLayers(const Team &team)
{
    QSet<int> ids;
    for (const auto &task : team.tasks) ids.insert(task.id);
    QMap<int, int> depth;
    QSet<int> active;
    std::function<int(int)> visit = [&](int id) -> int {
        if (depth.contains(id)) return depth[id];
        if (active.contains(id)) return -1;
        active.insert(id);
        int level = 0;
        if (const auto *task = team.task(id))
            for (const auto &dep : task->deps)
                if (ids.contains(dep.task)) { const int d = visit(dep.task); if (d >= 0) level = qMax(level, d + 1); }
        active.remove(id);
        depth[id] = level;
        return level;
    };
    for (const auto &task : team.tasks) visit(task.id);
    return depth;
}
}

const Member *Team::member(const QString &id) const
{
    for (const auto &m : members) if (m.id == id) return &m;
    return nullptr;
}

const Member *Team::memberOfSession(const QString &host, const QString &session) const
{
    for (const auto &m : members) if (!m.session.isEmpty() && m.session == session && m.host == host) return &m;
    return nullptr;
}

QString Team::reviewerOf(const QString &id) const
{
    if (id == lead) return {};
    const Member *self = member(id);
    const Member *parent = self && !self->parent.isEmpty() ? member(self->parent) : nullptr;
    return parent && parent->sub && parent->id != id && parent->id != lead ? parent->id : lead;
}

QList<const Member *> Team::reportsOf(const QString &id) const
{
    QList<const Member *> out;
    for (const auto &m : members) if (m.id != lead && reviewerOf(m.id) == id) out.append(&m);
    return out;
}

QList<Team::TreeNode> Team::memberTree(const std::function<bool(const QString &)> &shown) const
{
    const auto visible = [&](const QString &id) { return !shown || shown(id); };
    // The shown parent of each shown member: its reviewer, or the nearest
    // shown one above it (bounded: a broken chain ends at the lead).
    const auto parentOf = [&](const QString &id) {
        QString at = reviewerOf(id);
        for (int guard = 0; at != lead && !at.isEmpty() && !visible(at) && guard < members.size(); ++guard) at = reviewerOf(at);
        return at.isEmpty() || !visible(at) ? lead : at;
    };
    const auto childrenOf = [&](const QString &id) {
        QList<const Member *> out;
        for (const auto &m : members) if (m.id != lead && visible(m.id) && parentOf(m.id) == id) out.append(&m);
        return out;
    };
    QList<TreeNode> out;
    QSet<QString> placed{lead};
    // Depth-first from the lead; a member whose chain never reaches the lead
    // (a broken record with a cycle) is listed under the lead at the end.
    std::function<void(const QString &, int, QList<bool>)> walk = [&](const QString &id, int depth, QList<bool> through) {
        const auto children = childrenOf(id);
        for (int i = 0; i < children.size(); ++i) {
            if (placed.contains(children[i]->id)) continue;
            placed.insert(children[i]->id);
            const bool last = i == children.size() - 1;
            out.append({children[i], depth + 1, last, through});
            walk(children[i]->id, depth + 1, through + QList<bool>{!last});
        }
    };
    if (const Member *l = member(lead)) out.append({l, 0, true, {}});
    walk(lead, 0, {});
    for (const auto &m : members)
        if (!placed.contains(m.id) && visible(m.id)) out.append({&m, 1, true, {}});
    return out;
}

const Task *Team::task(int id) const
{
    for (const auto &t : tasks) if (t.id == id) return &t;
    return nullptr;
}

const Phase *Team::phase(int n) const
{
    for (const auto &p : phases) if (p.n == n) return &p;
    return nullptr;
}

const Team::PhaseSummary *Team::phaseSummaryOf(int n) const
{
    for (const auto &summary : phaseSummary) if (summary.n == n) return &summary;
    return nullptr;
}

const Team::Health *Team::phaseHealthOf(int n) const
{
    const auto it = phasesHealth.constFind(n);
    return it == phasesHealth.constEnd() ? nullptr : &it.value();
}

const LeftRunning *Team::leftOf(const QString &member) const
{
    for (const auto &left : leftRunning) if (left.member == member) return &left;
    return nullptr;
}

QList<int> Team::draftPhases() const
{
    QList<int> drafts;
    // A finished team opens nothing new, even if a record was left in draft.
    if (status == "done" || status == "stopped" || status == "archived") return drafts;
    for (const auto &phase : phases) if (phase.draft()) drafts.append(phase.n);
    std::sort(drafts.begin(), drafts.end());
    return drafts;
}

QList<int> Team::tasksOfPhase(int phase) const
{
    QList<int> ids;
    for (const auto &task : tasks) if (task.phase == phase) ids.append(task.id);
    return ids;
}

QString Team::roleOf(const QString &id) const
{    if (id == lead) return tr("Lead");
    if (const auto *m = member(id)) return m->role.isEmpty() ? m->id : m->role;
    return id;
}

QString Team::assigneeRole(const Task &task, const QString &none) const
{
    if (!task.assignee.isEmpty()) return roleOf(task.assignee);
    if (!task.role.isEmpty()) return QCoreApplication::translate("Teams", "Any free %1").arg(task.role);
    return none;
}

QList<int> memberQueue(const Team &team, const QString &member)
{
    // The same list hgs sends for an older hgs: every task with an open attempt
    // of this member (ascending, as hgs sorts them), then the tasks returned to
    // it (pending, with attempts), also ascending (L7).
    QList<int> open, returned;
    for (const auto &task : team.tasks) {
        if (const auto *attempt = openOf(task); attempt && attempt->member == member) open.append(task.id);
        // Returned to it: handed out before, not finished, and waiting.
        if (task.assignee == member && !task.finished() && task.status == "pending" && !task.attempts.isEmpty())
            returned.append(task.id);
    }
    std::sort(open.begin(), open.end());
    std::sort(returned.begin(), returned.end());
    return open + returned;
}

const StageRun *PipelineRun::openRun() const
{
    return !runs.isEmpty() && runs.last().open() ? &runs.last() : nullptr;
}

int PipelineRun::returned() const
{
    return trimmedReturns + int(std::count_if(runs.cbegin(), runs.cend(), [](const StageRun &run) { return run.state == QLatin1String("returned"); }));
}

bool stageWithoutCandidate(const Task &task)
{
    const auto *run = task.inStage() ? task.pipelineRun.openRun() : nullptr;
    if (!run || run->state != QLatin1String("waiting") || !task.stageCandidatesKnown || !task.stageCandidates.isEmpty()) return false;
    return !StageSpec{run->kind}.runsCommand();
}

QString implementerOf(const Task &task, const StageRun &run)
{
    for (const auto &attempt : task.attempts) if (attempt.n == run.attempt && !attempt.member.isEmpty()) return attempt.member;
    return task.assignee;
}

int scaleRoom(const Team &team)
{
    if (team.maxMembers <= 0) return 10;
    const auto seats = std::count_if(team.members.cbegin(), team.members.cend(), [](const Member &m) { return m.state != QLatin1String("removed"); });
    return team.maxMembers - int(seats);
}

Member::Hold folderHold(const Team &team, const QString &member)
{
    // #635 N4: hgs's status names each member's hold (stages::held_by): a
    // review, test, command or land stage, or a closed stage whose run still
    // goes. A status that names one names them all.
    if (const auto *m = team.member(member); m && m->heldBy.task) return m->heldBy;
    if (std::any_of(team.members.cbegin(), team.members.cend(), [](const Member &m) { return m.heldBy.task > 0; })) return {};
    // An older hgs: the member's waiting task says it ("folder held by the
    // stage of #N" since #680, "the test of #N" before).
    static const QRegularExpression said(QStringLiteral("folder held by the (test|stage) of #(\\d+)"));
    for (const auto &task : team.tasks) {
        if (task.assignee != member || task.status != "pending") continue;
        if (const auto match = said.match(task.wait.unblock); match.hasMatch()) {
            const int holder = match.captured(2).toInt();
            const auto *held = team.task(holder);
            const auto *run = held ? held->pipelineRun.openRun() : nullptr;
            return {holder, match.captured(1) == "test" ? QStringLiteral("test") : run ? run->kind : QString()};
        }
    }
    // Older still: as hgs's stages::held was then, a test waiting for its
    // member holds the folder under the hold rule; a delivered one when its
    // hand-off said so (#532), or under the hold rule when hgs sent no folder.
    for (const auto &task : team.tasks) {
        const auto *run = task.pipelineRun.openRun();
        if (!run || run->kind != "test") continue;
        const bool hold = task.pipelineRun.folder != "checkout";
        const bool holds = run->state == "waiting" ? hold : (run->holds || (hold && run->folder.isEmpty()));
        if (holds && implementerOf(task, *run) == member) return {task.id, QStringLiteral("test")};
    }
    return {};
}

int folderHeldBy(const Team &team, const QString &member)
{
    return folderHold(team, member).task;
}

QString holderText(const Member::Hold &hold)
{
    return hold.stage.isEmpty() ? tr("the stage of #%1").arg(hold.task) : tr("the %1 stage of #%2").arg(hold.stage).arg(hold.task);
}

QList<Member::StageEntry> stageQueue(const Team &team, const QString &member)
{
    if (const auto *m = team.member(member); m && m->stageQueueKnown) return m->stageQueue;
    // An hgs without #532's queue: the stage it has, then the waiting stages
    // it may take, oldest first.
    QList<Member::StageEntry> current, waiting;
    QList<std::pair<double, Member::StageEntry>> order;
    for (const auto &task : team.tasks) {
        const auto *run = task.pipelineRun.openRun();
        if (!run || !task.inStage()) continue;
        const Member::StageEntry entry{task.id, run->round, run->kind, run->state, run->deliveredAt > 0 ? run->deliveredAt : run->at};
        if (run->state == "delivered" && run->member == member) current.append(entry);
        else if (run->state == "waiting" && task.stageCandidates.contains(member)) order.append({run->at, entry});
    }
    std::stable_sort(order.begin(), order.end(), [](const auto &a, const auto &b) { return a.first < b.first || (a.first == b.first && a.second.task < b.second.task); });
    for (const auto &[at, entry] : order) waiting.append(entry);
    return current + waiting;
}

int stageQueuePlace(const Team &team, int task)
{
    const auto *mine = team.task(task);
    const auto *own = mine ? mine->pipelineRun.openRun() : nullptr;
    if (!own || own->state != "waiting") return 0;
    int place = 1;
    for (const auto &other : team.tasks) {
        const auto *run = other.inStage() ? other.pipelineRun.openRun() : nullptr;
        if (!run || other.id == task || run->state != "waiting" || run->kind != own->kind) continue;
        if (run->at < own->at || (run->at == own->at && other.id < task)) ++place;
    }
    return place;
}

QString stageName(const QString &kind)
{
    if (kind == "review") return tr("Review");
    if (kind == "test") return tr("Test");
    if (kind == "command") return tr("Command");
    if (kind == "land") return tr("Land");
    if (kind == "implement") return tr("Implement");
    if (kind == "auto") return tr("Auto");
    if (kind == "sub_lead") return tr("Sub-lead");
    if (kind == "lead") return tr("Lead");
    return kind.isEmpty() ? QString() : kind.left(1).toUpper() + kind.mid(1);
}

QString attemptPhase(const Task &task, const Attempt &attempt)
{
    // hgs derives the phase and sends null on purpose when the task or the
    // attempt has none: a finished task, a revoked attempt that another attempt
    // replaced, or a refused attempt nothing reached. Only an older hgs sends no
    // key at all; then the same answer is derived here, mirroring
    // ops::attempt_phase (which also knows the live session's state, so an
    // older hgs never yields "stopped").
    if (attempt.phaseKnown) return attempt.phase;
    if (task.finished()) return {};
    if (task.held) return QStringLiteral("held");
    if (attempt.state == "unconfirmed") return QStringLiteral("unconfirmed");
    if (attempt.state == "delivering") return QStringLiteral("sent");
    if (attempt.state == "rejected") return QStringLiteral("rejected");
    if (attempt.state == "refused" || attempt.state == "revoked") return {};
    if (attempt.state != "delivered" && attempt.state != "reported") return {};
    // A question is not a report, and the attempt stays open (A3).
    if (attempt.openQuestion()) return QStringLiteral("asking");
    if (attempt.state == "reported") return QStringLiteral("reported");
    if (attempt.noReport) return QStringLiteral("no_report");
    if (attempt.startedAt > 0) return QStringLiteral("working");
    if (attempt.startWarned) return QStringLiteral("late");
    return QStringLiteral("delivered");
}

QString lastPhase(const Task &task)
{
    const auto *last = task.last();
    return last ? attemptPhase(task, *last) : QString();
}

QList<Question> openQuestions(const Task &task)
{
    QList<Question> open;
    if (const auto *attempt = task.last())
        for (const auto &question : attempt->questions) if (!question.answered()) open.append(question);
    return open;
}

const Question *question(const Task &task, const QString &id)
{
    if (const auto *attempt = task.last())
        for (const auto &item : attempt->questions) if (item.id == id) return &item;
    return nullptr;
}

// C7: the plan-health numbers, with the starving members while running.
// Shared by the plan health and the G9 per-phase health.
Team::Health parseHealth(const QJsonObject &h)
{
    Team::Health health;
    if (h.isEmpty()) return health;
    health.known = true;
    health.tasks = h.value("tasks").toInt();
    health.criticalPath = h.value("critical_path").toInt();
    health.widestReady = h.value("widest_ready").toInt();
    health.parallelism = h.value("parallelism").toDouble();
    const auto members = h.value("per_member").toObject();
    for (auto it = members.begin(); it != members.end(); ++it) {
        const auto load = it.value().toObject();
        health.perMember.append({it.key(), {load.value("tasks").toInt(), load.value("on_critical_path").toInt()}});
    }
    for (const auto &value : h.value("fan_in").toArray()) {
        const auto entry = value.toObject();
        Team::Health::FanIn fanIn;
        fanIn.task = entry.value("task").toInt(); fanIn.kind = str(entry, "kind");
        for (const auto &id : entry.value("waits_for").toArray()) fanIn.waitsFor.append(id.toInt());
        health.fanIn.append(fanIn);
    }
    for (const auto &value : h.value("warnings").toArray()) health.warnings.append(value.toString());
    for (const auto &value : h.value("starved").toArray()) {
        const auto entry = value.toObject();
        Team::Health::Starved starved;
        starved.member = str(entry, "member"); starved.role = str(entry, "role");
        starved.minutes = entry.value("minutes").toInt();
        for (const auto &id : entry.value("blocking").toArray()) starved.blocking.append(id.toInt());
        health.starved.append(starved);
    }
    return health;
}

QString BuildState::text(double now) const
{
    if (building()) {
        const qint64 secs = qint64(std::max(0.0, now - since));
        const QString clock = QStringLiteral("%1:%2").arg(secs / 60).arg(secs % 60, 2, 10, QLatin1Char('0'));
        return snapshot ? tr("building (snapshot) %1").arg(clock) : tr("building (shared) %1").arg(clock);
    }
    if (waiting()) return ahead == 1 ? tr("waiting for a build, 1 ahead") : tr("waiting for a build, %1 ahead").arg(ahead);
    return {};
}

Team parse(const QJsonObject &v)
{
    Team team;
    team.id = str(v, "id"); team.name = str(v, "name"); team.status = str(v, "status");
    team.project = str(v, "project"); team.folder = str(v, "folder"); team.lead = str(v, "lead");
    // G1: the parent task this team came from and its sub-teams.
    if (v.value("parent").isObject()) {
        const auto parent = v.value("parent").toObject();
        team.parent.known = true;
        team.parent.team = str(parent, "team");
        team.parent.task = parent.value("task").toInt();
        team.parent.lead = str(parent, "lead");
        team.parent.member = str(parent, "member");
    }
    for (const auto &child : v.value("children").toArray()) {
        const auto c = child.toObject();
        Subteam subteam;
        subteam.known = true;
        subteam.team = str(c, "team");
        subteam.name = str(c, "name");
        subteam.status = str(c, "status");
        subteam.done = c.value("done").toInt();
        subteam.total = c.value("total").toInt();
        team.children.append(subteam);
    }
    for (const auto &folder : v.value("settings").toObject().value("evidence_folders").toArray())
        team.evidenceFolders.append(folder.toString());
    team.revision = v.value("revision").toInteger(); team.phaseNumbersRevision = v.value("phase_numbers_revision").toInteger(); team.created = v.value("created").toDouble(); team.updated = v.value("updated").toDouble();
    const auto settings = v.value("settings").toObject();
    team.allHard = settings.value("all_hard").toBool();
    team.reportTimeout = settings.value("report_timeout_seconds").toInt(120);
    team.approval = str(settings, "approval");
    team.phaseApproval = str(settings, "phase_approval");
    for (const auto &value : v.value("members").toArray()) {
        const auto m = value.toObject();
        if (str(m, "state") == "removed") continue;
        Member member;
        member.id = str(m, "id"); member.role = str(m, "role"); member.agent = str(m, "agent"); member.model = str(m, "model");
        member.computer = str(m, "computer"); member.state = str(m, "state"); member.error = str(m, "error");
        member.account = str(m, "account"); member.effort = str(m, "effort"); member.permissions = str(m, "permissions");
        member.sessionAccountKnown = m.contains("session_account"); member.sessionAccount = str(m, "session_account");
        member.folder = str(m, "folder"); member.directory = str(m, "directory"); member.prompt = str(m, "prompt");
        const auto session = m.value("session").toObject();
        member.host = str(session, "host"); member.session = str(session, "name");
        member.runId = str(session, "run_id"); member.conversationId = str(session, "conversation_id");
        if (m.value("limited").isObject()) {
            const auto limit = m.value("limited").toObject();
            member.limited.known = true;
            member.limited.at = limit.value("at").toDouble();
            member.limited.resetsAt = limit.value("resets_at").toDouble();
            member.limited.detail = str(limit, "detail");
        }
        member.limitResume = m.value("limit_resume").toBool();
        member.hostShared = m.value("host_shared").toBool();
        if (m.value("queue").isArray()) {
            member.queueKnown = true;
            for (const auto &item : m.value("queue").toArray()) {
                const auto object = item.toObject();
                const int id = item.isDouble() ? item.toInt() : object.value("task").toInt(object.value("id").toInt());
                if (id > 0) member.queue.append(id);
            }
        }
        // U36 (#532): the stage it has, then the waiting stages it may take.
        if (m.value("stage_queue").isArray()) {
            member.stageQueueKnown = true;
            for (const auto &item : m.value("stage_queue").toArray()) {
                const auto e = item.toObject();
                member.stageQueue.append({e.value("task").toInt(), e.value("round").toInt(), str(e, "stage"), str(e, "state"), e.value("since").toDouble()});
            }
        }
        // #635 N4: the stage whose hold keeps its folder.
        if (const auto held = m.value("held_by").toObject(); held.value("task").toInt() > 0)
            member.heldBy = {held.value("task").toInt(), str(held, "stage")};
        // G7: the services the member's session runs (MCP servers and the like).
        for (const auto &value : m.value("services").toArray()) {
            const auto service = value.toObject();
            Member::Service entry;
            entry.name = str(service, "name"); entry.command = str(service, "command");
            entry.since = service.value("since").toDouble();
            if (!entry.name.isEmpty()) member.services.append(entry);
        }
        // C4: the member's pending permission request and its answers, as hgs
        // sends them (members[].question, members[].permission_answers).
        if (m.value("question").isObject()) {
            const auto q = m.value("question").toObject();
            member.permission.id = str(q, "question_id");
            member.permission.tool = str(q, "tool");
            member.permission.command = str(q, "command");
            member.permission.commandSource = str(q, "command_source");
            member.permission.reason = str(q, "reason");
            member.permission.at = q.value("asked_at").toDouble();
            member.permission.hash = q.value("question_hash").toString(q.value("expected_question_hash").toString());
            member.permission.subagents = q.value("subagent_confirmation").toBool();
            member.permission.record = q;
            // C4: the card's own ids for Allow and Deny.
            if (q.value("answer").isObject()) {
                const auto a = q.value("answer").toObject();
                member.permission.native.questionId = str(a, "question_id");
                member.permission.native.allow = str(a, "allow");
                member.permission.native.deny = str(a, "deny");
                member.permission.native.known = !member.permission.native.questionId.isEmpty()
                    && !member.permission.native.allow.isEmpty() && !member.permission.native.deny.isEmpty();
            }
        }
        for (const auto &value : m.value("permission_answers").toArray()) {
            const auto a = value.toObject();
            PermissionAnswer answer;
            answer.questionId = str(a, "question_id"); answer.decision = str(a, "decision");
            answer.by = str(a, "by"); answer.reason = str(a, "reason"); answer.at = a.value("at").toDouble();
            answer.tool = str(a, "tool"); answer.command = str(a, "command");
            if (!answer.decision.isEmpty()) member.permissionAnswers.append(answer);
        }
        if (m.value("budget").isObject()) {
            const auto budget = m.value("budget").toObject();
            member.budget.known = true;
            member.budget.label = str(budget, "label");
            member.budget.state = str(budget, "state");
            member.budget.command = str(budget, "command");
            member.budget.minutes = budget.value("minutes").toInt();
            member.budget.redMinutes = budget.value("red_minutes").toInt();
            member.budget.elapsedMinutes = budget.value("elapsed_minutes").toInt();
            member.budget.since = budget.value("since").toDouble();
        }
        member.parent = str(m, "parent");
        member.sub = m.value("sub").toBool();
        member.models = parseModelChoices(m.value("models"));
        member.contextBeforeTask = str(m, "context_before_task");
        member.draining = m.value("draining").toBool();
        member.lead = member.id == team.lead;
        if (member.lead && member.role.isEmpty()) member.role = tr("Lead");
        team.members.append(member);
        // B5: a member whose plan widens its role's permissions profile.
        if (m.value("profile_widening").isObject()) {
            const auto w = m.value("profile_widening").toObject();
            Team::Widening widening;
            widening.member = str(w, "member"); if (widening.member.isEmpty()) widening.member = member.id;
            widening.role = member.role; widening.from = str(w, "from"); widening.to = str(w, "to");
            if (!widening.from.isEmpty() || !widening.to.isEmpty()) team.widenings.append(widening);
        }
    }
    if (v.value("profile_widening").isObject()) {
        const auto w = v.value("profile_widening").toObject();
        Team::Widening widening;
        widening.member = str(w, "member"); widening.role = str(w, "role");
        widening.from = str(w, "from"); widening.to = str(w, "to");
        if (!widening.from.isEmpty() || !widening.to.isEmpty()) team.widenings.append(widening);
    }
    for (const auto &value : v.value("profile_widening").toArray()) {
        const auto w = value.toObject();
        Team::Widening widening;
        widening.member = str(w, "member"); widening.role = str(w, "role");
        widening.from = str(w, "from"); widening.to = str(w, "to");
        if (!widening.from.isEmpty() || !widening.to.isEmpty()) team.widenings.append(widening);
    }
    for (const auto &value : v.value("tasks").toArray()) {
        const auto t = value.toObject();
        Task task;
        task.id = t.value("id").toInt(); task.phase = qMax(1, t.value("phase").toInt(1)); task.pinned = t.value("pinned").toBool();
        task.title = str(t, "title"); task.description = str(t, "description");
        task.status = str(t, "status"); task.assignee = str(t, "assignee");
        for (const auto &model : t.value("models").toArray()) task.models.append(model.toString());
        task.role = str(t, "role");
        task.decidedBy = str(t, "decided_by"); task.reason = str(t, "decision_reason");
        // U27: the visual lane the task is drawn in.
        if (t.value("lane").isObject()) {
            const auto lane = t.value("lane").toObject();
            task.lane.n = lane.value("n").toInt();
            task.lane.label = str(lane, "label");
        }
        for (const auto &c : t.value("criteria").toArray()) task.criteria.append(c.toString());
        for (const auto &d : t.value("deps").toArray()) task.deps.append({d.toObject().value("task").toInt(), str(d.toObject(), "kind") == "soft"});
        task.phaseBuild = t.value("phase_build").toBool();
        if (t.value("hold").isObject()) {
            // A5: parked by the lead or by you until `release`.
            const auto h = t.value("hold").toObject();
            task.held = true;
            task.hold.by = str(h, "by"); task.hold.reason = str(h, "reason"); task.hold.at = h.value("at").toDouble();
        }
        if (t.value("added_late").isObject()) {
            const auto a = t.value("added_late").toObject();
            task.addedLate = {str(a, "by"), str(a, "request"), str(a, "confirm"), a.value("at").toDouble(), true};
        }
        // Owner 2026-10-31 (#746): the estimate, from #745's status fields.
        if (t.contains("estimate_source")) {
            task.estimateSource = str(t, "estimate_source");
            task.estimateText = str(t, "estimate_text");
            task.estimateMinutes = t.value("estimate_minutes").isDouble() ? t.value("estimate_minutes").toDouble() : -1;
            task.estimateSpent = t.value("estimate_spent_minutes").isDouble() ? t.value("estimate_spent_minutes").toDouble() : -1;
        }
        if (t.value("timing").isObject()) {
            // U37: the working time and the waits, computed by hgs.
            const auto o = t.value("timing").toObject();
            const auto w = o.value("waits").toObject();
            auto &timing = task.timing;
            timing.known = true;
            timing.worked = qint64(o.value("worked_s").toDouble());
            timing.runningSince = o.value("running_since").toDouble();
            timing.deps = qint64(w.value("deps_s").toDouble());
            timing.member = qint64(w.value("member_s").toDouble());
            timing.lead = qint64(w.value("lead_s").toDouble());
            for (const auto &u : o.value("unknown").toArray()) timing.unknown.append(u.toString());
            for (const auto &v : o.value("attempts").toArray()) {
                const auto s = v.toObject();
                timing.attempts.append({s.value("n").toInt(), str(s, "member"), str(s, "state"), s.value("from").toDouble(), s.value("to").toDouble(),
                                        qint64(s.value("worked_s").toDouble()), qint64(s.value("member_s").toDouble()), qint64(s.value("lead_s").toDouble())});
            }
        }
        // U36: the pipeline chosen in the plan and its snapshot.
        task.pipeline = str(t, "pipeline");
        task.stageCandidatesKnown = t.value("stage_candidates").isArray();
        for (const auto &value : t.value("stage_candidates").toArray()) task.stageCandidates.append(value.toString());
        if (t.value("pipeline_run").isObject()) {
            const auto o = t.value("pipeline_run").toObject();
            auto &run = task.pipelineRun;
            run.known = true;
            run.id = str(o, "id"); run.name = str(o, "name"); run.folder = str(o, "folder"); run.accept = str(o, "accept");
            run.maxRounds = o.value("max_rounds").toInt(); run.round = o.value("round").toInt();
            run.trimmedReturns = qMax(0, o.value("trimmed_returns").toInt());
            for (const auto &v : o.value("stages").toArray()) {
                const auto st = v.toObject();
                StageSpec spec{str(st, "kind"), str(st, "role"), str(st, "member")};
                if (const auto command = st.value("command").toObject(); !command.isEmpty()) {
                    spec.command = str(command, "command"); spec.expectExit = command.value("expect_exit").toInt();
                    spec.timeoutMinutes = command.value("timeout_minutes").toInt(30);
                }
                const auto exits = st.value("exits").toObject();
                for (auto it = exits.begin(); it != exits.end(); ++it) spec.exits.insert(it.key(), it.value().toString());
                run.stages.append(spec);
            }
            for (const auto &v : o.value("runs").toArray()) {
                const auto r = v.toObject();
                StageRun stage;
                stage.id = str(r, "id"); stage.kind = str(r, "kind"); stage.member = str(r, "member"); stage.state = str(r, "state");
                stage.by = str(r, "by"); stage.text = str(r, "text"); stage.folder = str(r, "folder"); stage.checkout = str(r, "checkout");
                stage.stage = r.value("stage").toInt(); stage.round = r.value("round").toInt(); stage.attempt = r.value("attempt").toInt();
                stage.at = r.value("at").toDouble(); stage.deliveredAt = r.value("delivered_at").toDouble(); stage.endedAt = r.value("ended_at").toDouble();
                for (const auto &f : r.value("findings").toArray()) stage.findings.append(f.toString());
                stage.holds = r.value("holds").toBool();
                stage.changed = r.value("changed").toArray();
                stage.commit = str(r, "commit"); stage.outcome = str(r, "outcome"); stage.flaky = r.value("flaky").toBool();
                if (const auto check = r.value("check").toObject(); !check.isEmpty())
                    stage.check = {check.value("exit").toInt(), str(check, "tail"), true};
                for (const auto &value : r.value("runs").toArray()) {
                    const auto o = value.toObject();
                    const auto args = o.value("args").toObject();
                    StageRun::Ran ran;
                    ran.templateId = str(o, "template");
                    for (const char *key : {"suite", "filter", "bin", "package"}) if (const auto v = str(args, key); !v.isEmpty()) { ran.suite = v; break; }
                    ran.exited = o.value("exit").isDouble(); ran.exit = o.value("exit").toInt();
                    ran.timedOut = o.value("timed_out").toBool(); ran.ms = qint64(o.value("ms").toDouble());
                    stage.ran.append(ran);
                }
                run.runs.append(stage);
            }
        }
        // G1: the sub-team this task delegated to, with its rolled-up counts.
        if (t.value("subteam").isObject()) {
            const auto s = t.value("subteam").toObject();
            task.subteam.known = true;
            task.subteam.team = str(s, "team");
            task.subteam.status = str(s, "status");
            task.subteam.done = s.value("done").toInt();
            task.subteam.total = s.value("total").toInt();
        }
        // C3: why an open task waits, and the command that unblocks it.
        if (t.value("waits").isObject()) {
            const auto w = t.value("waits").toObject();
            task.wait.known = true;
            task.wait.for_ = str(w, "for"); task.wait.unblock = str(w, "unblock");
            for (const auto &value : w.value("on").toArray()) task.wait.on.append(value.isDouble() ? QString::number(value.toInt()) : value.toString());
        }
        // C5: a check task and the exit its run reported.
        task.kind = str(t, "kind");
        if (t.value("check").isObject()) {
            const auto c = t.value("check").toObject();
            task.check.command = str(c, "command");
            task.check.expectExit = c.value("expect_exit").toInt();
            task.check.timeoutMinutes = c.value("timeout_minutes").toInt();
        }
        if (t.value("check_result").isObject()) {
            const auto r = t.value("check_result").toObject();
            task.checkResult.known = true;
            task.checkResult.exit = r.value("exit").toInt();
            task.checkResult.tail = str(r, "tail");
        }
        const auto readAttempt = [&](const QJsonObject &a) {
            Attempt attempt;
            attempt.n = a.value("n").toInt(); attempt.id = str(a, "id"); attempt.member = str(a, "member");
            attempt.state = str(a, "state"); attempt.error = str(a, "error"); attempt.notice = str(a, "notice");
            attempt.noReport = a.value("no_report").toBool(); attempt.sentAt = a.value("sent_at").toDouble();
            // N6: the check this attempt ran, from attempts[].check.
            if (a.value("check").isObject()) {
                const auto c = a.value("check").toObject();
                attempt.checkResult.known = true;
                attempt.checkResult.exit = c.value("exit").toInt();
                attempt.checkResult.tail = str(c, "tail");
            }
            // A1/A2: when it was delivered and started, and the turn that ended.
            attempt.deliveredAt = a.value("delivered_at").toDouble(); attempt.startedAt = a.value("started_at").toDouble();
            attempt.startWarned = a.value("start_warned").toBool();
            attempt.workedAhead = a.value("worked_ahead").toBool();
            attempt.replyAt = a.value("reply_at").toDouble(); attempt.turnEndedAt = a.value("turn_ended_at").toDouble();
            attempt.turnReply = str(a, "turn_reply");
            // The key's presence is the contract: hgs sends null when the task
            // or the attempt has no phase, and an older hgs sends no key.
            attempt.phaseKnown = a.contains("phase");
            attempt.phase = str(a, "phase");
            const auto report = a.value("report").toObject();
            attempt.report = report.value("text").toString();
            attempt.reportAt = report.value("at").toDouble(); attempt.captured = report.value("captured").toBool();
            for (const auto &r : report.value("replaced").toArray()) {
                const auto item = r.toObject();
                attempt.replaced.append({item.value("text").toString(), item.value("at").toDouble()});
            }
            // D1: the structured report and its unstructured flag.
            attempt.unstructured = report.value("unstructured").toBool();
            const auto structured = report.value("structured").toObject();
            if (!structured.isEmpty()) {
                attempt.structured.present = true;
                attempt.structured.summary = str(structured, "summary");
                attempt.structured.commit = str(structured, "commit");
                for (const auto &c : structured.value("criteria").toArray()) {
                    const auto item = c.toObject();
                    ReportCriterion criterion;
                    criterion.n = item.value("n").toInt();
                    criterion.result = str(item, "result");
                    criterion.note = str(item, "note");
                    for (const auto &e : item.value("evidence").toArray()) criterion.evidence.append(e.toInt());
                    attempt.structured.criteria.append(criterion);
                }
                for (const auto &e : structured.value("evidence").toArray()) {
                    const auto item = e.toObject();
                    ReportEvidence evidence;
                    evidence.kind = str(item, "kind");
                    evidence.caption = str(item, "caption");
                    evidence.path = str(item, "path");
                    evidence.url = str(item, "url");
                    evidence.command = str(item, "command");
                    evidence.tail = str(item, "tail");
                    evidence.text = str(item, "text");
                    evidence.hasExit = item.contains("exit");
                    evidence.exit = item.value("exit").toInt();
                    attempt.structured.evidence.append(evidence);
                }
                for (const auto &l : structured.value("limits").toArray()) attempt.structured.limits.append(l.toString());
            }
            // A3: questions to the lead, answered or open.
            for (const auto &q : a.value("questions").toArray()) {
                const auto item = q.toObject();
                Question entry;
                entry.id = str(item, "id"); entry.text = str(item, "text"); entry.answer = str(item, "answer"); entry.by = str(item, "by");
                entry.at = item.value("at").toDouble(); entry.answeredAt = item.value("answered_at").toDouble();
                // B1: a permission request carries the exact command the agent
                // asked about, where it came from, and the agent's reason; the
                // answer records the decision, who made it and the deny text.
                entry.tool = str(item, "tool");
                entry.command = str(item, "command");
                entry.commandSource = str(item, "command_source");
                entry.reason = str(item, "reason");
                entry.decision = str(item, "decision");
                entry.hash = item.value("question_hash").toString(item.value("expected_question_hash").toString());
                attempt.questions.append(entry);
            }
            // B1: the permission answers kept on the attempt.
            for (const auto &value : a.value("permissions").toArray()) {
                const auto item = value.toObject();
                PermissionAnswer answer;
                answer.questionId = str(item, "question_id");
                answer.decision = str(item, "decision");
                answer.by = str(item, "by");
                answer.reason = str(item, "reason");
                // U17: the tool and command the agent asked about.
                answer.tool = str(item, "tool");
                answer.command = str(item, "command");
                answer.at = item.value("at").toDouble();
                attempt.permissions.append(answer);
            }
            if (attempt.member.isEmpty()) attempt.member = task.assignee;
            return attempt;
        };
        if (t.value("attempts").isArray()) for (const auto &a : t.value("attempts").toArray()) task.attempts.append(readAttempt(a.toObject()));
        else if (t.value("attempt").isObject()) task.attempts.append(readAttempt(t.value("attempt").toObject()));
        team.tasks.append(task);
    }
    // A5: the queue is in `team status`; an older hgs (or a list summary) gets
    // the same list from the attempts.
    for (auto &member : team.members) if (!member.queueKnown) member.queue = memberQueue(team, member.id);
    for (const auto &value : v.value("phases").toArray()) {
        const auto p = value.toObject();
        Phase phase;
        phase.n = p.value("n").toInt(); phase.title = str(p, "title"); phase.barrier = p.value("barrier").toBool();
        phase.approval = str(p, "approval"); phase.state = str(p, "state");
        phase.addedBy = str(p, "added_by"); phase.decidedBy = str(p, "decided_by");
        phase.startAhead = p.value("start_ahead").toBool();
        phase.addedAt = p.value("added_at").toDouble(); phase.decidedAt = p.value("decided_at").toDouble();
        for (const auto &id : p.value("tasks").toArray()) phase.tasks.append(id.toInt());
        // F10: the lead's diff for this draft, when the store reports one.
        for (const auto &value : p.value("changes").toArray()) {
            const auto entry = value.toObject();
            const auto text = [](const QJsonValue &value) {
                return value.isDouble() ? QString::number(value.toInt()) : value.toString();
            };
            PhaseChange change;
            change.kind = str(entry, "kind");
            change.task = entry.value("task").toInt();
            change.title = str(entry, "title");
            change.from = text(entry.value("from"));
            change.to = text(entry.value("to"));
            if (!change.kind.isEmpty()) phase.changes.append(change);
        }
        // F12: the draft's predecessor is not ready, with its open tasks.
        if (p.value("waiting").isObject()) {
            const auto w = p.value("waiting").toObject();
            phase.waiting.text = str(w, "text"); phase.waiting.phase = w.value("phase").toInt();
            phase.waiting.alongside = w.value("alongside").toBool();
            for (const auto &id : w.value("open").toArray()) phase.waiting.open.append(id.toInt());
        }
        // F13: the owner's sentence the lead approved on.
        const auto word = p.value("owner_word");
        phase.ownerWord = word.isObject() ? str(word.toObject(), "text") : word.toString();
        phase.note = str(p, "note");
        if (p.value("rejection").isObject()) {
            const auto r = p.value("rejection").toObject();
            phase.rejection = {str(r, "comment"), str(r, "by"), r.value("at").toDouble(), r.value("reworked_at").toDouble()};
        }
        if (phase.n < 1) continue;   // phase 0 and negative numbers collide with task ids
        team.phases.append(phase);
    }
    team.idle = str(v, "idle");
    if (v.value("finish").isObject()) {
        const auto f = v.value("finish").toObject();
        team.hasFinish = true;
        team.finish.by = str(f, "by"); team.finish.summary = str(f, "summary"); team.finish.at = f.value("at").toDouble();
    }
    // U59: the plan mode; an older hgs sends none and the team plans standard.
    team.planModeKnown = v.contains("plan_mode") && v.value("plan_mode").isString();
    team.planMode = team.planModeKnown ? str(v, "plan_mode") : QStringLiteral("standard");
    team.planModeOffer = str(v, "plan_mode_offer"); team.planModeReason = str(v, "plan_mode_reason");
    if (v.value("free_phase").isObject()) {
        const auto f = v.value("free_phase").toObject();
        auto &free = team.freePhase;
        free.known = true;
        free.phase = f.value("phase").toInt(); free.state = str(f, "state");
        for (const auto &id : f.value("running").toArray()) free.running.append(id.toInt());
        free.accepted = f.value("accepted").toInt(); free.acceptedSince = f.value("accepted_since").toInt();
        free.lastBuildAt = f.value("last_build_at").toDouble(); free.closePending = f.value("close_pending").toInt();
    }
    // hgs derives both; a team file without them (an older hgs) gets the same
    // answer from the tasks and the phase records.
    team.waitingForLead = v.value("waiting_for_lead").toBool() || (team.status == "running" && allDecided(team));
    // Spec 10: several drafts may wait; they keep their order. A team that is
    // over shows none, even if an old record was left in draft.
    const bool over = team.status == "done" || team.status == "stopped" || team.status == "archived";
    if (!over) {
        team.pendingPhase = v.value("pending_phase").toInt();
        for (const auto &value : v.value("pending_phases").toArray()) if (value.toInt() > 0) team.pendingPhases.append(value.toInt());
        if (team.pendingPhases.isEmpty())
            for (const auto &phase : team.phases) if (phase.draft()) team.pendingPhases.append(phase.n);
        std::sort(team.pendingPhases.begin(), team.pendingPhases.end());
        if (!team.pendingPhase) team.pendingPhase = team.pendingPhases.isEmpty() ? 0 : team.pendingPhases.first();
    }
    // F7: the member sessions that still run after the team ended.
    team.leftRunningKnown = v.contains("left_running");
    for (const auto &value : v.value("left_running").toArray()) {
        const auto l = value.toObject();
        LeftRunning left;
        left.member = str(l, "member"); left.role = str(l, "role");
        const auto session = l.value("session").toObject();
        left.host = str(session, "host"); left.session = str(session, "name");
        left.runId = str(session, "run_id"); left.conversationId = str(session, "conversation_id");
        if (!left.session.isEmpty()) team.leftRunning.append(left);
    }
    if (v.contains("layers")) {
        team.full = true;
        const auto layers = v.value("layers").toObject();
        for (auto it = layers.begin(); it != layers.end(); ++it) team.layers.insert(it.key().toInt(), it.value().toInt());
        for (const auto &id : v.value("cycle").toArray()) team.cycle.insert(id.toInt());
        for (const auto &id : v.value("ready").toArray()) team.ready.append(id.toInt());
        const auto validation = v.value("validation").toObject();
        for (const auto &f : validation.value("errors").toArray()) {
            team.errors.append(str(f.toObject(), "text"));
            team.errorItems.append({f.toObject().value("task").toInt(), str(f.toObject(), "text")});
        }
        for (const auto &f : validation.value("warnings").toArray()) {
            team.warnings.append(str(f.toObject(), "text"));
            team.warningItems.append({f.toObject().value("task").toInt(), str(f.toObject(), "text")});
        }
        // C7: the plan-health numbers, with the starving members while running.
        // hgs puts them in validation.health; a bare health is accepted too.
        const auto h = validation.value("health").isObject() ? validation.value("health").toObject() : v.value("health").toObject();
        if (!h.isEmpty()) {
            team.health.known = true;
            team.health.tasks = h.value("tasks").toInt();
            team.health.criticalPath = h.value("critical_path").toInt();
            team.health.widestReady = h.value("widest_ready").toInt();
            team.health.parallelism = h.value("parallelism").toDouble();
            const auto members = h.value("per_member").toObject();
            for (auto it = members.begin(); it != members.end(); ++it) {
                const auto load = it.value().toObject();
                team.health.perMember.append({it.key(), {load.value("tasks").toInt(), load.value("on_critical_path").toInt()}});
            }
            for (const auto &value : h.value("fan_in").toArray()) {
                const auto entry = value.toObject();
                Team::Health::FanIn fanIn;
                fanIn.task = entry.value("task").toInt(); fanIn.kind = str(entry, "kind");
                for (const auto &id : entry.value("waits_for").toArray()) fanIn.waitsFor.append(id.toInt());
                team.health.fanIn.append(fanIn);
            }
            for (const auto &value : h.value("warnings").toArray()) team.health.warnings.append(value.toString());
            for (const auto &value : h.value("starved").toArray()) {
                const auto entry = value.toObject();
                Team::Health::Starved starved;
                starved.member = str(entry, "member"); starved.role = str(entry, "role");
                starved.minutes = entry.value("minutes").toInt();
                for (const auto &id : entry.value("blocking").toArray()) starved.blocking.append(id.toInt());
                team.health.starved.append(starved);
            }
        }
        // G9 (spec 10.2): the phase headers, in display order, phase 1 included.
        for (const auto &value : v.value("phase_summary").toArray()) {
            const auto entry = value.toObject();
            Team::PhaseSummary summary;
            summary.n = entry.value("n").toInt();
            summary.title = str(entry, "title");
            summary.status = str(entry, "status");
            summary.state = str(entry, "state");
            summary.healthLine = str(entry, "health_line");
            summary.done = entry.value("done").toInt();
            summary.total = entry.value("total").toInt();
            summary.barrier = entry.value("barrier").toBool();
            if (summary.n > 0) team.phaseSummary.append(summary);
        }
        // G9 (spec 10.1): each phase's own plan health, keyed by phase number.
        const auto phaseHealth = v.value("phases_health").toObject();
        for (auto it = phaseHealth.begin(); it != phaseHealth.end(); ++it) {
            const int n = it.key().toInt();
            if (n <= 0 || !it.value().isObject()) continue;
            const auto health = parseHealth(it.value().toObject());
            if (health.known) team.phasesHealth.insert(n, health);
        }
    } else {
        team.layers = computeLayers(team);
        for (const auto &task : team.tasks) if (ready(team, task)) team.ready.append(task.id);
    }
    // C4: the undecided reports on the owner's attention list, also from the
    // polled team list.
    for (const auto &value : v.value("attention").toArray()) {
        const auto entry = value.toObject();
        Team::Attention attention;
        attention.kind = str(entry, "kind"); attention.task = entry.value("task").toInt();
        attention.member = str(entry, "member"); attention.since = entry.value("since").toDouble();
        attention.minutes = entry.value("minutes").toInt();
        attention.summary = str(entry, "summary"); attention.command = str(entry, "command");
        attention.phase = entry.value("phase").toInt(); attention.count = entry.value("count").toInt();
        team.attention.append(attention);
    }
    // F13: the plan's approval records the owner's sentence.
    team.approvedBy = str(v, "approved_by");
    const auto word = v.value("owner_word");
    team.ownerWord = word.isObject() ? str(word.toObject(), "text") : word.toString();
    // F5: the draft's own fields (spec 3).
    team.goal = str(v, "goal"); team.leadProtocol = str(v, "lead_protocol");
    team.folderPolicy = str(settings, "folder_policy");
    team.maxMembers = settings.value("max_members").toInt();
    team.settingsJson = settings;
    team.resumeAfterLimits = settings.value("resume_after_limits").toBool();
    team.autoAccept = settings.value("auto_accept").toBool() || v.value("auto_accept").toBool();
    team.workAhead = settings.value("work_ahead").toBool();
    team.buildBroker = settings.value("build_broker").toBool();
    const auto builds = v.value("build_states").toObject();
    for (auto it = builds.begin(); it != builds.end(); ++it) {
        const auto b = it.value().toObject();
        team.buildStates.insert(it.key(), {str(b, "state"), str(b, "template"), b.value("since").toDouble(), b.value("ahead").toInt(), b.value("snapshot").toBool()});
    }
    for (const auto &value : v.value("intakes").toArray()) {
        const auto i = value.toObject();
        team.intakes.append({str(i, "id"), str(i, "label"), i.value("created").toDouble()});
    }
    for (const auto &id : v.value("stuck_scope").toArray()) team.stuckScope.append(id.toInt());
    team.previous = str(v, "previous"); team.next = str(v, "next");
    for (const auto &value : v.value("proposals").toArray()) {
        const auto p = value.toObject();
        Team::Proposal proposal;
        proposal.id = str(p, "id"); proposal.kind = str(p, "kind"); proposal.by = str(p, "by");
        proposal.state = str(p, "state"); proposal.reason = str(p, "reason"); proposal.note = str(p, "note");
        proposal.goal = p.value("goal").toString();
        proposal.mode = str(p, "mode"); proposal.hash = str(p, "hash");
        proposal.at = p.value("at").toDouble();
        proposal.member = p.value("member"); proposal.change = p.value("change");
        if (!proposal.id.isEmpty()) team.proposals.append(proposal);
    }
    for (const auto &value : v.value("pending_additions").toArray()) {
        const auto a = value.toObject();
        Team::Addition addition;
        addition.id = str(a, "id"); addition.by = str(a, "by"); addition.request = str(a, "request");
        addition.phase = a.value("phase").toInt(); addition.warnedAt = a.value("warned_at").toDouble();
        for (const auto &task : a.value("tasks").toArray()) addition.titles << task.toObject().value("title").toString();
        if (!addition.id.isEmpty()) team.additions.append(addition);
    }
    for (const auto &value : v.value("swapped").toArray()) {
        const auto entry = value.toObject();
        team.swapped.append({str(entry, "member"), str(entry, "old"), str(entry, "by"), entry.value("at").toDouble()});
    }
    for (const auto &value : v.value("hosts").toArray()) {
        const auto entry = value.toObject();
        Team::Host host;
        host.agent = str(entry, "agent"); host.pid = entry.value("pid").toInteger(); host.started = entry.value("started").toDouble();
        for (const auto &id : entry.value("members").toArray()) host.members << id.toString();
        team.hosts.append(host);
    }
    const auto draft = v.value("draft").toObject();
    team.draftRevision = draft.value("revision").toInteger();
    for (const auto &field : draft.value("unseen").toArray()) team.draftUnseen << field.toString();
    // D2: findings is a page object {items, next_cursor}; the summary and the
    // deferred list come with it.
    const auto findings = v.value("findings").toObject();
    if (v.contains("findings")) {
        team.findingsKnown = true;
        for (const auto &item : findings.value("items").toArray()) {
            const auto f = item.toObject();
            TeamFinding finding;
            finding.id = str(f, "id");
            finding.fromTask = str(f, "from_task");
            finding.severity = str(f, "severity");
            finding.title = str(f, "title");
            finding.file = str(f, "file");
            finding.line = f.value("line").toInt();
            finding.scenario = str(f, "scenario");
            finding.task = str(f, "task");
            finding.state = str(f, "state");
            finding.by = str(f, "by");
            finding.at = f.value("at").toDouble();
            finding.note = str(f, "note");
            finding.proposal = str(f, "proposal");
            finding.phase = f.value("phase").toInt();
            team.findings.append(finding);
        }
        team.findingsNext = str(findings, "next_cursor");
    }
    if (v.value("findings_summary").isObject()) team.findingsSummary = v.value("findings_summary").toObject();
    return team;
}

QJsonObject Member::json() const
{
    QJsonObject o{{"id", id}, {"role", role}, {"agent", agent}, {"account", account}, {"model", model}, {"effort", effort},
                  {"permissions", permissions}, {"computer", computer}, {"directory", directory}, {"prompt", prompt}};
    if (!folder.isEmpty()) o.insert("folder", folder);
    if (!parent.isEmpty()) o.insert("parent", parent);
    if (sub) o.insert("sub", true);
    // U45/U46: kept through a plan edit, and absent when never set.
    if (!models.isEmpty()) o.insert("models", modelChoicesJson(models));
    if (!contextBeforeTask.isEmpty()) o.insert("context_before_task", contextBeforeTask);
    if (!session.isEmpty()) {
        // F8: an adopted session keeps its identity, so hgs finds the same one.
        QJsonObject ref{{"host", host}, {"name", session}};
        if (!runId.isEmpty()) ref.insert("run_id", runId);
        if (!conversationId.isEmpty()) ref.insert("conversation_id", conversationId);
        o.insert("session", ref);
    }
    return o;
}

QJsonObject Task::json() const
{
    QJsonArray d, c;
    for (const auto &dep : deps) d.append(QJsonObject{{"task", dep.task}, {"kind", dep.soft ? "soft" : "hard"}});
    for (const auto &criterion : criteria) c.append(criterion);
    QJsonObject value{{"id", id}, {"title", title}, {"description", description}, {"criteria", c}, {"assignee", assignee}, {"deps", d}};
    // C3 (#548): the phase it is planned in; hgs puts a task without one in phase 1.
    value.insert("phase", qMax(1, phase));
    if (lane.known()) value.insert("lane", QJsonObject{{"n", lane.n}, {"label", lane.label}});
    // U45: only a task that names them; a plan without them is as before.
    if (!models.isEmpty()) value.insert("models", QJsonArray::fromStringList(models));
    if (!role.isEmpty()) value.insert("role", role);
    // U36: the plan's pipeline choice; a plan without it keeps the default.
    if (!pipeline.isEmpty()) value.insert("pipeline", pipeline);
    // #746: only the lead's own estimate goes back in a plan; one from the
    // history stays the history's.
    if (estimateSource == "lead" && estimateMinutes > 0) value.insert("estimate_minutes", estimateMinutes);
    return value;
}

QJsonObject planInput(const Team &team)
{
    QJsonArray members, tasks, phases;
    for (const auto &m : team.members) if (!m.lead) members.append(m.json());
    for (const auto &t : team.tasks) if (t.status != "cancelled") tasks.append(t.json());
    // C3 (#548): the roadmap of later phases, in their order; `plan` replaces
    // team.phases with what it is sent, so a plan without it wipes them.
    QList<const Phase *> later;
    for (const auto &p : team.phases) if (p.n >= 2) later.append(&p);
    std::stable_sort(later.begin(), later.end(), [](const Phase *a, const Phase *b) { return a->n < b->n; });
    for (const auto *p : later) phases.append(QJsonObject{{"n", p->n}, {"title", p->title}, {"barrier", p->barrier}});
    return {{"members", members}, {"tasks", tasks}, {"phases", phases}};
}

int nextTaskId(const Team &team)
{
    int next = 1;
    for (const auto &t : team.tasks) next = qMax(next, t.id + 1);
    return next;
}

QString nextMemberId(const Team &team, const QString &role)
{
    QString base;
    for (const auto ch : role.toLower()) base += ch.isLetterOrNumber() && ch.unicode() < 128 ? ch : QChar('-');
    while (base.contains("--")) base.replace("--", "-");
    base = base.left(32).remove(QRegularExpression("^-+|-+$"));
    if (base.isEmpty()) base = QStringLiteral("member");
    QString id = base;
    for (int n = 2; team.member(id); ++n) id = QStringLiteral("%1-%2").arg(base).arg(n);
    return id;
}

QList<Team> parseList(const QJsonObject &value)
{
    QList<Team> teams;
    for (const auto &item : value.value("items").toArray()) teams.append(parse(item.toObject()));
    return teams;
}

bool ready(const Team &team, const Task &task)
{
    if (task.held) return false;   // A5: a held task is never ready
    if (task.status != "pending" || team.cycle.contains(task.id)) return false;
    for (const auto &dep : task.deps) {
        const auto *d = team.task(dep.task);
        if (!d) return false;
        const bool soft = dep.soft && !team.allHard;
        const bool ok = d->status == "accepted" || (soft && (d->status == "in_progress" || d->status == "submitted" || d->inStage()));
        if (!ok) return false;
    }
    return true;
}

Status memberStatus(const Team &team, const Member &member, const Live &live)
{
    const auto make = [](const QString &kind, const QString &text, const QString &shortText, Tone tone, int task = 0, bool problem = false) {
        Status s; s.kind = kind; s.text = text; s.shortText = shortText; s.tone = tone; s.task = task; s.problem = problem; return s;
    };
    // F5: nothing but the lead exists while the team is a draft.
    if (team.status == "draft")
        return member.session.isEmpty() ? make("not_created", tr("Created with the team"), tr("Draft"), Tone::Dim)
                                        : make("planning", tr("Drafting the team"), tr("Drafting"), Tone::Planning);
    if (team.status == "planning")
        return member.session.isEmpty() ? make("not_created", tr("Created on approval"), tr("Planned"), Tone::Dim)
                                        : make("planning", tr("Planning"), tr("Planning"), Tone::Planning);
    if (!member.error.isEmpty()) {
        // U54: a move or launch that waits for its old session, a folder left
        // on another branch and an unconfirmed switch say so.
        switch (AccountMove::blockOf(member).kind) {
        case AccountMove::Block::OldSession: return make("paused", tr("Waiting for its old session"), tr("Waiting"), Tone::Warn, 0, true);
        case AccountMove::Block::FolderBranch: return make("failed", tr("Folder on another branch"), tr("Branch"), Tone::Danger, 0, true);
        case AccountMove::Block::Uncertain: return make("stuck", tr("Account switch unconfirmed"), tr("Unconfirmed"), Tone::Warn, 0, true);
        case AccountMove::Block::None: break;
        }
        return make("failed", tr("Failed to start"), tr("Failed"), Tone::Danger, 0, true);
    }
    if (member.session.isEmpty()) return make("not_started", tr("Not started"), tr("Not started"), Tone::Dim);
    if (member.state == "paused") return make("paused", tr("Paused"), tr("Paused"), Tone::Muted);
    // The member's open attempt, if any.
    const Task *current = nullptr; const Attempt *attempt = nullptr;
    for (const auto &task : team.tasks)
        if (const auto *a = openOf(task); a && a->member == member.id) { current = &task; attempt = a; break; }
    if (live.offline) return make("offline", tr("Offline"), tr("Offline"), Tone::Danger, current ? current->id : 0, true);
    if (live.attention) return make("answer", tr("Needs your answer"), tr("Answer"), Tone::Warn, current ? current->id : 0, true);
    if (attempt && attempt->state == "unconfirmed")
        return make("unconfirmed", tr("Delivery unconfirmed"), tr("Unconfirmed"), Tone::Danger, current->id, true);
    if (attempt && attempt->noReport) return make("no_report", tr("No report"), tr("No report"), Tone::Warn, current->id, true);
    // A3: a question to the lead leaves the attempt open and pauses the work.
    if (attempt && attempt->openQuestion())
        return make("asking", tr("Waiting for your answer"), tr("Answer"), Tone::Warn, current->id, true);
    // QA: an attempt that only left for the member reads as sending, not as
    // work (its icon says the same).
    if (attempt && attempt->state == "delivering")
        return make("sent", tr("Sending #%1 %2").arg(current->id).arg(current->title), QStringLiteral("Sending"), Tone::Assigned, current->id);
    if (attempt)
        return make("working", tr("Working on #%1 %2").arg(current->id).arg(current->title), QStringLiteral("#%1").arg(current->id), Tone::Active, current->id);
    // U36: a stage it was given: "Reviewing #3 Card form UI · round 1".
    for (const auto &task : team.tasks) {
        const auto *run = task.inStage() ? task.pipelineRun.openRun() : nullptr;
        if (!run || run->state != "delivered" || run->member != member.id) continue;
        const QString what = run->kind == "test" ? tr("Testing #%1 %2 · round %3") : tr("Reviewing #%1 %2 · round %3");
        return make(run->kind == "test" ? "stage_test" : "stage_review", what.arg(task.id).arg(task.title).arg(run->round),
                    QStringLiteral("#%1").arg(task.id), Tone::Active, task.id);
    }
    // U36: its folder is held by a stage of one of its tasks; its next task waits.
    if (const auto hold = folderHold(team, member.id); hold.task && (team.status == "running" || team.status == "paused"))
        for (const auto &task : team.tasks)
            if (task.assignee == member.id && task.status == "pending" && !task.held && task.id != hold.task)
                return make("folder_held", tr("Waits: its folder is held by %1").arg(holderText(hold)), tr("Folder held"), Tone::Warn, task.id);
    // Spec 5.6: a sub-lead reviews its reports' work, or coordinates them.
    if (member.sub && (team.status == "running" || team.status == "paused")) {
        for (const auto &task : team.tasks)
            if (task.status == "submitted" && team.reviewerOf(task.assignee) == member.id)
                return make("reviewing", tr("Reviewing #%1 %2").arg(task.id).arg(task.title), QStringLiteral("#%1").arg(task.id), Tone::Active, task.id);
    }
    for (const auto &task : team.tasks)
        if (task.status == "submitted" && task.last() && task.last()->member == member.id)
            return make("submitted", tr("Submitted"), tr("Submitted"), Tone::Done, task.id);
    // U36: its report is in a stage.
    for (const auto &task : team.tasks)
        if (task.inStage() && task.last() && task.last()->member == member.id)
            return make(task.status, task.status == "in_test" ? tr("In test") : tr("In review"),
                        task.status == "in_test" ? tr("In test") : tr("In review"), Tone::Done, task.id);
    if (member.sub && (team.status == "running" || team.status == "paused")) {
        const qsizetype n = team.reportsOf(member.id).size();
        const QString text = n == 1 ? tr("Coordinating 1 report") : tr("Coordinating %1 reports").arg(n);
        return make("coordinating", text, tr("Coordinating"), Tone::Muted);
    }
    if (team.status == "done") return make("done", tr("Done"), tr("Done"), Tone::Done);
    if (live.working) return make("working", tr("Working"), tr("Working"), Tone::Active);
    return make("idle", tr("Idle"), tr("Idle"), Tone::Muted);
}

Status taskStatus(const Task &task)
{
    Status s; s.kind = task.status; s.task = task.id;
    const auto *open = openOf(task);
    const int n = task.last() ? task.last()->n : 0;
    const QString phase = lastPhase(task);
    if (open && open->state == "unconfirmed") { s.text = tr("Delivery unconfirmed"); s.tone = Tone::Danger; s.problem = true; }
    else if (open && open->noReport) { s.text = tr("No report"); s.tone = Tone::Warn; s.problem = true; }
    else if (task.held) { s.text = tr("Held"); s.tone = Tone::Muted; }
    // A1/A3: the attempt's own phase where it changes what the task is doing.
    else if (phase == "asking") { s.text = tr("Waiting for an answer"); s.tone = Tone::Warn; }
    else if (phase == "late") { s.text = tr("Not started"); s.tone = Tone::Warn; }
    else if (phase == "stopped") { s.text = tr("Stopped"); s.tone = Tone::Danger; }
    else if (task.status == "draft") { s.text = tr("Draft"); s.tone = Tone::Dim; }
    else if (task.status == "pending") { s.text = tr("Pending"); s.tone = Tone::Muted; }
    else if (task.status == "assigned") { s.text = tr("Assigned"); s.tone = Tone::Assigned; }
    else if (task.status == "in_progress") { s.text = tr("In progress"); s.tone = Tone::Active; }
    else if (task.status == "submitted") { s.text = tr("Submitted"); s.tone = Tone::Done; }
    // U36: the stages between the report and the lead.
    else if (task.status == "in_review") { s.text = tr("In review"); s.tone = Tone::Active; }
    else if (task.status == "in_test") { s.text = tr("In test"); s.tone = Tone::Active; }
    else if (task.status == "accepted") { s.text = tr("Accepted"); s.tone = Tone::Done; }
    else if (task.status == "rejected") { s.text = tr("Rejected"); s.tone = Tone::Danger; }
    else if (task.status == "cancelled") { s.text = tr("Cancelled"); s.tone = Tone::Dim; }
    else s.text = task.status;
    s.shortText = s.text;
    if (n > 1 && !task.finished()) s.text += tr(" · attempt %1").arg(n);
    return s;
}

QList<Problem> problems(const Team &team, const LiveLookup &live)
{
    QList<Problem> list;
    for (const auto &member : team.members) {
        const auto status = memberStatus(team, member, live ? live(member) : Live{});
        if (!status.problem) continue;
        Problem p; p.kind = status.kind; p.member = member.id; p.task = status.task; p.tone = status.tone;
        const auto *task = team.task(status.task);
        p.text = task ? QStringLiteral("#%1 %2 — %3 · %4").arg(task->id).arg(task->title, team.roleOf(member.id), status.text)
                      : QStringLiteral("%1 · %2").arg(team.roleOf(member.id), status.text);
        list.append(p);
    }
    return list;
}

QString displayStatus(const Team &team, const LiveLookup &live)
{
    if (team.status != "running") return team.status;
    bool open = false;
    // U25: only the open tasks of running phases can make a team Stuck; a
    // draft, a send-back or a queued phase never does (status.stuck_scope).
    const QSet<int> scope(team.stuckScope.cbegin(), team.stuckScope.cend());
    for (const auto &task : team.tasks) {
        if (task.finished() || task.status == "draft") continue;
        if (!scope.isEmpty() && !scope.contains(task.id)) continue;
        open = true;
        if (task.status == "submitted") return team.status;   // the lead decides
        // U36: a stage works on it, unless it waits for a member nobody can be
        // (#538 L6: no candidate; Zerus runs a command or land stage itself).
        if (task.inStage()) {
            if (!stageWithoutCandidate(task)) return team.status;
            continue;
        }
        const Member *assignee = team.member(task.assignee);
        const Live state = assignee && live ? live(*assignee) : Live{};
        const auto *attempt = openOf(task);
        if (attempt) {
            if (attempt->state != "unconfirmed" && !attempt->noReport && !state.offline && !state.attention) return team.status;
            continue;
        }
        if (task.assignee.isEmpty() && task.status == "in_progress") return team.status;   // taken over by you
        if (ready(team, task) && assignee && assignee->error.isEmpty() && assignee->state != "paused" && !state.offline)
            return team.status;
    }
    return open ? QStringLiteral("stuck") : team.status;
}

QString Team::Proposal::memberId() const
{
    if (member.isString()) return member.toString();
    const auto object = member.toObject();
    // A swap names the member it swaps; an add or remove carries the member.
    return object.value("member").isString() ? object.value("member").toString() : object.value("id").toString();
}

const Team::Host *Team::hostOf(const QString &member) const
{
    for (const auto &host : hosts) if (host.members.contains(member)) return &host;
    return nullptr;
}

QString resetText(double resetsAt)
{
    if (resetsAt <= 0) return {};
    const auto at = QDateTime::fromSecsSinceEpoch(qint64(resetsAt));
    return at.date() == QDate::currentDate() ? tr("resets %1").arg(at.toString(QStringLiteral("HH:mm")))
                                             : tr("resets %1").arg(at.toString(QStringLiteral("d MMM HH:mm")));
}

namespace {
// A proposed member, "Designer (claude · sonnet)".
QString proposedMember(const Team &team, const QJsonObject &object, const QString &id)
{
    const QString role = team.member(id) ? team.roleOf(id) : object.value("role").toString(id);
    QStringList how;
    for (const auto *key : {"agent", "model", "effort", "account"})
        if (const auto value = object.value(QLatin1String(key)).toString(); !value.isEmpty()) how << value;
    // #724: no dots as separators (the card shows on Waiting for you too).
    return how.isEmpty() ? role : QStringLiteral("%1 (%2)").arg(role, how.join(QStringLiteral(", ")));
}

QJsonArray proposedMembers(const Team::Proposal &proposal)
{
    const auto members = proposal.change.toObject().value("members").toArray();
    return members.isEmpty() && proposal.member.isObject() ? QJsonArray{proposal.member} : members;
}

QString phaseName(const Team &team, int n)
{
    const auto *phase = team.phase(n);
    return phase && !phase->title.isEmpty() ? tr("phase %1 (%2)").arg(n).arg(phase->title) : tr("phase %1").arg(n);
}
}

QString proposalText(const Team &team, const Team::Proposal &proposal)
{
    const auto object = proposal.member.toObject();
    const QString id = proposal.memberId();
    const QString role = team.member(id) ? team.roleOf(id) : object.value("role").toString(id);
    QStringList how;
    for (const auto *key : {"agent", "model", "effort", "account"})
        if (const auto value = object.value(QLatin1String(key)).toString(); !value.isEmpty()) how << value;
    const auto change = proposal.change.toObject();
    if (proposal.kind == "add-member") {
        const auto members = proposedMembers(proposal);
        if (members.size() > 1) return tr("Add %1 members").arg(members.size());
        const auto one = members.isEmpty() ? object : members.first().toObject();
        return tr("Add %1").arg(proposedMember(team, one, one.value("id").toString()));
    }
    if (proposal.kind == "remove-member") return tr("Remove %1").arg(role);
    if (proposal.kind == "swap-member") {
        const auto session = object.value("session").toObject().value("name").toString();
        if (!session.isEmpty()) return tr("Swap %1 to the session %2").arg(role, session);
        return how.isEmpty() ? tr("Swap %1 to a new session").arg(role) : tr("Swap %1 to %2").arg(role, how.join(QStringLiteral(", ")));
    }
    if (proposal.kind == "next-goal") return tr("Next goal: %1").arg(proposal.goal.section('\n', 0, 0).left(120));
    if (proposal.kind == "switch-plan-mode") return proposal.mode == "standard" ? tr("Switch to standard planning") : tr("Switch to free planning");
    // U57: tasks for a running phase, tasks to cancel, a phase to discard.
    if (proposal.kind == "add-to-phase") {
        const int count = int(change.value("tasks").toArray().size());
        const QString phase = phaseName(team, change.value("phase").toInt());
        return count == 1 ? tr("Add 1 task to %1").arg(phase) : tr("Add %1 tasks to %2").arg(count).arg(phase);
    }
    if (proposal.kind == "cancel-tasks") {
        const int count = int(change.value("tasks").toArray().size());
        return count == 1 ? tr("Cancel 1 task") : tr("Cancel %1 tasks").arg(count);
    }
    if (proposal.kind == "discard-phase") return tr("Discard %1").arg(phaseName(team, change.value("phase").toInt()));
    // #740: the lead's question to the owner, whole.
    if (proposal.kind == "question") return change.value("question").toString();
    // #756: the lead's switch card for one owner setting.
    if (proposal.kind == "switch-setting") {
        const bool on = change.value("value").toBool();
        if (change.value("setting").toString() == QLatin1String("build_broker"))
            return on ? tr("Turn on the build broker") : tr("Turn off the build broker");
        return on ? tr("Turn on %1").arg(change.value("setting").toString()) : tr("Turn off %1").arg(change.value("setting").toString());
    }
    return proposal.kind;
}

QStringList proposalChanges(const Team &team, const Team::Proposal &proposal)
{
    QStringList lines;
    const auto change = proposal.change.toObject();
    const auto taskLine = [&team](int id) {
        const auto *task = team.task(id);
        return task ? tr("#%1 %2 (%3)").arg(id).arg(task->title, statusLabel(task->status)) : tr("#%1").arg(id);
    };
    if (proposal.kind == "add-member") {
        const auto members = proposedMembers(proposal);
        if (members.size() > 1)
            for (const auto &member : members) lines << proposedMember(team, member.toObject(), member.toObject().value("id").toString());
    } else if (proposal.kind == "remove-member") {
        lines << tr("Its session keeps running; it gets no more tasks.");
    } else if (proposal.kind == "swap-member") {
        lines << tr("Its open task goes to the new session with what was done so far; the old session keeps running for you to stop.");
    } else if (proposal.kind == "next-goal") {
        lines << proposal.goal.left(600);
    } else if (proposal.kind == "add-to-phase") {
        for (const auto &value : change.value("tasks").toArray()) {
            const auto task = value.toObject();
            const QString title = task.value("title").toString(tr("Untitled task"));
            const QString assignee = task.value("assignee").toString();
            QString line = assignee.isEmpty() ? title : tr("%1, for %2").arg(title, team.member(assignee) ? team.roleOf(assignee) : assignee);
            // #746: the lead's estimate, when the proposal names one.
            const double estimate = task.value("estimate_minutes").toDouble();
            if (estimate > 0) line += tr(", ~%1 min").arg(qRound64(estimate));
            lines << line;
        }
    } else if (proposal.kind == "cancel-tasks") {
        for (const auto &value : change.value("tasks").toArray()) lines << taskLine(value.toInt());
    } else if (proposal.kind == "discard-phase") {
        const auto *phase = team.phase(change.value("phase").toInt());
        if (phase) for (int id : phase->tasks) lines << taskLine(id);
        if (lines.isEmpty()) lines << tr("It has no tasks.");
    } else if (proposal.kind == "switch-setting" && change.value("setting").toString() == QLatin1String("build_broker")) {
        // #756: who shares which folder, and what the broker gives them.
        for (const auto &value : change.value("shared").toArray()) {
            const auto shared = value.toObject();
            QStringList roles;
            for (const auto &member : shared.value("members").toArray()) roles << team.roleOf(member.toString());
            lines << tr("%1 share %2").arg(roles.join(QStringLiteral(", ")), shared.value("folder").toString());
        }
        if (change.value("value").toBool())
            lines << tr("Each member builds its own snapshot of the folder, one build at a time per build folder; an unchanged tree builds once, "
                        "and a failure in another member's file goes to that member.");
    }
    return lines;
}

QString statusLabel(const QString &status)
{
    if (status == "draft") return tr("Draft");
    if (status == "planning") return tr("Planning");
    if (status == "running") return tr("Running");
    if (status == "paused") return tr("Paused");
    if (status == "stuck") return tr("Stuck");
    if (status == "done") return tr("Done");
    if (status == "stopped") return tr("Stopped");
    if (status == "archived") return tr("Archived");
    return status;
}

Tone statusTone(const QString &status)
{
    if (status == "planning") return Tone::Planning;
    if (status == "running") return Tone::Active;
    if (status == "paused") return Tone::Muted;
    if (status == "stuck") return Tone::Warn;
    if (status == "done") return Tone::Done;
    if (status == "stopped") return Tone::Danger;
    return Tone::Dim;
}

QString nextLine(const Team &team)
{
    if (team.status == "planning") return tr("Plan from Lead — review before running");
    QStringList readyIds;
    for (int id : team.ready) readyIds << QStringLiteral("#%1").arg(id);
    if (!readyIds.isEmpty()) return tr("Ready: %1").arg(readyIds.join(", "));
    // The first unfinished dependency that holds pending tasks back.
    QMap<int, QStringList> waiting;
    for (const auto &task : team.tasks) {
        if (task.status != "pending") continue;
        for (const auto &dep : task.deps)
            if (const auto *d = team.task(dep.task); d && d->status != "accepted") { waiting[d->id] << QStringLiteral("#%1").arg(task.id); break; }
    }
    if (!waiting.isEmpty()) {
        const auto *blocker = team.task(waiting.firstKey());
        return tr("Next: %1 wait for #%2 %3").arg(waiting.first().join(", ")).arg(blocker->id).arg(blocker->title);
    }
    bool open = false;
    for (const auto &task : team.tasks) open = open || (!task.finished() && task.status != "draft");
    return open ? QString() : tr("Every task is decided");
}

QList<Tone> progress(const Team &team)
{
    QList<Tone> tones;
    for (const auto &task : team.tasks) tones.append(taskStatus(task).tone);
    return tones;
}

int accepted(const Team &team)
{
    int n = 0;
    for (const auto &task : team.tasks) n += task.status == "accepted";
    return n;
}

bool allDecided(const Team &team)
{
    // U59: a free phase's empty queue is idle, not done.
    if (team.tasks.isEmpty() || team.freeMode()) return false;
    for (const auto &task : team.tasks) if (!task.finished()) return false;
    return true;
}

int answers(const Team &team, const LiveLookup &live)
{
    int n = 0;
    if (live)
        for (const auto &member : team.members) n += !member.session.isEmpty() && live(member).attention;
    // A3: a question asked with `hgs team ask` waits for the lead too.
    for (const auto &task : team.tasks) n += openQuestions(task).size();
    return n;
}

QSet<int> chain(const Team &team, int id)
{
    QSet<int> result{id};
    QList<int> pending{id};
    while (!pending.isEmpty()) {   // ancestors
        const auto *task = team.task(pending.takeLast());
        if (task) for (const auto &dep : task->deps) if (!result.contains(dep.task)) { result.insert(dep.task); pending.append(dep.task); }
    }
    QSet<int> down{id};
    pending = {id};
    while (!pending.isEmpty()) {   // descendants
        const int current = pending.takeLast();
        for (const auto &task : team.tasks)
            for (const auto &dep : task.deps)
                if (dep.task == current && !down.contains(task.id)) { down.insert(task.id); pending.append(task.id); }
    }
    return result.unite(down);
}

QMap<int, QList<int>> layered(const Team &team)
{
    QMap<int, QList<int>> layers;
    for (const auto &task : team.tasks) layers[team.layers.value(task.id) + 1].append(task.id);
    return layers;
}

QList<PhaseGroup> phaseGroups(const Team &team)
{
    QList<PhaseGroup> groups;
    // Owner, 2026-10-29: a plan of one phase gets its phase section too, in
    // Graph, Board and Layers alike, so the controls never move between a
    // one-phase and a many-phase plan.
    QMap<int, PhaseGroup> byPhase;
    for (const auto &task : team.tasks) {
        PhaseGroup &group = byPhase[task.phase];
        group.n = task.phase;
        group.tasks.append(task.id);
        group.done += task.finished();
    }
    for (auto it = byPhase.cbegin(); it != byPhase.cend(); ++it) {
        PhaseGroup group = it.value();
        if (const auto *record = team.phase(it.key())) {
            group.title = record->title;
            group.barrier = record->barrier;
            group.draft = record->draft();
        }
        groups.append(group);
    }
    return groups;
}

bool phaseCancelled(const Team &team, int phase)
{
    if (const auto *record = team.phase(phase); record && record->state == QLatin1String("discarded")) return false;
    bool any = false;
    for (const auto &task : team.tasks) {
        if (task.phase != phase) continue;
        if (task.status != QLatin1String("cancelled")) return false;
        any = true;
    }
    return any;
}

int cancelledTasks(const Team &team)
{
    return int(std::count_if(team.tasks.cbegin(), team.tasks.cend(), [](const Task &task) { return task.status == QLatin1String("cancelled"); }));
}

Team withoutCancelled(const Team &team)
{
    if (!cancelledTasks(team)) return team;
    Team shown = team;
    shown.cancelledHidden = true;
    QSet<int> gone, phases;
    for (const auto &task : team.tasks) if (task.status == QLatin1String("cancelled")) gone.insert(task.id);
    for (const auto &task : team.tasks) if (phaseCancelled(team, task.phase)) phases.insert(task.phase);
    shown.tasks.erase(std::remove_if(shown.tasks.begin(), shown.tasks.end(), [&gone](const Task &task) { return gone.contains(task.id); }), shown.tasks.end());
    shown.phases.erase(std::remove_if(shown.phases.begin(), shown.phases.end(), [&phases](const Phase &phase) { return phases.contains(phase.n); }), shown.phases.end());
    for (auto &phase : shown.phases) phase.tasks.erase(std::remove_if(phase.tasks.begin(), phase.tasks.end(), [&gone](int id) { return gone.contains(id); }), phase.tasks.end());
    shown.phaseSummary.erase(std::remove_if(shown.phaseSummary.begin(), shown.phaseSummary.end(), [&phases](const Team::PhaseSummary &summary) { return phases.contains(summary.n); }),
                             shown.phaseSummary.end());
    for (int id : gone) { shown.layers.remove(id); shown.cycle.remove(id); shown.ready.removeAll(id); }
    return shown;
}

bool phaseFinished(const Team &team, int phase)
{
    if (const auto *record = team.phase(phase); record && record->state == QLatin1String("discarded")) return false;
    if (team.freeMode() && team.freePhase.known && team.freePhase.phase == phase) return false;
    bool any = false, cancelledOnly = true;
    for (const auto &task : team.tasks) {
        if (task.phase != phase) continue;
        if (!task.finished()) return false;
        any = true;
        cancelledOnly = cancelledOnly && task.status == QLatin1String("cancelled");
    }
    return any && !cancelledOnly;
}

int finishedPhases(const Team &team)
{
    QSet<int> phases;
    for (const auto &task : team.tasks) if (!phases.contains(task.phase) && phaseFinished(team, task.phase)) phases.insert(task.phase);
    return int(phases.size());
}

Team withoutFinished(const Team &team)
{
    QSet<int> phases;
    for (const auto &task : team.tasks) if (phaseFinished(team, task.phase)) phases.insert(task.phase);
    if (phases.isEmpty()) return team;
    Team shown = team;
    shown.cancelledHidden = true;
    QSet<int> gone;
    for (const auto &task : team.tasks) if (phases.contains(task.phase)) gone.insert(task.id);
    shown.tasks.erase(std::remove_if(shown.tasks.begin(), shown.tasks.end(), [&gone](const Task &task) { return gone.contains(task.id); }), shown.tasks.end());
    shown.phases.erase(std::remove_if(shown.phases.begin(), shown.phases.end(), [&phases](const Phase &phase) { return phases.contains(phase.n); }), shown.phases.end());
    shown.phaseSummary.erase(std::remove_if(shown.phaseSummary.begin(), shown.phaseSummary.end(), [&phases](const Team::PhaseSummary &summary) { return phases.contains(summary.n); }),
                             shown.phaseSummary.end());
    for (int id : gone) { shown.layers.remove(id); shown.cycle.remove(id); shown.ready.removeAll(id); }
    return shown;
}

bool taskStarted(const Task &task)
{
    return std::any_of(task.attempts.cbegin(), task.attempts.cend(), [](const Attempt &attempt) { return attempt.state != QLatin1String("refused"); });
}

QList<MoveTarget> moveTargets(const Team &team, int id)
{
    QList<MoveTarget> targets;
    const auto *task = team.task(id);
    if (!task || task->finished() || taskStarted(*task)) return targets;
    if (team.status != QLatin1String("planning") && team.status != QLatin1String("running") && team.status != QLatin1String("paused")) return targets;
    QList<int> phases{1};
    for (const auto &phase : team.phases) if (phase.n > 1 && phase.state != QLatin1String("discarded")) phases.append(phase.n);
    std::sort(phases.begin(), phases.end());
    phases.erase(std::unique(phases.begin(), phases.end()), phases.end());
    for (int n : phases) {
        if (n == task->phase) continue;
        MoveTarget target;
        target.phase = n;
        if (const auto *record = team.phase(n)) target.title = record->title;
        for (const auto &dep : task->deps)
            if (const auto *found = team.task(dep.task); found && found->phase > n) {
                target.blocked = tr("Depends on #%1 in phase %2; move it first").arg(found->id).arg(found->phase);
                break;
            }
        targets.append(target);
    }
    return targets;
}

QString ownerPhaseState(const Team &team, const PhaseGroup &group)
{
    // U58: an older hgs sends no cancelled state; read it from the tasks, as
    // the server does (before every other state).
    if (phaseCancelled(team, group.n)) return QStringLiteral("cancelled");
    const auto *summary = team.phaseSummaryOf(group.n);
    if (summary && !summary->state.isEmpty()) return summary->state;
    // An hgs before U25 sends only the G9 status: approved and not started waits.
    if (summary && team.status != "planning") {
        if (summary->status == "done") return QStringLiteral("done");
        if (summary->status == "in_work") return QStringLiteral("running");
        if (summary->status == "approved") return QStringLiteral("queued");
        if (summary->status == "paused") return QStringLiteral("paused");
    }
    if (team.status == "planning" || team.status == "draft") return QStringLiteral("planned");
    const auto *record = team.phase(group.n);
    if (record && record->rejected()) return QStringLiteral("rejected");
    if (record && record->draft()) return QStringLiteral("draft");
    if (record && record->queued()) return QStringLiteral("queued");
    return group.finished() ? QStringLiteral("done") : QStringLiteral("running");
}

QString PhaseProgress::tooltip() const
{
    if (!valid()) return {};
    // G8/spec 11: the tooltip names the phase the tasks bar counts.
    const QString name = title.isEmpty() ? tr("Phase %1").arg(phase) : tr("Phase %1 %2").arg(phase).arg(title);
    QString line = QStringLiteral("%1: %2/%3").arg(name).arg(done).arg(total);
    if (draft && autoDraft) line += QLatin1Char('\n') + autoDraftBadge() + QStringLiteral(": ") + autoDraftTip(startAhead);
    else if (draft) line += QStringLiteral(" · ") + tr("waiting for your approval");
    else if (byAuto) line += QLatin1Char('\n') + tr("Accepted by Auto when its first task started");
    return line + QLatin1Char('\n')
        + tr("%1 of %2 phases finished; discarded drafts do not count").arg(phasesDone).arg(phasesTotal);
}

PhaseProgress phaseProgress(const Team &team)
{
    const auto discarded = [&team](int phase) {
        const auto *record = team.phase(phase);
        return record && record->state == "discarded";
    };
    PhaseProgress progress;
    const auto groups = phaseGroups(team);
    for (const auto &group : groups) {
        if (discarded(group.n)) continue;   // a discarded draft is not planned
        ++progress.phasesTotal;
        progress.phasesDone += group.finished();
    }
    // The phase in work: the lowest non-draft phase that still has open tasks.
    for (const auto &group : groups) {
        if (discarded(group.n) || group.draft || group.finished()) continue;
        progress.phase = group.n; progress.title = group.title;
        progress.done = group.done; progress.total = group.tasks.size();
        break;
    }
    // Nothing in work: the next draft waiting for approval.
    if (!progress.phase) for (const auto &group : groups) {
        if (discarded(group.n) || !group.draft) continue;
        progress.phase = group.n; progress.title = group.title; progress.draft = true;
        progress.done = group.done; progress.total = group.tasks.size();
        break;
    }
    // Everything is decided and no draft waits: the last phase stands for the
    // finished plan, so the bar reads done/done.
    if (!progress.phase) for (const auto &group : groups) {
        if (discarded(group.n)) continue;
        progress.phase = group.n; progress.title = group.title;
        progress.done = group.done; progress.total = group.tasks.size();
    }
    // A team without phase records still gets the two bars: its plan is phase 1.
    if (!progress.phase && !team.tasks.isEmpty()) {
        int done = 0; for (const auto &task : team.tasks) done += task.finished();
        progress.phase = 1; progress.done = done; progress.total = team.tasks.size();
        progress.phasesTotal = 1; progress.phasesDone = done == team.tasks.size();
    }
    if (const auto *record = team.phase(progress.phase)) {
        progress.autoDraft = record->autoDraft(); progress.startAhead = record->startAhead; progress.byAuto = record->acceptedByAuto();
    }
    return progress;
}

QString autoDraftBadge() { return tr("Draft (auto)"); }

QString autoDraftTip(bool startAhead)
{
    return startAhead ? tr("The lead started it ahead: idle members take its tasks now, and Auto accepts it when the first one starts. Its reports need no approval from you.")
                      : tr("Auto accepts it when its first task starts, once the phases ahead are done. Until then the lead edits it freely.");
}

QString autoAcceptSwitchTip(bool on)
{
    return on ? tr("On: a phase draft without errors stays a draft the lead edits, and Auto accepts it when its first task starts")
              : tr("Off: every phase draft waits for you");
}

QString phaseHeading(const PhaseGroup &group)
{
    const QString phase = group.title.isEmpty() ? tr("Phase %1").arg(group.n)
                                                : tr("Phase %1 · %2").arg(group.n).arg(group.title);
    return tr("%1 · %2/%3").arg(phase).arg(group.done).arg(group.tasks.size());
}

bool leftMember(const Team &team, const QString &member)
{
    return team.leftOf(member) != nullptr;
}

// C2: one thing on the lead's desk, as `hgs team inbox` lists it.
namespace {
// A task's timing without what grows while it runs or waits: the worked
// seconds of a running attempt, and the waits of a task not decided yet.
QJsonObject steadyTiming(QJsonObject timing, bool decided)
{
    if (!timing.value("running_since").isNull() && timing.contains("running_since")) {
        timing.remove("worked_s");
        QJsonArray attempts;
        for (const auto &value : timing.value("attempts").toArray()) {
            auto attempt = value.toObject();
            if (attempt.value("to").isNull()) { attempt.remove("worked_s"); attempt.remove("member_s"); attempt.remove("lead_s"); }
            attempts.append(attempt);
        }
        timing["attempts"] = attempts;
    }
    if (!decided) timing.remove("waits");
    return timing;
}

// Health and its warnings anywhere: "idle 44 min" reads as "idle N min". The
// desktop ages those minutes in place (agedMinutes, #709 L4); other units
// are drawn as they come, so a change of them rebuilds.
QJsonValue steadyHealth(const QJsonValue &value)
{
    static const QRegularExpression minutes(QStringLiteral("\\b\\d+ min\\b"));
    if (value.isString()) return value.toString().replace(minutes, QStringLiteral("N min"));
    if (value.isArray()) {
        QJsonArray out;
        for (const auto &item : value.toArray()) out.append(steadyHealth(item));
        return out;
    }
    if (!value.isObject()) return value;
    auto object = value.toObject();
    object.remove("minutes");
    for (auto it = object.begin(); it != object.end(); ++it) it.value() = steadyHealth(it.value());
    return object;
}
}

QByteArray visibleShape(QJsonObject status)
{
    status.remove("revision"); status.remove("updated");
    QJsonArray tasks;
    for (const auto &value : status.value("tasks").toArray()) {
        auto task = value.toObject();
        const QString state = task.value("status").toString();
        const bool decided = state == "accepted" || state == "rejected" || state == "cancelled";
        if (task.value("timing").isObject()) task["timing"] = steadyTiming(task.value("timing").toObject(), decided);
        // #746: the spent minutes grow every minute; the views tick them from
        // the running time, so they never rebuild a view.
        task.remove("estimate_spent_minutes");
        tasks.append(task);
    }
    if (status.contains("tasks")) status["tasks"] = tasks;
    for (const char *key : {"health", "validation", "phases_health", "phase_summary"})
        if (status.contains(key)) status[key] = steadyHealth(status.value(key));
    return QJsonDocument(status).toJson(QJsonDocument::Compact);
}

QList<InboxItem> parseInbox(const QJsonObject &value)
{
    QList<InboxItem> items;
    for (const auto &entry : value.value("items").toArray()) {
        const auto item = entry.toObject();
        InboxItem inbox;
        inbox.kind = str(item, "kind"); inbox.member = str(item, "member");
        inbox.summary = str(item, "summary"); inbox.command = str(item, "command");
        inbox.task = item.value("task").toInt(); inbox.since = item.value("since").toDouble();
        items.append(inbox);
    }
    return items;
}

namespace {
// Why phase N may not be approved yet: the nearest lower phase that is still a
// draft or unapproved, or the nearest one with open tasks.
struct PhaseBlocker { int phase = 0; bool approved = false; int open = 0; };
PhaseBlocker nearestBlocker(const Team &team, int phase)
{
    for (int lower = phase - 1; lower >= 1; --lower) {
        const auto *previous = team.phase(lower);
        // A discarded phase is out of the plan, and phase 1 has no record: the
        // approved initial plan is approved by definition.
        if (previous && previous->state == "discarded") continue;
        // U25: a queued phase is accepted; it only waits for its turn.
        if (previous && previous->state != "approved" && !previous->queued())
            return {lower, false, 0};
        int open = 0;
        for (const auto &task : team.tasks) if (task.phase == lower && !task.finished()) ++open;
        if (open) return {lower, true, open};
    }
    return {};
}
}

QString approveBlocked(const Team &team, int phase)
{
    const auto *record = team.phase(phase);
    if (!record || !record->draft()) return tr("Phase %1 is not a draft").arg(phase);
    // QA: name the nearest phase that still has to open, not the first one
    // above the plan: for drafts 2, 3 and 4, phase 4 waits for phase 3.
    const auto blocker = nearestBlocker(team, phase);
    if (!blocker.phase || blocker.approved) return {};
    return tr("Phase %1 opens after phase %2 is approved").arg(phase).arg(blocker.phase);
}

// F12/N40: an approved phase still at work does not block the draft; the
// owner may approve it to run alongside, warned first.
int alongsidePhase(const Team &team, int phase)
{
    const auto *record = team.phase(phase);
    if (!record) return 0;
    if (record->waiting.alongside) return record->waiting.phase;
    if (!record->draft()) return 0;
    const auto blocker = nearestBlocker(team, phase);
    return blocker.phase && blocker.approved ? blocker.phase : 0;
}

Approval phaseApproval(const Team &team, int phase)
{
    int count = 0;
    bool barrier = false;
    for (const auto &group : phaseGroups(team)) if (group.n == phase) { count = int(group.tasks.size()); barrier = group.barrier; }
    // F12/N40: a draft approved while an earlier phase runs runs alongside it;
    // the owner is warned of what that costs before anything is sent.
    const int alongside = alongsidePhase(team, phase);
    Approval approval;
    approval.input = QJsonObject{{"phase", phase}};
    if (alongside) {
        // N6: a barrier phase's tasks wait for the earlier phase however it is
        // approved, so its confirmation says that instead of promising them now.
        approval.name = QStringLiteral("teamApproveAlongsideConfirm");
        approval.title = tr("Approve phase %1 now?").arg(phase);
        approval.text = barrier ? tr("Phase %1 is a barrier: its tasks start only when phase %2 is finished, so approving it "
                                     "now just locks the plan. Are you sure?").arg(phase).arg(alongside)
                                : tr("If you approve phase %1 now, the lead can send members to its tasks while phase %2 still runs, "
                                     "but can no longer edit this phase. Are you sure?").arg(phase).arg(alongside);
        approval.action = tr("Approve now");
        // N6: the confirmed alongside approval really hands the tasks out; a
        // barrier phase keeps its U25 queue because its barrier holds it anyway.
        if (!barrier) approval.input.insert("alongside", true);
    } else {
        approval.name = QStringLiteral("teamApprovePhaseConfirm");
        approval.title = tr("Approve phase?");
        approval.text = tr("Hand phase %1's %2 tasks to the members?").arg(phase).arg(count);
        approval.action = tr("Approve phase");
    }
    return approval;
}

Approval planApproval(const Team &team, const QString &mode)
{
    QStringList sessions;
    for (const auto &m : team.members) {
        if (m.lead || !m.session.isEmpty()) continue;
        const QString role = team.roleOf(m.id);
        sessions << (m.computer.isEmpty() ? tr("%1 (%2)").arg(role, m.agent) : tr("%1 (%2 on %3)").arg(role, m.agent, m.computer));
    }
    const QString starts = sessions.isEmpty() ? tr("Every member has a session already; none starts.")
                                              : sessions.size() == 1 ? tr("It starts 1 member session: %1.").arg(sessions.first())
                                              : tr("It starts %1 member sessions: %2.").arg(sessions.size()).arg(sessions.join(QStringLiteral(", ")));
    // U59: the owner's mode goes with the approval; with no choice, none (standard).
    const QString runs = mode == "free" ? tr(" It runs in free planning: one open phase that takes tasks as they come until you close it.")
                       : mode == "standard" ? tr(" It runs in standard planning.") : QString();
    Approval approval;
    if (!mode.isEmpty()) approval.input.insert("plan_mode", mode);
    if (!team.widenings.isEmpty()) {
        QStringList lines;
        for (const auto &widening : team.widenings) {
            const QString who = widening.role.isEmpty() ? widening.member : widening.role;
            lines << tr("%1: %2 → %3").arg(who, widening.from, widening.to);
        }
        approval.name = QStringLiteral("teamWideningConfirm");
        approval.title = tr("Widen permissions?");
        approval.text = tr("This plan widens the permissions of its members beyond the template:\n\n%1\n\n"
                           "Approve it only if they need it.").arg(lines.join(QStringLiteral("\n"))) + QStringLiteral("\n\n") + starts + runs;
        approval.action = tr("Confirm and approve");
        approval.input.insert("confirm_widening", true);
        return approval;
    }
    approval.name = QStringLiteral("teamApproveConfirm");
    approval.title = tr("Approve plan and run?");
    approval.text = tr("Approve the plan of %1 and run it? %2 Ready tasks are handed out at once.").arg(team.name, starts) + runs;
    approval.action = tr("Approve and run");
    return approval;
}

QString acceptBlocked(const Team &team, int phase)
{
    const auto *record = team.phase(phase);
    if (!record || !record->draft()) return tr("Phase %1 is not a draft").arg(phase);
    const auto blocker = nearestBlocker(team, phase);
    if (blocker.phase && !blocker.approved) return tr("Phase %1 opens after phase %2 is approved").arg(phase).arg(blocker.phase);
    return {};
}

// The same reason, short enough for the compact draft row; the full sentence
// stays in the row's tooltip.
QString approveBlockedShort(const Team &team, int phase)
{
    const auto *record = team.phase(phase);
    if (!record || !record->draft()) return {};
    const auto blocker = nearestBlocker(team, phase);
    if (!blocker.phase || blocker.approved) return {};
    return tr("after phase %1").arg(blocker.phase);
}

QMap<int, int> phaseLayers(const Team &team, const QList<int> &tasks)
{
    QSet<int> ids;
    for (int id : tasks) ids.insert(id);
    QMap<int, int> depth;
    QSet<int> active;
    std::function<int(int)> visit = [&](int id) -> int {
        if (depth.contains(id)) return depth[id];
        if (active.contains(id)) return -1;
        active.insert(id);
        int level = 0;
        if (const auto *task = team.task(id))
            for (const auto &dep : task->deps)
                if (ids.contains(dep.task)) { const int d = visit(dep.task); if (d >= 0) level = qMax(level, d + 1); }
        active.remove(id);
        depth[id] = level;
        return level;
    };
    for (int id : tasks) visit(id);
    return depth;
}

QList<QPair<int, QList<int>>> laneGroups(const Team &team, const QList<int> &ids)
{
    QMap<int, QList<int>> byLane;
    QList<int> none;
    for (int id : ids) {
        const auto *task = team.task(id);
        if (task && task->lane.known()) byLane[task->lane.n].append(id);
        else none.append(id);
    }
    QList<QPair<int, QList<int>>> groups;
    for (auto it = byLane.cbegin(); it != byLane.cend(); ++it) groups.append({it.key(), it.value()});
    if (!none.isEmpty()) groups.append({0, none});
    return groups;
}

QString laneLabel(const Team &team, const QList<int> &ids, int lane)
{
    if (lane <= 0) return {};
    for (int id : ids) {
        const auto *task = team.task(id);
        if (task && task->lane.n == lane && !task->lane.label.isEmpty()) return task->lane.label;
    }
    return {};
}

FeedLine feedLine(const Team &team, const QJsonObject &event)
{
    FeedLine line;
    line.at = event.value("at").toDouble(); line.seq = event.value("seq").toInteger(); line.kind = str(event, "kind");
    const auto d = event.value("detail").toObject();
    line.by = d.value("by").toString(d.value("member").toString());
    const int id = d.value("task").toInt();
    const auto *task = team.task(id);
    const QString title = d.value("title").toString(task ? task->title : QString());
    const QString ref = id ? (title.isEmpty() ? QStringLiteral("#%1").arg(id) : QStringLiteral("#%1 %2").arg(id).arg(title)) : QString();
    const QString attempt = d.value("attempt").isDouble() ? tr(" · attempt %1").arg(d.value("attempt").toInt()) : QString();
    const QString k = line.kind;
    auto &t = line.text;
    if (k == "created") t = tr("Created the team");
    else if (k == "planned") t = d.value("errors").toInt() ? tr("Changed the plan · %1 errors").arg(d.value("errors").toInt()) : tr("Changed the plan · %1 tasks").arg(d.value("tasks").toInt());
    else if (k == "approved") t = tr("Approved the plan");
    else if (k == "discarded") t = tr("Discarded the plan");
    else if (k == "paused") t = tr("Paused the team");
    else if (k == "resumed") t = tr("Resumed the team");
    else if (k == "stopped") { t = tr("Stopped the team"); line.tone = Tone::Danger; }
    else if (k == "archived") t = tr("Archived the team");
    else if (k == "done") { t = tr("Done · %1 of %2 tasks accepted").arg(d.value("accepted").toInt()).arg(d.value("tasks").toInt()); line.tone = Tone::Done; }
    else if (k == "phase_added") {
        const int n = d.value("phase").toInt();
        const QString title = d.value("title").toString();
        t = title.isEmpty() ? tr("Added phase %1 · %2 tasks").arg(n).arg(d.value("tasks").toInt())
                            : tr("Added phase %1 · %2 · %3 tasks").arg(n).arg(title).arg(d.value("tasks").toInt());
    }
    else if (k == "phase_approved") t = tr("Approved phase %1 · %2 tasks").arg(d.value("phase").toInt()).arg(d.value("tasks").toInt());
    // U29: a task joins a running phase only after the warning is confirmed.
    else if (k == "phase_addition_warned") {
        const int count = d.value("tasks").toArray().size();
        const int phase = d.value("phase").toInt();
        t = count == 1 ? tr("Asked to add 1 task to running phase %1 · waits for your confirmation").arg(phase)
                       : tr("Asked to add %1 tasks to running phase %2 · waits for your confirmation").arg(count).arg(phase);
        line.tone = Tone::Warn;
    }
    else if (k == "phase_addition_declined") {
        const int count = d.value("tasks").toArray().size();
        const int phase = d.value("phase").toInt();
        t = count == 1 ? tr("Declined adding 1 task to running phase %1").arg(phase)
                       : tr("Declined adding %1 tasks to running phase %2").arg(count).arg(phase);
    }
    else if (k == "phase_tasks_added") {
        QStringList ids; for (const auto &id : d.value("tasks").toArray()) ids << QStringLiteral("#%1").arg(id.toInt());
        t = tr("Added %1 to running phase %2 · outside its original scope").arg(ids.join(QStringLiteral(", "))).arg(d.value("phase").toInt());
        line.tone = Tone::Warn;
    }
    else if (k == "phase_moved") {
        // F10: the drafts are numbered live; a move renumbers them, and the
        // feed says where the phase went.
        // hgs names the moved phase "phase"; "from" is the older test shape.
        const int moved = d.contains("phase") ? d.value("phase").toInt() : d.value("from").toInt();
        t = d.value("renumbered").toBool() ? tr("Moved phase %1 to position %2 · drafts renumbered").arg(moved).arg(d.value("to").toInt())
                                           : tr("Moved phase %1 to position %2").arg(moved).arg(d.value("to").toInt());
    }
    else if (k == "phase_discarded") {
        // A discarded draft lists its tasks' titles; older events counted them.
        const QJsonValue tasks = d.value("tasks");
        t = tr("Discarded phase %1 · %2 tasks").arg(d.value("phase").toInt()).arg(tasks.isArray() ? tasks.toArray().size() : tasks.toInt());
        line.tone = Tone::Muted;
    }
    else if (k == "phase_renumbered") {
        // A discard or move closes the gap: the drafts after it take new numbers.
        QStringList moves;
        for (const auto &value : d.value("phases").toArray())
            moves << QStringLiteral("%1→%2").arg(value.toObject().value("from").toInt()).arg(value.toObject().value("to").toInt());
        t = moves.isEmpty() ? tr("Drafts renumbered") : tr("Drafts renumbered: phases %1").arg(moves.join(QStringLiteral(", ")));
        line.tone = Tone::Muted;
    }
    else if (k == "all_decided") { t = tr("All tasks decided · waiting for the lead"); line.tone = Tone::Active; }
    else if (k == "finished") { t = tr("Finished · %1 of %2 tasks accepted").arg(d.value("accepted").toInt()).arg(d.value("tasks").toInt()); line.tone = Tone::Done; }
    else if (k == "lead_told" && d.value("kind").toString() == "all_decided") t = tr("Told the Lead that all tasks are decided");
    // Spec 5.6: a sub-lead hands out its reports' tasks ("QA lead: Task to Tester").
    else if (k == "handed_out") {
        const QString by = d.value("by").toString();
        line.by = by.isEmpty() || by == "Lead" || by == "Zerus" ? tr("Lead") : by;
        t = tr("Task to %1 · %2%3").arg(d.value("to").toString(), ref, attempt);
    }
    else if (k == "delivered") t = tr("Delivered %1%2").arg(ref, attempt);
    else if (k == "started") t = tr("Started %1%2").arg(ref, attempt);
    else if (k == "start_late") { t = tr("Delivered and not started yet · %1%2").arg(ref, attempt); line.tone = Tone::Warn; }
    else if (k == "refused") { t = tr("Not delivered %1%2 (%3); handed out again later").arg(ref, attempt, d.value("error").toString()); line.tone = Tone::Warn; }
    else if (k == "unconfirmed") { t = tr("Delivery unconfirmed · %1%2 (%3)").arg(ref, attempt, d.value("error").toString()); line.tone = Tone::Danger; }
    else if (k == "reported") {
        const QString to = d.value("to").toString();
        t = tr("Report to %1 · %2%3").arg(to.isEmpty() ? tr("Lead") : to, ref, attempt);
    }
    else if (k == "report_replaced") t = tr("Replaced the captured reply · %1%2").arg(ref, attempt);
    else if (k == "question_asked") {
        const QString text = d.value("text").toString();
        t = text.isEmpty() ? tr("A question for the Lead · %1%2").arg(ref, attempt)
                           : tr("Question from the member · %1%2: %3").arg(ref, attempt, text);
        line.tone = Tone::Warn;
    }
    else if (k == "question_answered") t = tr("Answered the question · %1%2").arg(ref, attempt);
    else if (k == "held") {
        const QString reason = d.value("reason").toString();
        t = reason.isEmpty() ? tr("Held %1%2").arg(ref, attempt) : tr("Held %1%2: %3").arg(ref, attempt, reason);
        line.tone = Tone::Muted;
    }
    else if (k == "released") t = tr("Released %1%2").arg(ref, attempt);
    else if (k == "member_adopted") t = tr("Adopted the existing session · %1").arg(d.value("session").toString());
    else if (k == "stale_report") { t = tr("Late report refused · %1%2").arg(ref, attempt); line.tone = Tone::Warn; }
    // U36: the pipeline's stages and the overrides, each with who and why.
    else if (k == "stage_started") {
        const QString who = d.value("who").toString();
        const int rounds = d.value("max_rounds").toInt() > 0 ? d.value("max_rounds").toInt() : 3;
        t = tr("%1 round %2 of %3 started · %4").arg(stageName(d.value("stage").toString())).arg(d.value("round").toInt()).arg(rounds).arg(ref);
        if (!who.isEmpty()) t += QStringLiteral(" · ") + who;
    }
    else if (k == "stage_passed") { t = tr("%1 passed %2 · round %3").arg(stageName(d.value("stage").toString()), ref).arg(d.value("round").toInt()); line.tone = Tone::Done; }
    else if (k == "stage_returned") {
        const int n = int(d.value("findings").toArray().size());
        t = tr("%1 returned %2 · round %3").arg(stageName(d.value("stage").toString()), ref).arg(d.value("round").toInt())
            + QStringLiteral(" · ") + (n == 1 ? tr("1 finding") : tr("%1 findings").arg(n));
        line.tone = Tone::Warn;
    }
    else if (k == "round_limit") { t = tr("Out of rounds · %1 goes to the lead after %2 returns").arg(ref).arg(d.value("rounds").toInt()); line.tone = Tone::Warn; }
    else if (k == "stages_passed") t = tr("Every stage passed · %1 waits for %2").arg(ref, d.value("to").toString(tr("Lead")));
    else if (k == "stage_skipped") {
        t = tr("Skipped %1 on %2 at round %3: “%4”").arg(stageName(d.value("stage").toString()), ref).arg(d.value("round").toInt()).arg(d.value("reason").toString());
        line.tone = Tone::Muted;
    }
    else if (k == "sent_back") {
        const QString stage = d.value("stage").toString();
        t = stage.isEmpty() ? tr("Sent %1 back: “%2”").arg(ref, d.value("reason").toString())
                            : tr("Sent %1 back from %2: “%3”").arg(ref, stageName(stage), d.value("reason").toString());
        line.tone = Tone::Warn;
    }
    else if (k == "accepted") {
        const QString reason = d.value("reason").toString();
        t = line.by == "Pipeline" || reason.isEmpty() ? tr("Accepted %1").arg(ref) : tr("Accepted %1: “%2”").arg(ref, reason);
        line.tone = Tone::Done;
    }
    else if (k == "rejected") { t = tr("Rejected %1%2: “%3”").arg(ref, attempt, d.value("reason").toString()); line.tone = Tone::Danger; }
    else if (k == "reassigned") t = tr("Reassigned %1 to %2").arg(ref, d.value("to").toString());
    else if (k == "retried") t = tr("Retried %1").arg(ref);
    else if (k == "taken_over") t = tr("Took over %1").arg(ref);
    else if (k == "cancelled") t = tr("Cancelled %1").arg(ref);
    else if (k == "checked") t = d.value("delivered").toBool() ? tr("Confirmed delivery of %1%2").arg(ref, attempt) : tr("Marked %1%2 as not delivered").arg(ref, attempt);
    else if (k == "no_report") { t = tr("Turn ended without a report · %1%2").arg(ref, attempt); line.tone = Tone::Warn; }
    else if (k == "notice_unconfirmed") { t = tr("Report for %1 may not have reached the Lead").arg(ref); line.tone = Tone::Warn; }
    else if (k == "member_started") t = tr("Session started · %1").arg(d.value("session").toString());
    else if (k == "launch_failed") { t = tr("Session failed to start: %1").arg(d.value("error").toString()); line.tone = Tone::Danger; }
    else if (k == "relaunch") t = tr("Start %1 again").arg(d.value("member").toString());
    else if (k == "member_paused") t = tr("Paused %1").arg(d.value("member").toString());
    else if (k == "member_resumed") t = tr("Resumed %1").arg(d.value("member").toString());
    else if (k == "member_added") {
        // U32: the roster says how the member joined, with the owner's quote.
        const QString by = d.value("by").toString();
        const QString word = d.value("owner_word").toString();
        t = word.isEmpty() ? tr("Added %1 by %2").arg(d.value("member").toString(), by)
                           : tr("Added %1 by %2: “%3”").arg(d.value("member").toString(), by, word);
        line.by = by;
    }
    else if (k == "max_members_raised") t = tr("Raised the member cap %1 → %2 on your word").arg(d.value("old").toInt()).arg(d.value("new").toInt());
    else if (k == "member_removed") t = tr("Removed %1 from the team").arg(d.value("member").toString());
    else t = k;
    return line;
}


Role parseRole(const QJsonObject &v)
{
    Role r;
    r.id = str(v, "id"); r.name = str(v, "name"); r.color = str(v, "color"); r.prompt = str(v, "prompt");
    r.agent = str(v, "agent"); r.account = str(v, "account"); r.model = str(v, "model"); r.effort = str(v, "effort");
    r.permissions = str(v, "permissions"); r.computer = str(v, "computer"); r.folder = str(v, "folder");
    r.permissionsProfile = str(v, "permissions_profile");   // B5: read, write, build, commit, network
    r.canMessage = str(v, "can_message"); r.dangerous = v.value("dangerous").toBool();
    for (const auto &tag : v.value("tags").toArray()) r.tags.append(tag.toString());
    // U25: skills are the specialization of a newer catalog.
    if (r.tags.isEmpty()) for (const auto &tag : v.value("skills").toArray()) r.tags.append(tag.toString());
    r.about = str(v, "about"); r.icon = str(v, "icon"); r.message = str(v, "message"); r.joins = str(v, "joins");
    r.protocolExtra = str(v, "protocol_extra"); r.taskTemplate = str(v, "task_template");
    const auto list = [&v](const char *key) { QStringList out; for (const auto &x : v.value(QLatin1String(key)).toArray()) out.append(x.toString()); return out; };
    r.machines = list("machines"); r.messageTo = list("message_to"); r.promptOrder = list("prompt_order"); r.promptOff = list("prompt_off");
    r.danger = v.value("danger").toObject();
    r.models = parseModelChoices(v.value("models"));
    r.contextBeforeTask = str(v, "context_before_task");
    r.pipeline = str(v, "pipeline");
    const auto notes = v.value("notes").toObject();
    for (auto it = notes.begin(); it != notes.end(); ++it) r.notes.insert(it.key(), it.value().toString());
    r.raw = v;
    return r;
}

namespace {
// The known fields over the item's own object.
QJsonObject over(QJsonObject raw, const QJsonObject &known)
{
    for (auto it = known.begin(); it != known.end(); ++it) raw.insert(it.key(), it.value());
    return raw;
}
}

QJsonObject Role::json() const
{
    auto o = over(raw, {{"id", id}, {"name", name}, {"color", color}, {"prompt", prompt}, {"agent", agent}, {"account", account},
            {"model", model}, {"effort", effort}, {"permissions", permissions}, {"computer", computer}, {"folder", folder},
            {"can_message", canMessage}, {"dangerous", dangerous}, {"tags", QJsonArray::fromStringList(tags)}});
    if (!permissionsProfile.isEmpty()) o.insert("permissions_profile", permissionsProfile);
    else o.remove("permissions_profile");
    // U25: a field is written when it has a value or came with the role, so
    // an older catalog gains no empty fields.
    const auto put = [&o, this](const char *key, const QJsonValue &value, bool empty) {
        if (!empty || raw.contains(QLatin1String(key))) o.insert(QLatin1String(key), value);
    };
    put("skills", QJsonArray::fromStringList(tags), tags.isEmpty());
    put("about", about, about.isEmpty()); put("icon", icon, icon.isEmpty()); put("message", message, message.isEmpty());
    put("joins", joins, joins.isEmpty()); put("protocol_extra", protocolExtra, protocolExtra.isEmpty());
    put("task_template", taskTemplate, taskTemplate.isEmpty());
    put("machines", QJsonArray::fromStringList(machines), machines.isEmpty());
    put("message_to", QJsonArray::fromStringList(messageTo), messageTo.isEmpty());
    put("prompt_order", QJsonArray::fromStringList(promptOrder), promptOrder.isEmpty());
    put("prompt_off", QJsonArray::fromStringList(promptOff), promptOff.isEmpty());
    put("danger", danger, danger.isEmpty());
    QJsonObject noteValues; for (auto it = notes.cbegin(); it != notes.cend(); ++it) noteValues.insert(it.key(), it.value());
    put("notes", noteValues, notes.isEmpty());
    // U45/U46: written only when set or when the role came with them.
    put("models", modelChoicesJson(models), models.isEmpty());
    put("context_before_task", contextBeforeTask, contextBeforeTask.isEmpty());
    // U36: the server skips an empty pipeline; an older catalog gains none.
    if (pipeline.isEmpty()) o.remove("pipeline"); else o.insert("pipeline", pipeline);
    return o;
}

QList<ModelChoice> parseModelChoices(const QJsonValue &value)
{
    QList<ModelChoice> choices;
    for (const auto &item : value.toArray()) {
        const auto o = item.toObject();
        if (!str(o, "model").isEmpty()) choices.append({str(o, "model"), str(o, "effort")});
    }
    return choices;
}

QJsonArray modelChoicesJson(const QList<ModelChoice> &choices)
{
    QJsonArray out;
    for (const auto &choice : choices) {
        QJsonObject o{{"model", choice.model}};
        if (!choice.effort.isEmpty()) o.insert("effort", choice.effort);
        out.append(o);
    }
    return out;
}

Template parseTemplate(const QJsonObject &v)
{
    Template t;
    t.id = str(v, "id"); t.name = str(v, "name"); t.description = str(v, "description"); t.leadProtocol = str(v, "lead_protocol");
    t.planning = str(v, "planning"); t.approval = str(v, "approval"); t.phaseApproval = str(v, "phase_approval");
    t.maxMembers = v.value("max_members").toInt(6);
    t.work = str(v, "work"); t.phases = str(v, "phases"); t.pipeline = str(v, "pipeline");
    for (const auto &seat : v.value("roster").toArray()) {
        const auto o = seat.toObject();
        t.roster.append({str(o, "role"), o.value("count").toInt(1), o.value("optional").toBool(), o});
    }
    for (const auto &task : v.value("tasks").toArray()) {
        const auto o = task.toObject();
        SkeletonTask k{str(o, "title"), str(o, "role"), {}, o, qMax(1, o.value("phase").toInt(1))};
        for (const auto &d : o.value("deps").toArray()) k.deps.append(d.toInt());
        t.tasks.append(k);
    }
    t.raw = v;
    return t;
}

QJsonObject Template::json() const
{
    QJsonArray seats, skeleton;
    for (const auto &s : roster) seats.append(over(s.raw, {{"role", s.role}, {"count", s.count}, {"optional", s.optional}}));
    for (const auto &k : tasks) {
        QJsonArray deps; for (int d : k.deps) deps.append(d);
        skeleton.append(over(k.raw, {{"title", k.title}, {"role", k.role}, {"deps", deps}, {"phase", k.phase}}));
    }
    auto o = over(raw, {{"id", id}, {"name", name}, {"description", description}, {"lead_protocol", leadProtocol}, {"roster", seats},
            {"tasks", skeleton}, {"planning", planning}, {"approval", approval}, {"phase_approval", phaseApproval},
            {"max_members", maxMembers}});
    if (!work.isEmpty() || raw.contains("work")) o.insert("work", work);
    if (!phases.isEmpty() || raw.contains("phases")) o.insert("phases", phases);
    if (pipeline.isEmpty()) o.remove("pipeline"); else o.insert("pipeline", pipeline);
    return o;
}

PipelineStage parsePipelineStage(const QJsonObject &v)
{
    PipelineStage s;
    s.kind = str(v, "kind"); s.role = str(v, "role"); s.member = str(v, "member");
    // U47: {command, expect_exit (0), timeout_minutes (30)}, as model.rs Check reads it.
    if (const auto command = v.value("command").toObject(); v.value("command").isObject()) {
        s.hasCommand = true;
        s.command = str(command, "command");
        s.expectExit = command.value("expect_exit").toInt(0);
        s.timeoutMinutes = command.value("timeout_minutes").toInt(30);
    }
    const auto exits = v.value("exits").toObject();
    for (auto it = exits.begin(); it != exits.end(); ++it) s.exits.insert(it.key(), it.value().toString());
    s.raw = v;
    return s;
}

// A review or test stage names a role or a member and nothing to run; a
// command or land stage only its command (catalog.rs validate).
QJsonObject PipelineStage::json() const
{
    auto o = over(raw, {{"kind", kind}});
    for (const auto &key : {"role", "member", "command", "exits"}) o.remove(QLatin1String(key));
    if (runByZerus()) {
        auto check = raw.value("command").toObject();
        check.insert("command", command); check.insert("expect_exit", expectExit); check.insert("timeout_minutes", timeoutMinutes);
        o.insert("command", check);
        if (kind == QLatin1String("land") && !exits.isEmpty()) {
            QJsonObject codes; for (auto it = exits.cbegin(); it != exits.cend(); ++it) codes.insert(it.key(), it.value());
            o.insert("exits", codes);
        }
    } else if (!member.isEmpty()) o.insert("member", member);
    else o.insert("role", role);
    return o;
}

Pipeline parsePipeline(const QJsonObject &v)
{
    Pipeline p;
    p.id = str(v, "id"); p.name = str(v, "name");
    for (const auto &stage : v.value("stages").toArray()) p.stages.append(parsePipelineStage(stage.toObject()));
    p.maxRounds = v.value("max_rounds").toInt(0);
    p.folder = str(v, "folder"); p.accept = str(v, "accept");
    p.raw = v;
    return p;
}

// Skipped when empty, as the server writes a preset.
QJsonObject Pipeline::json() const
{
    QJsonArray list; for (const auto &stage : stages) list.append(stage.json());
    auto o = over(raw, {{"id", id}, {"name", name}, {"stages", list}});
    const auto put = [&o](const char *key, const QJsonValue &value, bool empty) { if (empty) o.remove(QLatin1String(key)); else o.insert(QLatin1String(key), value); };
    put("max_rounds", maxRounds, maxRounds <= 0);
    put("folder", folder, folder.isEmpty()); put("accept", accept, accept.isEmpty());
    return o;
}

// U25: `use` is a template id or "+role" and comes before `template`; the
// editor shows a template rule's template either way.
Rule parseRule(const QJsonObject &v)
{
    const auto use = str(v, "use");
    return {str(v, "id"), str(v, "when"), !use.isEmpty() && !use.startsWith('+') ? use : str(v, "template"), str(v, "approval"), v, use};
}

// A "+role" rule keeps its use (validate reads use before template); a
// template rule that came with use takes the edited template there too.
QJsonObject Rule::json() const
{
    const bool adds = use.startsWith('+');
    auto o = over(raw, {{"id", id}, {"when", when}, {"template", adds ? str(raw, "template") : templateId}, {"approval", approval}});
    if (adds) o.insert("use", use);
    else if (!use.isEmpty() || raw.contains("use")) o.insert("use", templateId);
    return o;
}

const Role *Catalog::role(const QString &id) const
{
    for (const auto &r : roles) if (r.id == id) return &r;
    return nullptr;
}

const Role *Catalog::roleNamed(const QString &name) const
{
    for (const auto &r : roles)
        if (r.id.compare(name, Qt::CaseInsensitive) == 0 || r.name.compare(name, Qt::CaseInsensitive) == 0) return &r;
    return nullptr;
}

const Template *Catalog::templ(const QString &id) const
{
    for (const auto &t : templates) if (t.id == id) return &t;
    return nullptr;
}

const Pipeline *Catalog::pipeline(const QString &id) const
{
    for (const auto &p : pipelines) if (p.id == id) return &p;
    return nullptr;
}

QJsonObject Catalog::json() const
{
    QJsonArray r, t, u;
    for (const auto &x : roles) r.append(x.json());
    for (const auto &x : templates) t.append(x.json());
    for (const auto &x : routing) u.append(x.json());
    QJsonObject out{{"version", version}, {"roles", r}, {"templates", t}, {"routing", u}};
    // U28: a save keeps the rules; leaving them out would reset them.
    if (planningRulesKnown) out.insert("planning_rules", planningRules);
    // U36: the presets go back whole; a catalog that came without them gains
    // the field only once one is added.
    if (pipelinesKnown || !pipelines.isEmpty()) {
        QJsonArray p; for (const auto &x : pipelines) p.append(x.json());
        out.insert("pipelines", p);
    }
    return out;
}

QHash<QString, QString> &catalogRoleColors()
{
    static QHash<QString, QString> colors;
    return colors;
}

QString roleColorKey(const QString &role)
{
    QString key;
    for (const QChar c : role.toLower()) if (c.isLetterOrNumber()) key += c;
    return key;
}

void rememberRoleColors(const Catalog &catalog)
{
    auto &colors = catalogRoleColors();
    colors.clear();
    for (const auto &role : catalog.roles) {
        // A plain #hex or color name; the desktop checks it again as a QColor.
        static const QRegularExpression plain(QStringLiteral("\\A(?:#[0-9A-Fa-f]{3,8}|[A-Za-z]{3,30})\\z"));
        if (!plain.match(role.color.trimmed()).hasMatch()) continue;
        for (const auto &name : {role.id, role.name}) if (!roleColorKey(name).isEmpty()) colors.insert(roleColorKey(name), role.color.trimmed());
    }
}

Catalog parseCatalog(const QJsonObject &get)
{
    Catalog c;
    const auto catalog = get.value("catalog").toObject();
    c.version = catalog.value("version").toInteger(); c.builtin = get.value("builtin").toBool();
    for (const auto &v : catalog.value("roles").toArray()) c.roles.append(parseRole(v.toObject()));
    for (const auto &v : catalog.value("templates").toArray()) c.templates.append(parseTemplate(v.toObject()));
    for (const auto &v : catalog.value("routing").toArray()) c.routing.append(parseRule(v.toObject()));
    c.planningRulesKnown = catalog.value("planning_rules").isString();
    c.planningRules = catalog.value("planning_rules").toString();
    c.pipelinesKnown = catalog.value("pipelines").isArray();
    for (const auto &v : catalog.value("pipelines").toArray()) c.pipelines.append(parsePipeline(v.toObject()));
    const auto draft = get.value("draft").toObject();
    c.draftBase = draft.value("base").toInteger();
    for (const auto &v : draft.value("proposals").toArray()) {
        const auto o = v.toObject();
        c.proposals.append({str(o, "id"), str(o, "kind"), str(o, "action"), str(o, "target"), str(o, "title"), str(o, "reason"),
                            o.value("state").toString(QStringLiteral("proposed")), o.value("value").toObject(), o.value("at").toDouble(),
                            o.value("value").isString() ? o.value("value").toString() : QString()});
    }
    return c;
}

Catalog withProposals(const Catalog &catalog, bool includeUndecided)
{
    Catalog c = catalog;
    for (const auto &p : catalog.proposals) {
        if (p.state == "rejected" || (!includeUndecided && p.state != "kept")) continue;
        const bool remove = p.action == "remove";
        const auto apply = [&](auto &items, const auto &parsed) {
            int at = -1;
            for (int i = 0; i < items.size(); ++i) if (items[i].id == p.target) { at = i; break; }
            if (remove) { if (at >= 0) items.removeAt(at); }
            else if (at >= 0) items[at] = parsed;
            else items.append(parsed);
        };
        if (p.kind == "role") apply(c.roles, parseRole(p.value));
        else if (p.kind == "template") apply(c.templates, parseTemplate(p.value));
        else if (p.kind == "rule") apply(c.routing, parseRule(p.value));
        else if (p.kind == "pipeline") apply(c.pipelines, parsePipeline(p.value));
        else if (p.kind == "planning_rules" && !remove) { c.planningRules = p.text; c.planningRulesKnown = true; }
    }
    return c;
}

}
