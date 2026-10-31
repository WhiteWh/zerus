#include "TeamWaitingAnswer.h"
#include "NewSessionDense.h"
#include "SubagentRequest.h"
#include "TeamController.h"
#include "TeamProposalCard.h"
#include "TeamWidgets.h"
#include <QCheckBox>
#include <QJsonArray>
#include <QLineEdit>
#include <QRadioButton>
#include <QTimer>
#include <QMessageBox>
#include <QScopedValueRollback>
#include <QVBoxLayout>
#include <algorithm>

using namespace TeamUi;
using Teams::Team;

namespace {
const QString additionPrefix = QStringLiteral("/owner_word_asked/addition-");
}

bool TeamWaitingAnswer::opensCard(const QString &action)
{
    return action == QLatin1String("decide_proposal") || action == QLatin1String("confirm_addition") || action == QLatin1String("answer_permission")
           || action == QLatin1String("decide_tool");
}

bool TeamWaitingAnswer::approves(const QString &action)
{
    return action == QLatin1String("approve_phase") || action == QLatin1String("approve_plan");
}

QString TeamWaitingAnswer::additionId(const Item &item)
{
    const int at = item.id.indexOf(additionPrefix);
    return at < 0 ? QString() : item.id.mid(at + additionPrefix.size());
}

TeamWaitingAnswer::TeamWaitingAnswer(TeamController *teams, const Item &item, bool dark, QWidget *parent)
    : QWidget(parent), m_teams(teams), m_item(item), m_dark(dark)
{
    setObjectName("teamWaitingAnswer");
    setProperty("itemId", item.id);
    m_layout = new QVBoxLayout(this); m_layout->setContentsMargins(0, 6, 0, 0); m_layout->setSpacing(4);
    // The team view's buttons and fields, in the banner as on the page.
    setStyleSheet(teamSheet(colorsFor(dark)));
    if (item.action == QLatin1String("decide_proposal")) {
        m_cards = new TeamProposalCards(this);
        m_cards->setOnly(item.proposal);
        m_cards->setDark(dark);
        m_cards->current = [this]() -> const Team * { return m_team ? &*m_team : nullptr; };
        m_cards->decide = [this](const QJsonObject &input, const TeamProposalCards::Done &done) {
            QPointer<TeamWaitingAnswer> guard(this);
            // The proposal as it is now: decided, withdrawn or changed sends nothing.
            read([guard, input, done](const Team *team) {
                if (!guard) return;
                if (!team) { done(false, guard->m_notice); return; }
                const auto found = std::find_if(team->proposals.cbegin(), team->proposals.cend(),
                                                [&input](const Team::Proposal &p) { return p.id == input.value("proposal").toString(); });
                if (found == team->proposals.cend() || !found->open() || found->hash != input.value("expected_hash").toString()) {
                    done(false, tr("This proposal was decided or changed meanwhile; nothing was sent."));
                    return;
                }
                QJsonObject body = input;
                body.insert("team", guard->m_item.team);
                guard->m_sending = true; guard->render();
                guard->m_teams->act(QStringLiteral("decide-proposal"), body, [guard, done](bool ok, const QString &message) {
                    done(ok, message);
                    if (!guard) return;
                    guard->m_sending = false;
                    // #754: a refusal keeps the card, its reason and its buttons.
                    if (ok) guard->finish(); else guard->render();
                });
            });
        };
    }
    // #742: a tool's offer has no team to read; the item carries all of it.
    if (opensCard(item.action) && item.action != QLatin1String("decide_tool")) read([](const Team *) {});
    else render();
}

bool TeamWaitingAnswer::hasContent() const
{
    return m_layout->count() > 0;
}

void TeamWaitingAnswer::setDark(bool dark)
{
    if (m_dark == dark) return;
    m_dark = dark;
    setStyleSheet(teamSheet(colorsFor(dark)));
    if (m_cards) m_cards->setDark(dark);
    render();
}

