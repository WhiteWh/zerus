import XCTest
@testable import ZerusKit

/// zerus-a4mub.9: the open conversation acknowledges the current shared reply and attention from its verified
/// inspection's `current_read`, never from a catalog row, and only what it showed. The cases are shared with Android
/// (mobile/fixtures/current-read.json).
final class SharedReadCurrentTests: XCTestCase {
    private func fixture() throws -> JSONObject {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../../../mobile/fixtures/current-read.json").standardized
        return try XCTUnwrap(try JSON.parse(Data(contentsOf: url)).object)
    }

    private func target(_ value: JSONObject) -> Target {
        let agent = value.string("agent_id")
        return Target(computerId: "computer", session: "claude/project/session", run: value.string("run"),
                      conversation: value.string("conversation"), connectionId: "gateway", archiveId: value.string("archive_id"),
                      agentId: agent, parentConversation: agent.isEmpty ? "" : value.string("conversation"))
    }

    private func row(_ target: Target, _ value: JSONObject?) -> Session? {
        value.map {
            Session(target: target, title: "Session", agent: "claude", status: "idle", project: "project", preview: "", pendingCount: 0,
                    raw: ["run_id": .string(target.run), "conversation_id": .string(target.conversation),
                          "state": .string($0.string("state")), "read": $0["read"] ?? .null])
        }
    }

    private func parsed(_ current: CurrentRead?) -> JSONObject? {
        current.map {
            ["reply_id": $0.reply.map(JSON.string) ?? .null, "attention_signature": $0.attention.map(JSON.string) ?? .null,
             "attention_complete": .bool($0.attentionComplete)]
        }
    }

    func testEverySharedCaseParsesAndAcknowledgesExactlyWhatWasShown() throws {
        let fixture = try fixture()
        XCTAssertEqual(fixture.int("schema"), 1)
        let cases = fixture.objects("cases")
        XCTAssertGreaterThanOrEqual(cases.count, 30)
        for item in cases {
            let name = item.string("name")
            let target = target(try XCTUnwrap(item.object("target"), name))
            let current = SharedReadPolicies.current(item.object("inspection"), target: target, verified: item.bool("verified"))
            XCTAssertEqual(parsed(current), item.object("current"), name)
            let payload = current.flatMap {
                SharedReadPolicies.currentPayload(row(target, item.object("row")), target: target, current: $0,
                                                  replyShown: item.bool("reply_shown"), attentionShown: item.bool("attention_shown"))
            }
            XCTAssertEqual(payload, item.object("payload"), name)
            if let payload { XCTAssertEqual(Set(payload.keys), ["run_id", "conversation_id", "reply_id", "attention_signature"], name) }
        }
    }

    func testEverySharedViewportShowsExactlyItsReplyAndAttention() throws {
        let viewports = try fixture().objects("viewports")
        XCTAssertGreaterThanOrEqual(viewports.count, 8)
        for item in viewports {
            let flags = CurrentReadFlags(tail: item.bool("tail_visible"), newest: item.bool("newest_visible"), questions: item.bool("questions_visible"))
            let shown = SharedReadPolicies.currentShown(latest: item.bool("latest"), questionsListed: item.bool("questions_listed"), flags: flags)
            XCTAssertEqual(shown.reply, item.bool("reply_shown"), item.string("name"))
            XCTAssertEqual(shown.attention, item.bool("attention_shown"), item.string("name"))
        }
    }

