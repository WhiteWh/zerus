import XCTest
@testable import ZerusKit

/// Ported from Android ChildSessionsTest (zerus-sa9qn.5): the desktop's child-session contract.
final class ChildSessionsTests: XCTestCase {
    private let machine = "11111111-1111-4111-8111-111111111111"
    private let other = "99999999-9999-4999-8999-999999999999"
    private let upper = "AAAAAAAA-1111-4111-8111-111111111111"
    private func id(_ n: Int) -> String { String(format: "%08d-0000-4000-8000-000000000000", n) }
    private func link(_ parent: Int, state: String = "live", conversation: String = "", sessionId: String? = nil) -> JSONObject {
        ["schema": .int(1), "machine": .string(machine), "session_id": .string(sessionId ?? id(parent)), "name": .string("claude/project/p\(parent)"),
         "conversation_id": .string(conversation), "run_id": .string("r"), "source": .string("launch"), "state": .string(state)]
    }
    private func session(_ n: Int, _ raw: JSONObject = [:], parent: JSONObject? = nil) -> Session {
        var row = raw.putting("session_id", .string(id(n))).putting("conversation_id", .string("conversation-\(n)")).putting("state", .string("running"))
        if let parent { row["parent"] = .object(parent) }
        return Session(target: Target(computerId: "computer", session: "claude/project/s\(n)", run: "run-\(n)", conversation: "conversation-\(n)",
                                      connectionId: "workspace"),
                       title: "s\(n)", agent: "claude", status: "", project: "project", preview: "", pendingCount: 0, raw: row)
    }
    private func uuid(_ row: Session) -> String? { machine }
    private func number(_ s: Session) -> Int { Int(s.title.dropFirst()) ?? -1 }
    private func nest(_ rows: [Session], collapsed: Set<Int> = []) -> [SessionCardEntry] {
        ChildSessions.nest(rows, parentKey: ChildSessions.parentIndex(rows, machine: uuid), online: { _ in true }) { collapsed.contains(number($0)) }
    }
    private func shape(_ entries: [SessionCardEntry]) -> [String] { entries.map { String(repeating: "  ", count: $0.depth) + $0.session.title } }

    /// The supervised-attention cases the desktop core checks (client/core/tests/supervised_attention.rs).
    func testEverySharedFixtureRowHasTheDesktopQuietAttention() throws {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../../../client/core/tests/fixtures/supervised-attention.json").standardized
        let fixture = try XCTUnwrap(try JSON.parse(Data(contentsOf: url)).object)
        XCTAssertEqual(fixture.int("schema"), 1)
        let cases = fixture.objects("cases")
        XCTAssertGreaterThanOrEqual(cases.count, 10)
        for item in cases {
            XCTAssertEqual(ChildSessions.quiet(try XCTUnwrap(item.object("row"))), item.bool("quiet"), item.string("name"))
        }
    }

    func testOnlyWellFormedParentLinksCount() {
        XCTAssertNotNil(ChildSessions.parent(["parent": .object(link(1))]))
        // An older parent record without a session ID matches by conversation.
        XCTAssertNotNil(ChildSessions.parent(["parent": .object(link(1, conversation: "c", sessionId: ""))]))
        for broken in [link(1).putting("machine", .string(upper)), link(1).putting("name", .string("")), link(1, sessionId: "not-a-uuid"),
                       link(1, sessionId: ""), link(1).putting("machine", .int(7)), link(1).putting("name", .string("a\u{7}b"))] {
            XCTAssertNil(ChildSessions.parent(["parent": .object(broken)]), "\(broken)")
        }
        XCTAssertTrue(ChildSessions.supervised(["parent": .object(link(1))]))
        XCTAssertFalse(ChildSessions.supervised(["parent": .object(link(1, state: "saved"))]))
        XCTAssertEqual(ChildSessions.parent(["parent": .object(link(1))])?.shortName, "p1")
    }

