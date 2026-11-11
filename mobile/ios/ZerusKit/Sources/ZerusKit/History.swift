import Foundation

/// Public display rows only. Cached history never verifies native input identity.
public enum RollingConversationHistory {
    public struct Result: Sendable {
        public let events: [Event]
        public let truncated: Bool
        public let supersededIds: [String: String]
    }

    /// With `inspection`, `incoming` is a whole native inspection: the only source of queued and not-delivered rows,
    /// so earlier ones are not retained.
    public static func merge(target: Target, previousTarget: Target?, previous: [Event], incoming: [Event],
                             maxEvents: Int = 500, maxBytes: Int = 2 * 1024 * 1024, inspection: Bool = false) -> Result {
        var order: [String] = []
        var rows: [String: Event] = [:]
        func put(_ event: Event) {
            if rows.updateValue(event, forKey: event.id) == nil { order.append(event.id) }
        }
        func remove(_ id: String) {
            if rows.removeValue(forKey: id) != nil { order.removeAll { $0 == id } }
        }
        if target == previousTarget { previous.filter { !inspection || $0.deliveryState == nil }.forEach(put) }
        for row in incoming {
            if let voice = row.voice {
                let latest = VoiceProtocol.newer(rows[row.id]?.voice, voice)
                var copy = row
                copy.voice = latest
                copy.text = latest.transcript
                copy.attachments = [latest.audio]
                put(copy)
            } else {
                put(row)
            }
        }
        var aliases: [String: String] = [:]
        for canonical in incoming where !canonical.historyId.isEmpty {
            for original in NativeEvents.unique((canonical.originalIds + [canonical.originalId]).filter { !$0.isEmpty }) {
                if let legacy = rows[original], legacy.id != canonical.id {
                    remove(legacy.id)
                    aliases[legacy.id] = canonical.id
                }
            }
        }
        for voice in incoming where voice.voice != nil {
            for original in voice.originalIds {
                if let legacy = rows[original], legacy.voice == nil, legacy.role == "You", original != voice.id {
                    remove(original)
                    aliases[original] = voice.id
                }
            }
        }
        var reconciled = order.compactMap { rows[$0] }
        // Native Stop is a hook excerpt of a later public provider reply, not a second message.
        var superseded = aliases
        func replaces(_ provider: Event, _ hook: Event) -> Bool {
            hook.nativeType == "Stop" && hook.id.hasPrefix("journal:") && !hook.text.trimmingCharacters(in: .whitespaces).isEmpty &&
                provider.nativeType == "AgentMessage" && provider.id.hasPrefix("provider:") && provider.at.isFinite && hook.at.isFinite &&
                NativeStopExcerpt.matches(provider.text, hook.text) && abs(provider.at - hook.at) < 10
        }
        let all = reconciled
        reconciled.removeAll { hook in
            let candidates = all.filter { replaces($0, hook) }
            guard candidates.count == 1 else { return false }
            let demonstrated = all.filter { replaces(candidates[0], $0) }.count == 1
            if demonstrated { superseded[hook.id] = candidates[0].id }
            return demonstrated
        }
        var replaced = Set<String>()
        for receipt in reconciled where receipt.source == "hgs_delivery" && receipt.role == "You" && !receipt.requestId.isEmpty && !receipt.submittedText.isEmpty {
            let candidates = reconciled.filter {
                !replaced.contains($0.id) && $0.role == "You" && $0.receiptRequestId.isEmpty && NativeEvents.userTypes.contains($0.nativeType) &&
                    AttachmentReceiptMatch.matches(receipt, $0)
            }
            guard candidates.count == 1 else { continue }
            let candidate = candidates[0]
            let competing = reconciled.filter {
                $0.source == "hgs_delivery" && $0.role == "You" && !$0.requestId.isEmpty && AttachmentReceiptMatch.matches($0, candidate)
            }.count
            if competing == 1 {
                replaced.insert(candidate.id)
                superseded[candidate.id] = receipt.id
            }
        }
        reconciled.removeAll { replaced.contains($0.id) }
        let ordered = reconciled.enumerated().sorted { lhs, rhs in
            let a = lhs.element.at.isFinite ? lhs.element.at : 0, b = rhs.element.at.isFinite ? rhs.element.at : 0
            return a != b ? a < b : lhs.offset < rhs.offset
        }.map(\.element)
        var retained = ArraySlice(ordered)
        var bytes = 2 + ordered.reduce(0) { $0 + $1.approximateBytes + 1 }
        var truncated = false
        while retained.count > maxEvents || (bytes > maxBytes && !retained.isEmpty) {
            bytes -= retained.first!.approximateBytes + 1
            retained = retained.dropFirst()
            truncated = true
        }
        let events = Array(retained)
        let kept = Set(events.map(\.id))
        return Result(events: events, truncated: truncated, supersededIds: superseded.filter { kept.contains($0.value) })
    }
}

