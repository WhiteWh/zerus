import Foundation

public struct SavedConversationViewport: Codable, Hashable, Sendable {
    public var eventId: String
    public var offset: Int
    public var cursor: String
    public var epoch: String
    public var following: Bool
    public init(eventId: String, offset: Int, cursor: String = "", epoch: String = "", following: Bool = false) {
        self.eventId = eventId
        self.offset = offset
        self.cursor = cursor
        self.epoch = epoch
        self.following = following
    }

    private enum CodingKeys: String, CodingKey { case eventId, offset, cursor, epoch, following }
    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        eventId = try values.decode(String.self, forKey: .eventId)
        offset = try values.decode(Int.self, forKey: .offset)
        cursor = try values.decode(String.self, forKey: .cursor)
        epoch = try values.decode(String.self, forKey: .epoch)
        // Existing saved positions represented deliberate reading locations.
        following = try values.decodeIfPresent(Bool.self, forKey: .following) ?? false
    }
}

/// Private read progress for one exact conversation, ported from Android `ConversationReadRecord`.
public struct ConversationReadRecord: Codable, Hashable, Sendable {
    public var target: Target
    public var seen: [String] = []
    public var viewport: SavedConversationViewport?
    public var incomplete = false
    public var epoch = ""
    public var readThrough: Int64 = 0
    public var headIncoming: Int64?
    public var headTotal: Int64?
    public var headComplete = false
    public var reviewLater = false
    public var established = false

    public init(target: Target, seen: [String] = [], viewport: SavedConversationViewport? = nil, incomplete: Bool = false, epoch: String = "",
                readThrough: Int64 = 0, headIncoming: Int64? = nil, headTotal: Int64? = nil, headComplete: Bool = false,
                reviewLater: Bool = false, established: Bool = false) {
        self.target = target
        self.seen = seen
        self.viewport = viewport
        self.incomplete = incomplete
        self.epoch = epoch
        self.readThrough = readThrough
        self.headIncoming = headIncoming
        self.headTotal = headTotal
        self.headComplete = headComplete
        self.reviewLater = reviewLater
        self.established = established
    }
}

public struct ConversationReadState: Codable, Hashable, Sendable {
    public var records: [ConversationReadRecord] = []
    public init(records: [ConversationReadRecord] = []) { self.records = records }

    public func record(_ target: Target) -> ConversationReadRecord? {
        let key = ConversationReadPolicies.key(target)
        return records.first { ConversationReadPolicies.key($0.target) == key }
    }

    func put(_ next: ConversationReadRecord) -> ConversationReadState {
        if records.count >= 500 && record(next.target) == nil { return self }
        let key = ConversationReadPolicies.key(next.target)
        return ConversationReadState(records: records.filter { ConversationReadPolicies.key($0.target) != key } + [next])
    }

    public func viewed(_ target: Target, _ ids: Set<String>) -> ConversationReadState {
        var previous = record(target) ?? ConversationReadRecord(target: target)
        let additions = ids.sorted().filter { !previous.seen.contains($0) }
        let used = records.reduce(0) { $0 + $1.seen.count }
        let available = min(max(50_000 - used, 0), max(5000 - previous.seen.count, 0))
        // Legacy exact IDs are never silently evicted. Native canonical epochs use a watermark.
        previous.seen += additions.prefix(available)
        previous.incomplete = previous.incomplete || additions.count > available
        previous.established = previous.established || !ids.isEmpty
        return put(previous)
    }

    public func visible(_ target: Target, _ events: [Event]) -> ConversationReadState {
        let incoming = events.filter(ConversationReadPolicies.incoming)
        let previous = record(target)
        let canonical = incoming.filter { previous?.epoch.isEmpty == false && $0.historyEpoch == previous?.epoch && $0.incomingSeq != nil }
        var next = self
        if let sequence = canonical.compactMap(\.incomingSeq).max(), var updated = previous {
            updated.readThrough = max(updated.readThrough, sequence)
            updated.established = true
            next = put(updated)
        }
        return next.viewed(target, Set(incoming.filter { $0.incomingSeq == nil || $0.historyEpoch != previous?.epoch }.map(\.id)))
    }

    /// Raises the watermark of the record's current epoch; it never lowers it.
    public func acknowledged(_ target: Target, through: Int64) -> ConversationReadState {
        guard var record = record(target), !record.epoch.isEmpty, through > record.readThrough else { return self }
        record.readThrough = through
        record.established = true
        return put(record)
    }