    func testParentsMatchByMachineAndSessionIdThenByConversationOnlyForAnUnidentifiedParent() {
        let parent = session(1)
        let child = session(2, parent: link(1))
        let elsewhere = session(3, parent: link(1).putting("machine", .string(other)))
        let index = ChildSessions.parentIndex([parent, child, elsewhere], machine: uuid)
        XCTAssertEqual(index(child), parent.target.key)
        XCTAssertNil(index(elsewhere)); XCTAssertNil(index(parent))
        // Fallback by conversation: only to a listed parent without a session ID.
        let legacy = Session(target: Target(computerId: "computer", session: "claude/project/legacy", run: "r", conversation: "c-old", connectionId: "workspace"),
                             title: "legacy", agent: "claude", status: "", project: "project", preview: "", pendingCount: 0, raw: ["conversation_id": .string("c-old")])
        let old = session(4, parent: link(9, conversation: "c-old", sessionId: ""))
        let identified = session(5, parent: link(9, conversation: "c-old"))
        let fallback = ChildSessions.parentIndex([legacy, old, identified], machine: uuid)
        XCTAssertEqual(fallback(old), legacy.target.key)
        XCTAssertEqual(fallback(identified), legacy.target.key)
        var renamed = session(6)
        renamed.raw["conversation_id"] = .string("c-old")
        XCTAssertNil(ChildSessions.parentIndex([renamed, identified], machine: uuid)(identified))
        // A computer without a machine UUID lists nothing to match.
        XCTAssertNil(ChildSessions.parentIndex([parent, child], machine: { _ in nil })(child))
    }

    func testChildrenNestUnderTheirParentInListOrderAndCollapseHidesOnlyDescendants() {
        let rows = [session(3, parent: link(1)), session(1), session(2, parent: link(1)), session(4, parent: link(3)), session(5)]
        XCTAssertEqual(shape(nest(rows)), ["s1", "  s3", "    s4", "  s2", "s5"])
        let entries = Dictionary(uniqueKeysWithValues: nest(rows).map { ($0.session.title, $0) })
        XCTAssertEqual(entries["s3"]?.parentKey, rows[1].target.key)
        XCTAssertEqual(entries["s3"]?.lastChild, false); XCTAssertEqual(entries["s2"]?.lastChild, true)
        XCTAssertEqual(entries["s1"]?.team?.total, 3); XCTAssertEqual(entries["s3"]?.team?.total, 1)
        XCTAssertNil(entries["s5"]?.team); XCTAssertNil(entries["s4"]?.team)
        XCTAssertEqual(shape(nest(rows, collapsed: [1])), ["s1", "s5"])
        XCTAssertEqual(shape(nest(rows, collapsed: [3])), ["s1", "  s3", "  s2", "s5"])
        // Collapsing a session without children changes nothing.
        XCTAssertEqual(shape(nest(rows, collapsed: [5])), shape(nest(rows)))
    }

    func testNestingNeverHidesARowWithoutAShownParent() {
        // Parent filtered out: the child stays at top level and names it.
        let orphan = nest([session(2, parent: link(1))])
        XCTAssertEqual(orphan.count, 1)
        XCTAssertEqual(orphan[0].depth, 0); XCTAssertEqual(orphan[0].fromParent?.shortName, "p1"); XCTAssertNil(orphan[0].fromParentKey)
        // A loop starts at its first card in list order.
        XCTAssertEqual(shape(nest([session(1, parent: link(2)), session(2, parent: link(1))])), ["s1", "  s2"])
        // Beyond the depth bound a card starts at top level again.
        let chain = (1...(ChildSessions.maxDepth + 2)).map { n in n == 1 ? session(1) : session(n, parent: link(n - 1)) }
        let depths = nest(chain).map(\.depth)
        XCTAssertEqual(depths.count, chain.count)
        XCTAssertEqual(depths.max(), ChildSessions.maxDepth)
        XCTAssertEqual(depths[ChildSessions.maxDepth + 1], 0)
    }