    /// zerus-a4mub.30/.31: the settled layout reaches the shown items through the shared readable-visibility rule, the
    /// shown items the flags, and the flags the decision, exactly as Android maps the same layouts.
    func testEverySharedLayoutMapsToItsFlagsAndDecision() throws {
        let shared = try fixture()
        let layouts = shared.objects("layouts")
        XCTAssertGreaterThanOrEqual(layouts.count, 19)
        let readable = shared.double("readable")
        XCTAssertEqual(readable, ConversationReadViewport.readableHeight)
        func number(_ value: JSON) -> Double {
            switch value {
            case .int(let value): Double(value)
            case .double(let value): value
            default: .nan
            }
        }
        for item in layouts {
            let name = item.string("name")
            let viewport = (item["viewport"]?.array ?? []).map(number)
            XCTAssertEqual(viewport.count, 2, name)
            let items = item.objects("items").map {
                ConversationReadViewport.Item(key: $0.string("key"), offset: $0.double("offset"), size: $0.double("size"))
            }
            let visible = ConversationReadViewport.visible(items, start: viewport[0], end: viewport[1], readable: readable)
            let expectedVisible = (item["visible"]?.array ?? []).compactMap { value -> String? in
                if case .string(let text) = value { return text }
                return nil
            }
            XCTAssertEqual(visible, expectedVisible, name)
            let flags = ConversationReadViewport.flags(visible: visible, newestId: item.string("newest"))
            let expected = try XCTUnwrap(item.object("flags"), name)
            XCTAssertEqual(flags, CurrentReadFlags(tail: expected.bool("tail"), newest: expected.bool("newest"), questions: expected.bool("questions")), name)
            let shown = SharedReadPolicies.currentShown(latest: item.bool("latest"), questionsListed: item.bool("questions_listed"), flags: flags)
            XCTAssertEqual(shown.reply, item.bool("reply_shown"), name)
            XCTAssertEqual(shown.attention, item.bool("attention_shown"), name)
        }
    }

    /// zerus-a4mub.31: SwiftUI places items on fractional points; rounding never hides a wholly visible row or the
    /// tail, and never shows a row half a point short of its readable part.
    func testFractionalLayoutRoundingKeepsTheSharedDecision() {
        let end = 900.0 + 1.0 / 3.0
        let short = ConversationReadViewport.Item(key: "event:e2", offset: end - 40 + 1e-9, size: 40)
        XCTAssertTrue(ConversationReadViewport.shows(short, start: 0, end: end))
        let tail = ConversationReadViewport.Item(key: "history:tail", offset: end + 1.0 / 3.0, size: 1)
        XCTAssertTrue(ConversationReadViewport.shows(tail, start: 0, end: end))
        let tall = ConversationReadViewport.Item(key: "event:e2", offset: end - 47.4, size: 600)
        XCTAssertFalse(ConversationReadViewport.shows(tall, start: 0, end: end))
        let readable = ConversationReadViewport.Item(key: "event:e2", offset: end - 47.6, size: 600)
        XCTAssertTrue(ConversationReadViewport.shows(readable, start: 0, end: end))
    }

    func testACatalogRowNeverStandsInForTheDisplayedInspection() throws {
        let target = Target(computerId: "computer", session: "claude/project/session", run: "run-1", conversation: "conversation-1",
                            connectionId: "gateway")
        // The catalog row already names a newer reply and request; the inspection on screen does not.
        let catalog = Session(target: target, title: "Session", agent: "claude", status: "input", project: "project", preview: "", pendingCount: 0,
                              raw: ["run_id": "run-1", "conversation_id": "conversation-1", "reply_id": "8:200.5",
                                    "attention_signature": .string(String(repeating: "b", count: 64)), "read": .null])
        var inspection: JSONObject = ["run_id": "run-1", "conversation_id": "conversation-1"]
        XCTAssertNil(SharedReadPolicies.current(inspection, target: target, verified: true))
        inspection["current_read"] = .object(["schema": .int(1), "run_id": "run-1", "conversation_id": "conversation-1",
                                              "reply_id": "7:100.5", "attention_signature": .null, "attention_complete": .bool(true)])
        let current = try XCTUnwrap(SharedReadPolicies.current(inspection, target: target, verified: true))
        let payload = try XCTUnwrap(SharedReadPolicies.currentPayload(catalog, target: target, current: current, replyShown: true, attentionShown: true))
        XCTAssertEqual(payload["reply_id"], "7:100.5")
        XCTAssertEqual(payload["attention_signature"], .null)
        // The canonical path is unchanged: without a fresh head the catalog row sends nothing.
        XCTAssertNil(SharedReadPolicies.payload(catalog, freshHead: false, localUnread: nil))
    }
}
