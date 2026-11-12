import XCTest
@testable import ZerusKit

/// Shared fixtures used by both the Android and iOS test suites.
enum SharedFixture {
    static func url(_ name: String) -> URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../../android/app/src/test/resources/\(name)").standardized
    }
    static func json(_ name: String) throws -> JSON { try JSON.parse(Data(contentsOf: url(name))) }
}

func object(_ text: String) -> JSONObject { (try! JSON.parse(text)).object! }

/// Ported from Android SessionFiltersTest.
final class SessionFiltersTests: XCTestCase {
    private let machines = [Machine(connectionId: "a", id: "same", name: "Computer A", online: true),
                            Machine(connectionId: "b", id: "same", name: "Computer B", online: false)]

    private func session(_ raw: String = "{}", connection: String = "a", pending: Int? = nil, archive: String = "",
                         name: String = "codex/project/task") -> Session {
        let data = object(raw)
        return Session(target: Target(computerId: "same", session: name, run: data.string("run_id").isEmpty ? "run" : data.string("run_id"),
                                      conversation: data.string("conversation_id").isEmpty ? "conversation" : data.string("conversation_id"),
                                      connectionId: connection, archiveId: archive),
                       title: "Long title", agent: "codex", status: "MISLEADING UI STATUS", project: "Project", preview: "Preview",
                       pendingCount: pending ?? QuestionPolicies.blockingRequests(data), raw: data)
    }

    private func with(_ session: Session, _ changes: [String: JSON]) -> Session {
        var copy = session
        for (key, value) in changes { copy.raw[key] = value }
        return copy
    }

    func testFiltersFollowSharedDesktopContract() throws {
        let contract = try SharedFixture.json("session-filter-contract.json")
        XCTAssertEqual(contract["filters"]?.array?.compactMap(\.textValue), SessionFilter.allCases.map(\.id))
        let cases = contract.objects("cases")
        XCTAssertGreaterThan(cases.count, 10)
        for example in cases {
            let item = session(example.object("raw")!.compactJSON)
            let matches = SessionFilter.allCases.filter { SessionFilters.matches(item, $0, example.bool("online"), hasDraft: example.bool("draft")) }.map(\.id)
            XCTAssertEqual(matches, example.strings("matches"), example.string("name"))
            // The quiet-child and mute rules read the same classification as the desktop core (zerus-sa9qn.19).
            if example["quiet"] != nil { XCTAssertEqual(ChildSessions.quiet(item.raw), example.bool("quiet"), example.string("name") + " quiet") }
            if example["muted"] != nil { XCTAssertEqual(SessionNotificationPreferences.muted(item), example.bool("muted"), example.string("name") + " muted") }
        }
    }

