#pragma once
#include <QHash>
#include <QJsonArray>
#include <QPointer>
#include <QSet>
#include <QWidget>
#include <functional>
#include <optional>

#include "TeamModel.h"
#include "TeamWaitingPage.h"

class QLabel;
class QVBoxLayout;
namespace TeamUi { struct Colors; }
class TeamController;
class TeamProposalCards;

// WhiteWh fork, #724 (owner, 2026-10-30: "can the user answer right in the
// Waiting for you tab?"): one item of Waiting for you answered where it is
// shown, in the page's row and the banner's, with the team view's commands
// and confirmations:
// - the lead's proposal: its card (U57's TeamProposalCards), Deny with an
//   optional reason and Allow, bound to the proposal's content hash;
// - an addition to a running phase: the tasks that would join, the typed
//   "add" (or the phase number) and Add, bound to the addition's id; Cancel
//   declines it (hgs team add-to-phase with "decline"), so it leaves the
//   list and nothing joins;
// - a permission escalated to the owner: the command, Deny with an optional
//   reason and Allow, bound to the question's id and hash;
// - a phase or plan draft: Approve…, asking the team view's question
//   (Teams::phaseApproval, Teams::planApproval);
// - #742: a tool the project lacks on this computer: why, what it saves and
//   the install command; Deny with a reason, or Allow, which Zerus runs as the
//   owner's operation (hgs team decide-tool). It has no team to read.
// It reads the team's status before it shows anything and again before it
// sends: an item decided, withdrawn or changed meanwhile says so and sends
// nothing. A report to accept and a held task are read in the team first and
// keep Open team.
class TeamWaitingAnswer : public QWidget {
public:
    using Item = TeamWaitingPage::Item;
    TeamWaitingAnswer(TeamController *teams, const Item &item, bool dark, QWidget *parent = nullptr);
    // The actions answered in place: Answer opens the card; Approve… asks.
    static bool opensCard(const QString &action);
    static bool approves(const QString &action);
    static bool answersInPlace(const QString &action) { return opensCard(action) || approves(action); }
    const Item &item() const { return m_item; }
    void setDark(bool dark);
    // Something to show: a card, a notice or the reading line.
    bool hasContent() const;
    // Approve… for a phase or plan draft: read, ask, read again, send.
    void approve();
    // A confirmation or a send is under way; the widget must not go.
    bool busy() const { return m_asking || m_sending; }
    // The store took the answer: the item leaves the page and the banner.
    std::function<void(const Item &item)> answered;
    // The row closes its answer.
    std::function<void()> closed;
    // The confirmation dialogs; empty: a warning box like TeamPanel::confirm.
    std::function<bool(const QString &name, const QString &title, const QString &text, const QString &action)> confirm;
    // The addition id an item of `confirm_addition` names (its id's suffix).
    static QString additionId(const Item &item);
private:
    using Then = std::function<void(const Teams::Team *team)>;
    // Reads `team status` of the item's team; null when the read failed.
    void read(const Then &then);
    void render();
    void finish();
    bool ask(const Teams::Approval &approval);
    // Why the item no longer waits as it was shown, empty when it does.
    QString staleReason(const Teams::Team &team) const;
    void sendPermission(bool allow);
    void sendAddition();
    // Cancel of an addition: the owner declines it; nothing joins.
    void declineAddition();
    // #740: the member the item names: by id, else (an older hgs) by role.
    const Teams::Member *memberOf(const Teams::Team &team) const;
    // #740: a session's question (the lead's AskUserQuestion, a member's
    // question): its options and Other, answered as on the session's card.
    QJsonArray questionAnswers(const QJsonArray &questions, bool *complete) const;
    void sendQuestion();
    QHash<QString, QSet<QString>> m_chosen;   // by question id
    QHash<QString, QString> m_otherText;
    QSet<QString> m_otherOn;
    // #742: Allow or Deny of a missing tool's install.
    void sendTool(bool allow);
    void renderTool(const TeamUi::Colors &c, bool waiting);
    TeamController *m_teams;
    Item m_item;
    std::optional<Teams::Team> m_team;
    QString m_shownQuestion, m_shownHash;   // the permission as shown
    QStringList m_shownTitles;              // the addition as shown
    QString m_notice, m_typed, m_reason;
    QVBoxLayout *m_layout;
    QPointer<TeamProposalCards> m_cards;
    quint64 m_read = 0;
    bool m_dark = true, m_reading = false, m_asking = false, m_sending = false, m_done = false;
};