    public func head(_ target: Target, epoch: String, latest: Int64?, total: Int64?, complete: Bool) -> ConversationReadState {
        if epoch.isEmpty || epoch.count > 512 || (latest.map { $0 < 0 } ?? false) || (total.map { $0 < 0 } ?? false) { return self }
        var old = record(target) ?? ConversationReadRecord(target: target)
        let same = old.epoch == epoch
        old.readThrough = same ? old.readThrough : 0
        old.headIncoming = same ? (latest.map { max(old.headIncoming ?? 0, $0) } ?? old.headIncoming) : latest
        old.headTotal = same ? (total.map { max(old.headTotal ?? 0, $0) } ?? old.headTotal) : total
        old.headComplete = complete && latest != nil && total != nil
        old.epoch = epoch
        return put(old)
    }

    public func review(_ target: Target, _ value: Bool) -> ConversationReadState {
        var record = record(target) ?? ConversationReadRecord(target: target)
        record.reviewLater = value
        return put(record)
    }

    public func supersede(_ target: Target, _ aliases: [String: String]) -> ConversationReadState {
        guard let old = record(target) else { return self }
        var next = viewed(target, Set(aliases.filter { old.seen.contains($0.key) }.values))
        if let anchor = old.viewport, let alias = aliases[anchor.eventId] {
            next = next.viewport(target, eventId: alias, offset: anchor.offset, cursor: anchor.cursor, epoch: anchor.epoch, following: anchor.following)
        }
        return next
    }

    public func viewport(_ target: Target, eventId: String, offset: Int, cursor: String = "", epoch: String = "", following: Bool = false) -> ConversationReadState {
        if eventId.isEmpty || eventId.count > 2048 || offset < 0 || cursor.count > 8192 || epoch.count > 512 { return self }
        var previous = record(target) ?? ConversationReadRecord(target: target)
        previous.viewport = SavedConversationViewport(eventId: eventId, offset: offset, cursor: cursor, epoch: epoch, following: following)
        previous.established = true
        return put(previous)
    }

    public func promote(_ action: SessionAction) -> ConversationReadState {
        guard let destination = action.resultTarget, ActionDraftPolicies.proof(action, action.target, destination), record(destination) == nil,
              var previous = record(action.target) else { return self }
        previous.target = destination
        return put(previous)
    }

    /// Bounded state as loaded from storage.
    public func validated() -> ConversationReadState {
        var total = 0
        var rows: [ConversationReadRecord] = []
        for record in records.prefix(500) {
            guard record.seen.count <= 5000, record.seen.allSatisfy({ $0.count <= 2048 }), record.epoch.count <= 512, record.readThrough >= 0 else { continue }
            total += record.seen.count
            if total > 50_000 { break }
            rows.append(record)
        }
        return ConversationReadState(records: rows)
    }
}

/// A later canonical page can identify rows already read from the bounded native inspection.
public enum ConversationReadHead {
    /// Exact native-provided aliases of canonical rows in this page's epoch keep their seen identity; equal text never does.
    /// The watermark then advances only along consecutive incoming sequences whose rows are seen, stopping at the first
    /// unseen, missing or duplicated sequence, so no unseen incoming row is ever acknowledged and a later reply stays unread.
    public static func observe(_ state: ConversationReadState, _ page: HistoryPage) -> ConversationReadState {
        let canonical = page.events.filter { $0.historyEpoch == page.epoch && !$0.historyId.isEmpty && !$0.historyCursor.isEmpty }
        let aliases = HistoryAliases.exact(canonical, known: Set(state.record(page.target)?.seen ?? []))
        let next = state.head(page.target, epoch: page.epoch, latest: page.latestIncoming, total: page.totalIncoming, complete: page.complete)
            .supersede(page.target, aliases)
        guard let record = next.record(page.target), !record.epoch.isEmpty, record.epoch == page.epoch else { return next }
        var rows: [Int64: [String]] = [:]
        for event in canonical where ConversationReadPolicies.incoming(event) {
            if let sequence = event.incomingSeq { rows[sequence, default: []].append(event.id) }
        }
        let seen = Set(record.seen)
        var through = record.readThrough
        // A maximum watermark has no successor; checking first keeps the walk from overflowing.
        while through < Int64.max, let ids = rows[through + 1], ids.count == 1, seen.contains(ids[0]) { through += 1 }
        return next.acknowledged(page.target, through: through)
    }
}

public enum ConversationReadPolicies {
    public static func key(_ target: Target) -> String {
        JSON([target.connectionId, target.computerId, target.session, target.conversation, target.archiveId, target.agentId, target.parentConversation]).compact
    }

    /// A fork's inherited reply is its source's: never incoming, so never unread here.
    public static func incoming(_ event: Event) -> Bool {
        ["AgentMessage", "Stop"].contains(event.nativeType) && event.role != "You" && event.role != "You (answer)" && event.outgoingId.isEmpty &&
            (event.inheritedFrom ?? "").isEmpty
    }