void TeamWaitingAnswer::read(const Then &then)
{
    if (!m_teams) { m_notice = tr("The team was not read, so nothing was sent"); render(); then(nullptr); return; }
    const quint64 read = ++m_read;
    m_reading = true;
    render();
    QPointer<TeamWaitingAnswer> guard(this);
    m_teams->call(QStringLiteral("status"), QJsonObject{{"team", m_item.team}, {"include_cancelled", true}},
        [guard, read, then](bool ok, const QJsonObject &result, const QString &error) {
            if (!guard || read != guard->m_read) return;
            guard->m_reading = false;
            if (!ok) {
                guard->m_notice = tr("The team was not read, so nothing was sent: %1").arg(error);
                guard->render();
                then(nullptr);
                return;
            }
            guard->m_team = Teams::parse(result);
            guard->render();
            then(&*guard->m_team);
        }, false);
}

QString TeamWaitingAnswer::staleReason(const Team &team) const
{
    const QString &action = m_item.action;
    if (action == QLatin1String("decide_proposal")) {
        for (const auto &proposal : team.proposals) if (proposal.id == m_item.proposal && proposal.open()) return {};
        return tr("This proposal was decided or withdrawn meanwhile; nothing was sent.");
    }
    if (action == QLatin1String("confirm_addition")) {
        const QString id = additionId(m_item);
        for (const auto &addition : team.additions)
            if (addition.id == id && (m_shownTitles.isEmpty() || addition.titles == m_shownTitles)) return {};
        return tr("This addition was confirmed, replaced or expired meanwhile; nothing was sent.");
    }
    if (action == QLatin1String("answer_permission")) {
        const auto *member = memberOf(team);
        const auto *question = member && (member->permission.permission() || !member->permission.record.value("questions").toArray().isEmpty())
            && !member->permission.answered() ? &member->permission : nullptr;
        if (question && (m_shownQuestion.isEmpty() || (question->id == m_shownQuestion && question->hash == m_shownHash))) return {};
        return tr("The member's question was answered or changed meanwhile; nothing was sent.");
    }
    if (action == QLatin1String("approve_phase")) {
        const auto *phase = team.phase(m_item.phase);
        if (team.status != QLatin1String("planning") && phase && phase->draft()) return {};
        return tr("Phase %1 no longer waits for your approval; nothing was sent.").arg(m_item.phase);
    }
    if (action == QLatin1String("approve_plan")) {
        if (team.status == QLatin1String("planning")) return {};
        return tr("The plan no longer waits for your approval; nothing was sent.");
    }
    return {};
}

const Teams::Member *TeamWaitingAnswer::memberOf(const Team &team) const
{
    if (const auto *found = team.member(m_item.memberId.isEmpty() ? m_item.member : m_item.memberId)) return found;
    for (const auto &member : team.members)
        if (team.roleOf(member.id) == m_item.member || member.id == m_item.member) return &member;
    return nullptr;
}

QJsonArray TeamWaitingAnswer::questionAnswers(const QJsonArray &questions, bool *complete) const
{
    // The session card's answer: each question's chosen option ids and, with
    // Other, its text (QuestionCard::answers).
    QJsonArray result;
    bool all = !questions.isEmpty();
    for (const auto &value : questions) {
        const auto question = value.toObject();
        const QString id = question.value("id").toString();
        QJsonArray selected;
        for (const auto &option : question.value("options").toArray())
            if (const QString optionId = option.toObject().value("id").toString(); m_chosen.value(id).contains(optionId)) selected.append(optionId);
        const QString text = m_otherOn.contains(id) ? m_otherText.value(id).trimmed() : QString();
        if ((selected.isEmpty() && text.isEmpty()) || (m_otherOn.contains(id) && text.isEmpty())) all = false;
        result.append(QJsonObject{{"question_id", id}, {"selected_option_ids", selected}, {"text", text}});
    }
    if (complete) *complete = all;
    return result;
}

void TeamWaitingAnswer::sendQuestion()
{
    if (busy() || m_reading || m_done || !m_team) return;
    bool complete = false;
    const QJsonArray answers = questionAnswers(memberOf(*m_team)->permission.record.value("questions").toArray(), &complete);
    if (!complete) return;
    m_notice.clear();
    read([this, answers](const Team *team) {
        if (!team) return;
        if (const QString stale = staleReason(*team); !stale.isEmpty()) { m_notice = stale; render(); return; }
        // hgs team answer: the session's own question id and hash, once.
        const auto *member = memberOf(*team);
        const QJsonObject input{{"team", m_item.team}, {"member", member->id}, {"question_id", member->permission.id},
                                {"expected_question_hash", member->permission.hash}, {"answers", answers}};
        m_sending = true; render();
        QPointer<TeamWaitingAnswer> guard(this);
        m_teams->act(QStringLiteral("answer"), input, [guard](bool ok, const QString &message) {
            if (!guard) return;
            guard->m_sending = false;
            if (ok) guard->finish();
            else { guard->m_notice = message; guard->render(); }
        });
    });
}

