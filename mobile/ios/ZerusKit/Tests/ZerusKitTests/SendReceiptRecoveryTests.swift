import XCTest
@testable import ZerusKit

final class SendReceiptRecoveryTests: XCTestCase {
    private let original = "22222222-2222-4222-8222-222222222222"
    private let read = "11111111-1111-4111-8111-111111111111"
    private let machine = "33333333-3333-4333-8333-333333333333"
    private let session = "44444444-4444-4444-8444-444444444444"
    private var target: Target { Target(computerId: machine, session: "codex/example/send", run: "run", conversation: "conversation", connectionId: "gateway") }
    private func intent(_ text: String = "exact text", provider: String = "codex", compaction: String? = nil) -> SendIntent {
        SendReceiptRecovery.freeze(target: target, id: original, text: text, provider: provider, machineId: machine,
                                   sessionId: session, encoding: "python_default", compactionId: compaction)!
    }
    private func message(_ i: SendIntent? = nil) -> OutgoingMessage {
        let i = i ?? intent()
        return OutgoingMessage(requestId: original, target: target, text: i.value()!.object("payload")!.string("text"), attachments: [], createdAt: 1, status: "uncertain", sendIntent: i)
    }
    private func answer(_ id: String? = nil, intent i: SendIntent? = nil) -> JSONObject {
        let i = i ?? intent(), v = i.value()!
        var result: JSONObject = ["schema": 1, "request_id": .string(id ?? read), "message_request_id": .string(original),
            "computer_id": .string(machine), "machine_id": .string(machine), "session_id": .string(session), "session": .string(target.session),
            "run_id": "run", "conversation_id": "conversation", "agent_id": "", "provider": v["provider"]!,
            "intent_sha256": .string(i.intentSha256), "native_digest": .string(SendReceiptRecovery.nativeDigest(v)!),
            "status": "submitted", "proof": "original_receipt", "submitted_at": 2.0, "queued": false]
        if v.string("provider") == "opencode" { result["native_message_id"] = "msg_exact" }
        return result
    }
    func testSharedGoldensKeepExactUnicodeWhitespaceAndNativeBytes() throws {
        let bytes = try Data(contentsOf: SharedFixture.url("send-receipt-intents.json"))
        let central = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../../../services/mobile/vectors/send_receipt/intents.json")
        XCTAssertEqual(bytes, try Data(contentsOf: central))
        let fixture = try JSON.parse(bytes)
        for vector in fixture.objects("vectors") {
            let v = try XCTUnwrap(vector.object("intent"))
            XCTAssertEqual(try SendReceiptRecovery.encode(.object(v)), vector.string("canonical"))
            XCTAssertEqual(SendReceiptRecovery.hash(try SendReceiptRecovery.encode(.object(v))), vector.string("intent_sha256"))
            XCTAssertEqual(SendReceiptRecovery.nativeDigest(v), vector.string("terminal_python_default_sha256"))
            XCTAssertEqual(SendReceiptRecovery.nativeDigest(v.putting("native_encoding", "compact")), vector.string("terminal_compact_sha256"))
            for provider in ["dsh", "opencode"] { XCTAssertEqual(SendReceiptRecovery.nativeDigest(v.putting("provider", .string(provider))), vector.string("native_agent_sha256")) }
            XCTAssertEqual(SendReceiptRecovery.valid(v), vector.bool("admitted"))
        }
        XCTAssertNotEqual(intent("\u{e9}").intentSha256, intent("e\u{301}").intentSha256)
        // Swift String equality normalizes; intent matching deliberately compares UTF-8.
        XCTAssertFalse(SendReceiptRecovery.matches(message(intent("\u{e9}")), intent("e\u{301}")))
    }
    func testEnqueuePersistenceRecoveryKeepsOptionsAndExactOriginal() throws {
        let i = intent("  e\u{301}\nexact trailing space  ", compaction: read), draft = Draft(target: target, text: message(i).text)
        let written = try MessageState(drafts: [draft]).enqueue(draft, requestId: original, at: 1, sendIntent: i)
        let decoded = try JSONDecoder().decode(MessageState.self, from: JSONEncoder().encode(written))
        XCTAssertEqual(decoded.outgoing[0].sendIntent, i)
        XCTAssertEqual(decoded.drafts[0].text, "")
        XCTAssertEqual(decoded.outgoing[0].recover().status, "uncertain")
        XCTAssertEqual(i.value()?.object("payload")?.string("expected_compaction_id"), read)
        XCTAssertNotEqual(SendReceiptRecovery.nativeDigest(i.value()!), SendReceiptRecovery.nativeDigest(intent(message(i).text).value()!))
        XCTAssertEqual(String(reflecting: i), "SendIntent(retained)")
    }
    func testEveryOriginalIdentityDigestStatusAndTypeIsRequired() throws {
        let i = intent()
        XCTAssertNotNil(SendReceiptRecovery.proof(read, i, answer()))
        for key in ["request_id", "message_request_id", "computer_id", "machine_id", "session_id", "session", "run_id", "conversation_id", "provider", "agent_id", "intent_sha256", "native_digest", "status", "proof", "submitted_at", "queued", "schema"] {
            XCTAssertNil(SendReceiptRecovery.proof(read, i, answer().putting(key, "foreign")), key)
            XCTAssertNil(SendReceiptRecovery.proof(read, i, answer().putting(key, nil)), key)
        }
        for status in ["pending", "failed", "unknown", "uncertain", "queued"] { XCTAssertNil(SendReceiptRecovery.proof(read, i, answer().putting("status", .string(status)))) }
        XCTAssertNil(SendReceiptRecovery.proof(read, i, answer().putting("submitted_at", -1)))
    }
    func testCorruptNativeIdsNeverBecomeEchoAuthority() {
        let i = intent(provider: "opencode")
        XCTAssertNil(SendReceiptRecovery.proof(read, i, answer(intent: i).putting("native_message_id", nil)))
        for id in ["", "msg_", "wrong", "/private/fixture/session.json", "msg_bad/name", "msg_bad\nname", "msg_" + String(repeating: "a", count: 125)] {
            XCTAssertNil(SendReceiptRecovery.proof(read, i, answer(intent: i).putting("native_message_id", .string(id))))
        }
        XCTAssertNil(SendReceiptRecovery.proof(read, intent(), answer().putting("native_message_id", "msg_foreign")))
    }
    func testSettlementIsExactMonotonicAndWithinOriginalDeadline() throws {
        let i = intent(), captured = message(), proof = try XCTUnwrap(SendReceiptRecovery.proof(read, i, answer()))
        var dismissed = captured; dismissed.deliveryDismissed = true
        var other = captured; other.requestId = read; other.target.run = "other"; other.sendIntent = nil
        let before = MessageState(drafts: [Draft(target: target, text: "new composition")], outgoing: [dismissed, other])
        let settled = SendReceiptRecovery.settle(before, captured: captured, intent: i, proof: proof, deadline: 100, now: 99)
        XCTAssertEqual(settled.outgoing[0].status, "submitted"); XCTAssertTrue(settled.outgoing[0].deliveryDismissed)
        XCTAssertEqual(settled.outgoing[1], other); XCTAssertEqual(settled.drafts, before.drafts)
        XCTAssertEqual(settled, SendReceiptRecovery.settle(settled, captured: captured, intent: i, proof: proof, deadline: 200, now: 101))
        XCTAssertEqual(before, SendReceiptRecovery.settle(before, captured: captured, intent: i, proof: proof, deadline: 100, now: 100))
        var foreign = captured; foreign.text = "changed"
        XCTAssertEqual(before, SendReceiptRecovery.settle(before, captured: foreign, intent: i, proof: proof, deadline: 100, now: 99))
    }
    func testConfirmedEchoDeduplicatesButUncertaintyNeedsReceiptSettlement() throws {
        let i = intent(), p = try XCTUnwrap(SendReceiptRecovery.proof(read, i, answer()))
        var captured = message(); captured.submittedAt = 2
        let row = Event(id: "native", role: "You", text: captured.text, time: "", at: 2, source: "codex_native")
        var ordinary = captured; ordinary.status = "submitted"
        let normal = ConversationMerge.merge(target: target, native: [row], outgoing: [ordinary])
        XCTAssertEqual(normal.events.count, 1); XCTAssertEqual(normal.recordedRequestIds, [original])
        var untrustedRequest = row; untrustedRequest.requestId = original
        var nativeId = row; nativeId.source = "opencode_native"; nativeId.nativeMessageId = "msg_unproven"
        for echo in [row, untrustedRequest, nativeId] {
            let unknown = ConversationMerge.merge(target: target, native: [echo], outgoing: [captured])
            XCTAssertEqual(unknown.events.count, 2); XCTAssertTrue(unknown.recordedRequestIds.isEmpty)
            XCTAssertEqual(captured.status, "uncertain")
        }
        var otherConfirmed = ordinary; otherConfirmed.requestId = read; otherConfirmed.createdAt = 5; otherConfirmed.sendIntent = nil
        let concurrent = ConversationMerge.merge(target: target, native: [row], outgoing: [captured, otherConfirmed])
        XCTAssertEqual(concurrent.recordedRequestIds, [read])
        XCTAssertTrue(concurrent.events.contains { $0.outgoingId == original && $0.source == "mobile_outbox" && $0.delivery == "uncertain" })
        let sent = SendReceiptRecovery.settle(MessageState(outgoing: [captured]), captured: captured, intent: i, proof: p, deadline: 100, now: 99).outgoing[0]
        let recovered = ConversationMerge.merge(target: target, native: [row], outgoing: [sent])
        XCTAssertEqual(recovered.events.count, 1); XCTAssertEqual(recovered.recordedRequestIds, [original])
        let oc = intent(provider: "opencode"), msg = message(oc), proof = try XCTUnwrap(SendReceiptRecovery.proof(read, oc, answer(intent: oc)))
        let admitted = SendReceiptRecovery.settle(MessageState(outgoing: [msg]), captured: msg, intent: oc, proof: proof, deadline: 100, now: 99).outgoing[0]
        var exact = row; exact.text = "provider transformed text"; exact.at = 1000; exact.source = "opencode_native"; exact.nativeMessageId = "msg_exact"
        XCTAssertEqual(ConversationMerge.merge(target: target, native: [exact], outgoing: [admitted]).recordedRequestIds, [original])
        var inherited = exact; inherited.inheritedFrom = "foreign"
        var wrongProvider = exact; wrongProvider.source = "codex_native"
        var wrongId = exact; wrongId.nativeMessageId = "msg_other"
        for other in [inherited, wrongProvider, wrongId] { XCTAssertTrue(ConversationMerge.merge(target: target, native: [other], outgoing: [admitted]).recordedRequestIds.isEmpty) }
    }
    func testLostOriginalUsesFreshReadUuidAndNeverASend() async throws {
        let i = intent(), original = original, template = answer()
        StubProtocol.reset { request in
            if request.method == "GET" { return StubProtocol.json(200, #"{"request_id":"\#(original)","state":"uncertain"}"#) }
            let id = request.json!.string("request_id")
            let result = template.putting("request_id", .string(id))
            return StubProtocol.json(200, JSON.object(["request_id": .string(id), "state": "completed", "result": .object(result)]).compact)
        }
        let connection = Connection(id: "gateway", label: "Fixture", url: "https://gateway.example.test", token: "synthetic")
        let checked = try await StubProtocol.client().checkDelivery(connection, message: message(), intent: i, limit: .seconds(1))
        guard case .native = checked else { return XCTFail("Exact original was not recovered") }
        let envelope = try XCTUnwrap(StubProtocol.requests.last?.json)
        XCTAssertEqual(envelope.string("operation"), "send_receipt"); XCTAssertNotEqual(envelope.string("request_id"), original)
        XCTAssertEqual(envelope.object("payload")?.object("intent")?.object("payload")?.string("request_id"), original)
        XCTAssertEqual(StubProtocol.requests.map(\.method), ["GET", "POST"])
    }
    func testBoundsAndOldGatewaysFailClosed() {
        XCTAssertNil(SendReceiptRecovery.freeze(target: target, id: original, text: String(repeating: "x", count: 65537), provider: "codex", machineId: machine, sessionId: session, encoding: "compact"))
        XCTAssertNil(SendReceiptRecovery.freeze(target: target, id: original, text: "exact\r\ntext", provider: "codex", machineId: machine, sessionId: session, encoding: "compact"))
        XCTAssertFalse(SendReceiptRecovery.supported(relay: ["send_receipt"], native: ["send"]))
        XCTAssertEqual(WorkspaceProtocol.operationClass("send_receipt"), "read")
        XCTAssertEqual(WorkspaceProtocol.operationClass("send"), "mutation")
        XCTAssertNil(SendReceiptRecovery.encoding(version: 1, route: .unknown))
        XCTAssertEqual(SendReceiptRecovery.encoding(version: 1, route: .direct), "compact")
        XCTAssertEqual(SendReceiptRecovery.encoding(version: 2, route: .direct), "python_default")
        XCTAssertEqual(SendReceiptRecovery.encoding(version: 2, route: .via(gatewayId: "gateway", gatewayName: "Gateway")), "compact")
    }
    func testLegacyReconstructionRequiresTheOriginalOptionsDigest() throws {
        let plain = intent(), continuation = intent(compaction: read)
        var old = message(); old.sendIntent = nil
        let p = try XCTUnwrap(SendReceiptRecovery.proof(read, plain, answer().putting("queued", true)))
        let state = SendReceiptRecovery.settle(MessageState(outgoing: [old]), captured: old, intent: plain, proof: p, deadline: 100, now: 99)
        XCTAssertEqual(state.outgoing[0].status, "submitted"); XCTAssertEqual(state.outgoing[0].sendIntent, plain)
        XCTAssertEqual(ConversationMerge.merge(target: target, native: [], outgoing: state.outgoing).events[0].deliveryState, "queued")
        let lost = answer(intent: continuation).putting("intent_sha256", .string(plain.intentSha256))
        XCTAssertNil(SendReceiptRecovery.proof(read, plain, lost))
    }
    func testOrdinaryAcknowledgementUsesExactOriginalBinding() {
        let i = intent()
        let positive = answer(original).putting("name", .string(target.session)).putting("digest", .string(SendReceiptRecovery.nativeDigest(i.value()!)!))
            .putting("delivery_binding", ["schema": 1, "machine_id": .string(machine), "session_id": .string(session), "provider": "codex"])
        XCTAssertNotNil(SendReceiptRecovery.acknowledgement(i, positive))
        for key in ["machine_id", "session_id", "provider"] {
            let binding = positive.object("delivery_binding")!.putting(key, "foreign")
            XCTAssertNil(SendReceiptRecovery.acknowledgement(i, positive.putting("delivery_binding", .object(binding))))
        }
        XCTAssertNil(SendReceiptRecovery.acknowledgement(i, positive.putting("digest", .string(String(repeating: "0", count: 64)))))
    }
    func testCampaignCancelsEitherHungReadWithoutStartingAnother() async throws {
        let original = original
        for originalHangs in [true, false] {
            StubProtocol.reset { request in
                if !originalHangs && request.method == "GET" { return StubProtocol.json(200, #"{"request_id":"\#(original)","state":"uncertain"}"#) }
                return .hang
            }
            let c = Connection(id: "gateway", label: "Fixture", url: "https://gateway.example.test", token: "synthetic")
            do {
                _ = try await StubProtocol.client().checkDelivery(c, message: message(), intent: intent(), limit: .milliseconds(300))
                XCTFail("Hung read escaped the one campaign deadline")
            } catch { XCTAssertEqual(error as? ZerusError, Deadline.expired) }
            try await Task.sleep(for: .milliseconds(100))
            XCTAssertEqual(StubProtocol.requests.count, originalHangs ? 1 : 2)
            XCTAssertEqual(StubProtocol.stoppedHanging.withLock { $0 }, 1)
        }
    }

    func testNativeIdentitySurvivesThePrivateHistoryCodec() throws {
        let raw: JSONObject = ["type": "UserPromptSubmit", "source": "opencode_native", "detail": "exact text", "message_id": "msg_exact"]
        let row = try XCTUnwrap(NativeEvents.events(["provider_messages": .array([.object(raw)])]).first)
        XCTAssertEqual(row.nativeMessageId, "msg_exact")
        XCTAssertNil(NativeEvents.events(["provider_messages": .array([.object(raw.putting("message_id", nil).putting("id", "msg_invented"))])]).first?.nativeMessageId)
        let restored = try JSONDecoder().decode(Event.self, from: JSONEncoder().encode(row))
        XCTAssertEqual(restored.nativeMessageId, "msg_exact")
    }

    func testSharedParityRejectsAmbiguousMachinesAndNonBooleanQueued() throws {
        let vectors = try JSON.parse(Data(contentsOf: SharedFixture.url("send-receipt-phone-parity.json")))
        for row in vectors.objects("machines") {
            let catalog = (0..<row.int("count")).map { _ in Machine(connectionId: "gateway", id: machine, name: "Fixture", online: true, machineId: machine) }
            XCTAssertEqual(SendReceiptRecovery.machine(catalog, target: target) != nil, row.bool("selected"))
        }
        let i = intent()
        let positive = answer(original).putting("name", .string(target.session)).putting("digest", .string(SendReceiptRecovery.nativeDigest(i.value()!)!))
            .putting("delivery_binding", ["schema": 1, "machine_id": .string(machine), "session_id": .string(session), "provider": "codex"])
        for row in vectors.objects("queued") {
            let value = positive.putting("queued", row.bool("present") ? row["value"] : nil)
            XCTAssertEqual(SendReceiptRecovery.acknowledgement(i, value) != nil, row.bool("accepted"))
        }
    }

}