    /// Canonical order proves the incoming prefix before a visible message position, including an own row.
    public static func incomingThroughPosition(_ epoch: String, _ ordered: [Event], _ visible: [Event]) -> [Event] {
        let incomingVisible = visible.filter(incoming)
        if epoch.isEmpty { return incomingVisible }
        func canonical(_ row: Event) -> Bool { row.historyEpoch == epoch && !row.historyId.isEmpty && !row.historyCursor.isEmpty }
        func position(_ row: Event) -> Bool {
            incoming(row) || (row.inheritedFrom ?? "").isEmpty && ["You", "You (answer)"].contains(row.role) &&
                ["UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted", "QuestionAnswered"].contains(row.nativeType)
        }
        let positions = visible.filter { canonical($0) && position($0) }.compactMap { shown -> Int? in
            let matches = ordered.indices.filter { index in
                let row = ordered[index]
                return canonical(row) && row.historyId == shown.historyId && row.historyCursor == shown.historyCursor &&
                    row.role == shown.role && row.nativeType == shown.nativeType && row.source == shown.source
            }
            return matches.count == 1 ? matches[0] : nil
        }
        guard let through = positions.max() else { return incomingVisible }
        let prefix = ordered.prefix(through + 1).filter { canonical($0) && incoming($0) && $0.incomingSeq != nil }
        var seen = Set<String>()
        return (prefix + incomingVisible).filter { seen.insert($0.id).inserted }
    }

    public struct Visibility: Hashable, Sendable {
        public let id: String
        public let epoch: String
        public let sequence: Int64?
    }

    /// Resample visible content when canonical read evidence changes, independently of its text or geometry.
    public static func visibilityEvidence(_ rows: [Event]) -> [Visibility] {
        rows.filter(incoming).map { Visibility(id: $0.id, epoch: $0.historyEpoch, sequence: $0.incomingSeq) }
    }

    public static func unread(_ record: ConversationReadRecord?, _ event: Event) -> Bool {
        guard incoming(event) else { return false }
        if let record, !record.epoch.isEmpty, record.epoch == event.historyEpoch, let sequence = event.incomingSeq {
            return sequence > record.readThrough
        }
        return !(record?.seen.contains(event.id) ?? false)
    }

    public static func lowerBound(_ record: ConversationReadRecord?, _ rows: [Event]?) -> Int? {
        var known: Int?
        if let record, !record.epoch.isEmpty, let head = record.headIncoming { known = Int(clamping: max(0, head - record.readThrough)) }
        if rows == nil && known == nil { return nil }
        let loaded = rows?.filter { unread(record, $0) }.count ?? 0
        return max(loaded, known ?? 0)
    }
}

/// A device-local review choice captures evidence; it never answers native questions.
public struct ReadAttentionEvidence: Hashable, Sendable {
    public var target: Target
    public var fingerprint: String
    public var epoch: String
    public var head: Int64?
    public var authoritative: Bool
    public var loadedIds: Set<String>
    public var loadedThrough: Int64?

    public init(target: Target, fingerprint: String, epoch: String, head: Int64?, authoritative: Bool, loadedIds: Set<String>, loadedThrough: Int64? = nil) {
        self.target = target
        self.fingerprint = fingerprint
        self.epoch = epoch
        self.head = head
        self.authoritative = authoritative
        self.loadedIds = loadedIds
        self.loadedThrough = loadedThrough
    }
}

public enum ReadAttentionPolicies {
    public static func mark(_ state: ConversationReadState, _ evidence: ReadAttentionEvidence) -> ConversationReadState {
        let old = state.record(evidence.target)
        let key = ConversationReadPolicies.key(evidence.target)
        if evidence.authoritative {
            guard let old, old.headComplete, old.epoch == evidence.epoch, old.headIncoming == evidence.head, let head = evidence.head else { return state }
            _ = old
            return ConversationReadState(records: state.records.map { record in
                guard ConversationReadPolicies.key(record.target) == key else { return record }
                var copy = record
                copy.readThrough = max(copy.readThrough, head)
                copy.reviewLater = false
                copy.established = true
                return copy
            })
        }
        var loaded = state.viewed(evidence.target, evidence.loadedIds).review(evidence.target, false)
        if !evidence.loadedIds.isEmpty, let through = evidence.loadedThrough, through >= 0, let old, !old.epoch.isEmpty, old.epoch == evidence.epoch {
            loaded = ConversationReadState(records: loaded.records.map { record in
                guard ConversationReadPolicies.key(record.target) == key else { return record }
                var copy = record
                copy.readThrough = max(copy.readThrough, through)
                copy.established = true
                return copy
            })
        }
        return loaded
    }
}

