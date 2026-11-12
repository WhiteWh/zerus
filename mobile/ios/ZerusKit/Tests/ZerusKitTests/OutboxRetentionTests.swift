import XCTest
import Synchronization
@testable import ZerusKit

final class OutboxRetentionTests: XCTestCase {
    private let machine = "33333333-3333-4333-8333-333333333333"
    private let session = "44444444-4444-4444-8444-444444444444"
    private var target: Target { Target(computerId: machine, session: "opencode/example/outbox", run: "run", conversation: "conversation", connectionId: "gateway") }
    private func row(_ text: String = "original", provider: String = "opencode", status: String = "submitted", compaction: String? = nil) -> OutgoingMessage {
        let id = UUID().uuidString.lowercased()
        let intent = SendReceiptRecovery.freeze(target: target, id: id, text: text, provider: provider, machineId: machine, sessionId: session,
                                                encoding: "python_default", compactionId: compaction)!
        return OutgoingMessage(requestId: id, target: target, text: text, attachments: [], createdAt: 1, status: status, submittedAt: 2, submittedText: text,
            sendIntent: intent, sendReceiptProof: SendReceiptProof(messageRequestId: id, intentSha256: intent.intentSha256,
                nativeDigest: SendReceiptRecovery.nativeDigest(intent.value()!)!, submittedAt: 2, queued: false,
                nativeMessageId: provider == "opencode" ? "msg_" + id.replacingOccurrences(of: "-", with: "") : ""))
    }
    private func native(_ row: OutgoingMessage) -> Event {
        var event = Event(id: "native:" + row.requestId, role: "You", text: row.text, time: "", at: 2, source: "opencode_native")
        event.nativeMessageId = row.sendReceiptProof!.nativeMessageId
        return event
    }
    private func represented(_ text: String = "original") -> OutgoingMessage {
        var row = row(text); row.coverage = OutgoingCoverage.native(row, native(row))!; row.status = "recorded"; return row
    }
    func testSharedCorrelationGoldensMatchBothNativeEncodings() throws {
        let bytes = try Data(contentsOf: SharedFixture.url("send-correlation.json"))
        let central = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../../../services/mobile/vectors/send_receipt/compaction.json")
        XCTAssertEqual(bytes, try Data(contentsOf: central))
        let fixture = try JSON.parse(bytes)
        XCTAssertEqual(fixture.int("max_settled_rows"), OutboxRetention.maxRows)
        XCTAssertEqual(fixture.int("max_settled_bytes"), OutboxRetention.maxBytes)
        for v in fixture.objects("vectors") {
            let i = try XCTUnwrap(v.object("intent")), p = try XCTUnwrap(i.object("payload")), expected = try XCTUnwrap(v.object("correlation"))
            let t = try XCTUnwrap(v.object("target"))
            let target = Target(computerId: t.string("computer"), session: t.string("session"), run: t.string("run"), conversation: t.string("conversation"),
                                connectionId: t.string("connection"), archiveId: t.string("archive"), agentId: t.string("agent"), parentConversation: t.string("parent_conversation"))
            let original = OutgoingMessage(requestId: p.string("request_id"), target: target, text: p.string("text"), attachments: [], createdAt: 1, status: "submitted",
                                           sendIntent: SendIntent(canonical: v.string("canonical"), intentSha256: expected.string("intent_sha256")))
            let compact = OutboxRetention.compact(original)
            XCTAssertEqual(try SendReceiptRecovery.encode(.object(XCTUnwrap(compact.sendCorrelation).json())), try SendReceiptRecovery.encode(.object(expected)), v.string("id"))
            XCTAssertEqual(compact.originalSendIntent(), original.sendIntent)
        }
    }
    func testExactIdentitySurvivesCompactionAndPersistence() throws {
        for provider in ["claude", "codex", "kimi", "dsh", "opencode"] {
            for option in [nil, "", "22222222-2222-4222-8222-222222222222"] as [String?] {
                let original = row("  e\u{301} 日本語 😀\nexact trailing space  ", provider: provider, compaction: option)
                let compact = OutboxRetention.compact(original)
                XCTAssertNil(compact.sendIntent); XCTAssertNotNil(compact.sendCorrelation); XCTAssertTrue(compact.hasOriginalSendIdentity)
                XCTAssertEqual(compact.originalSendIntent(), original.sendIntent)
                XCTAssertEqual(SendReceiptRecovery.exactNativeId(compact), SendReceiptRecovery.exactNativeId(original))
                XCTAssertEqual(compact.sendReceiptProof, original.sendReceiptProof)
                let restored = try JSONDecoder().decode(OutgoingMessage.self, from: JSONEncoder().encode(compact))
                XCTAssertEqual(restored, compact); XCTAssertEqual(restored.originalSendIntent(), original.sendIntent)
                XCTAssertFalse(String(decoding: try JSONEncoder().encode(compact.sendCorrelation), as: UTF8.self).contains("trailing space"))
            }
        }
    }
    func testEveryBindingFailsClosedAndUnknownVersionIsPreserved() throws {
        let original = row(), compact = OutboxRetention.compact(original), metadata = compact.sendCorrelation!.json()
        for key in ["schema", "header", "intent_sha256", "native_digest", "row_sha256"] {
            var bad = compact; bad.sendCorrelation = try SendCorrelation(metadata.putting(key, "foreign"))
            XCTAssertNil(bad.originalSendIntent(), key); XCTAssertNil(SendReceiptRecovery.exactNativeId(bad)); XCTAssertTrue(bad.hasOriginalSendIdentity)
        }
        var changed = compact; changed.text = "changed"
        var request = compact; request.requestId = UUID().uuidString.lowercased()
        var connection = compact; connection.target.connectionId = "other"
        var run = compact; run.target.run = "other"
        for bad in [changed, request, connection, run] { XCTAssertNil(bad.originalSendIntent()); XCTAssertNil(SendReceiptRecovery.exactNativeId(bad)) }
        var unknown = compact; unknown.sendCorrelation = try SendCorrelation(metadata.putting("schema", 2))
        XCTAssertEqual(unknown, try JSONDecoder().decode(OutgoingMessage.self, from: JSONEncoder().encode(unknown)))
        unknown.status = "recorded"; unknown.coverage = OutgoingCoverage(kind: "native", binding: OutboxRetention.binding(unknown), identity: "event")
        XCTAssertFalse(OutboxRetention.evictable(unknown))
        unknown.sendIntent = original.sendIntent
        XCTAssertEqual(OutboxRetention.compact(unknown), unknown); XCTAssertFalse(OutboxRetention.evictable(unknown))
        var badProof = original
        badProof.sendReceiptProof = SendReceiptProof(messageRequestId: original.requestId, intentSha256: original.sendIntent!.intentSha256,
            nativeDigest: String(repeating: "0", count: 64), submittedAt: 2, queued: false, nativeMessageId: "msg_invalid")
        XCTAssertNil(OutboxRetention.compact(badProof).sendCorrelation)
        // Swift canonical equivalence must not authorize dropping distinct UTF-8.
        let nfc = row("\u{e9}"); var nfd = nfc; nfd.text = "e\u{301}"
        nfd.sendIntent = nil; nfd.sendCorrelation = OutboxRetention.compact(nfc).sendCorrelation
        XCTAssertNil(nfd.originalSendIntent())
        XCTAssertNil(OutgoingCoverage.draft(nfc, Draft(target: target, text: "e\u{301}")))
        var value = try XCTUnwrap(JSON.parse(JSONEncoder().encode(compact)).object); value["sendCorrelation"] = 42
        XCTAssertThrowsError(try JSONDecoder().decode(OutgoingMessage.self, from: JSON.object(value).data()))
    }
    func testUnresolvedFailedAndUnrepresentedRowsRemainRecoverable() {
        var sending = row(status: "sending"); sending.sendReceiptProof = nil
        var uncertain = sending; uncertain.status = "uncertain"; uncertain.deliveryDismissed = true
        var failed = row(status: "failed"); failed.sendReceiptProof = nil
        var heuristic = represented(); heuristic.coverage = nil
        var reviewed = failed; reviewed.status = "reviewed"
        let protected = [sending, uncertain, failed, row(), heuristic, reviewed]
        let normalized = OutboxRetention.normalize(MessageState(outgoing: protected + (0..<300).map { _ in represented() }))
        XCTAssertEqual(normalized.outgoing.count, 100 + protected.count)
        XCTAssertEqual(normalized.outgoing[0].sendIntent, sending.sendIntent); XCTAssertEqual(normalized.outgoing[1].sendIntent, uncertain.sendIntent)
        XCTAssertEqual(normalized.outgoing[2].originalSendIntent(), failed.sendIntent); XCTAssertTrue(normalized.outgoing[1].deliveryDismissed)
        XCTAssertEqual(Array(normalized.outgoing.prefix(protected.count)).map(\.text), protected.map(\.text))
    }
    func testRepresentationRequiresExactIdentityAndCompleteOriginalContent() {
        let row = row(), echo = native(row)
        XCTAssertNotNil(OutgoingCoverage.native(row, echo))
        var wrongId = echo; wrongId.nativeMessageId = "msg_other"
        var wrongProvider = echo; wrongProvider.source = "codex_native"
        var inherited = echo; inherited.inheritedFrom = "parent"
        var transformed = echo; transformed.text = "provider transformed"
        var truncated = echo; truncated.detailTruncated = true
        for event in [wrongId, wrongProvider, inherited, transformed, truncated] { XCTAssertNil(OutgoingCoverage.native(row, event)) }
        var heuristic = echo; heuristic.source = "codex_native"; heuristic.nativeMessageId = ""
        let merged = ConversationMerge.merge(target: target, native: [heuristic], outgoing: [row])
        XCTAssertEqual(merged.recordedRequestIds, [row.requestId]); XCTAssertTrue(merged.exactRepresentations.isEmpty)
        let exact = ConversationMerge.merge(target: target, native: [echo], outgoing: [OutboxRetention.compact(row)])
        XCTAssertEqual(Set(exact.exactRepresentations.keys), [row.requestId])
        transformed.at = 1000
        let displayOnly = ConversationMerge.merge(target: target, native: [transformed], outgoing: [OutboxRetention.compact(row)])
        XCTAssertEqual(displayOnly.recordedRequestIds, [row.requestId]); XCTAssertTrue(displayOnly.exactRepresentations.isEmpty)
    }
    func testGlobalNewestRowAndByteLimitsAndPins() throws {
        let rows = (0..<10001).map { _ in represented() }
        let reduced = OutboxRetention.normalize(MessageState(outgoing: rows), pinned: [rows[0].requestId])
        XCTAssertEqual(reduced.outgoing.map(\.requestId), [rows[0].requestId] + rows.suffix(100).map(\.requestId))
        XCTAssertEqual(OutboxRetention.normalize(reduced).outgoing.count, 100)
        let large = (0..<35).map { _ in represented(String(repeating: "Я", count: 32768)) }
        let bounded = OutboxRetention.normalize(MessageState(outgoing: large))
        XCTAssertFalse(bounded.outgoing.isEmpty); XCTAssertLessThan(bounded.outgoing.count, 35)
        let bytes = try bounded.outgoing.reduce(0) { try $0 + JSONEncoder().encode($1).count }
        XCTAssertLessThanOrEqual(bytes, OutboxRetention.maxBytes); XCTAssertEqual(bounded.outgoing.last?.requestId, large.last?.requestId)
    }
    func testDraftTransferAndLateCallbacksCannotLoseOrRecreateComposition() throws {
        let file = Attachment(id: UUID().uuidString.lowercased(), name: "file.txt", mime: "text/plain", bytes: 4, sha256: String(repeating: "a", count: 64))
        let failed = OutgoingMessage(requestId: UUID().uuidString.lowercased(), target: target, text: "original", attachments: [file], createdAt: 1, status: "failed")
        var reviewed = failed; let draft = Draft(target: target, text: failed.text, attachments: failed.attachments)
        reviewed.status = "reviewed"; reviewed.coverage = OutgoingCoverage.draft(failed, draft)
        let bounded = OutboxRetention.normalize(MessageState(drafts: [draft], outgoing: [reviewed] + (0..<101).map { _ in represented() }))
        XCTAssertEqual(bounded.drafts, [draft]); XCTAssertFalse(bounded.outgoing.contains { $0.requestId == failed.requestId })
        XCTAssertNil(OutgoingCoverage.draft(failed, Draft(target: target, text: failed.text)))
        var echo = Event(id: "receipt", role: "You", text: failed.text, time: "", requestId: failed.requestId, source: "hgs_delivery")
        echo.attachments = [file.displayed()]
        XCTAssertNil(OutgoingCoverage.native(failed, echo))
        echo.attachments[0].messageId = failed.requestId; echo.attachments[0].index = 0
        XCTAssertNotNil(OutgoingCoverage.native(failed, echo))
        echo.attachments[0].sha256 = String(repeating: "b", count: 64)
        XCTAssertNil(OutgoingCoverage.native(failed, echo))
        let rows = (0..<110).map { _ in represented() }
        let decoded = try JSONDecoder().decode(MessageState.self, from: JSONEncoder().encode(MessageState(outgoing: rows)))
        let compact = OutboxRetention.normalize(decoded)
        XCTAssertEqual(compact, OutboxRetention.normalize(compact)); XCTAssertEqual(compact.outgoing.count, 100)
        XCTAssertEqual(compact, MessageChanges.result(compact, rows[0].requestId) { row in var copy = row; copy.status = "submitted"; return copy })
        var captured = rows[0]; captured.status = "uncertain"
        XCTAssertEqual(compact, SendReceiptRecovery.settle(compact, captured: captured, intent: rows[0].sendIntent!, proof: rows[0].sendReceiptProof!, deadline: 100, now: 99))
    }
    private final class Sink: Sendable {
        let data: Mutex<Data>
        let failing = Mutex(true)
        init(_ state: MessageState) throws { data = Mutex(try JSONEncoder().encode(state)) }
        func write(_ state: MessageState) throws {
            let encoded = try JSONEncoder().encode(state)
            if failing.withLock({ $0 }) { throw ZerusError.invalid("synthetic storage failure") }
            data.withLock { $0 = encoded }
        }
    }
    @MainActor func testFailedMigrationAndDraftTransferKeepAuthoritativeDataUntilSuccessfulCommit() async throws {
        var failed = row(status: "failed"); failed.sendReceiptProof = nil
        let recovered = Draft(target: target, text: failed.text, attachments: failed.attachments)
        let rows = (0..<110).map { _ in represented() }
        let before = MessageState(outgoing: [failed] + rows), sink = try Sink(before)
        let saved = sink.data.withLock { $0 }, captured = failed
        let store = OrderedStatePersistence(initial: before, normalize: { OutboxRetention.normalize($0) }, write: { try sink.write($0) })
        let transfer: @Sendable (MessageState) -> MessageState = { state in
            var copy = state
            copy.drafts += [recovered]
            copy.outgoing = state.outgoing.map { row in
                guard row.requestId == captured.requestId else { return row }
                var updated = row; updated.status = "reviewed"; updated.coverage = OutgoingCoverage.draft(row, recovered); return updated
            }
            return copy
        }
        do { _ = try await store.durable(transfer); XCTFail("Failed write acknowledged") } catch {}
        XCTAssertEqual(store.state, before); XCTAssertEqual(sink.data.withLock { $0 }, saved)
        sink.failing.withLock { $0 = false }
        let committed = try await store.durable(transfer, acknowledge: { _, written in written })
        XCTAssertEqual(committed.outgoing.count, 100); XCTAssertEqual(committed.drafts, [recovered])
        XCTAssertEqual(store.state, committed)
        XCTAssertEqual(try JSONDecoder().decode(MessageState.self, from: sink.data.withLock { $0 }), committed)
        let late = captured.requestId
        store.edit { MessageChanges.result($0, late) { row in var copy = row; copy.status = "submitted"; return copy } }
        let flushed = try await store.flush()
        XCTAssertEqual(flushed, committed); XCTAssertEqual(store.state, committed)
        store.close()
    }
    @MainActor func testDebouncedWriteReleasesPinsAndPublishesTheSameBoundedState() async throws {
        let rows = (0..<105).map { _ in represented() }, pins = OutboxPins()
        pins.replace(Set(rows.map(\.requestId)))
        let initial = MessageState(outgoing: rows), sink = try Sink(initial)
        sink.failing.withLock { $0 = false }
        let store = OrderedStatePersistence(initial: initial, debounce: .milliseconds(200),
            normalize: { OutboxRetention.normalize($0, pinned: pins.snapshot()) }, write: { try sink.write($0) })
        store.edit { $0 }
        XCTAssertEqual(store.state.outgoing.count, 105)
        pins.replace([])
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(store.state.outgoing.count, 100)
        XCTAssertEqual(try JSONDecoder().decode(MessageState.self, from: sink.data.withLock { $0 }), store.state)
        store.close()
    }