bool TeamWaitingAnswer::ask(const Teams::Approval &approval)
{
    const QScopedValueRollback<bool> asking(m_asking, true);
    if (confirm) return confirm(approval.name, approval.title, approval.text, approval.action);
    // TeamPanel::confirm's box: the action is destructive, Cancel the default.
    QMessageBox dialog(QMessageBox::Warning, approval.title, approval.text, QMessageBox::NoButton, this);
    dialog.setObjectName(approval.name); dialog.setTextFormat(Qt::PlainText);
    auto *ok = dialog.addButton(approval.action, QMessageBox::DestructiveRole);
    auto *cancel = dialog.addButton(QMessageBox::Cancel);
    dialog.setDefaultButton(cancel); dialog.setEscapeButton(cancel);
    dialog.exec();
    return dialog.clickedButton() == ok;
}

void TeamWaitingAnswer::approve()
{
    if (!approves(m_item.action) || busy() || m_reading || m_done) return;
    m_notice.clear();
    const bool phase = m_item.action == QLatin1String("approve_phase");
    const auto approvalOf = [this, phase](const Team &team) {
        return phase ? Teams::phaseApproval(team, m_item.phase) : Teams::planApproval(team, QString());
    };
    read([this, phase, approvalOf](const Team *team) {
        if (!team) return;
        if (const QString stale = staleReason(*team); !stale.isEmpty()) { m_notice = stale; render(); return; }
        // What keeps Approve off in the team keeps it off here.
        QString blocked;
        if (phase) blocked = Teams::approveBlocked(*team, m_item.phase);
        else if (team->tasks.isEmpty()) blocked = tr("The plan has no tasks yet");
        else if (!team->errors.isEmpty()) blocked = tr("Fix %n plan error(s) to run", nullptr, int(team->errors.size()));
        if (!blocked.isEmpty()) { m_notice = blocked; render(); return; }
        const auto approval = approvalOf(*team);
        QPointer<TeamWaitingAnswer> guard(this);
        const bool yes = ask(approval);
        if (!guard) return;
        if (!yes) { render(); return; }
        // The team may have changed while the question was open: read it again.
        read([this, phase, approval, approvalOf](const Team *fresh) {
            if (!fresh) return;
            if (const QString stale = staleReason(*fresh); !stale.isEmpty()) { m_notice = stale; render(); return; }
            const auto now = approvalOf(*fresh);
            if (now.text != approval.text || now.input != approval.input) {
                m_notice = tr("It changed while you answered; nothing was sent. Approve again.");
                render();
                return;
            }
            QJsonObject input = approval.input;
            input.insert("team", m_item.team);
            m_sending = true; render();
            QPointer<TeamWaitingAnswer> guard(this);
            m_teams->act(phase ? QStringLiteral("approve-phase") : QStringLiteral("approve"), input, [guard](bool ok, const QString &message) {
                if (!guard) return;
                guard->m_sending = false;
                if (ok) guard->finish();
                else { guard->m_notice = message; guard->render(); }
            });
        });
    });
}

void TeamWaitingAnswer::sendPermission(bool allow)
{
    if (busy() || m_reading || m_done) return;
    m_notice.clear();
    const QString reason = allow ? QString() : m_reason.trimmed();
    read([this, allow, reason](const Team *team) {
        if (!team) return;
        if (const QString stale = staleReason(*team); !stale.isEmpty()) { m_notice = stale; render(); return; }
        // TeamWorkspace's member card: the question id, its hash and the native answer.
        const auto *member = memberOf(*team);
        const auto &question = member->permission;
        const QJsonObject input{{"team", m_item.team}, {"member", member->id}, {"question_id", question.id},
                                {"expected_question_hash", question.hash},
                                {"answers", QJsonArray{QJsonObject{{"question_id", question.native.questionId},
                                                                   {"selected_option_ids", QJsonArray{allow ? question.native.allow : question.native.deny}},
                                                                   {"text", reason}}}}};
        m_sending = true; render();
        QPointer<TeamWaitingAnswer> guard(this);
        m_teams->act(QStringLiteral("answer"), input, [guard](bool ok, const QString &message) {
            if (!guard) return;
            guard->m_sending = false;
            if (ok) guard->finish();
            else { guard->m_notice = message; guard->render(); }
        });
    });
}