/// Opening may know unread exists before its canonical page is loaded.
public enum OpeningUnreadBoundary {
    public struct Decision: Hashable, Sendable {
        public let captured: Bool
        public let eventId: String
    }

    public static func decide(ready: Bool, record: ConversationReadRecord?, rows: [Event]) -> Decision {
        if !ready { return Decision(captured: false, eventId: "") }
        guard let record, record.established, !record.incomplete else { return Decision(captured: true, eventId: "") }
        if let candidate = rows.first(where: { ConversationReadPolicies.unread(record, $0) }) { return Decision(captured: true, eventId: candidate.id) }
        if !record.epoch.isEmpty && (record.headIncoming ?? 0) > record.readThrough { return Decision(captured: false, eventId: "") }
        return Decision(captured: true, eventId: "")
    }
}

public struct ReadAllResult: Hashable, Sendable {
    public var marked = 0
    public var skipped = 0
    public var partial = 0
    public init(marked: Int = 0, skipped: Int = 0, partial: Int = 0) {
        self.marked = marked
        self.skipped = skipped
        self.partial = partial
    }
}

public struct ReadAllCapture: Hashable, Sendable {
    public var target: Target
    public var evidence: ReadAttentionEvidence?
    public init(target: Target, evidence: ReadAttentionEvidence?) {
        self.target = target
        self.evidence = evidence
    }
}

public enum ReadAllPolicies {
    /// List-level Read all follows visible reply badges; an explicit review reminder stays actionable.
    public static func listCandidate(_ session: Session, online: Bool, reviewLater: Bool = false) -> Bool {
        !SessionFilters.archived(session) && (reviewLater || session.raw.bool("review_later") || session.raw.bool("mobile_review_later") ||
            !SharedReadPolicies.replyRead(session) && !SessionFilters.replyInProgress(session, online))
    }

    public static func available(_ evidence: ReadAttentionEvidence?) -> Bool {
        guard let evidence else { return false }
        return evidence.authoritative && !evidence.epoch.isEmpty && evidence.head != nil || !evidence.loadedIds.isEmpty
    }

    /// A known head past the watermark, or a loaded reply this phone counts as unread.
    public static func unreadReplies(_ record: ConversationReadRecord?, _ rows: [Event]) -> Bool {
        (record.map { !$0.epoch.isEmpty && ($0.headIncoming ?? 0) > $0.readThrough } ?? false) || rows.contains { ConversationReadPolicies.unread(record, $0) }
    }

    /// Cheap check before capturing evidence: a local reminder or replies counted unread.
    public static func pending(_ record: ConversationReadRecord?, _ rows: [Event]) -> Bool { record?.reviewLater == true || unreadReplies(record, rows) }

    /// Whether `apply` with this evidence would clear something this phone shows.
    public static func changes(_ record: ConversationReadRecord?, _ evidence: ReadAttentionEvidence?, _ rows: [Event], full: Bool = false) -> Bool {
        guard available(evidence), let evidence, !(record == nil && full), (record?.epoch ?? "") == evidence.epoch, record?.headIncoming == evidence.head,
              !evidence.authoritative || record?.headComplete == true else { return false }
        if record?.reviewLater == true { return true }
        if evidence.authoritative { return (evidence.head ?? 0) > (record?.readThrough ?? 0) }
        return rows.contains { evidence.loadedIds.contains($0.id) && ConversationReadPolicies.unread(record, $0) }
    }

    /// All evidence is captured before the single ordered edit; reducers never fetch or inspect UI state.
    public static func apply(_ state: ConversationReadState, captured: [ReadAllCapture], current: [ReadAllCapture])
        -> (state: ConversationReadState, result: ReadAllResult) {
        var next = state
        var result = ReadAllResult()
        var seen = Set<String>()
        for capture in captured where seen.insert(ConversationReadPolicies.key(capture.target)).inserted {
            let matching = current.first { $0.target == capture.target }?.evidence
            let old = next.record(capture.target)
            guard let evidence = capture.evidence, available(evidence), evidence.target == capture.target, evidence == matching,
                  (old?.epoch ?? "") == evidence.epoch, old?.headIncoming == evidence.head, !evidence.authoritative || old?.headComplete == true else {
                result.skipped += 1
                continue
            }
            let updated = ReadAttentionPolicies.mark(next, evidence)
            guard let record = updated.record(capture.target) else { result.skipped += 1; continue }
            next = updated
            result.marked += 1
            if !evidence.authoritative || record.incomplete { result.partial += 1 }
        }
        return (next, result)
    }
}