/// Merges native rows with this phone's outgoing envelopes, ported from Android `ConversationMerge`.
public enum ConversationMerge {
    public struct Result: Sendable {
        public let events: [Event]
        public let recordedRequestIds: Set<String>
        public init(events: [Event], recordedRequestIds: Set<String>) {
            self.events = events
            self.recordedRequestIds = recordedRequestIds
        }
    }

    public static func merge(target: Target, native: [Event], outgoing: [OutgoingMessage]) -> Result {
        var result = native
        var used = Set<Int>()
        var recorded = Set<String>()
        let mine = outgoing.filter { $0.belongs(to: target) && $0.status != "reviewed" }
            .enumerated().sorted { $0.element.createdAt != $1.element.createdAt ? $0.element.createdAt < $1.element.createdAt : $0.offset < $1.offset }
            .map(\.element)
        for message in mine {
            // Only a user row from durable native receipt metadata, or a native OpenCode prompt naming the request that
            // sent it, is authoritative by request ID; a receipt comes first. Only a canonical request names a row.
            func named(_ source: String) -> Int? {
                guard AttachmentPolicy.validId(message.requestId) else { return nil }
                return result.indices.first { index in
                    !used.contains(index) && result[index].role == "You" && result[index].source == source && result[index].requestId == message.requestId
                }
            }
            var match = named("hgs_delivery") ?? named("opencode_native")
            if match == nil, let nativeId = SendReceiptRecovery.exactNativeId(message) {
                let candidates = result.indices.filter { index in
                    let event = result[index]
                    return !used.contains(index) && event.role == "You" && (event.inheritedFrom ?? "").isEmpty &&
                        event.source == (message.sendIntent?.value()?.string("provider") ?? "") + "_native" && event.nativeMessageId == nativeId
                }
                if candidates.count == 1 { match = candidates[0] }
            }
            if match == nil, message.sendIntent == nil, message.sendReceiptProof == nil, ["submitted", "recorded"].contains(message.status), message.submittedAt > 0 {
                let text = (message.submittedText.isEmpty ? message.text : message.submittedText).trimmingCharacters(in: .whitespacesAndNewlines)
                if !text.isEmpty {
                    // A fork's inherited row was never sent from this session: it is
                    // no candidate, so it neither takes the send nor makes it ambiguous.
                    let candidates = result.indices.filter { index in
                        let event = result[index]
                        return !used.contains(index) && event.role == "You" && (event.inheritedFrom ?? "").isEmpty && event.requestId.isEmpty &&
                            event.text.trimmingCharacters(in: .whitespacesAndNewlines) == text && abs(event.at - message.submittedAt) <= 10
                    }
                    if candidates.count == 1 {
                        let index = candidates[0]
                        let competing = outgoing.filter { other in
                            other.belongs(to: target) && ["submitted", "recorded"].contains(other.status) && other.submittedAt > 0 &&
                                (other.submittedText.isEmpty ? other.text : other.submittedText).trimmingCharacters(in: .whitespacesAndNewlines) ==
                                result[index].text.trimmingCharacters(in: .whitespacesAndNewlines) && abs(result[index].at - other.submittedAt) <= 10
                        }.count
                        if competing == 1 { match = index }
                    }
                }
            }
            if let match {
                used.insert(match)
                recorded.insert(message.requestId)
                var event = result[match]
                event.id = "outgoing:\(message.requestId)"
                event.outgoingId = message.requestId
                event.attachments = event.attachments.isEmpty ? message.attachments.map { $0.displayed() }
                    : AttachmentIdentity.withLocal(event.attachments, message.attachments)
                event.delivery = "recorded"
                result[match] = event
            } else {
                result.append(Event(id: "outgoing:\(message.requestId)", role: "You",
                                    text: message.submittedText.isEmpty ? message.text : message.submittedText, time: "",
                                    at: message.submittedAt > 0 ? message.submittedAt : message.createdAt, requestId: message.requestId,
                                    attachments: message.attachments.map { $0.displayed() }, source: "mobile_outbox", delivery: message.status,
                                    outgoingId: message.requestId, deliveryState: ["submitted", "recorded"].contains(message.status) && message.sendReceiptProof?.queued == true ? "queued" : nil))
            }
        }
        var ordered = Array(result.prefix(native.count))
        for fallback in result.dropFirst(native.count) {
            if let position = ordered.firstIndex(where: { $0.at > fallback.at }) { ordered.insert(fallback, at: position) }
            else { ordered.append(fallback) }
        }
        return Result(events: ordered, recordedRequestIds: recorded)
    }
}

