import Foundation

/// Frozen, bounded header with no second copy of the composition. Unknown versions round-trip.
public struct SendCorrelation: Codable, Hashable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    private let value: JSONObject
    public var description: String { "SendCorrelation(retained)" }
    public var debugDescription: String { description }
    public init(_ value: JSONObject) throws {
        guard try JSONEncoder().encode(value).count <= 8192 else { throw ZerusError.invalid("Invalid saved send correlation.") }
        self.value = value
    }
    public init(from decoder: Decoder) throws { try self.init(JSONObject(from: decoder)) }
    public func encode(to encoder: Encoder) throws { try value.encode(to: encoder) }
    public func json() -> JSONObject { value }
    public func intent(_ row: OutgoingMessage) -> SendIntent? {
        guard Set(value.keys) == Set(["schema", "header", "intent_sha256", "native_digest", "row_sha256"]), value["schema"] == .int(1),
              value.string("row_sha256") == OutboxRetention.binding(row), var header = value.object("header"),
              var payload = header.object("payload"), !payload.has("text") else { return nil }
        payload["text"] = .string(row.text); header["payload"] = .object(payload)
        guard SendReceiptRecovery.valid(header), let canonical = try? SendReceiptRecovery.encode(.object(header)),
              SendReceiptRecovery.hash(canonical) == value.string("intent_sha256"),
              SendReceiptRecovery.nativeDigest(header) == value.string("native_digest") else { return nil }
        let result = SendIntent(canonical: canonical, intentSha256: value.string("intent_sha256"))
        return SendReceiptRecovery.matches(row, result) ? result : nil
    }
    public static func capture(_ row: OutgoingMessage, _ intent: SendIntent) -> SendCorrelation? {
        guard SendReceiptRecovery.matches(row, intent), var header = intent.value(), var payload = header.object("payload"),
              let digest = SendReceiptRecovery.nativeDigest(header) else { return nil }
        payload["text"] = nil; header["payload"] = .object(payload)
        return try? SendCorrelation(["schema": 1, "header": .object(header), "intent_sha256": .string(intent.intentSha256),
                                     "native_digest": .string(digest), "row_sha256": .string(OutboxRetention.binding(row))])
    }
}

/// Complete representation, distinct from the presentation-only `recorded` status.
public struct OutgoingCoverage: Codable, Hashable, Sendable {
    public let schema: Int
    public let kind: String
    public let binding: String
    public let identity: String
    public init(kind: String, binding: String, identity: String) { schema = 1; self.kind = kind; self.binding = binding; self.identity = identity }
    public func valid(_ row: OutgoingMessage) -> Bool {
        schema == 1 && ["native", "draft"].contains(kind) && !identity.isEmpty && identity.utf8.count <= 2048 && binding == OutboxRetention.binding(row)
    }
    public static func native(_ row: OutgoingMessage, _ event: Event) -> OutgoingCoverage? {
        guard !event.id.isEmpty, event.role == "You", (event.inheritedFrom ?? "").isEmpty, !event.detailTruncated,
              SendReceiptRecovery.exact(event.text, row.text) else { return nil }
        let request = ["hgs_delivery", "opencode_native"].contains(event.source) && event.requestId == row.requestId
        let id = SendReceiptRecovery.exactNativeId(row)
        let native = id != nil && (event.requestId.isEmpty || event.requestId == row.requestId) && event.nativeMessageId == id && event.source == (row.originalSendIntent()?.value()?.string("provider") ?? "") + "_native"
        guard request || native, event.attachments.count == row.attachments.count else { return nil }
        for i in row.attachments.indices {
            let local = row.attachments[i], remote = event.attachments[i]
            guard AttachmentIdentity.remote(remote), remote.messageId == row.requestId, remote.index == i,
                  SendReceiptRecovery.exact(remote.name, local.name), remote.mime == local.mime, remote.bytes == local.bytes, remote.sha256 == local.sha256 else { return nil }
        }
        let result = Self(kind: "native", binding: OutboxRetention.binding(row), identity: event.source + ":" + event.id)
        return result.valid(row) ? result : nil
    }
    public static func draft(_ row: OutgoingMessage, _ draft: Draft) -> OutgoingCoverage? {
        var candidate = row; candidate.target = draft.target; candidate.text = draft.text; candidate.attachments = draft.attachments
        guard OutboxRetention.binding(candidate) == OutboxRetention.binding(row) else { return nil }
        let result = Self(kind: "draft", binding: OutboxRetention.binding(row), identity: draft.generation)
        return result.valid(row) ? result : nil
    }
}

extension OutgoingMessage {
    public var hasOriginalSendIdentity: Bool { sendIntent != nil || sendCorrelation != nil }
    public func originalSendIntent() -> SendIntent? { sendIntent ?? sendCorrelation?.intent(self) }
}