/// Seed metadata heads without opening chats, retaining bounded storage and scheduler ownership.
public enum ReadAllHeadCandidates {
    public static func candidates(_ sessions: [Session], _ state: ConversationReadState) -> [Session] {
        var slots = max(500 - state.records.count, 0)
        let recorded = Set(state.records.map { ConversationReadPolicies.key($0.target) })
        var seen = Set<String>()
        return sessions.filter { seen.insert(ConversationReadPolicies.key($0.target)).inserted }.filter { row in
            let target = row.target
            if SessionFilters.archived(row) || target.run.isEmpty || target.conversation.isEmpty { return false }
            if recorded.contains(ConversationReadPolicies.key(target)) { return true }
            if slots > 0 && (!row.raw.string("reply_id").isEmpty || row.raw.double("reply_at", default: 0) > 0 ||
                row.raw.bool("unread_reply") || row.raw.bool("mobile_unread_reply")) {
                slots -= 1
                return true
            }
            return false
        }
    }

    public static func hint(_ row: Session) -> String {
        JSON.array([.string(row.target.key), .string(HistoryHeadHints.signature(row.preview, row.raw)), .string(row.raw.string("reply_id")),
                    HistoryHeadHints.value(row.raw, "reply_at")]).compact
    }
}

public enum HistoryHeadHints {
    /// Numbers compare by value, so a copy that normalizes 1.0 to 1 is no native change.
    public static func value(_ raw: JSONObject?, _ key: String) -> JSON {
        guard let value = raw?[key] else { return .null }
        if let number = value.numberValue { return number == number.rounded() && abs(number) < 1e15 ? .int(Int64(number)) : .double(number) }
        return value
    }

    public static func signature(_ preview: String, _ raw: JSONObject) -> String {
        JSON.array([.string(preview), value(raw, "last_event_at"), value(raw, "turn_started"), value(raw, "last_response_at")]).compact
    }

    public static func readFingerprint(_ target: Target, _ raw: JSONObject?, _ record: ConversationReadRecord?) -> String {
        JSON.array([.string(target.key), raw.map { .string($0.string("phase")) } ?? .null, value(raw, "last_event_at"), value(raw, "last_response_at"),
                    raw.map { .string($0.string("prompt")) } ?? .null, raw?["mobile_attention"] ?? .null,
                    record.map { .string($0.epoch) } ?? .null, record?.headIncoming.map { .int($0) } ?? .null]).compact
    }
}

/// Fair bounded reads: an unavailable or indexing source cannot starve later chats.
public final class HistoryHeadScheduler {
    struct Attempt { var at: Double; var failures = 0; var next: Double = 0 }
    var attempts: [String: Attempt] = [:]
    public init() {}

    /// `now` is monotonic seconds.
    public func choose(_ targets: [Target], now: Double, limit: Int = 2) -> [Target] {
        var seen = Set<String>()
        let chosen = targets.filter { seen.insert(ConversationReadPolicies.key($0)).inserted }
            .filter { (attempts[ConversationReadPolicies.key($0)]?.next ?? 0) <= now }
            .enumerated()
            .sorted { (attempts[ConversationReadPolicies.key($0.element)]?.at ?? -.infinity, $0.offset) < (attempts[ConversationReadPolicies.key($1.element)]?.at ?? -.infinity, $1.offset) }
            .prefix(limit).map(\.element)
        for target in chosen {
            let key = ConversationReadPolicies.key(target)
            attempts[key] = Attempt(at: now, failures: attempts[key]?.failures ?? 0, next: now + 5)
        }
        return chosen
    }

    public func result(_ target: Target, now: Double, success: Bool) {
        let key = ConversationReadPolicies.key(target)
        guard let previous = attempts[key] else { return }
        let failures = success ? 0 : min(previous.failures + 1, 5)
        attempts[key] = Attempt(at: previous.at, failures: failures, next: now + (success ? 5 : min(5 * pow(2, Double(failures)), 120)))
    }
}

/// Read marks shared through the session's computer (`mark_read`). The computer applies a
/// field only while it is current, so marks never hide newer work.
/// An open conversation's `current_read` (`hgs inspect --current-read`): the reply and attention its computer would
/// accept now, from the same inspection this phone displays (zerus-a4mub.9).
public struct CurrentRead: Equatable, Sendable {
    public let reply: String?
    public let attention: String?
    public let attentionComplete: Bool
    public init(reply: String?, attention: String?, attentionComplete: Bool) {
        self.reply = reply
        self.attention = attention
        self.attentionComplete = attentionComplete
    }
}