public enum AttachmentIdentity {
    public static let operation = "attachment_read"
    public static let chunkBytes = 512 * 1024
    public static let updateRequired = "Update Zerus on this computer to open attachments."

    public static func validMessageId(_ value: String) -> Bool { AttachmentPolicy.validId(value) }

    /// A native receipt position the computer can resolve; the phone never names a path.
    public static func remote(_ file: DisplayedAttachment) -> Bool {
        validMessageId(file.messageId) && (0...7).contains(file.index) && (1...AttachmentPolicy.maxFileBytes).contains(file.bytes)
    }

    /// This phone's private encrypted copy of a draft/outbox file, when still present.
    public static func local(_ file: DisplayedAttachment) -> Attachment? {
        guard !file.localId.isEmpty else { return nil }
        let attachment = Attachment(id: file.localId, name: file.name, mime: file.mime, bytes: file.bytes, sha256: file.sha256)
        return (try? AttachmentPolicy.validate(attachment)) == nil ? nil : attachment
    }

    public static func conversation(_ target: Target) -> String { target.agentId.isEmpty ? target.conversation : target.parentConversation }

    /// Native rows of this phone's own message keep its private copies, by position.
    public static func withLocal(_ native: [DisplayedAttachment], _ local: [Attachment]) -> [DisplayedAttachment] {
        native.enumerated().map { index, file in
            let position = file.index >= 0 ? file.index : index
            guard local.indices.contains(position), local[position].name == file.name, local[position].bytes == file.bytes else { return file }
            var copy = file
            copy.localId = local[position].id
            copy.sha256 = local[position].sha256
            return copy
        }
    }
}

// MARK: Canonical history pages

public struct HistoryPage: Sendable {
    public var target: Target
    public var epoch: String
    public var events: [Event]
    public var before: String?
    public var after: String?
    public var hasBefore: Bool
    public var hasAfter: Bool
    public var latestIncoming: Int64?
    public var totalIncoming: Int64?
    public var complete: Bool
    public var indexing: Bool
    public var truncated: Bool
    public var projectionApplied: Int64

    public init(target: Target, epoch: String, events: [Event], before: String?, after: String?, hasBefore: Bool, hasAfter: Bool,
                latestIncoming: Int64?, totalIncoming: Int64?, complete: Bool, indexing: Bool, truncated: Bool, projectionApplied: Int64 = 0) {
        self.target = target
        self.epoch = epoch
        self.events = events
        self.before = before
        self.after = after
        self.hasBefore = hasBefore
        self.hasAfter = hasAfter
        self.latestIncoming = latestIncoming
        self.totalIncoming = totalIncoming
        self.complete = complete
        self.indexing = indexing
        self.truncated = truncated
        self.projectionApplied = projectionApplied
    }