    func testTheParentCountsWorkingQuietRequestsAndRequestsOnlyThePersonCanAnswer() {
        func quiet(_ raw: JSONObject) -> JSONObject { raw.putting("human_request", .bool(false)) }
        func row(_ phase: String, activity: String = "idle") -> JSONObject {
            ["activity": .string(activity), "phase": .string(phase), "tracked": .bool(true)]
        }
        let rows = [session(1), session(2, row("tool", activity: "busy"), parent: link(1)), session(3, quiet(row("input")), parent: link(1)),
                    session(4, quiet(row("approval")), parent: link(1)), session(5, row("input"), parent: link(1)),
                    session(6, quiet(row("error")), parent: link(1))]
        let team = try? XCTUnwrap(nest(rows).first?.team)
        XCTAssertEqual(team, SessionTeam(total: 5, working: 1, asking: 1, needsYou: 2, errors: 1))
        XCTAssertEqual(team?.summary, ["1 of 5 working", "Needs you: 2", "Asking: 1", "Errors: 1"])
        // A quiet child's badge is gray; its approval stays amber (spec 2.1, 2.2).
        XCTAssertEqual(DesktopSessionPalette.badge(rows[2], true).kind, .neutral)
        XCTAssertEqual(DesktopSessionPalette.badge(rows[3], true).kind, .attention)
        XCTAssertEqual(DesktopSessionPalette.nativeKind(rows[2], true), .attention)
        // A quiet child's error is gray too; an explicitly muted error stays red, quiet or not (zerus-nlck0).
        XCTAssertEqual(DesktopSessionPalette.nativeKind(rows[5], true), .error)
        XCTAssertEqual(DesktopSessionPalette.badge(rows[5], true).kind, .neutral)
        XCTAssertEqual(DesktopSessionPalette.badge(session(7, row("error"), parent: link(1)), true).kind, .error)
        var mutedError = session(8, quiet(row("error")), parent: link(1))
        mutedError.raw["notification_preferences"] = .object(["conversation_id": .string("conversation-8"), "muted": .bool(true), "revision": .int(2),
            "suppressed": .object(["reply_id": .null, "attention_signature": .null, "optional_question_ids": .array([]), "main_attention": .null,
                                   "child_attention": .object([:])])])
        XCTAssertTrue(SessionNotificationPreferences.muted(mutedError))
        XCTAssertEqual(DesktopSessionPalette.badge(mutedError, true).kind, .error)
        // Its request waits for the parent, so it leaves Needs attention and Active; an approval only the person can
        // answer stays in both (spec 2.1, zerus-sa9qn.18/.19).
        XCTAssertTrue(SessionFilters.needsAttention(rows[2], true))
        XCTAssertFalse(SessionFilters.matches(rows[2], .attention, true))
        XCTAssertFalse(SessionFilters.matches(rows[2], .active, true))
        XCTAssertTrue(SessionFilters.matches(rows[3], .attention, true))
        XCTAssertTrue(SessionFilters.matches(rows[3], .active, true))
        // Shown subagents join the one counter.
        let subagents = [SessionCardChild(id: "a", name: "a", status: "Working", detail: "", provider: "claude", kind: .working, group: false),
                         SessionCardChild(id: "b", name: "b", status: "Ready", detail: "", provider: "claude", kind: .neutral, group: false)]
        XCTAssertEqual(team?.withSubagents(subagents), SessionTeam(total: 7, working: 2, asking: 1, needsYou: 2, errors: 1))
    }

    func testTheListProjectionNestsByComputerUUID() {
        let rows = [session(2, parent: link(1)), session(1)]
        let machines = [Machine(connectionId: "workspace", id: "computer", name: "Arch", online: true, machineId: machine)]
        let collapsed = TeamCollapse(stored: [ChildSessions.teamId(machine, rows[1])!])
        let open = SessionListPresentation.prepare(sessions: rows, machines: machines, projects: [], selectedMachines: [], query: "", project: nil,
                                                   filter: .all, drafts: [])
        XCTAssertEqual(open.groups.first.map { shape($0.cards) }, ["s1", "  s2"])
        let closed = SessionListPresentation.prepare(sessions: rows, machines: machines, projects: [], selectedMachines: [], query: "", project: nil,
                                                     filter: .all, drafts: [], teams: collapsed)
        XCTAssertEqual(closed.groups.first.map { shape($0.cards) }, ["s1"])
        XCTAssertEqual(closed.groups.first?.sessions.count, 2)
    }