void TeamWaitingAnswer::sendAddition()
{
    if (busy() || m_reading || m_done) return;
    m_notice.clear();
    const QString typed = m_typed.trimmed();
    read([this, typed](const Team *team) {
        if (!team) return;
        if (const QString stale = staleReason(*team); !stale.isEmpty()) { m_notice = stale; render(); return; }
        // TeamWorkspace's add-to-phase dialog: the pending id and the typed word.
        int phase = m_item.phase;
        for (const auto &addition : team->additions) if (addition.id == additionId(m_item)) phase = addition.phase;
        m_sending = true; render();
        QPointer<TeamWaitingAnswer> guard(this);
        m_teams->call(QStringLiteral("add-to-phase"), {{"team", m_item.team}, {"phase", phase}, {"confirm", additionId(m_item)}, {"confirmation", typed}},
            [guard](bool ok, const QJsonObject &, const QString &message) {
                if (!guard) return;
                guard->m_sending = false;
                if (ok) guard->finish();
                else { guard->m_notice = message; guard->render(); }
            });
    });
}

void TeamWaitingAnswer::declineAddition()
{
    if (busy() || m_reading || m_done || !m_teams) return;
    m_notice.clear();
    // A decline of an addition already gone (confirmed, replaced or expired)
    // changes nothing and succeeds, so the item never stays stuck.
    int phase = m_item.phase;
    if (m_team) for (const auto &addition : m_team->additions) if (addition.id == additionId(m_item)) phase = addition.phase;
    m_sending = true; render();
    QPointer<TeamWaitingAnswer> guard(this);
    m_teams->call(QStringLiteral("add-to-phase"), {{"team", m_item.team}, {"phase", phase}, {"confirm", additionId(m_item)}, {"decline", true}},
        [guard](bool ok, const QJsonObject &, const QString &message) {
            if (!guard) return;
            guard->m_sending = false;
            if (ok) guard->finish();
            else { guard->m_notice = message; guard->render(); }
        });
}

void TeamWaitingAnswer::sendTool(bool allow)
{
    if (busy() || m_done || !m_teams) return;
    m_notice.clear();
    // The owner's answer: Allow installs with the command shown; Deny is
    // remembered for this project on this computer.
    QJsonObject input{{"offer", m_item.offer}, {"allow", allow}};
    if (!allow && !m_reason.trimmed().isEmpty()) input.insert("reason", m_reason.trimmed());
    m_sending = true; render();
    QPointer<TeamWaitingAnswer> guard(this);
    m_teams->act(QStringLiteral("decide-tool"), input, [guard, allow](bool ok, const QString &message) {
        if (!guard) return;
        guard->m_sending = false;
        if (!ok) { guard->m_notice = message; guard->render(); return; }
        guard->finish();
        if (guard && allow) { guard->m_notice = tr("Installing; its output shows here if it fails"); guard->render(); }
    });
}

void TeamWaitingAnswer::finish()
{
    m_done = true;
    m_notice = tr("Answered");
    render();
    if (answered) answered(m_item);
}

