import XCTest
@testable import ZerusKit

/// zerus-sa9qn.19: every status tab lists exactly what its count counts, through the production list projection, over
/// the whole shared contract fleet at once (desktop shared-session-contract.test.ts and Android StatusFilterParityTest
/// load the same fleet).
final class StatusFilterParityTests: XCTestCase {
    private let up = Machine(connectionId: "fleet", id: "up", name: "Online", online: true)
    private let down = Machine(connectionId: "fleet", id: "down", name: "Offline", online: false)

    private func session(_ name: String, _ raw: JSONObject, computer: String = "up", archive: String = "") -> Session {
        Session(target: Target(computerId: computer, session: "codex/contract/\(name)",
                               run: raw.string("run_id").isEmpty ? "run-\(name)" : raw.string("run_id"),
                               conversation: raw.string("conversation_id").isEmpty ? "conversation-\(name)" : raw.string("conversation_id"),
                               connectionId: "fleet", archiveId: archive),
                title: name, agent: "codex", status: "", project: "Project", preview: "",
                pendingCount: QuestionPolicies.blockingRequests(raw), raw: raw)
    }

    private func names(_ presentation: SessionListPresentation) -> [String] {
        presentation.groups.flatMap { group in group.sessions.map { String($0.target.session.dropFirst("codex/contract/".count)) } }.sorted()
    }

    func testEachTabListsExactlyWhatItsCountCountsOverTheContractFleet() throws {
        let contract = try SharedFixture.json("session-filter-contract.json")
        let live = contract.objects("cases").filter { $0.object("raw")?.string("state") != "archived" }
        XCTAssertGreaterThan(live.count, 40)
        let rows = live.map { example in
            session(example.string("name"), example.object("raw") ?? [:], computer: example.bool("online") ? "up" : "down")
        }
        let drafts = zip(live, rows).filter { $0.0.bool("draft") }.map { Draft(target: $0.1.target, text: "draft") }
        for filter in [SessionFilter.all, .attention, .active, .saved] {
            let presentation = SessionListPresentation.prepare(sessions: rows, machines: [up, down], projects: [], selectedMachines: [],
                                                               query: "", project: nil, filter: filter, drafts: drafts)
            let expected = live.filter { $0.strings("matches").contains(filter.id) }.map { $0.string("name") }.sorted()
            XCTAssertEqual(names(presentation), expected, filter.id)
            XCTAssertEqual(presentation.counts[filter], presentation.visibleCount, filter.id + " count")
            XCTAssertEqual(presentation.machineCounts.values.reduce(0, +), presentation.visibleCount, filter.id + " machine chooser")
        }
    }

    func testArchiveListsAndCountsInsideTheSharedDateWindow() throws {
        let contract = try SharedFixture.json("session-filter-contract.json")
        let window = try XCTUnwrap(contract.object("archive_window"))
        let rows = window.objects("rows").map { row in
            session(row.string("name"), ["state": .string("archived"), "archived_at": .int(row.int64("archived_at"))],
                    archive: "archive-" + row.string("name"))
        }
        let range = ArchiveRange(from: window.int64("from"), until: window.int64("until"))
        let presentation = SessionListPresentation.prepare(sessions: rows, machines: [up], projects: [], selectedMachines: [], query: "",
                                                           project: nil, filter: .archive, drafts: [], archiveRange: range)
        let listed = window.strings("listed").sorted()
        XCTAssertEqual(names(presentation), listed)
        XCTAssertEqual(presentation.counts[.archive], listed.count)
        XCTAssertEqual(presentation.machineCounts.values.reduce(0, +), listed.count)
    }
}