    public static func parse(target: Target, id: String, raw: JSONObject) throws -> HistoryPage {
        func require(_ condition: Bool) throws { if !condition { throw ZerusError.mismatch("Computer returned an invalid history page.") } }
        try require(raw.string("request_id") == id && raw.string("name") == target.session && raw.string("run_id") == target.run &&
                    raw.string("conversation_id") == target.conversation)
        try require(raw.string("archive_id") == target.archiveId && raw.string("agent_id") == target.agentId &&
                    (target.agentId.isEmpty || raw.string("parent_conversation_id") == target.parentConversation))
        guard case .string(let epoch)? = raw["history_epoch"] else { throw ZerusError.mismatch("Computer returned an invalid history page.") }
        try require(!epoch.isEmpty && epoch.count <= 512)
        func cursor(_ name: String) throws -> String? {
            guard case .string(let value)? = raw[name] else { return nil }
            try require(!value.isEmpty && value.count <= 8192)
            return value
        }
        guard let head = raw.object("head") else { throw ZerusError.mismatch("Computer returned an invalid history page.") }
        func number(_ name: String) throws -> Int64? {
            guard let value = head[name] else { return nil }
            if value.isNull { return nil }
            guard case .int(let number) = value, number >= 0 else { throw ZerusError.mismatch("Computer returned an invalid history page.") }
            return number
        }
        let complete = head.bool("complete"), latest = try number("incoming_seq"), total = try number("total_incoming")
        try require(!complete || (latest != nil && total != nil))
        guard let nativeRows = raw.array("events") else { throw ZerusError.mismatch("Computer returned an invalid history page.") }
        try require(nativeRows.count <= 100)
        for row in nativeRows.compactMap(\.object) {
            let history = row.string("history_id"), cursorValue = row.string("history_cursor")
            try require(!history.isEmpty && history.count <= 2048 && !cursorValue.isEmpty && cursorValue.count <= 8192)
            if let sequence = row["incoming_seq"], !sequence.isNull {
                guard case .int(let value) = sequence, value >= 0 else { throw ZerusError.mismatch("Computer returned an invalid history page.") }
            }
        }
        let events = NativeEvents.events(raw)
        try require(events.count <= 100 && events.allSatisfy { !$0.historyId.isEmpty && !$0.historyCursor.isEmpty && $0.historyEpoch == epoch })
        let before = try cursor("next_before"), after = try cursor("next_after")
        guard case .bool(let hasBefore)? = raw["has_more_before"], case .bool(let hasAfter)? = raw["has_more_after"] else {
            throw ZerusError.mismatch("Computer returned an invalid history page.")
        }
        try require(!hasBefore || before != nil)
        try require(!hasAfter || after != nil)
        return HistoryPage(target: target, epoch: epoch, events: events, before: before, after: after, hasBefore: hasBefore, hasAfter: hasAfter,
                           latestIncoming: latest, totalIncoming: total, complete: complete, indexing: raw.bool("indexing"), truncated: raw.bool("truncated"),
                           projectionApplied: complete ? try HistoryProjection.generation(raw) : 0)
    }
}

/// A bounded movable window of canonical rows.
public struct HistoryWindow: Hashable, Sendable {
    public var target: Target
    public var epoch: String
    public var events: [Event]
    public var before: String?
    public var after: String?
    public var hasBefore: Bool
    public var hasAfter: Bool
    public var atHead: Bool
    public var projectionApplied: Int64 = 0

    public static func from(_ page: HistoryPage) -> HistoryWindow {
        HistoryWindow(target: page.target, epoch: page.epoch, events: page.events, before: page.before, after: page.after,
                      hasBefore: page.hasBefore, hasAfter: page.hasAfter, atHead: !page.hasAfter, projectionApplied: page.projectionApplied)
    }

    public func merge(_ page: HistoryPage, direction: String, maxEvents: Int = 500, maxBytes: Int = 2 * 1024 * 1024) -> HistoryWindow {
        precondition(target == page.target && epoch == page.epoch && (direction == "before" || direction == "after"))
        let aliases = RollingConversationHistory.merge(target: target, previousTarget: target, previous: events, incoming: page.events,
                                                       maxEvents: Int.max, maxBytes: Int.max)
        var values: [String: Event] = [:]
        for event in aliases.events { values[event.id] = event }
        // Canonical server order is authoritative; equal timestamps must not reorder pages.
        let incomingCanonical = Set(page.events.filter { !$0.historyId.isEmpty }.map(\.id))
        let oldOrder = events.filter { !($0.historyId.isEmpty && aliases.supersededIds[$0.id].map(incomingCanonical.contains) == true) }
        let order = direction == "before" ? page.events + oldOrder : oldOrder + page.events
        var seen = Set<String>()
        var rows = order.compactMap { row -> Event? in values[aliases.supersededIds[row.id] ?? row.id] }.filter { seen.insert($0.id).inserted }
        var dropped = false
        var bytes = rows.reduce(2) { $0 + $1.approximateBytes + 1 }
        while !rows.isEmpty && (rows.count > maxEvents || bytes > maxBytes) {
            let removed = direction == "before" ? rows.removeLast() : rows.removeFirst()
            bytes -= removed.approximateBytes + 1
            dropped = true
        }
        let nextHasAfter = direction == "after" ? page.hasAfter : (hasAfter || dropped)
        return HistoryWindow(target: target, epoch: epoch, events: rows,
                             before: direction == "before" ? page.before : (dropped ? rows.first?.historyCursor : before),
                             after: direction == "after" ? page.after : (dropped ? rows.last?.historyCursor : after),
                             hasBefore: direction == "before" ? page.hasBefore : (hasBefore || dropped),
                             hasAfter: nextHasAfter, atHead: !nextHasAfter, projectionApplied: min(projectionApplied, page.projectionApplied))
    }