    func testCollapsedParentsAreKeptByMachineAndSessionIdWithBoundedOverrides() {
        let parent = session(1)
        let durable = "\(machine)\n\(id(1))"
        XCTAssertEqual(ChildSessions.teamId(machine, parent), durable)
        XCTAssertNil(ChildSessions.teamId(nil, parent)); XCTAssertNil(ChildSessions.teamId(upper, parent))
        var noId = session(2)
        noId.raw["session_id"] = .string("")
        XCTAssertNil(ChildSessions.teamId(machine, noId))
        XCTAssertTrue(TeamCollapse(stored: [durable]).collapsed(parent, machine: machine))
        XCTAssertFalse(TeamCollapse().collapsed(parent, machine: machine))
        // This app's choice wins by row key, also for a row without durable IDs.
        XCTAssertFalse(TeamCollapse(stored: [durable], overrides: [(parent.target.key, false)]).collapsed(parent, machine: machine))
        XCTAssertTrue(TeamCollapse(overrides: [(parent.target.key, true)]).collapsed(parent, machine: nil))
        let second = "\(other)\n\(id(2))"
        XCTAssertEqual(ChildSessions.parseTeams([durable, "garbage", "\(machine)\nnot-a-uuid", durable, second]), [durable, second])
        XCTAssertEqual(ChildSessions.withTeam([durable, second], durable, collapsed: true), [second, durable])
        XCTAssertEqual(ChildSessions.withTeam([durable, second], durable, collapsed: false), [second])
        let full = (0..<ChildSessions.maxCollapsedTeams).map { "\(machine)\n\(id($0))" }
        XCTAssertEqual(ChildSessions.withTeam(full, "\(other)\n\(id(1))", collapsed: true).count, ChildSessions.maxCollapsedTeams)
        XCTAssertEqual(ChildSessions.withTeam(full, "\(other)\n\(id(1))", collapsed: true).last, "\(other)\n\(id(1))")
        // A saved choice needs no override; a row without durable IDs keeps its own, bounded.
        XCTAssertEqual(TeamCollapse(overrides: [(parent.target.key, true)]).choose(parent.target.key, collapsed: true, saved: [durable]), TeamCollapse(stored: [durable]))
        var transient = TeamCollapse()
        for n in 0..<(TeamCollapse.maxOverrides + 5) { transient = transient.choose("row-\(n)", collapsed: n % 2 == 0, saved: nil) }
        XCTAssertEqual(transient.overrides.count, TeamCollapse.maxOverrides)
        XCTAssertEqual(transient.overrides.first, "row-5")
        transient = transient.choose("row-5", collapsed: false, saved: nil)
        XCTAssertEqual(transient.overrides.last, "row-5"); XCTAssertEqual(transient.override("row-5"), false)
        XCTAssertNil(transient.override("row-0"))
    }

    func testAccountMarksMatchTheDesktopLetterSlotAndInk() {
        // Slots computed by the desktop's labelSlot (client/src/store/accountMarks.ts).
        for (label, slot) in [("Work", 1), ("Personal", 0), ("ops@example.com", 7), ("Ünïcode Team", 3), (String(repeating: "a", count: 40), 0), ("😀 Fun", 2)] {
            XCTAssertEqual(ChildSessions.labelSlot(label), slot, label)
        }
        XCTAssertEqual(ChildSessions.markLetter("  work", provider: "claude"), "W")
        XCTAssertEqual(ChildSessions.markLetter("😀 fun", provider: "claude"), "F")
        XCTAssertEqual(ChildSessions.markLetter("-- 2nd", provider: "claude"), "2")
        XCTAssertEqual(ChildSessions.markLetter("", provider: "claude"), "C")
        XCTAssertEqual(ChildSessions.markLetter("--", provider: ""), "?")
        // Any Unicode number, as the desktop's [\p{L}\p{N}].
        for (label, letter) in [("① Work", "①"), ("½ cup", "½"), ("Ⅻ team", "Ⅻ"), ("٣ arabic", "٣"), ("ⅻ small", "Ⅻ")] {
            XCTAssertEqual(ChildSessions.markLetter(label, provider: "claude"), letter, label)
        }
        XCTAssertEqual(ChildSessions.inkFor(0xFFCF33), 0x10151B); XCTAssertEqual(ChildSessions.inkFor(0x1F2A44), 0xFFFFFF)
        XCTAssertEqual(ChildSessions.inkFor(0x5A2CA0), 0xFFFFFF); XCTAssertEqual(ChildSessions.inkFor(0x2B97FF), 0x10151B)
    }