    func testExplicitWaitKeepsNativePriorityAndRejectsReusedIdentity() {
        var wait = object(#"{"schema":1,"status":"ok","run_id":"run","conversation_id":"conversation","count":1,"items":[{"state":"awaiting_ack","created_at":1700000000,"producer":{"name":"claude/example/worker","host":"peer"}}]}"#)
        var row = session(#"{"activity":"idle","phase":"idle","subagent_active_count":0,"unread_reply":true}"#)
        row.raw["coordination_wait"] = .object(wait)
        XCTAssertTrue(SessionFilters.active(row, true))
        XCTAssertFalse(SessionFilters.visibleUnreadReply(row, true))
        XCTAssertTrue(SessionFilters.status(row, true).hasPrefix("Waiting for acceptance from worker on peer"))
        XCTAssertEqual(DesktopSessionPalette.badge(row, true).kind, .waiting)
        row.raw["phase"] = "approval"
        XCTAssertEqual(SessionFilters.status(row, true), "Needs approval")
        row = with(row, ["phase": "tool", "activity": "busy"])
        XCTAssertEqual(SessionFilters.status(row, true), "Using a tool")
        row = with(row, ["phase": "idle", "activity": "idle", "process_work": ["count": 1]])
        XCTAssertEqual(SessionFilters.status(row, true), "Background")
        wait["count"] = 0
        row = with(row, ["process_work": ["count": 0], "coordination_wait": .object(wait)])
        XCTAssertEqual(SessionFilters.status(row, true), "Ready")
        XCTAssertTrue(SessionFilters.visibleUnreadReply(row, true))
        wait["count"] = 1
        wait["run_id"] = "other-run"
        row.raw["coordination_wait"] = .object(wait)
        XCTAssertFalse(SessionFilters.hasCoordinationWait(row))
    }

    func testIntermediateRepliesKeepIdentityAndReappearWhenWorkFinishes() {
        for flag in ["unread_reply", "mobile_unread_reply"] {
            for work in [#"{"activity":"busy","phase":"working"}"#, #"{"activity":"idle","phase":"idle","process_work":{"count":1}}"#,
                         #"{"activity":"idle","phase":"idle","subagent_active_count":1}"#, #"{"activity":"idle","phase":"idle","subagent_count":1}"#] {
                var raw = object(work)
                raw[flag] = true
                raw["reply_id"] = "progress"
                let item = session(raw.compactJSON)
                XCTAssertFalse(SessionFilters.visibleUnreadReply(item, true))
                XCTAssertFalse(SessionFilters.needsAttention(item, true))
                XCTAssertTrue(SessionFilters.active(item, true))
                let finished = with(item, ["activity": "idle", "phase": "idle", "process_work": ["count": 0], "subagent_active_count": 0, "subagent_count": 0])
                XCTAssertTrue(SessionFilters.visibleUnreadReply(finished, true))
                XCTAssertTrue(SessionFilters.needsAttention(finished, true))
                for phase in ["input", "approval", "error"] {
                    XCTAssertTrue(SessionFilters.needsAttention(with(item, ["phase": .string(phase)]), true))
                }
                XCTAssertTrue(SessionFilters.needsAttention(with(item, ["mobile_review_later": true]), true))
            }
        }
    }

    func testLiveShellWorkIsActiveAndOutranksSchedule() {
        let item = session(#"{"activity":"idle","phase":"idle","process_work":{"count":1},"schedules":{"count":1}}"#)
        XCTAssertEqual(SessionFilters.status(item, true), "Background")
        XCTAssertTrue(SessionFilters.working(item, true))
        XCTAssertTrue(SessionFilters.active(item, true))
        XCTAssertFalse(SessionFilters.scheduled(item, true))
        XCTAssertFalse(SessionFilters.needsAction(item))
        XCTAssertFalse(SessionFilters.working(item, false))
        let finished = with(item, ["process_work": ["count": 0], "schedules": ["count": 0]])
        XCTAssertEqual(SessionFilters.status(finished, true), "Ready")
        XCTAssertFalse(SessionFilters.active(finished, true))
    }

    func testMachineChooserCountsKeepWorkspaceIdentityStateProjectAndZeroMachines() {
        let a = MachineKey(connectionId: "a", computerId: "same"), b = MachineKey(connectionId: "b", computerId: "same")
        let zero = MachineKey(connectionId: "a", computerId: "empty")
        let hosts = machines + [Machine(connectionId: "a", id: "empty", name: "Empty", online: true)]
        let project = ProjectKey(connectionId: "a", swarmId: "swarm", id: "project")
        var busy = session(#"{"activity":"busy","phase":"tool"}"#, name: "codex/project/busy"); busy.projectKey = project
        var offline = session(#"{"activity":"idle","phase":"idle"}"#, connection: "b"); offline.projectKey = ProjectKey(connectionId: "b", swarmId: "swarm", id: "project")
        var saved = session(#"{"state":"paused"}"#, name: "codex/project/saved"); saved.projectKey = project
        var archive = busy; archive.target.archiveId = "archive"
        var other = busy; other.target.session = "codex/other/busy"; other.projectKey = nil
        let sessions = [busy, offline, saved, archive, other]
        let drafts: Set<String> = [busy.target.key, offline.target.key, archive.target.key]
        XCTAssertEqual(SessionFilters.machineCounts(sessions, hosts, .all, drafts: drafts), [a: 3, b: 1, zero: 0])
        XCTAssertEqual(SessionFilters.machineCounts(sessions, hosts, .active, drafts: drafts), [a: 2, b: 1, zero: 0])
        XCTAssertEqual(SessionFilters.machineCounts(sessions, hosts, .active, drafts: drafts, project: project), [a: 1, b: 0, zero: 0])
        XCTAssertEqual(SessionFilters.machineCounts(sessions, hosts, .saved, drafts: drafts, project: project), [a: 1, b: 0, zero: 0])
        XCTAssertEqual(SessionFilters.machineCounts(sessions, hosts, .archive, drafts: drafts, project: project), [a: 1, b: 0, zero: 0])
        XCTAssertEqual(SessionFilters.machineCounts(sessions, hosts, .active, project: project), [a: 1, b: 0, zero: 0])
    }

    func testLocalDraftsJoinActiveOnceWithinScope() {
        let project = ProjectKey(connectionId: "a", swarmId: "swarm", id: "project")
        var idle = session(#"{"activity":"idle"}"#, name: "codex/project/draft"); idle.projectKey = project
        var working = session(#"{"activity":"busy","phase":"input"}"#, name: "codex/project/overlap"); working.projectKey = project
        var offline = idle; offline.target.connectionId = "b"
        var archive = idle; archive.target.archiveId = "archive"
        let items = [idle, working, offline, archive]
        let file = Attachment(id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", name: "notes.txt", mime: "text/plain", bytes: 4, sha256: String(repeating: "a", count: 64))
        let local = [Draft(target: idle.target, text: "unsent"), Draft(target: working.target, text: "unsent"),
                     Draft(target: offline.target, text: "", attachments: [file]), Draft(target: archive.target, text: "unsent")]
        let drafts = SessionFilters.draftedSessions(items, local)
        XCTAssertEqual(SessionFilters.counts(items, machines, drafts: drafts)[.active], 3)
        XCTAssertEqual(SessionFilters.counts(items, machines, drafts: drafts)[.attention], 1)
        XCTAssertTrue(SessionFilters.matches(offline, .active, false, hasDraft: drafts.contains(offline.target.key)))
        XCTAssertFalse(SessionFilters.matches(archive, .active, true, hasDraft: true))
        let scoped = SessionFilters.scoped(items, machines, [MachineKey(connectionId: "a", computerId: "same")], "draft", project: project)
        XCTAssertEqual(scoped, [idle, archive])
        XCTAssertEqual(SessionFilters.counts(scoped, machines, drafts: drafts)[.active], 1)
        XCTAssertEqual(SessionFilters.counts(items, machines)[.active], 1)
    }

    func testDraftPresenceExcludesBlankAndDispatchedPayloads() {
        let item = session(#"{"activity":"idle","subagents":{"child":{"state":"idle"}}}"#)
        let editing = Draft(target: item.target, text: "unsent")
        var excluded: [Draft] = []
        for change in [(\Draft.text, " \n\t")] { var copy = editing; copy[keyPath: change.0] = change.1; excluded.append(copy) }
        for status in ["submitting", "submitted", "recorded"] { var copy = editing; copy.status = status; excluded.append(copy) }
        var detached = editing; detached.detachedId = "detached"; excluded.append(detached)
        var question = editing; question.questionId = "question"; excluded.append(question)
        var foreign = editing; foreign.target.connectionId = "b"; excluded.append(foreign)
        for draft in excluded { XCTAssertTrue(SessionFilters.draftedSessions([item], [draft]).isEmpty) }
        var older = editing; older.target.run = "old-run"; older.target.conversation = "old-conversation"
        XCTAssertEqual(SessionFilters.draftedSessions([item], [older]), [item.target.key])
        var child = editing; child.target = item.target.with(conversation: "child-conversation", agentId: "child", parentConversation: item.target.conversation)
        XCTAssertEqual(SessionFilters.draftedSessions([item], [child]), [item.target.key])
        var stale = child; stale.target.parentConversation = "older-parent"
        XCTAssertTrue(SessionFilters.draftedSessions([item], [stale]).isEmpty)
        let profile = with(item, ["subagent_source": "hook_profiles"])
        XCTAssertTrue(SessionFilters.draftedSessions([profile], [child]).isEmpty)
    }

    func testIdentitySearchFindsSavedArchiveAndChildIds() {
        let item = session(#"{"state":"paused","subagents":{"agent-one":{"conversation_id":"child-conversation"}}}"#, archive: "archive-one")
        for query in ["CONVERSATION", "versat", "run", "archive-one", "agent-one", "child-conversation"] {
            XCTAssertEqual(SessionFilters.scoped([item], machines, [], query), [item], query)
            XCTAssertTrue(SessionFilters.scoped([item], machines, [MachineKey(connectionId: "b", computerId: "same")], query).isEmpty)
        }
        var profiles = item; profiles.raw = object(#"{"subagent_source":"hook_profiles","subagents":{"profile-only":{"conversation_id":"profile-conversation"}}}"#)
        XCTAssertTrue(SessionFilters.scoped([profiles], machines, [], "profile-only").isEmpty)
        var preview = item; preview.raw = object(#"{"subagent_previews":[{"agent_id":"preview-child","conversation_id":"preview-conversation"}]}"#)
        XCTAssertEqual(SessionFilters.scoped([preview], machines, [], "preview-child"), [preview])
        var mention = item; mention.preview = "Working on task zerus-ab12"
        XCTAssertEqual(SessionFilters.scoped([mention], machines, [], "zerus-ab12"), [mention])
    }

    func testActiveWorkRequiresReachableCurrentProcess() {
        let item = session(#"{"activity":"busy","phase":"working","state":"running"}"#)
        XCTAssertTrue(SessionFilters.matches(item, .active, true))
        XCTAssertFalse(SessionFilters.matches(item, .active, false))
        for raw in [#"{"activity":"busy","process_state":"exited"}"#, #"{"activity":"busy","state":"paused"}"#,
                    #"{"activity":"busy","conversation_state":"ended"}"#, #"{"activity":"unknown"}"#, #"{"activity":"idle"}"#] {
            XCTAssertFalse(SessionFilters.matches(session(raw), .active, true), raw)
        }
        let omitted = session(#"{"activity":"busy","phase":"tool","process_state":"running"}"#)
        XCTAssertEqual(SessionFilters.state(omitted), "running")
        XCTAssertFalse(SessionFilters.matches(omitted, .saved, true))
    }

    func testActiveCountsUnionOnceAndPreservesExplicitAttentionOffline() {
        let working = session(#"{"activity":"busy","phase":"tool"}"#, name: "codex/project/working")
        let attention = session(#"{"activity":"idle","phase":"input"}"#, name: "codex/project/input")
        let overlap = session(#"{"activity":"busy"}"#, pending: 1, name: "codex/project/pending-request")
        let child = session(#"{"activity":"idle","subagents":{"child":{"display_state":"input"}}}"#, name: "codex/project/child")
        let reminder = session(#"{"state":"paused","review_later":true}"#, connection: "b", name: "codex/project/reminder")
        let archived = session(#"{"activity":"busy","phase":"input","review_later":true}"#, archive: "archive-one")
        let saved = session(#"{"state":"paused","activity":"busy"}"#)
        let items = [working, attention, overlap, child, reminder, archived, saved]
        let counts = SessionFilters.counts(items, machines)
        XCTAssertEqual(counts[.active], 5)
        XCTAssertEqual(counts[.attention], 4)
        XCTAssertEqual(items.filter { SessionFilters.matches($0, .active, SessionFilters.online($0, machines)) }, [working, attention, overlap, child, reminder])
        XCTAssertFalse(SessionFilters.matches(session(#"{"phase":"approval","attention_acknowledged":true}"#), .active, true))
    }

    func testRecoveryErrorsAndChildRequests() {
        for recovery in ["waiting", "dispatching", "retrying"] {
            XCTAssertFalse(SessionFilters.needsAction(session(#"{"phase":"error","recovery":{"state":"\#(recovery)"}}"#)))
        }
        for recovery in ["uncertain", "blocked", "exhausted"] {
            XCTAssertTrue(SessionFilters.needsAction(session(#"{"phase":"idle","recovery":{"state":"\#(recovery)"}}"#)))
        }
        XCTAssertFalse(SessionFilters.needsAction(session(#"{"state":"paused","recovery":{"state":"blocked"}}"#)))
        let retrying = session(#"{"tracked":true,"activity":"busy","phase":"tool","recovery":{"state":"retrying","acknowledged":true}}"#)
        XCTAssertEqual("Retrying", SessionFilters.status(retrying, true))
        XCTAssertEqual(.neutral, DesktopSessionPalette.detailKind(retrying, true))
        let resumed = session(#"{"tracked":true,"activity":"busy","phase":"tool","recovery":{"state":"retrying","acknowledged":true,"resumed_at":125}}"#)
        XCTAssertEqual("Using a tool", SessionFilters.status(resumed, true))
        XCTAssertEqual(.working, DesktopSessionPalette.detailKind(resumed, true))
        XCTAssertTrue(SessionFilters.needsAttention(session(#"{"activity":"busy"}"#, pending: 1), true))
        XCTAssertFalse(SessionFilters.needsAttention(session(#"{"activity":"busy"}"#, pending: 1), false))
        for state in ["input", "approval", "attention"] {
            XCTAssertTrue(SessionFilters.needsAttention(session(#"{"activity":"busy","subagents":{"child":{"display_state":"\#(state)"}}}"#), true))
        }
        XCTAssertFalse(SessionFilters.needsAttention(session(#"{"activity":"busy","subagents":{"child":{"state":"error"}}}"#), true))
        XCTAssertFalse(SessionFilters.needsAttention(session(#"{"activity":"busy","subagent_source":"hook_profiles","subagents":{"child":{"display_state":"input"}}}"#), true))
        XCTAssertTrue(SessionFilters.needsAttention(session(#"{"review_later":true}"#), false))
        XCTAssertFalse(SessionFilters.needsAttention(session(#"{"unread_reply":true,"phase":"approval"}"#), false))
    }

    func testOptionalOnlySessionsAreNotAttentionButRequiredAndExplicitChildRequestsAre() {
        let optional: JSON = ["question_id": "codex-async:c:0", "question_hash": "h0", "optional": true]
        // Older connectors omit `optional`; every codex-async card is optional.
        let olderOptional: JSON = ["question_id": "codex-async:c:1", "question_hash": "h1"]
        let required: JSON = ["question_id": "request", "question_hash": "h2"]
        let summary: JSON = ["count": 2, "ids": ["codex-async:c:0", "codex-async:c:1"], "latest_at": 1760000000]
        for base in [#"{"activity":"busy","phase":"tool"}"#, #"{"activity":"idle","phase":"idle"}"#] {
            var listed = object(base)
            listed["mobile_attention"] = [optional, olderOptional]
            listed["optional_questions"] = summary
            var olderComputer = object(base)
            olderComputer["mobile_attention"] = [olderOptional]
            for raw in [listed, olderComputer] {
                // The agent keeps working without them, as on the desktop: no status, filter or counter.
                let item = session(raw.compactJSON)
                XCTAssertEqual(item.pendingCount, 0)
                XCTAssertFalse(SessionFilters.needsAttention(item, true))
                XCTAssertEqual(SessionFilters.active(item, true), SessionFilters.working(item, true))
                XCTAssertEqual(SessionFilters.counts([item], machines)[.attention], 0)
                XCTAssertNotEqual(DesktopSessionPalette.badge(item, true).kind, .attention)
                // A required request, approval, error or explicit child request beside them still needs attention.
                var requiredRaw = raw
                requiredRaw["mobile_attention"] = .array((raw.array("mobile_attention") ?? []) + [required])
                let withRequired = session(requiredRaw.compactJSON)
                XCTAssertEqual(withRequired.pendingCount, 1)
                XCTAssertTrue(SessionFilters.needsAttention(withRequired, true))
                XCTAssertEqual(SessionFilters.counts([withRequired], machines)[.attention], 1)
                for phase in ["input", "approval", "error"] {
                    XCTAssertTrue(SessionFilters.needsAttention(with(item, ["phase": .string(phase)]), true))
                }
                XCTAssertTrue(SessionFilters.needsAttention(with(item, ["subagents": ["child": ["display_state": "input"]]]), true))
                // Offline rows keep only device reminders, as before.
                XCTAssertFalse(SessionFilters.needsAttention(withRequired, false))
                XCTAssertFalse(SessionFilters.needsAttention(item, false))
                XCTAssertTrue(SessionFilters.needsAttention(with(item, ["mobile_review_later": true]), false))
            }
        }
        // Without connector items, the older pending count stays a blocking request.
        XCTAssertTrue(SessionFilters.needsAttention(session(#"{"activity":"busy","pending_question_count":1}"#), true))
        XCTAssertFalse(SessionFilters.needsAttention(session(#"{"activity":"busy","pending_question_count":1}"#), false))
    }

    func testArchivesAreExclusiveAndSavedMeansNonRunning() {
        let archived = session(#"{"state":"archived","activity":"busy","phase":"input"}"#, archive: "archive-one")
        for filter in SessionFilter.allCases where filter != .archive { XCTAssertFalse(SessionFilters.matches(archived, filter, true)) }
        XCTAssertTrue(SessionFilters.matches(archived, .archive, false))
        for state in ["paused", "stopped"] { XCTAssertTrue(SessionFilters.matches(session(#"{"state":"\#(state)"}"#), .saved, true)) }
        XCTAssertFalse(SessionFilters.matches(session(), .archive, true))
    }

    func testScopeAndGroupsKeepWorkspacesAndCatalogOrder() {
        let a = session(#"{"activity":"busy"}"#), b = session(#"{"activity":"busy"}"#, connection: "b")
        XCTAssertEqual(SessionFilters.scoped([a, b], machines, [MachineKey(connectionId: "a", computerId: "same")], ""), [a])
        let key = ProjectKey(connectionId: "a", swarmId: "swarm", id: "project")
        var tagged = session(#"{"activity":"busy","git_branch":"feature/synthetic"}"#); tagged.projectKey = key
        var other = session(#"{"activity":"busy"}"#); other.projectKey = ProjectKey(connectionId: "a", swarmId: "swarm", id: "other")
        XCTAssertEqual(SessionFilters.scoped([tagged, other], machines, [], "synthetic", project: key), [tagged])
        XCTAssertFalse(SessionFilters.scoped([tagged], machines, [], "computer a").isEmpty)
        let pa = ProjectKey(connectionId: "a", swarmId: "swarm", id: "a"), pb = ProjectKey(connectionId: "b", swarmId: "swarm", id: "b")
        let empty = ProjectKey(connectionId: "a", swarmId: "swarm", id: "empty")
        let projects = [ProjectSummary(key: pb, name: "Same", color: "", folders: [], computers: []),
                        ProjectSummary(key: empty, name: "Empty", color: "", folders: [], computers: []),
                        ProjectSummary(key: pa, name: "Same", color: "", folders: [], computers: [])]
        var first = session(); first.projectKey = pa
        var second = session(connection: "b"); second.projectKey = pb
        XCTAssertEqual(SessionFilters.grouped([first, second], projects).map(\.key), [pb, pa])
    }

    func testLifecycleStatusAndShortLabelMirrorDesktop() {
        XCTAssertEqual(SessionFilters.label(session()), "task")
        XCTAssertEqual(SessionFilters.label(session(name: "codex/project/nested/task")), "nested/task")
        XCTAssertEqual(SessionFilters.label(session(#"{"tag":"custom"}"#)), "custom")
        XCTAssertEqual(SessionFilters.status(session(#"{"activity":"idle"}"#, archive: "archive-one"), true), "Archived")
        XCTAssertEqual(SessionFilters.status(session(#"{"state":"paused","activity":"idle"}"#), true), "Paused")
        XCTAssertEqual(SessionFilters.status(session(#"{"state":"stopped","activity":"idle"}"#), true), "Stopped")
        XCTAssertEqual(SessionFilters.status(session(#"{"activity":"busy"}"#), false), "Offline")
    }

    func testArchiveDatesUseLocalTimeWithSeconds() {
        let utc = TimeZone(identifier: "UTC")!
        let first = SessionFilters.archiveDate(session(#"{"archived_at":3}"#, archive: "first"), locale: Locale(identifier: "en_US"), timeZone: utc)
        XCTAssertTrue(first.contains("12:00:03"), first)
        XCTAssertNotEqual(first, SessionFilters.archiveDate(session(#"{"archived_at":4}"#, archive: "second"), locale: Locale(identifier: "en_US"), timeZone: utc))
        XCTAssertEqual(SessionFilters.archiveDate(session(#"{"archived_at":3}"#)), "")
        for raw in ["{}", #"{"archived_at":null}"#, #"{"archived_at":"invalid"}"#, #"{"archived_at":0}"#, #"{"archived_at":-1}"#, #"{"archived_at":1e100}"#] {
            XCTAssertEqual(SessionFilters.archiveDate(session(raw, archive: "first")), "", raw)
        }
    }
}

extension JSONObject {
    var compactJSON: String { JSON.object(self).compact }
}