    /// Inspection rows that update or extend a head window; canonical rows stay in journal arrival order. Queued and
    /// not-delivered rows are never history: only the current inspection's are shown, appended after the canonical rows.
    /// `scope` is the target `incoming` was inspected for; only this window's own inspection names requests on its rows.
    public func inspection(_ incoming: [Event], previous previousInspection: [Event] = [], scope: Target? = nil) -> HistoryWindow {
        guard atHead else { return self }
        let pending = incoming.filter { $0.deliveryState != nil }
        let events = self.events.filter { $0.deliveryState == nil }
        let known = Set(events.flatMap(HistoryInspection.identities))
        let updated = events.map { canonical -> Event in
            let identities = HistoryInspection.identities(canonical)
            let exact = incoming.filter { identities.contains($0.id) && $0.source == canonical.source && $0.nativeType == canonical.nativeType && $0.role == canonical.role }
            guard exact.count == 1 else { return canonical }
            var copy = canonical
            if !exact[0].detailTruncated || canonical.detailTruncated {
                let voice = exact[0].voice.map { VoiceProtocol.newer(canonical.voice, $0) } ?? canonical.voice
                copy.text = voice?.transcript ?? exact[0].text
                copy.attachments = voice.map { [$0.audio] } ?? exact[0].attachments
                copy.detailTruncated = exact[0].detailTruncated
                copy.voice = voice
            }
            if scope == target, let request = HistoryInspection.request(canonical, named: exact[0]) { copy.requestId = request }
            return copy
        }
        let watermark = HistoryInspection.journalWatermark(events + previousInspection)
        let newer = incoming.filter { row in
            !known.contains(row.id) && watermark != nil && (HistoryInspection.journalSequence(row).map { $0 > watermark! } ?? false)
        }
        var copy = self
        copy.events = updated
        if !newer.isEmpty {
            let cursor = events.last { !$0.historyCursor.isEmpty }?.historyCursor ?? after
            let page = HistoryPage(target: target, epoch: epoch, events: newer, before: before, after: cursor, hasBefore: hasBefore, hasAfter: false,
                                   latestIncoming: nil, totalIncoming: nil, complete: false, indexing: false, truncated: false, projectionApplied: projectionApplied)
            copy = copy.merge(page, direction: "after")
        }
        let shown = Set(copy.events.map(\.id))
        copy.events += pending.filter { !shown.contains($0.id) }
        return copy
    }
}

public enum HistoryRequests {
    /// An empty canonical head has no cursor, but can still gain its first voice message.
    public static func refreshAtHead(_ window: HistoryWindow) -> (direction: String, cursor: String)? {
        guard window.atHead else { return nil }
        guard let cursor = window.events.last(where: { !$0.historyCursor.isEmpty })?.historyCursor else { return ("", "") }
        return ("after", cursor)
    }