    func testARowShowsTheMarkOfItsRecordedProfileOnItsComputerOnly() {
        func account(_ profile: String, _ provider: String, _ label: String, _ email: String) -> ReportedAccount {
            ReportedAccount(id: profile, provider: provider, label: label, native: true, installed: true, isDefault: false, status: "ok",
                            authStatus: "signed_in", identity: AccountIdentity(email: email), windows: [], creditBalance: nil, unlimited: nil,
                            balances: [], checkedAt: 10, identityCached: false, refreshing: false, refreshError: false, stale: false)
        }
        func catalog(_ computer: String, _ accounts: [ReportedAccount]) -> AccountCatalog {
            AccountCatalog(key: MachineKey(connectionId: "workspace", computerId: computer), machineName: computer, supported: true, available: true,
                           stale: false, online: true, observedAt: 10, accounts: accounts)
        }
        let groups = AccountGrouping.group([
            catalog("computer", [account("work", "claude", "Work", "w@example.com"), account("plain", "claude", "", "p@example.com"),
                                 account("ds", "dsh", "Deep", "d@example.com")]),
            catalog("laptop", [account("away", "claude", "Away", "a@example.com")]),
        ], fold: { $0.lowercased() })
        func row(_ raw: JSONObject, agent: String = "claude") -> Session {
            Session(target: Target(computerId: "computer", session: "s", run: "r", conversation: "c", connectionId: "workspace"),
                    title: "s", agent: agent, status: "", project: "", preview: "", pendingCount: 0, raw: raw)
        }
        let work = ChildSessions.accountMark(row(["account_id": .string("work"), "cmd": .string("claude")]), accounts: groups)
        XCTAssertEqual(work?.label, "Work"); XCTAssertEqual(work?.letter, "W"); XCTAssertEqual(work?.provider, "claude")
        XCTAssertEqual(work?.color, 0x8EA9F4)
        // The color the computer keeps for the account wins (zerus-sa9qn.6); an invalid one is ignored.
        let kept = ChildSessions.accountMark(row(["account_id": .string("work"), "cmd": .string("claude"),
                                                  "account_mark": .object(["color": .string("#ffcf33"), "emoji": .string("🦊")])]), accounts: groups)
        XCTAssertEqual(kept?.color, 0xFFCF33); XCTAssertEqual(kept?.ink, 0x10151B)
        for bad in ["#FFCF33", "red", "#ffcf3"] {
            XCTAssertEqual(ChildSessions.accountMark(row(["account_id": .string("work"), "cmd": .string("claude"),
                                                          "account_mark": .object(["color": .string(bad)])]), accounts: groups)?.color, 0x8EA9F4, bad)
        }
        // An unnamed account is labeled by its email.
        XCTAssertEqual(ChildSessions.accountMark(row(["account_id": .string("plain"), "cmd": .string("claude")]), accounts: groups)?.label, "p@example.com")
        // DeepSeek sessions record dsh profiles.
        XCTAssertEqual(ChildSessions.accountMark(row(["account_id": .string("ds"), "cmd": .string("deepseek")], agent: "deepseek"), accounts: groups)?.label, "Deep")
        // No guess: no binding, another computer's profile, or a profile the catalog does not list.
        XCTAssertNil(ChildSessions.accountMark(row(["cmd": .string("claude")]), accounts: groups))
        XCTAssertNil(ChildSessions.accountMark(row(["account_id": .string("away"), "cmd": .string("claude")]), accounts: groups))
        XCTAssertNil(ChildSessions.accountMark(row(["account_id": .string("missing"), "cmd": .string("claude")]), accounts: groups))
        XCTAssertNil(ChildSessions.accountMark(row(["account_id": .string("work"), "cmd": .string("codex")], agent: "codex"), accounts: groups))
    }