/// What one settled viewport showed for the shared current acknowledgement (zerus-a4mub.9): the tail sentinel, the
/// newest displayed event row and the questions item.
public struct CurrentReadFlags: Equatable, Sendable {
    public let tail: Bool
    public let newest: Bool
    public let questions: Bool
    public init(tail: Bool = false, newest: Bool = false, questions: Bool = false) {
        self.tail = tail
        self.newest = newest
        self.questions = questions
    }
}

/// The conversation list's visible item ids mapped to reading inputs.
public enum ConversationReadViewport {
    /// One listed item of a settled viewport: its key, top offset and height in the viewport's coordinates (points).
    public struct Item: Equatable, Sendable {
        public let key: String
        public let offset: Double
        public let size: Double
        public init(key: String, offset: Double, size: Double) {
            self.key = key
            self.offset = offset
            self.size = size
        }
    }

    /// The readable part of an item (points; dp on Android): about one line of body text with its padding.
    public static let readableHeight: Double = 48
    /// Layout rounding every comparison tolerates.
    private static let rounding = 0.5

    /// Whether `item` counts as shown in the viewport from `start` to `end`, after the settled dwell, for both the
    /// canonical read and the current flags (zerus-a4mub.31). Android applies the same rule to the same shared layouts
    /// (mobile/fixtures/current-read.json): the part inside the viewport is at least min(its height, `readable`, the
    /// viewport height). A short item must be wholly visible; a taller one, however tall, needs its readable part.
    /// The 1-point tail sentinel counts when it starts inside the viewport and ends at most 2 points past it.
    public static func shows(_ item: Item, start: Double, end: Double, readable: Double = readableHeight) -> Bool {
        if item.key == "history:tail" {
            return item.offset + rounding >= start && item.offset + item.size <= end + 2 + rounding
        }
        let visible = min(item.offset + item.size, end) - max(item.offset, start)
        return visible > 0 && visible + rounding >= min(item.size, readable, end - start)
    }

    /// The keys of the items `shows` counts, top to bottom.
    public static func visible(_ items: [Item], start: Double, end: Double, readable: Double = readableHeight) -> [String] {
        items.filter { shows($0, start: start, end: end, readable: readable) }.sorted { $0.offset < $1.offset }.map(\.key)
    }

    /// The event ids among the visible item ids (`event:` keys).
    public static func eventIds(_ visible: [String]) -> Set<String> {
        Set(visible.filter { $0.hasPrefix("event:") }.map { String($0.dropFirst(6)) })
    }

    /// The current acknowledgement flags of one settled viewport: the tail sentinel, the newest displayed event row
    /// (`newestId`, empty when the latest messages are not shown) and the questions item among the visible ids.
    public static func flags(visible: [String], newestId: String) -> CurrentReadFlags {
        CurrentReadFlags(tail: visible.contains("history:tail"), newest: !newestId.isEmpty && eventIds(visible).contains(newestId),
                         questions: visible.contains("history:questions"))
    }
}

public enum SharedReadPolicies {
    private static let currentKeys: Set<String> = ["schema", "run_id", "conversation_id", "reply_id", "attention_signature", "attention_complete"]

    private static func signature(_ value: String) -> Bool {
        value.count == 64 && value.allSatisfy { ("0"..."9").contains($0) || ("a"..."f").contains($0) }
    }