// #742: the offer of a missing tool: why, what it saves, the exact command
// and, after a failed install, its output; Deny with a reason, or Allow.
void TeamWaitingAnswer::renderTool(const Colors &c, bool waiting)
{
    auto *box = new QFrame; box->setObjectName("teamWaitingTool");
    box->setProperty("offer", m_item.offer);
    box->setStyleSheet(QStringLiteral("QFrame#teamWaitingTool{background:%1;border:1px solid %2;border-radius:7px;}")
                           .arg(alpha(c.warn, .10).name(QColor::HexArgb), alpha(c.warn, .40).name(QColor::HexArgb)));
    auto *v = new QVBoxLayout(box); v->setContentsMargins(8, 5, 8, 6); v->setSpacing(3);
    const bool failed = m_item.toolState == QLatin1String("failed");
    auto *head = label(failed ? tr("Installing %1 failed").arg(m_item.tool) : tr("Install %1 on this computer?").arg(m_item.tool),
                       c.fg, 12, true, "teamWaitingToolHead");
    head->setWordWrap(true);
    v->addWidget(head);
    if (!m_item.why.isEmpty()) {
        auto *why = label(tr("Needed for %1").arg(m_item.why), c.text2, 11, false, "teamWaitingToolWhy");
        why->setWordWrap(true); v->addWidget(why);
    }
    if (!m_item.saves.isEmpty()) {
        auto *saves = label(tr("Saves %1").arg(m_item.saves), c.text2, 11, false, "teamWaitingToolSaves");
        saves->setWordWrap(true); v->addWidget(saves);
    }
    const bool installable = !m_item.command.isEmpty();
    auto *command = label(installable ? m_item.command : tr("No package manager on this computer installs it; install it yourself or deny."),
                          installable ? c.fg : c.warn, 11, false, "teamWaitingToolCommand");
    if (installable) command->setFont(NewSessionDense::mono(11));
    command->setWordWrap(true);
    command->setTextInteractionFlags(Qt::TextSelectableByMouse);
    v->addWidget(command);
    if (failed && !m_item.output.isEmpty()) {
        auto *output = label(m_item.output.right(1200), c.text3, 10, false, "teamWaitingToolOutput");
        output->setFont(NewSessionDense::mono(10));
        output->setWordWrap(true);
        output->setTextInteractionFlags(Qt::TextSelectableByMouse);
        v->addWidget(output);
    }
    auto *answers = row(6);
    auto *reasonEdit = new QLineEdit(m_reason); reasonEdit->setObjectName("teamWaitingToolDenyReason");
    reasonEdit->setPlaceholderText(tr("Why deny? Remembered for this project here"));
    QObject::connect(reasonEdit, &QLineEdit::textChanged, this, [this](const QString &text) { m_reason = text; });
    auto *deny = button(tr("Deny"), "teamWaitingToolDeny"); deny->setProperty("danger", true);
    deny->setToolTip(tr("Zerus does not offer it again for this project on this computer"));
    auto *allow = button(failed ? tr("Retry install") : tr("Allow install"), "teamWaitingToolAllow", true);
    allow->setToolTip(tr("Runs the command shown as your operation and records its output"));
    allow->setEnabled(installable && !waiting); deny->setEnabled(!waiting); reasonEdit->setEnabled(!waiting);
    QObject::connect(deny, &QPushButton::clicked, this, [this] { sendTool(false); });
    QObject::connect(allow, &QPushButton::clicked, this, [this] { sendTool(true); });
    answers->addWidget(reasonEdit, 1); answers->addWidget(deny); answers->addWidget(allow);
    v->addLayout(answers);
    m_layout->addWidget(box);
}