    // MARK: Notifications, as the desktop tracker (client/core/src/attention.rs)

    private let now = 1_000.0
    private let preferences = NotificationPreferences(generation: 0)
    private func current(_ kind: SessionAlertKind? = .input, idle: Bool = false, quiet: Bool = false) -> NotificationCurrent {
        NotificationCurrent(slot: NotificationSlot(connection: "workspace", computer: "computer", session: "review"), target: "run/conversation",
                            title: "Review requested", machine: "QA laptop", kind: kind, identity: "question-1", idle: idle, quiet: quiet)
    }
    private func event(_ id: Int64, kind: String = "attention", at: Double? = nil) -> NotificationEvent {
        NotificationEvent(id: id, computer: "computer", session: "review", kind: kind, at: at ?? now)
    }
    private func ready(_ values: [NotificationCurrent]) -> NotificationState {
        NotificationPolicy.plan(NotificationState(bootstrap: false, settingsGeneration: 0), current: values, events: [], preferences: preferences, now: now - 10).state
    }
    private func plan(_ state: NotificationState, _ value: NotificationCurrent, _ event: NotificationEvent?, _ prefs: NotificationPreferences? = nil,
                      at: Double? = nil) -> NotificationPlan {
        NotificationPolicy.plan(state, current: [value], events: event.map { [(value.slot, $0)] } ?? [], preferences: prefs ?? preferences, now: at ?? now)
    }
    private func roundtrip(_ state: NotificationState) throws -> NotificationState {
        try JSONDecoder().decode(NotificationState.self, from: JSONEncoder().encode(state)).restored()
    }

    func testASupervisedChildsRequestWaitsForItsParentAndIsAnnouncedOnceWhenOrphaned() throws {
        // A quiet request posts nothing and is not remembered, so it notifies once the parent is gone (spec 2.4).
        let quiet = plan(ready([]), current(quiet: true), event(1))
        XCTAssertTrue(quiet.desired.isEmpty); XCTAssertTrue(quiet.retained.isEmpty)
        let orphaned = plan(try roundtrip(quiet.state), current(), event(2), at: now + 1)
        XCTAssertEqual(orphaned.desired.count, 1)
        XCTAssertEqual(orphaned.desired.first?.fresh, true)
        let posted = try roundtrip(NotificationPolicy.reserve(orphaned.state, orphaned.desired[0], audible: true, elapsed: 10_000))
        XCTAssertEqual(plan(posted, current(), event(3), at: now + 2).desired.first?.fresh, false)
        // Supervision withdraws a posted card.
        let withdrawn = plan(posted, current(quiet: true), event(3), at: now + 2)
        XCTAssertTrue(withdrawn.desired.isEmpty); XCTAssertTrue(withdrawn.retained.isEmpty)
    }

    func testACompletionSeenWhileSupervisedIsNotReplayedOnceOrphaned() throws {
        var finished = preferences
        finished.finished = true
        let idle = current(nil, idle: true)
        let completed = event(2, kind: "completed", at: now + 1)
        let quiet = plan(ready([idle]), current(nil, idle: true, quiet: true), completed, finished, at: now + 1)
        XCTAssertTrue(quiet.desired.isEmpty)
        XCTAssertTrue(plan(try roundtrip(quiet.state), idle, completed, finished, at: now + 2).desired.isEmpty)
        // Without supervision the same completion announces.
        XCTAssertEqual(plan(ready([idle]), idle, completed, finished, at: now + 2).desired.first?.kind, .finished)
    }

    func testACatalogRowOfAQuietChildIsMarkedQuiet() {
        var raw = session(2, ["activity": .string("idle"), "phase": .string("input"), "human_request": .bool(false)], parent: link(1)).raw
        XCTAssertTrue(ChildSessions.quiet(raw))
        raw["human_request"] = .bool(true)
        XCTAssertFalse(ChildSessions.quiet(raw))
    }
}