    public static func payload(_ target: Target, id: String, direction: String = "", cursor: String = "", limit: Int = 100) throws -> JSONObject {
        guard !target.run.isEmpty, !target.conversation.isEmpty, (1...100).contains(limit),
              ["", "before", "after", "around", "unread"].contains(direction), direction.isEmpty == cursor.isEmpty, cursor.count <= 8192 else {
            throw ZerusError.invalid("Invalid history request.")
        }
        var raw: JSONObject = ["request_id": .string(id), "expected_run_id": .string(target.run),
                               "expected_conversation_id": .string(target.agentId.isEmpty ? target.conversation : target.parentConversation),
                               "limit": .int(Int64(limit))]
        if !target.archiveId.isEmpty { raw["archive_id"] = .string(target.archiveId) }
        if !target.agentId.isEmpty {
            guard !target.parentConversation.isEmpty else { throw ZerusError.invalid("Invalid history request.") }
            raw["agent_id"] = .string(target.agentId)
        }
        if direction == "unread" {
            guard let marker = try? JSON.parse(cursor).array, marker.count == 2, case .string(let epoch) = marker[0], case .int(let sequence) = marker[1],
                  !epoch.isEmpty, epoch.count <= 512, sequence >= 0 else { throw ZerusError.invalid("Invalid history request.") }
            raw["history_epoch"] = .string(epoch)
            raw["around_incoming_seq"] = .int(sequence)
        } else if !direction.isEmpty {
            raw[direction] = .string(cursor)
        }
        return raw
    }

    public static func unreadCursor(epoch: String, readThrough: Int64) -> String { JSON.array([.string(epoch), .int(readThrough)]).compact }
}

/// Journal sequence proves arrival within that journal; provider IDs and clocks do not.
public enum HistoryInspection {
    public static func identities(_ row: Event) -> [String] { (row.originalIds + [row.id, row.originalId]).filter { !$0.isEmpty } }

    static func sequence(_ id: String) -> Int64? {
        guard id.hasPrefix("journal:"), let range = id.range(of: ":native:", options: .backwards) else { return nil }
        let token = id[range.upperBound...]
        guard !token.isEmpty, token.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        return Int64(token)
    }

    public static func journalSequence(_ row: Event) -> Int64? { identities(row).compactMap(sequence).max() }
    public static func journalWatermark(_ rows: [Event]) -> Int64? { rows.compactMap(journalSequence).max() }

    /// The request a canonical OpenCode prompt naming none takes from its own inspection row. A prompt indexed before hgs
    /// named requests keeps none in history; the inspection row of the exact same OpenCode message (`msg_` ID) may name
    /// it now, which hgs does only while the message text equals what the request sent. No text or clock heuristic.
    static func request(_ canonical: Event, named row: Event) -> String? {
        guard canonical.source == "opencode_native", row.source == canonical.source, canonical.role == "You", row.role == canonical.role,
              NativeEvents.userTypes.contains(canonical.nativeType), row.nativeType == canonical.nativeType, canonical.requestId.isEmpty,
              AttachmentPolicy.validId(row.requestId), let range = row.originalId.range(of: ":native:", options: .backwards),
              row.originalId[range.upperBound...].hasPrefix("msg_"), identities(canonical).contains(row.originalId) else { return nil }
        return row.requestId
    }

    /// Queued and not-delivered rows are not canonical history and never change it.
    public static func changed(_ window: HistoryWindow, _ incoming: [Event], _ previous: [Event]) -> Bool {
        let known = Set((window.events + previous).flatMap(identities))
        return incoming.contains { row in row.deliveryState == nil && !identities(row).contains(where: known.contains) }
    }

    public static func retain(_ current: HistoryWindow, _ candidate: HistoryWindow?, _ incoming: [Event], _ previous: [Event],
                              scope: Target? = nil) -> HistoryWindow {
        if current == candidate { return current }
        return current.atHead ? current.inspection(incoming, previous: previous, scope: scope) : current
    }
}

/// Native-provided provenance only; no text or clock heuristic for durable read identity.
public enum HistoryAliases {
    public static func exact(_ events: [Event], known: Set<String>) -> [String: String] {
        var aliases: [String: Set<String>] = [:]
        for event in events where !event.historyId.isEmpty {
            for old in NativeEvents.unique((event.originalIds + [event.originalId]).filter { !$0.isEmpty && known.contains($0) && $0 != event.id }) {
                aliases[old, default: []].insert(event.id)
            }
        }
        return aliases.compactMapValues { $0.count == 1 ? $0.first : nil }
    }

    public static func boundary(_ id: String, _ aliases: [String: String]) -> String { aliases[id] ?? id }
}