    /// The verified open inspection's `current_read` when it names exactly `target`: schema 1, exactly the native
    /// fields, and the target's and the inspection's own run and conversation. Nil for archives, subagents, cached
    /// frames, older computers or anything malformed; nothing is repaired.
    public static func current(_ raw: JSONObject?, target: Target, verified: Bool) -> CurrentRead? {
        guard verified, let raw, target.archiveId.isEmpty, target.agentId.isEmpty, !target.run.isEmpty, !target.conversation.isEmpty,
              raw["run_id"] == .string(target.run), raw["conversation_id"] == .string(target.conversation),
              let value = raw.object("current_read"), Set(value.keys) == currentKeys, value["schema"] == .int(1),
              value["run_id"] == .string(target.run), value["conversation_id"] == .string(target.conversation),
              case .bool(let complete)? = value["attention_complete"] else { return nil }
        let reply: String?
        switch value["reply_id"] {
        case .null?: reply = nil
        case .string(let text)? where !text.isEmpty && text.utf16.count <= 256 && !text.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }):
            reply = text
        default: return nil
        }
        let attention: String?
        switch value["attention_signature"] {
        case .null?: attention = nil
        case .string(let text)? where signature(text): attention = text
        default: return nil
        }
        return CurrentRead(reply: reply, attention: attention, attentionComplete: complete)
    }

    /// What one settled viewport of the open conversation showed of its current inspection: the reply when it shows
    /// the latest messages and the tail or the newest displayed row was visible; the attention when the listed
    /// questions were visible or, without questions, when the reply was shown.
    public static func currentShown(latest: Bool, questionsListed: Bool, flags: CurrentReadFlags) -> (reply: Bool, attention: Bool) {
        let reply = latest && (flags.tail || flags.newest)
        return (reply, latest && (questionsListed ? flags.questions : reply))
    }

    /// The `mark_read` the open conversation may send for `current`: its reply once the conversation's tail or newest
    /// row stayed visible, its attention once complete and its presentation stayed visible. Values the computer already
    /// has are left out; nil when nothing is new. This never moves the phone's private read position, which only
    /// canonical history can advance.
    public static func currentPayload(_ row: Session?, target: Target, current: CurrentRead, replyShown: Bool,
                                      attentionShown: Bool) -> JSONObject? {
        if let row, SessionFilters.archived(row) { return nil }
        let read = row.flatMap(mark)
        let reply = current.reply.flatMap { replyShown && read?.string("reply_id") != $0 ? $0 : nil }
        let attention = current.attention.flatMap {
            attentionShown && current.attentionComplete && read?.string("attention_signature") != $0 ? $0 : nil
        }
        if reply == nil && attention == nil { return nil }
        return ["run_id": .string(target.run), "conversation_id": .string(target.conversation),
                "reply_id": reply.map(JSON.string) ?? .null, "attention_signature": attention.map(JSON.string) ?? .null]
    }

    public static func mark(_ row: Session) -> JSONObject? {
        guard let read = row.raw.object("read"), !row.target.conversation.isEmpty, read.string("conversation_id") == row.target.conversation else { return nil }
        return read
    }

    public static func replyRead(_ row: Session) -> Bool {
        let reply = row.raw.string("reply_id")
        return !reply.isEmpty && mark(row)?.string("reply_id") == reply
    }

    public static func attentionAcknowledged(_ row: Session) -> Bool {
        let signature = row.raw.string("attention_signature")
        return !signature.isEmpty && mark(row)?.string("attention_signature") == signature
    }

    /// Whether a `mark_read` receipt shows the computer applied every field this phone sent. A completed request can
    /// apply nothing: the value is no longer current, or the computer's question index is not ready. That needs a newer
    /// snapshot, so the same mark waits.
    public static func applied(_ receipt: JSONObject, payload: JSONObject) -> Bool {
        guard receipt.string("state") == "completed", let result = receipt.object("result"), result["ok"] == .bool(true),
              let applied = result.object("applied") else { return false }
        return ["reply_id", "attention_signature"].allSatisfy { field in (payload[field] ?? .null) == .null || applied[field] == .bool(true) }
    }

    /// Delay before the same unapplied mark may be sent again: two minutes, doubling up to 16.
    public static func retryDelay(_ unapplied: Int) -> Duration { .seconds(120 << min(max(unapplied, 0), 3)) }

    /// The `mark_read` payload once this phone has shown the current reply; nil when nothing is new.
    public static func payload(_ row: Session, freshHead: Bool, localUnread: Int?, acknowledge: Bool = true) -> JSONObject? {
        let target = row.target
        guard freshHead, localUnread == 0, target.archiveId.isEmpty, target.agentId.isEmpty, !target.run.isEmpty, !target.conversation.isEmpty,
              row.raw.string("run_id") == target.run, row.raw.string("conversation_id") == target.conversation, !SessionFilters.archived(row) else { return nil }
        let replyValue = row.raw.string("reply_id")
        let reply = !replyValue.isEmpty && !replyRead(row) ? replyValue : nil
        let signature = row.raw.string("attention_signature")
        let valid = signature.count == 64 && signature.allSatisfy { $0.isHexDigit && !$0.isUppercase }
        let attention = acknowledge && valid && !attentionAcknowledged(row) ? signature : nil
        if reply == nil && attention == nil { return nil }
        return ["run_id": .string(target.run), "conversation_id": .string(target.conversation),
                "reply_id": reply.map(JSON.string) ?? .null, "attention_signature": attention.map(JSON.string) ?? .null]
    }
}