void TeamWaitingAnswer::render()
{
    const Colors c = colorsFor(m_dark);
    if (m_cards) { m_layout->removeWidget(m_cards); m_cards->hide(); }
    while (auto *child = m_layout->takeAt(0)) {
        if (auto *widget = child->widget()) retire(widget);
        delete child;
    }
    const bool waiting = m_reading || m_sending;
    if (m_reading && !m_team) m_layout->addWidget(label(tr("Reading the team"), c.muted, 11, false, "teamWaitingAnswerReading"));
    if (m_sending) m_layout->addWidget(label(tr("Sending"), c.muted, 11, false, "teamWaitingAnswerSending"));
    const QString stale = m_team && !m_done ? staleReason(*m_team) : QString();
    const QString notice = !m_notice.isEmpty() ? m_notice : stale;
    if (!notice.isEmpty()) {
        auto *text = label(notice, m_done ? c.accent : c.warn, 11, false, "teamWaitingAnswerNotice");
        text->setWordWrap(true);
        m_layout->addWidget(text);
    }
    // A card of a decided proposal never shows again (its own late render).
    if (m_cards && (m_done || !stale.isEmpty())) m_cards->setTeam(nullptr);
    if (m_item.action == QLatin1String("decide_tool")) {
        if (!m_done) renderTool(c, waiting);
        if (parentWidget()) setVisible(hasContent());
        return;
    }
    if (m_done || !m_team || !stale.isEmpty() || !opensCard(m_item.action)) {
        // Parentless (between the row rebuilds) it never shows as a window.
    if (parentWidget()) setVisible(hasContent());
        return;
    }
    const Team &team = *m_team;
    const QString &action = m_item.action;
    if (action == QLatin1String("decide_proposal")) {
        m_cards->setTeam(&team);
        m_layout->addWidget(m_cards);
        m_cards->setEnabled(!waiting);
        m_cards->show();
    } else if (action == QLatin1String("confirm_addition")) {
        const Team::Addition *addition = nullptr;
        for (const auto &found : team.additions) if (found.id == additionId(m_item)) addition = &found;
        if (m_shownTitles.isEmpty()) m_shownTitles = addition->titles;
        auto *box = new QFrame; box->setObjectName("teamWaitingAddition");
        box->setStyleSheet(QStringLiteral("QFrame#teamWaitingAddition{background:%1;border:1px solid %2;border-radius:7px;}")
                               .arg(alpha(c.warn, .10).name(QColor::HexArgb), alpha(c.warn, .40).name(QColor::HexArgb)));
        auto *v = new QVBoxLayout(box); v->setContentsMargins(8, 5, 8, 6); v->setSpacing(3);
        auto *head = label(addition->titles.size() == 1 ? tr("Phase %1 is running: this task joins it now").arg(addition->phase)
                                                        : tr("Phase %1 is running: these %2 tasks join it now").arg(addition->phase).arg(addition->titles.size()),
                           c.fg, 12, true, "teamWaitingAdditionHead");
        head->setWordWrap(true);
        v->addWidget(head);
        for (const auto &title : addition->titles) {
            auto *line = label(title, c.text2, 11, false, "teamWaitingAdditionTask");
            line->setWordWrap(true); line->setContentsMargins(6, 0, 0, 0);
            v->addWidget(line);
        }
        if (!addition->request.isEmpty()) {
            auto *asked = label(tr("Asked: %1").arg(addition->request), c.text3, 10, false, "teamWaitingAdditionRequest");
            asked->setWordWrap(true);
            v->addWidget(asked);
        }
        auto *answers = row(6);
        auto *typed = new QLineEdit(m_typed); typed->setObjectName("teamWaitingAdditionConfirm");
        typed->setPlaceholderText(tr("Type add or %1 to confirm").arg(addition->phase));
        typed->setEnabled(!waiting);
        auto *cancel = button(tr("Cancel"), "teamWaitingAdditionCancel"); cancel->setProperty("ghost", true);
        cancel->setToolTip(tr("Decline the addition: these tasks do not join and it leaves the list"));
        cancel->setEnabled(!waiting);
        auto *add = button(tr("Add"), "teamWaitingAdditionAdd", true);
        const int phase = addition->phase;
        const auto fits = [phase](const QString &text) {
            const QString word = text.trimmed().toLower();
            return word == QLatin1String("add") || word == QString::number(phase);
        };
        add->setEnabled(!waiting && fits(m_typed));
        QObject::connect(typed, &QLineEdit::textChanged, this, [this, add, fits](const QString &text) { m_typed = text; add->setEnabled(fits(text)); });
        QObject::connect(typed, &QLineEdit::returnPressed, this, [this, fits] { if (fits(m_typed)) sendAddition(); });
        QObject::connect(add, &QPushButton::clicked, this, [this] { sendAddition(); });
        QObject::connect(cancel, &QPushButton::clicked, this, [this] { declineAddition(); });
        answers->addWidget(typed, 1); answers->addWidget(cancel); answers->addWidget(add);
        v->addLayout(answers);
        m_layout->addWidget(box);
    } else if (action == QLatin1String("answer_permission") && !memberOf(team)->permission.record.value("questions").toArray().isEmpty()) {
        // #740: a session's question: each prompt whole, its options and Other.
        const auto &asked = memberOf(team)->permission;
        if (m_shownQuestion.isEmpty()) { m_shownQuestion = asked.id; m_shownHash = asked.hash; }
        auto *box = new QFrame; box->setObjectName("teamWaitingQuestion");
        box->setProperty("question", asked.id); box->setProperty("questionHash", asked.hash);
        box->setStyleSheet(QStringLiteral("QFrame#teamWaitingQuestion{background:%1;border:1px solid %2;border-radius:7px;}")
                               .arg(alpha(c.warn, .10).name(QColor::HexArgb), alpha(c.warn, .40).name(QColor::HexArgb)));
        auto *v = new QVBoxLayout(box); v->setContentsMargins(8, 6, 8, 6); v->setSpacing(4);
        const auto questions = asked.record.value("questions").toArray();
        for (const auto &value : questions) {
            const auto question = value.toObject();
            const QString id = question.value("id").toString();
            const bool multi = question.value("multi_select").toBool();
            if (const QString header = question.value("header").toString(); !header.isEmpty()) {
                auto *head = row(6); head->addWidget(new Pill(header, c.warn)); head->addStretch(); v->addLayout(head);
            }
            auto *prompt = label(question.value("question").toString(), c.fg, 12, true, "teamWaitingQuestionPrompt");
            prompt->setWordWrap(true); prompt->setTextInteractionFlags(Qt::TextSelectableByMouse);
            v->addWidget(prompt);
            for (const auto &optionValue : question.value("options").toArray()) {
                const auto option = optionValue.toObject();
                const QString optionId = option.value("id").toString();
                QAbstractButton *pick = multi ? static_cast<QAbstractButton *>(new QCheckBox(option.value("label").toString()))
                                              : new QRadioButton(option.value("label").toString());
                pick->setObjectName("teamWaitingQuestionOption");
                pick->setProperty("questionId", id); pick->setProperty("optionId", optionId);
                pick->setChecked(m_chosen.value(id).contains(optionId));
                pick->setEnabled(!waiting);
                v->addWidget(pick);
                if (const QString description = option.value("description").toString(); !description.isEmpty()) {
                    auto *detail = label(description, c.text3, 11, false, "teamWaitingQuestionDescription");
                    detail->setWordWrap(true); detail->setContentsMargins(22, 0, 0, 2);
                    v->addWidget(detail);
                }
                QObject::connect(pick, &QAbstractButton::toggled, this, [this, id, optionId, multi](bool on) {
                    if (on && !multi) { m_chosen[id].clear(); m_otherOn.remove(id); }
                    if (on) m_chosen[id].insert(optionId); else m_chosen[id].remove(optionId);
                    QTimer::singleShot(0, this, [this] { render(); });
                });
            }
            // Other, as the session's card offers it (allow_other).
            if (question.value("allow_other").toBool(true)) {
                QAbstractButton *other = multi ? static_cast<QAbstractButton *>(new QCheckBox(tr("Other"))) : new QRadioButton(tr("Other"));
                other->setObjectName("teamWaitingQuestionOther"); other->setProperty("questionId", id);
                other->setChecked(m_otherOn.contains(id)); other->setEnabled(!waiting);
                v->addWidget(other);
                QObject::connect(other, &QAbstractButton::toggled, this, [this, id, multi](bool on) {
                    if (on && !multi) m_chosen[id].clear();
                    if (on) m_otherOn.insert(id); else m_otherOn.remove(id);
                    QTimer::singleShot(0, this, [this] { render(); });
                });
                if (m_otherOn.contains(id)) {
                    auto *text = new QLineEdit(m_otherText.value(id)); text->setObjectName("teamWaitingQuestionOtherText");
                    text->setProperty("questionId", id);
                    text->setPlaceholderText(tr("Write your answer")); text->setMaxLength(4000); text->setEnabled(!waiting);
                    QObject::connect(text, &QLineEdit::textChanged, this, [this, id](const QString &typed) {
                        m_otherText.insert(id, typed);
                        if (auto *send = findChild<QPushButton *>(QStringLiteral("teamWaitingQuestionAnswer"))) {
                            bool complete = false;
                            questionAnswers(m_team ? memberOf(*m_team)->permission.record.value("questions").toArray() : QJsonArray(), &complete);
                            send->setEnabled(complete && !m_reading && !m_sending);
                        }
                    });
                    QObject::connect(text, &QLineEdit::returnPressed, this, [this] { sendQuestion(); });
                    v->addWidget(text);
                }
            }
        }
        auto *answers = row(6);
        answers->addStretch();
        auto *send = button(tr("Answer"), "teamWaitingQuestionAnswer", true);
        bool complete = false;
        questionAnswers(questions, &complete);
        send->setEnabled(complete && !waiting);
        QObject::connect(send, &QPushButton::clicked, this, [this] { sendQuestion(); });
        answers->addWidget(send);
        v->addLayout(answers);
        m_layout->addWidget(box);
    } else if (action == QLatin1String("answer_permission")) {
        const auto &question = memberOf(team)->permission;
        if (m_shownQuestion.isEmpty()) { m_shownQuestion = question.id; m_shownHash = question.hash; }
        auto *box = new QFrame; box->setObjectName("teamWaitingPermission");
        box->setProperty("question", question.id); box->setProperty("questionHash", question.hash);
        box->setStyleSheet(QStringLiteral("QFrame#teamWaitingPermission{background:%1;border:1px solid %2;border-radius:7px;}")
                               .arg(alpha(c.warn, .10).name(QColor::HexArgb), alpha(c.warn, .40).name(QColor::HexArgb)));
        auto *v = new QVBoxLayout(box); v->setContentsMargins(8, 5, 8, 6); v->setSpacing(3);
        auto *head = row(6);
        if (!question.tool.isEmpty()) head->addWidget(new Pill(question.tool, c.warn));
        const bool subagents = question.subagents;
        if (subagents) {
            const auto calls = SubagentRequest::calls(question.record, question.reason);
            head->addWidget(label(SubagentRequest::title(SubagentRequest::count(question.record, calls)), c.fg, 12, true, "teamWaitingSubagentsTitle"), 1);
        } else {
            head->addWidget(label(question.commandSource == "process" ? tr("command from the running process")
                                  : question.commandSource == "tool_input" ? tr("command from the tool input")
                                  : tr("the agent did not show the command"), c.text3, 10), 1);
        }
        v->addLayout(head);
        if (!subagents && !question.command.isEmpty()) {
            auto *command = label(question.command, c.fg, 11, false, "teamWaitingPermissionCommand");
            command->setFont(NewSessionDense::mono(11));
            command->setWordWrap(true);
            command->setTextInteractionFlags(Qt::TextSelectableByMouse);
            v->addWidget(command);
        }
        if (!subagents && !question.reason.isEmpty()) {
            auto *reason = label(tr("Agent: %1").arg(question.reason), c.text2, 11, false, "teamWaitingPermissionReason");
            reason->setWordWrap(true);
            v->addWidget(reason);
        }
        auto *answers = row(6);
        auto *reasonEdit = new QLineEdit(m_reason); reasonEdit->setObjectName("teamWaitingPermissionDenyReason");
        reasonEdit->setPlaceholderText(tr("Why deny? The member receives this"));
        QObject::connect(reasonEdit, &QLineEdit::textChanged, this, [this](const QString &text) { m_reason = text; });
        auto *deny = button(tr("Deny"), "teamWaitingPermissionDeny"); deny->setProperty("danger", true);
        auto *allow = button(subagents ? tr("Allow all") : tr("Allow"), "teamWaitingPermissionAllow", true);
        // C4: without the question's hash and answer ids the card cannot answer.
        const bool hasHash = !question.id.isEmpty() && !question.hash.isEmpty();
        const bool answerable = hasHash && question.native.known;
        allow->setEnabled(answerable && !waiting); deny->setEnabled(answerable && !waiting); reasonEdit->setEnabled(!waiting);
        if (!answerable) {
            const auto tip = hasHash ? tr("This hgs sends no answer ids for the question; answer it in the session.")
                                     : tr("This hgs sends no question hash in status; answer it in the session.");
            allow->setToolTip(tip); deny->setToolTip(tip); reasonEdit->setToolTip(tip);
        }
        QObject::connect(deny, &QPushButton::clicked, this, [this] { sendPermission(false); });
        QObject::connect(allow, &QPushButton::clicked, this, [this] { sendPermission(true); });
        answers->addWidget(reasonEdit, 1); answers->addWidget(deny); answers->addWidget(allow);
        v->addLayout(answers);
        m_layout->addWidget(box);
    }
    if (parentWidget()) setVisible(hasContent());
}
