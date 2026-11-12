import XCTest
@testable import ZerusKit

/// The person's per-child question route switch against the shared contract (question-routing-contract.json),
/// ported from Android SessionQuestionRoutingTest.
final class SessionQuestionRoutingTests: XCTestCase {
    private func contract() throws -> JSONObject { try XCTUnwrap(try SharedFixture.json("question-routing-contract.json").object) }
    private func target(_ raw: JSONObject, run: String = "run-1", conversation: String = "conversation-1") -> Target {
        let name = raw.string("name")
        return Target(computerId: "00000000-0000-4000-8000-000000000001", session: name.isEmpty ? "claude/project/child" : name,
                      run: run, conversation: conversation, connectionId: "workspace")
    }
    private func routing(_ value: JSONObject?) -> QuestionRouting? {
        guard let value else { return nil }
        return QuestionRouting(sendToParent: value.bool("send_to_parent"), revision: value.int64("revision"))
    }

    func testTheOperationIsTheContracts() throws {
        XCTAssertEqual(try contract().string("operation"), SessionQuestionRouting.operation)
        XCTAssertTrue(SessionActionPolicies.operations.contains(SessionQuestionRouting.operation))
    }

    func testOnlyTheExactShapeIsKnownAndAnythingElseIsUnknown() throws {
        for item in try contract().objects("parse") {
            let raw: JSONObject = ["question_routing": item["question_routing"] ?? .null]
            XCTAssertEqual(SessionQuestionRouting.parse(raw), routing(item.object("routing")), item.string("name"))
        }
        XCTAssertNil(SessionQuestionRouting.parse([:]))
        XCTAssertNil(SessionQuestionRouting.parse(nil))
    }

    func testTheSessionActionsShowItForLiveClaudeChildrenOnly() throws {
        for item in try contract().objects("availability") {
            let raw = try XCTUnwrap(item.object("row"))
            let name = item.string("name")
            let facts = SessionMenuFacts(target: target(raw), row: raw, state: raw.string("state"), archived: raw.string("state") == "archived",
                                         online: true, supported: [SessionQuestionRouting.operation])
            let entry = SessionActionsMenu.items(facts).flatMap { $0 }.first { $0.id == SessionQuestionRouting.operation }
            XCTAssertEqual(entry != nil, item.bool("shown"), name)
            guard let entry else { continue }
            XCTAssertEqual(entry.enabled, item.bool("enabled"), name)
            XCTAssertTrue(entry.label.hasSuffix(item.string("parent")), name)
            var offline = facts
            offline.online = false
            XCTAssertEqual(SessionActionsMenu.questionRoutingReason(offline), "The machine is offline.")
            var unsupported = facts
            unsupported.supported = []
            XCTAssertEqual(SessionActionsMenu.questionRoutingReason(unsupported), SessionQuestionRouting.unavailable)
        }
    }

    func testTheLabelSaysWhatChoosingItDoes() {
        let parent: JSONObject = ["parent": .object(["name": .string("codex/project/lead")])]
        XCTAssertEqual(SessionQuestionRouting.label(parent.putting("question_routing", .object(["send_to_parent": .bool(true), "revision": .int(0)]))),
                       "Stop sending questions to lead")
        XCTAssertEqual(SessionQuestionRouting.label(parent.putting("question_routing", .object(["send_to_parent": .bool(false), "revision": .int(1)]))),
                       "Send questions to lead")
        XCTAssertEqual(SessionQuestionRouting.label(parent), "Questions to lead")
    }

    func testTheChangeTurnsTheCurrentValueTheOtherWayAtItsRevision() throws {
        let change = try XCTUnwrap(contract().object("change"))
        let session = try XCTUnwrap(change.object("session"))
        let t = target([:], run: session.string("run_id"), conversation: session.string("conversation_id"))
        let args = try SessionActionPolicies.arguments(SessionQuestionRouting.operation, SessionQuestionRouting.toggle(routing(change.object("routing"))))
        XCTAssertEqual(SessionQuestionRouting.payload(t, args), try XCTUnwrap(change.object("payload")))
        for bad: JSONObject in [["send_to_parent": .bool(true)], ["send_to_parent": .int(1), "expected_revision": .int(0)],
                                ["send_to_parent": .bool(true), "expected_revision": .int(-1)],
                                ["send_to_parent": .bool(true), "expected_revision": .int(0), "muted": .bool(true)]] {
            XCTAssertThrowsError(try SessionActionPolicies.arguments(SessionQuestionRouting.operation, bad))
        }
    }

    func testAStaleRevisionOrANonChildIsRefusedBeforeSending() throws {
        let raw: JSONObject = ["name": .string("claude/project/child"), "cmd": .string("claude"), "state": .string("running"),
                               "parent": .object(["name": .string("codex/project/lead")]),
                               "question_routing": .object(["send_to_parent": .bool(true), "revision": .int(2)])]
        let t = target(raw)
        let args = try SessionActionPolicies.arguments(SessionQuestionRouting.operation, ["send_to_parent": .bool(false), "expected_revision": .int(2)])
        XCTAssertEqual(SessionActionPolicies.inspectReason(t, SessionQuestionRouting.operation, args, raw), "")
        let stale = try SessionActionPolicies.arguments(SessionQuestionRouting.operation, ["send_to_parent": .bool(false), "expected_revision": .int(1)])
        XCTAssertEqual(SessionActionPolicies.inspectReason(t, SessionQuestionRouting.operation, stale, raw), SessionQuestionRouting.stale)
        XCTAssertEqual(SessionActionPolicies.inspectReason(t, SessionQuestionRouting.operation, args, raw.putting("cmd", .string("codex"))),
                       SessionQuestionRouting.notChild)
    }

    func testOnlyTheComputersExactReplySettlesTheChange() throws {
        let fixture = try contract()
        let change = try XCTUnwrap(fixture.object("change"))
        let session = try XCTUnwrap(change.object("session"))
        let payload = try XCTUnwrap(change.object("payload"))
        let t = target([:], run: session.string("run_id"), conversation: session.string("conversation_id"))
        let args: JSONObject = ["send_to_parent": payload["send_to_parent"] ?? .null, "expected_revision": payload["expected_revision"] ?? .null]
        let action = SessionAction(requestId: "00000000-0000-4000-8000-0000000000aa", target: t, operation: SessionQuestionRouting.operation, arguments: args)
        for item in fixture.objects("results") {
            let receipt: JSONObject = ["request_id": .string(action.requestId), "state": .string("completed"), "result": item["result"] ?? .null]
            let settled = SessionActionPolicies.result(action, receipt)
            let expected = ["confirmed": "completed", "refused": "failed"][item.string("outcome")] ?? "uncertain"
            XCTAssertEqual(settled.status, expected, item.string("name"))
            if expected == "failed" { XCTAssertEqual(settled.error, SessionQuestionRouting.refusals[item.string("error")], item.string("name")) }
        }
        let other: JSONObject = ["request_id": .string("00000000-0000-4000-8000-0000000000bb"), "state": .string("completed"),
                                 "result": fixture.objects("results").first.map(JSON.object) ?? .null]
        XCTAssertEqual(SessionActionPolicies.result(action, other).status, "uncertain")
    }
}