/// Bounds redundant settled cache only; protected user backlog belongs to zerus-a4mub.47.
public enum OutboxRetention {
    public static let maxRows = 100
    public static let maxBytes = 1024 * 1024
    public static func binding(_ row: OutgoingMessage) -> String {
        let t = row.target
        var value: JSONObject = ["connection": .string(t.connectionId), "computer": .string(t.computerId), "session": .string(t.session),
                                 "run": .string(t.run), "conversation": .string(t.conversation), "request": .string(row.requestId),
                                 "text_sha256": .string(SendReceiptRecovery.hash(row.text)), "attachments": .array(row.attachments.map { .object($0.json()) })]
        if !t.archiveId.isEmpty { value["archive"] = .string(t.archiveId) }
        if !t.agentId.isEmpty { value["agent"] = .string(t.agentId); value["parent_conversation"] = .string(t.parentConversation) }
        return (try? SendReceiptRecovery.encode(.object(value))).map(SendReceiptRecovery.hash) ?? ""
    }
    public static func compact(_ row: OutgoingMessage) -> OutgoingMessage {
        guard row.sendCorrelation == nil, let intent = row.sendIntent, ["submitted", "recorded", "failed", "reviewed"].contains(row.status),
              let metadata = SendCorrelation.capture(row, intent) else { return row }
        if row.sendReceiptProof != nil && !validProof(row, intent) { return row }
        var copy = row; copy.sendIntent = nil; copy.sendCorrelation = metadata
        return copy
    }
    private static func validProof(_ row: OutgoingMessage, _ intent: SendIntent) -> Bool {
        guard let proof = row.sendReceiptProof, let value = intent.value() else { return false }
        return proof.messageRequestId == row.requestId && proof.intentSha256 == intent.intentSha256 && proof.nativeDigest == SendReceiptRecovery.nativeDigest(value) &&
            proof.submittedAt.isFinite && proof.submittedAt >= 0 &&
            (proof.nativeMessageId.isEmpty && value.string("provider") != "opencode" || SendReceiptRecovery.exactNativeId(row) != nil)
    }
    public static func evictable(_ row: OutgoingMessage, pinned: Set<String> = []) -> Bool {
        guard !pinned.contains(row.requestId), ["recorded", "reviewed"].contains(row.status), let coverage = row.coverage,
              coverage.valid(row), (row.status == "reviewed") == (coverage.kind == "draft") else { return false }
        if let metadata = row.sendCorrelation, metadata.intent(row) == nil { return false }
        return !row.hasOriginalSendIdentity || row.originalSendIntent() != nil
    }
    /// Stateless migration/contract entry point. Each live writer lane owns a Normalizer.
    public static func normalize(_ state: MessageState, pinned: Set<String> = []) -> MessageState {
        Normalizer().normalize(state, pinned: pinned)
    }

    /// COW storage identity is stable while the cached immutable array is retained.
    static func sameStorage<T>(_ a: [T], _ b: [T]) -> Bool {
        a.count == b.count && a.withUnsafeBufferPointer { left in b.withUnsafeBufferPointer { right in left.baseAddress == right.baseAddress } }
    }

    // Swift String equality admits canonical equivalence. Memo hits must preserve exact bytes.
    static func sameRow(_ a: OutgoingMessage, _ b: OutgoingMessage) -> Bool {
        guard a == b, a.createdAt.bitPattern == b.createdAt.bitPattern, a.submittedAt.bitPattern == b.submittedAt.bitPattern else { return false }
        let x = a.target, y = b.target
        let left = [a.requestId, a.text, a.status, a.submittedText, a.error, x.connectionId, x.computerId, x.session, x.run, x.conversation, x.archiveId, x.agentId, x.parentConversation]
        let right = [b.requestId, b.text, b.status, b.submittedText, b.error, y.connectionId, y.computerId, y.session, y.run, y.conversation, y.archiveId, y.agentId, y.parentConversation]
        guard zip(left, right).allSatisfy({ SendReceiptRecovery.exact($0, $1) }) else { return false }
        for (l, r) in zip(a.attachments, b.attachments) {
            guard zip([l.id, l.name, l.mime, l.sha256], [r.id, r.name, r.mime, r.sha256]).allSatisfy({ SendReceiptRecovery.exact($0, $1) }) else { return false }
        }
        if let l = a.sendIntent, let r = b.sendIntent, !SendReceiptRecovery.exact(l.intentSha256, r.intentSha256) { return false }
        if let l = a.sendCorrelation, let r = b.sendCorrelation,
           !SendReceiptRecovery.exact(JSON.object(l.json()).compact, JSON.object(r.json()).compact) { return false }
        if let l = a.sendReceiptProof, let r = b.sendReceiptProof {
            guard l.submittedAt.bitPattern == r.submittedAt.bitPattern else { return false }
            guard zip([l.messageRequestId, l.intentSha256, l.nativeDigest, l.nativeMessageId], [r.messageRequestId, r.intentSha256, r.nativeDigest, r.nativeMessageId])
                .allSatisfy({ SendReceiptRecovery.exact($0, $1) }) else { return false }
        }
        if let l = a.coverage, let r = b.coverage {
            guard zip([l.kind, l.binding, l.identity], [r.kind, r.binding, r.identity]).allSatisfy({ SendReceiptRecovery.exact($0, $1) }) else { return false }
        }
        return true
    }