/// Read-state decoration over the whole catalog; prepared off the main actor.
public enum SessionReadAttention {
    public static func prepare(_ sessions: [Session], _ state: ConversationReadState, ready: Bool, heads: Set<String>,
                               loaded: [Target: [Event]]) -> [Session] {
        var records: [String: ConversationReadRecord] = [:]
        for record in state.records where records[ConversationReadPolicies.key(record.target)] == nil {
            records[ConversationReadPolicies.key(record.target)] = record
        }
        return sessions.map { row in
            let key = ConversationReadPolicies.key(row.target)
            let record = records[key]
            var unreadCount: Int?
            if ready, let record, record.headComplete, heads.contains(key), let head = record.headIncoming {
                unreadCount = Int(clamping: max(0, head - record.readThrough))
            }
            let loadedCount = !ready || record?.incomplete == true || (state.records.count >= 500 && record == nil)
                ? nil : ConversationReadPolicies.lowerBound(record, loaded[row.target])
            let later = record?.reviewLater == true
            let unread = !SharedReadPolicies.replyRead(row) && (unreadCount ?? loadedCount ?? 0) > 0
            let acknowledged = SharedReadPolicies.attentionAcknowledged(row)
            if row.raw.bool("mobile_review_later") == later && row.raw.bool("mobile_unread_reply") == unread &&
                row.raw.bool("attention_acknowledged") == acknowledged { return row }
            var copy = row
            copy.raw["mobile_review_later"] = .bool(later)
            copy.raw["mobile_unread_reply"] = .bool(unread)
            copy.raw["attention_acknowledged"] = .bool(acknowledged)
            return copy
        }
    }
}

public struct UnreadBadge: Hashable, Sendable {
    public let text: String
    public let description: String
}

public enum UnreadPresentation {
    public static func badge(total: Int?, loaded: Int?, nativeUnread: Bool = false) -> UnreadBadge? {
        if let total, total > 0 { return UnreadBadge(text: "\(total)", description: "\(total) unread messages") }
        if total != nil { return nil }
        if let loaded, loaded > 0 { return UnreadBadge(text: "\(loaded)+", description: "At least \(loaded) unread messages; the total is not reported") }
        if nativeUnread { return UnreadBadge(text: "?", description: "The computer reports an unread reply; the total is not reported") }
        return nil
    }

    public struct Opening: Hashable, Sendable {
        public let anchor: ConversationAnchor?
        public let unavailableSaved: Bool
    }

    /// The first reading position: remembered or saved place, else the unread divider, else the newest message.
    /// A missing saved position never silently drops the reader at the newest message.
    public static func opening(memory: ConversationAnchor?, saved: SavedConversationViewport?, boundary: String, keys: [String]) -> Opening {
        let remembered = memory.flatMap { $0.following ? nil : $0 } ?? saved.flatMap { $0.following ? nil : ConversationAnchor(itemKey: "event:" + $0.eventId, offset: $0.offset, following: false) }
        if let remembered, keys.contains(remembered.itemKey) { return Opening(anchor: remembered, unavailableSaved: false) }
        if !boundary.isEmpty && keys.contains("history:unread") {
            return Opening(anchor: ConversationAnchor(itemKey: "history:unread", offset: 0, following: false), unavailableSaved: remembered != nil)
        }
        if remembered != nil {
            return Opening(anchor: keys.first { $0.hasPrefix("event:") }.map { ConversationAnchor(itemKey: $0, offset: 0, following: false) }, unavailableSaved: true)
        }
        return Opening(anchor: nil, unavailableSaved: false)
    }
}

/// Read-operation ownership and delayed presentation, using an injected monotonic clock.
public final class LoadingGate {
    public struct Operation: Hashable, Sendable {
        public let id: Int64
        public let scope: String
        public let startedAt: Double
        public var explicit: Bool
    }
    public struct Start: Sendable {
        public let operation: Operation
        public let newRequest: Bool
    }

    private let now: () -> Double
    private let delay: Double
    private var sequence: Int64 = 0
    public private(set) var current: Operation?

    public init(now: @escaping () -> Double, delay: Double = 5) {
        self.now = now
        self.delay = delay
    }

    public func begin(_ scope: String, explicit: Bool = false) -> Start {
        if var existing = current, existing.scope == scope {
            existing.explicit = existing.explicit || explicit
            current = existing
            return Start(operation: existing, newRequest: false)
        }
        sequence += 1
        let operation = Operation(id: sequence, scope: scope, startedAt: now(), explicit: explicit)
        current = operation
        return Start(operation: operation, newRequest: true)
    }

    public func owns(_ id: Int64) -> Bool { current?.id == id }
    public func visible() -> Bool { current.map { $0.explicit || now() - $0.startedAt >= delay } ?? false }
    public func remaining(_ id: Int64) -> Double {
        guard let current, current.id == id else { return 0 }
        return current.explicit ? 0 : max(0, delay - (now() - current.startedAt))
    }
    @discardableResult public func finish(_ id: Int64) -> Bool {
        guard owns(id) else { return false }
        current = nil
        return true
    }
    public func cancel() { current = nil }
}