    func testCheckingCompactUncertaintyDoesNotReintroduceCanonicalPayloadCopies() {
        let original = row()
        var captured = OutboxRetention.compact(original); captured.status = "uncertain"
        let settled = SendReceiptRecovery.settle(MessageState(outgoing: [captured]), captured: captured,
            intent: original.sendIntent!, proof: original.sendReceiptProof!, deadline: 100, now: 99)
        let normalized = OutboxRetention.normalize(settled).outgoing[0]
        XCTAssertEqual(normalized.status, "submitted"); XCTAssertNil(normalized.sendIntent)
        XCTAssertEqual(normalized.originalSendIntent(), original.sendIntent)
    }

    func testMemoizationRechecksChangedRowsAndPinsAndDropsStaleReferences() {
        let engine = OutboxRetention.Normalizer()
        let initial = engine.normalize(MessageState(outgoing: (0..<100).map { _ in represented() }))
        let evaluations = engine.rowEvaluations, walks = engine.walks
        var typed = initial; typed.drafts = [Draft(target: target, text: "typing")]
        XCTAssertEqual(engine.normalize(typed), typed); XCTAssertEqual(engine.walks, walks)
        // Force a fresh COW buffer with the same values, and change only the pin input.
        var pinned = initial; pinned.outgoing = initial.outgoing.map { $0 }
        _ = engine.normalize(pinned, pinned: [initial.outgoing[0].requestId])
        XCTAssertEqual(engine.rowEvaluations, evaluations); XCTAssertEqual(engine.walks, walks + 1)
        pinned.outgoing[0].text = "changed"
        let checked = engine.normalize(pinned)
        XCTAssertEqual(engine.rowEvaluations, evaluations + 1); XCTAssertFalse(OutboxRetention.evictable(checked.outgoing[0]))
        let large = engine.normalize(MessageState(outgoing: (0..<700).map { _ in row(status: "submitted") }))
        XCTAssertEqual(large.outgoing.count, 700); XCTAssertLessThanOrEqual(engine.memoizedRows, 512)
        _ = engine.normalize(MessageState()); XCTAssertEqual(engine.memoizedRows, 0)
        // A Unicode-equivalent edit must miss the memo and lose eviction eligibility.
        var unicode = row("\u{e9}"); unicode.status = "recorded"; unicode.coverage = OutgoingCoverage.native(unicode, native(unicode))
        var state = engine.normalize(MessageState(outgoing: [unicode]))
        let before = engine.rowEvaluations
        state.outgoing[0].text = "e\u{301}"
        let changed = engine.normalize(state)
        XCTAssertEqual(engine.rowEvaluations, before + 1); XCTAssertFalse(OutboxRetention.evictable(changed.outgoing[0]))
    }
    @MainActor func testDraftEditingSkipsRetentionWalksOnThePublishingActor() throws {
        let pins = OutboxPins(), writer = OutboxRetention.Normalizer(), publication = OutboxRetention.Normalizer()
        let rows = (0..<58).map { _ in represented(String(repeating: "Я", count: 4096)) }
        let draft = Draft(target: target, text: "typing"), events = rows.map(native)
        pins.retain(delivery: [], receipts: [], events: events)
        let initial = publication.normalize(writer.normalize(MessageState(drafts: [draft], outgoing: rows), pinned: pins.snapshot()), pinned: pins.snapshot())
        let store = OrderedStatePersistence(initial: initial, debounce: .seconds(100), normalize: { writer.normalize($0, pinned: pins.snapshot()) },
            normalizePublished: { publication.normalize($0, pinned: pins.snapshot()) }, write: { _ in })
        let walks = publication.walks, evaluations = publication.rowEvaluations
        for i in 0..<100 {
            pins.retain(delivery: [], receipts: [], events: events)
            var editor = draft; editor.text = "typed-\(i)"; editor.revision = "revision-\(i)"
            let captured = editor
            store.edit { MessageChanges.text($0, captured) }
        }
        XCTAssertEqual(publication.walks, walks); XCTAssertEqual(publication.rowEvaluations, evaluations)
        XCTAssertEqual(store.state.outgoing, initial.outgoing)
        store.close()
    }

}