    /// Bounded row metadata, with independent publication and background-writer instances.
    public final class Normalizer: @unchecked Sendable {
        private final class Memo {
            let row: OutgoingMessage
            let eligible: Bool
            let bytes: Int
            var seen: Int
            init(_ row: OutgoingMessage, eligible: Bool, bytes: Int, seen: Int) { self.row = row; self.eligible = eligible; self.bytes = bytes; self.seen = seen }
        }
        private let lock = NSLock()
        private var memo: [String: Memo] = [:]
        private var order: [String] = []
        private var outgoing: [OutgoingMessage]?
        private var pins: Set<String> = []
        private var walkCount = 0
        private var evaluationCount = 0
        var memoizedRows: Int { lock.withLock { memo.count } }
        var walks: Int { lock.withLock { walkCount } }
        var rowEvaluations: Int { lock.withLock { evaluationCount } }
        public init() {}
        public func normalize(_ state: MessageState, pinned: Set<String> = []) -> MessageState {
            lock.withLock {
                if let outgoing, sameStorage(outgoing, state.outgoing), pins == pinned { return state }
                walkCount += 1
                let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
                let rows: [Memo] = state.outgoing.map { row in
                    let entry: Memo
                    if let saved = memo[row.requestId], sameRow(saved.row, row) { entry = saved }
                    else {
                        evaluationCount += 1
                        let normalized = compact(row), eligible = evictable(normalized)
                        let bytes = eligible ? (try? encoder.encode(normalized).count) : nil
                        entry = Memo(normalized, eligible: eligible && bytes != nil, bytes: (bytes ?? 0) + 1, seen: walkCount)
                        if memo[row.requestId] == nil { order.append(row.requestId) }
                        memo[row.requestId] = entry
                        if memo.count > 512 { memo.removeValue(forKey: order.removeFirst()) }
                    }
                    entry.seen = walkCount
                    return entry
                }
                var count = 0, bytes = 2, retained = Array(repeating: true, count: rows.count)
                for i in rows.indices.reversed() {
                    let entry = rows[i]
                    guard entry.eligible, !pinned.contains(entry.row.requestId) else { continue }
                    if count >= maxRows || entry.bytes > maxBytes - bytes {
                        retained[i] = false
                        if memo[entry.row.requestId] === entry { memo.removeValue(forKey: entry.row.requestId) }
                    } else { count += 1; bytes += entry.bytes }
                }
                memo = memo.filter { $0.value.seen == walkCount }
                order.removeAll { memo[$0] == nil }
                var result = state
                let next = rows.enumerated().compactMap { retained[$0.offset] ? $0.element.row : nil }
                if next != state.outgoing { result.outgoing = next }
                outgoing = result.outgoing; pins = pinned
                return result
            }
        }
    }
    /// The caller must have verified this fresh frame's exact scope.
    public static func recorded(_ state: MessageState, _ ids: Set<String>, exact: [String: OutgoingCoverage]) -> MessageState {
        var copy = state
        copy.outgoing = state.outgoing.map { row in
            guard ids.contains(row.requestId), row.status != "reviewed" else { return row }
            var updated = row; updated.status = "recorded"; updated.error = ""
            if let coverage = exact[row.requestId], coverage.valid(row) { updated.coverage = coverage }
            return updated
        }
        return copy
    }
}

/// Process-local pins; no durable statuses or transport authority are reconstructed here.
public final class OutboxPins: @unchecked Sendable {
    private let lock = NSLock()
    private var ids: Set<String> = []
    private var delivery: Set<String>?
    private var receipts: Set<String>?
    private var events: [Event]?
    public init() {}
    public func replace(_ value: Set<String>) { lock.withLock { ids = value; delivery = nil; receipts = nil; events = nil } }
    public func retain(delivery: Set<String>, receipts: Set<String>, events: [Event]) {
        lock.withLock {
            if let old = self.events, delivery == self.delivery, receipts == self.receipts, OutboxRetention.sameStorage(old, events) { return }
            self.delivery = delivery; self.receipts = receipts; self.events = events
            let next = delivery.union(receipts.map { String($0.split(separator: ":").last ?? "") }).union(events.map(\.outgoingId).filter { !$0.isEmpty })
            if next != ids { ids = next }
        }
    }
    public func snapshot() -> Set<String> { lock.withLock { ids } }
}
