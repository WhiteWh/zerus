import Foundation

/// An app-private immutable file snapshot; no URI or filesystem path is retained.
public struct Attachment: Codable, Hashable, Sendable {
    public var id: String
    public var name: String
    public var mime: String
    public var bytes: Int64
    public var sha256: String

    public init(id: String, name: String, mime: String, bytes: Int64, sha256: String) {
        self.id = id
        self.name = name
        self.mime = mime
        self.bytes = bytes
        self.sha256 = sha256
    }

    public func displayed() -> DisplayedAttachment {
        DisplayedAttachment(name: name, mime: mime, bytes: bytes, localId: id, sha256: sha256)
    }

    public func json() -> JSONObject {
        ["id": .string(id), "name": .string(name), "mime": .string(mime), "bytes": .int(bytes), "sha256": .string(sha256)]
    }
}

/// Display metadata plus the identities that can open the exact bytes: a native
/// receipt position (`messageId`, `index`) and/or this phone's private copy.
public struct DisplayedAttachment: Codable, Hashable, Sendable {
    public var name: String
    public var mime: String
    public var bytes: Int64
    public var reference: String
    public var messageId: String
    public var index: Int
    public var localId: String
    public var sha256: String

    public init(name: String, mime: String, bytes: Int64, reference: String = "", messageId: String = "", index: Int = -1,
                localId: String = "", sha256: String = "") {
        self.name = name
        self.mime = mime
        self.bytes = bytes
        self.reference = reference
        self.messageId = messageId
        self.index = index
        self.localId = localId
        self.sha256 = sha256
    }
}

public enum AttachmentPolicy {
    public static let maxFiles = 8
    public static let maxFileBytes: Int64 = 10 * 1024 * 1024
    public static let maxTotalBytes: Int64 = 20 * 1024 * 1024
    public static let maxTextBytes = 65_536
    static let mimePattern = try! NSRegularExpression(pattern: "^[a-z0-9][a-z0-9!#$&^_.+\\-]{0,126}/[a-z0-9][a-z0-9!#$&^_.+\\-]{0,126}$")

    public static func validId(_ id: String) -> Bool {
        id.count == 36 && UUID(uuidString: id)?.uuidString.lowercased() == id
    }

    /// A safe leaf file name of at most 255 UTF-8 bytes.
    public static func filename(_ value: String?) -> String {
        var leaf = Substring(value ?? "")
        if let slash = leaf.lastIndex(of: "/") { leaf = leaf[leaf.index(after: slash)...] }
        if let backslash = leaf.lastIndex(of: "\\") { leaf = leaf[leaf.index(after: backslash)...] }
        var result = ""
        var length = 0
        for scalar in leaf.unicodeScalars {
            let category = scalar.properties.generalCategory
            let unsafe = category == .control || category == .format || category == .surrogate ||
                [47, 92, 58, 42, 63, 34, 60, 62, 124].contains(scalar.value)
            let text = unsafe ? "_" : String(scalar)
            let size = text.utf8.count
            if length + size > 255 { break }
            result += text
            length += size
        }
        var trimmed = result.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix(".") { trimmed.removeLast() }
        return trimmed.trimmingCharacters(in: .whitespaces).isEmpty || trimmed == "." || trimmed == ".." ? "attachment" : trimmed
    }

    public static func mime(_ value: String?) -> String {
        guard let lower = value?.lowercased(), lower.utf8.count <= 100,
              mimePattern.firstMatch(in: lower, range: NSRange(lower.startIndex..., in: lower)) != nil else {
            return "application/octet-stream"
        }
        return lower
    }

    public static func validate(_ value: Attachment) throws {
        guard validId(value.id), value.name == filename(value.name), value.name.utf8.count <= 255, value.mime == mime(value.mime),
              (1...maxFileBytes).contains(value.bytes), value.sha256.count == 64,
              value.sha256.allSatisfy({ $0.isHexDigit && !$0.isUppercase }) else {
            throw ZerusError.invalid("Saved attachment metadata is invalid. Select the file again.")
        }
    }

    @discardableResult
    public static func selection(_ values: [Attachment], adding: Bool = false) throws -> Int64 {
        if values.count + (adding ? 1 : 0) > maxFiles { throw ZerusError.invalid("Attach at most 8 files.") }
        if Set(values.map(\.id)).count != values.count { throw ZerusError.invalid("The same attachment cannot appear twice.") }
        for value in values { try validate(value) }
        let total = values.reduce(Int64(0)) { $0 + $1.bytes }
        if total > maxTotalBytes { throw ZerusError.invalid("Attachments exceed 20 MiB in total.") }
        return total
    }
}

/// One private composition: a message, a question answer or a recovered copy.
public struct Draft: Codable, Hashable, Sendable {
    public var target: Target
    public var text: String
    public var status: String
    public var requestId: String
    public var questionId: String
    public var questionHash: String
    public var answers: String
    public var updatedAt: Int64
    public var attachments: [Attachment]
    public var revision: String
    public var detachedId: String
    public var generation: String

    public init(target: Target, text: String, status: String = "editing", requestId: String = "", questionId: String = "",
                questionHash: String = "", answers: String = "", updatedAt: Int64 = Clock.milliseconds(),
                attachments: [Attachment] = [], revision: String = UUID().uuidString.lowercased(), detachedId: String = "",
                generation: String = UUID().uuidString.lowercased()) {
        self.target = target
        self.text = text
        self.status = status
        self.requestId = requestId
        self.questionId = questionId
        self.questionHash = questionHash
        self.answers = answers
        self.updatedAt = updatedAt
        self.attachments = attachments
        self.revision = revision
        self.detachedId = detachedId
        self.generation = generation
    }

    public var key: String {
        target.key + ":" + questionId + ":" + questionHash + (detachedId.isEmpty ? "" : ":\(detachedId)")
    }

    public func begin() -> Draft {
        var copy = self
        copy.status = "submitting"
        copy.requestId = UUID().uuidString.lowercased()
        return copy
    }

    /// A process restart never makes an interrupted submission resendable.
    public func recover() -> Draft {
        guard status == "submitting" else { return self }
        var copy = self
        copy.status = "uncertain"
        return copy
    }

    /// Unsent text or files that keep a session in Active.
    public var hasContent: Bool { !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty }
}

public enum Clock {
    public static func milliseconds() -> Int64 { Int64((Date().timeIntervalSince1970 * 1000).rounded(.down)) }
    public static func seconds() -> Double { Date().timeIntervalSince1970 }
}

public struct OutgoingMessage: Codable, Hashable, Sendable {
    public var requestId: String
    public var target: Target
    public var text: String
    public var attachments: [Attachment]
    public var createdAt: Double
    public var status: String
    public var submittedAt: Double
    public var submittedText: String
    public var error: String
    public var deliveryDismissed: Bool
    public var sendIntent: SendIntent?
    public var sendReceiptProof: SendReceiptProof?
    public var sendCorrelation: SendCorrelation?
    public var coverage: OutgoingCoverage?

    public init(requestId: String, target: Target, text: String, attachments: [Attachment], createdAt: Double, status: String = "sending",
                submittedAt: Double = 0, submittedText: String = "", error: String = "", deliveryDismissed: Bool = false, sendIntent: SendIntent? = nil, sendReceiptProof: SendReceiptProof? = nil, sendCorrelation: SendCorrelation? = nil, coverage: OutgoingCoverage? = nil) {
        self.requestId = requestId
        self.target = target
        self.text = text
        self.attachments = attachments
        self.createdAt = createdAt
        self.status = status
        self.submittedAt = submittedAt
        self.submittedText = submittedText
        self.error = error
        self.deliveryDismissed = deliveryDismissed
        self.sendIntent = sendIntent
        self.sendReceiptProof = sendReceiptProof
        self.sendCorrelation = sendCorrelation
        self.coverage = coverage
    }

    private enum CodingKeys: String, CodingKey {
        case requestId, target, text, attachments, createdAt, status, submittedAt, submittedText, error, deliveryDismissed, sendIntent, sendReceiptProof, sendCorrelation, coverage
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        requestId = try values.decode(String.self, forKey: .requestId)
        target = try values.decode(Target.self, forKey: .target)
        text = try values.decode(String.self, forKey: .text)
        attachments = try values.decode([Attachment].self, forKey: .attachments)
        createdAt = try values.decode(Double.self, forKey: .createdAt)
        status = try values.decode(String.self, forKey: .status)
        submittedAt = try values.decode(Double.self, forKey: .submittedAt)
        submittedText = try values.decode(String.self, forKey: .submittedText)
        error = try values.decode(String.self, forKey: .error)
        deliveryDismissed = try values.decodeIfPresent(Bool.self, forKey: .deliveryDismissed) ?? false
        sendIntent = try values.decodeIfPresent(SendIntent.self, forKey: .sendIntent)
        sendReceiptProof = try values.decodeIfPresent(SendReceiptProof.self, forKey: .sendReceiptProof)
        sendCorrelation = try values.decodeIfPresent(SendCorrelation.self, forKey: .sendCorrelation)
        coverage = try values.decodeIfPresent(OutgoingCoverage.self, forKey: .coverage)
    }

    public func recover() -> OutgoingMessage {
        guard status == "sending" else { return self }
        var copy = self
        copy.status = "uncertain"
        copy.error = "Delivery was interrupted. Check its receipt before sending again."
        return copy
    }

    public func belongs(to current: Target) -> Bool { target == current || target.resolves(to: current) }
    public var blocksSending: Bool { status == "sending" || status == "uncertain" && !deliveryDismissed }
    /// Its delivery needs review: the message offers Review, and Check delivery while uncertain.
    public var recoverable: Bool { status == "failed" || status == "uncertain" }
}

/// One automatic receipt lookup for the current navigation, never a message submission.
public struct OutgoingReceiptProbe: Sendable {
    private struct Attempt: Hashable, Sendable {
        let target: Target
        let navigation: Int64
        let requestId: String
    }
    private var attempted: Attempt?

    public init() {}

    public mutating func claim(_ target: Target, navigation: Int64, message: OutgoingMessage, busy: Bool) -> Bool {
        guard message.belongs(to: target), message.status == "uncertain", !busy else { return false }
        let attempt = Attempt(target: target, navigation: navigation, requestId: message.requestId)
        if attempted == attempt { return false }
        attempted = attempt
        return true
    }
}

extension Target {
    /// Only the initial blank conversation can acquire an identity under the same verified run.
    public func resolves(to current: Target) -> Bool {
        agentId.isEmpty && current.agentId.isEmpty && archiveId.isEmpty && current.archiveId.isEmpty && conversation.isEmpty &&
            !current.conversation.isEmpty && connectionId == current.connectionId && computerId == current.computerId &&
            session == current.session && run == current.run
    }

    public var machineKey: MachineKey { MachineKey(connectionId: connectionId, computerId: computerId) }

    public func with(session: String? = nil, run: String? = nil, conversation: String? = nil, connectionId: String? = nil,
                     archiveId: String? = nil, agentId: String? = nil, parentConversation: String? = nil, computerId: String? = nil) -> Target {
        Target(computerId: computerId ?? self.computerId, session: session ?? self.session, run: run ?? self.run,
               conversation: conversation ?? self.conversation, connectionId: connectionId ?? self.connectionId,
               archiveId: archiveId ?? self.archiveId, agentId: agentId ?? self.agentId,
               parentConversation: parentConversation ?? self.parentConversation)
    }
}

/// One immutable native action attempt. Uncertain attempts are checked, never replayed.
public struct SessionAction: Codable, Hashable, Sendable {
    public var requestId: String
    public var target: Target
    public var operation: String
    public var arguments: JSONObject
    public var status: String
    public var createdAt: Int64
    public var error: String
    public var resultTarget: Target?
    public var pendingId: String
    public var applyAttemptId: String
    public var resultPath: String
    public var confirmedNotificationPreference: String?

    public init(requestId: String, target: Target, operation: String, arguments: JSONObject, status: String = "sending",
                createdAt: Int64 = Clock.milliseconds(), error: String = "", resultTarget: Target? = nil, pendingId: String = "",
                applyAttemptId: String = "", resultPath: String = "", confirmedNotificationPreference: String? = nil) {
        self.requestId = requestId
        self.target = target
        self.operation = operation
        self.arguments = arguments
        self.status = status
        self.createdAt = createdAt
        self.error = error
        self.resultTarget = resultTarget
        self.pendingId = pendingId
        self.applyAttemptId = applyAttemptId
        self.resultPath = resultPath
        self.confirmedNotificationPreference = confirmedNotificationPreference
    }

    public var blocksSending: Bool { status == "sending" || status == "uncertain" }
    public var needsProjectReview: Bool { operation == "launch" && status == "completed" && !error.isEmpty && resultTarget != nil }

    public func recover() -> SessionAction {
        guard status == "sending" else { return self }
        var copy = self
        copy.status = "uncertain"
        copy.error = "The action was interrupted. Check its original receipt; it has not been retried."
        return copy
    }
}

/// Durable identity of one native context command; it never authorizes an automatic retry.
public struct ContextOperation: Codable, Hashable, Sendable {
    public var requestId: String
    public var target: Target
    public var operation: String
    public var status: String
    public var createdAt: Int64
    public var error: String
    public var resolvedConversation: String

    public init(requestId: String, target: Target, operation: String, status: String = "sending", createdAt: Int64 = Clock.milliseconds(),
                error: String = "", resolvedConversation: String = "") {
        self.requestId = requestId
        self.target = target
        self.operation = operation
        self.status = status
        self.createdAt = createdAt
        self.error = error
        self.resolvedConversation = resolvedConversation
    }

    public var blocksSending: Bool { ["sending", "submitted", "uncertain"].contains(status) }

    public func recover() -> ContextOperation {
        guard status == "sending" || status == "submitted" else { return self }
        var copy = self
        copy.status = "uncertain"
        copy.error = "Context command was interrupted. Check its original receipt; it has not been retried."
        return copy
    }
}

/// Terminal input is separate from ordinary messages and immutable after enqueue.
public struct TerminalBuffer: Codable, Hashable, Sendable {
    public var target: Target
    public var binding: String
    public var text: String
    public var revision: String
    public var generation: String

    public init(target: Target, binding: String, text: String = "", revision: String = UUID().uuidString.lowercased(),
                generation: String = UUID().uuidString.lowercased()) {
        self.target = target
        self.binding = binding
        self.text = text
        self.revision = revision
        self.generation = generation
    }

    public var key: String { target.key + ":terminal:" + binding }
}

/// Bounded navigation index. It carries no credentials or mutation capabilities.
public struct IndexedConversation: Codable, Hashable, Sendable {
    public var target: Target
    public var title: String
    public var agent: String
    public var project: String
    public var preview: String
    public var computerName: String
    public var state: String
    public var archivedAt: Double

    public init(target: Target, title: String, agent: String, project: String, preview: String, computerName: String, state: String,
                archivedAt: Double = 0) {
        self.target = target
        self.title = title
        self.agent = agent
        self.project = project
        self.preview = preview
        self.computerName = computerName
        self.state = state
        self.archivedAt = archivedAt
    }

    public func session() -> Session {
        let raw: JSONObject = ["name": .string(target.session), "run_id": .string(target.run), "conversation_id": .string(target.conversation),
                               "state": .string(state), "archived_at": .double(archivedAt), "archive_id": .string(target.archiveId),
                               "last_known": true]
        return Session(target: target, title: title, agent: agent, status: state, project: project, preview: preview, pendingCount: 0, raw: raw)
    }
}

public enum ConversationIndex {
    public static func capture(_ sessions: [Session], _ machines: [Machine]) -> [IndexedConversation] {
        var names: [MachineKey: String] = [:]
        for machine in machines where names[machine.key] == nil { names[machine.key] = machine.nativeName }
        return sessions.prefix(1000).map { session in
            let archivedAt = session.raw.double("archived_at", default: 0)
            return IndexedConversation(target: session.target, title: String(session.title.prefix(512)), agent: String(session.agent.prefix(120)),
                                       project: String(session.project.prefix(4096)), preview: String(session.preview.prefix(1024)),
                                       computerName: String((names[session.target.machineKey] ?? "").prefix(512)),
                                       state: session.raw.string("state").isEmpty ? "running" : session.raw.string("state"),
                                       archivedAt: archivedAt.isFinite ? archivedAt : 0)
        }
    }
}

/// The phone's private conversation state. Sending moves the composition into an
/// immutable outgoing envelope in one commit.
public struct MessageState: Codable, Hashable, Sendable {
    public var drafts: [Draft] = []
    public var outgoing: [OutgoingMessage] = []
    public var pickerDraft: Draft?
    public var pickerId: String = ""
    public var contextOperations: [ContextOperation] = []
    public var sessionActions: [SessionAction] = []
    public var conversationIndex: [IndexedConversation] = []
    public var terminalBuffers: [TerminalBuffer] = []
    public var machineAliases: [MachineAlias] = []
    public var machineColors: [MachineColorOverride] = []
    public var speechAppliedIds: [String] = []

    public init(drafts: [Draft] = [], outgoing: [OutgoingMessage] = [], pickerDraft: Draft? = nil, pickerId: String = "",
                contextOperations: [ContextOperation] = [], sessionActions: [SessionAction] = [],
                conversationIndex: [IndexedConversation] = [], terminalBuffers: [TerminalBuffer] = [],
                machineAliases: [MachineAlias] = [], machineColors: [MachineColorOverride] = [], speechAppliedIds: [String] = []) {
        self.drafts = drafts
        self.outgoing = outgoing
        self.pickerDraft = pickerDraft
        self.pickerId = pickerId
        self.contextOperations = contextOperations
        self.sessionActions = sessionActions
        self.conversationIndex = conversationIndex
        self.terminalBuffers = terminalBuffers
        self.machineAliases = machineAliases
        self.machineColors = machineColors
        self.speechAppliedIds = speechAppliedIds
    }

    /// State as loaded after a restart: interrupted attempts become uncertain, never resendable.
    public func recovered() -> MessageState {
        var copy = self
        copy.drafts = drafts.map { $0.recover() }
        copy.outgoing = outgoing.map { $0.recover() }
        copy.pickerDraft = pickerDraft?.recover()
        copy.contextOperations = contextOperations.map { $0.recover() }
        copy.sessionActions = SessionNotificationPreferences.boundedProofs(sessionActions.map { $0.recover() })
        copy.conversationIndex = Array(conversationIndex.prefix(1000))
        copy.speechAppliedIds = Array(speechAppliedIds.suffix(100))
        return copy
    }

    public func action(_ action: SessionAction) -> MessageState {
        let next = sessionActions.filter { $0.requestId != action.requestId } + [action]
        let retained: (SessionAction) -> Bool = { $0.blocksSending || $0.status == "scheduled" || $0.needsProjectReview }
        var copy = self
        copy.sessionActions = SessionNotificationPreferences.boundedProofs(next.filter(retained) + next.filter { !retained($0) }.suffix(100))
        return copy
    }

    public func context(_ operation: ContextOperation) -> MessageState {
        let next = contextOperations.filter { $0.requestId != operation.requestId } + [operation]
        var copy = self
        copy.contextOperations = next.filter(\.blocksSending) + next.filter { !$0.blocksSending }.suffix(50)
        return copy
    }

    /// Adds picked files to the draft that owned the picker, or keeps them as a detached recovery draft.
    public func selectionResult(_ origin: Draft, _ added: [Attachment]) throws -> MessageState {
        guard pickerDraft?.generation == origin.generation, pickerDraft?.target == origin.target else {
            throw ZerusError.invalid("File selection was canceled. The original draft is preserved.")
        }
        var copy = self
        copy.pickerDraft = nil
        copy.pickerId = ""
        if added.isEmpty { return copy }
        let current = drafts.first { draft in
            draft.generation == origin.generation && draft.detachedId.isEmpty && draft.status == "editing" &&
                (draft.target == origin.target || origin.target.resolves(to: draft.target) ||
                 ContextPolicies.clearMoved(self, origin.target, draft.target) || ActionDraftPolicies.moved(self, origin.target, draft.target))
        }
        var restored = current ?? origin
        restored.attachments += added
        restored.revision = UUID().uuidString.lowercased()
        if current == nil { restored.detachedId = UUID().uuidString.lowercased() }
        try AttachmentPolicy.selection(restored.attachments)
        copy.drafts = drafts.filter { $0.key != restored.key } + [restored]
        return copy
    }

    public func enqueue(_ draft: Draft, requestId: String, at: Double, sendIntent: SendIntent? = nil) throws -> MessageState {
        guard draft.target.archiveId.isEmpty else { throw ZerusError.invalid("Archived conversations are read-only.") }
        guard draft.detachedId.isEmpty, draft.questionId.isEmpty, draft.status == "editing" else {
            throw ZerusError.invalid("Review this draft before sending.")
        }
        guard !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !draft.attachments.isEmpty else {
            throw ZerusError.invalid("Write a message or attach a file.")
        }
        guard draft.text.utf8.count <= AttachmentPolicy.maxTextBytes else { throw ZerusError.invalid("Messages can contain at most 64 KiB of text.") }
        guard !sessionActions.contains(where: { $0.target == draft.target && $0.blocksSending }) else {
            throw ZerusError.invalid("Resolve the pending session action first.")
        }
        guard !contextOperations.contains(where: { ContextPolicies.sameSessionRun($0.target, draft.target) && $0.blocksSending }) else {
            throw ZerusError.invalid("Check the pending context command before sending.")
        }
        guard !outgoing.contains(where: { $0.belongs(to: draft.target) && $0.blocksSending }) else {
            throw ZerusError.invalid("Check the previous delivery before sending again.")
        }
        guard pickerDraft?.target != draft.target else { throw ZerusError.invalid("Wait for the selected files to finish importing.") }
        guard !outgoing.contains(where: { $0.requestId == requestId }) else { throw ZerusError.invalid("This request already exists.") }
        if let current = drafts.first(where: { $0.key == draft.key }), current.revision != draft.revision {
            throw ZerusError.invalid("The draft changed. Review it before sending.")
        }
        try AttachmentPolicy.selection(draft.attachments)
        let message = OutgoingMessage(requestId: requestId, target: draft.target, text: draft.text, attachments: draft.attachments, createdAt: at, sendIntent: sendIntent)
        guard sendIntent == nil || SendReceiptRecovery.matches(message, sendIntent!) else { throw ZerusError.invalid("The saved message details changed before sending.") }
        var cleared = draft
        cleared.text = ""
        cleared.attachments = []
        cleared.revision = UUID().uuidString.lowercased()
        cleared.generation = UUID().uuidString.lowercased()
        cleared.updatedAt = Int64(at * 1000)
        var copy = self
        copy.drafts = drafts.filter { $0.key != draft.key } + [cleared]
        copy.outgoing = outgoing + [message]
        return copy
    }

    public func resolveConversation(from: Target, to: Target) -> MessageState {
        guard from.resolves(to: to),
              let editor = drafts.first(where: { $0.target == from && $0.questionId.isEmpty && $0.detachedId.isEmpty && $0.status == "editing" }),
              !drafts.contains(where: { $0.target == to && $0.questionId.isEmpty && $0.detachedId.isEmpty }) else { return self }
        var copy = self
        copy.drafts = drafts.map { draft in
            guard draft.key == editor.key else { return draft }
            var moved = draft
            moved.target = to
            return moved
        }
        return copy
    }

    public func update(_ message: OutgoingMessage) -> MessageState {
        var copy = self
        copy.outgoing = outgoing.map { $0.requestId == message.requestId ? message : $0 }
        return copy
    }
}

public enum ContextPolicies {
    public static func sameSession(_ a: Target, _ b: Target) -> Bool {
        a.agentId.isEmpty && b.agentId.isEmpty && a.archiveId.isEmpty && b.archiveId.isEmpty &&
            a.connectionId == b.connectionId && a.computerId == b.computerId && a.session == b.session
    }

    public static func sameSessionRun(_ a: Target, _ b: Target) -> Bool { sameSession(a, b) && a.run == b.run }

    public static func clearMoved(_ state: MessageState, _ from: Target, _ to: Target) -> Bool {
        from != to && sameSessionRun(from, to) && state.contextOperations.contains {
            $0.target == from && $0.operation == "clear_context" && $0.status == "completed" &&
                !$0.resolvedConversation.isEmpty && $0.resolvedConversation == to.conversation
        }
    }

    public static func sameDraft(_ expected: Draft, _ current: Draft) -> Bool {
        expected.target == current.target && expected.key == current.key && expected.status == "editing" && current.status == "editing" &&
            expected.generation == current.generation && expected.revision == current.revision && expected.text == current.text &&
            expected.answers == current.answers && expected.attachments == current.attachments
    }
}

/// Only a confirmed user action can route an ordinary editor to an authoritative target.
public enum ActionDraftPolicies {
    public static func proof(_ action: SessionAction, _ from: Target, _ to: Target) -> Bool {
        action.status == "completed" && ["rename", "resume"].contains(action.operation) && action.target == from && action.resultTarget == to &&
            from.agentId.isEmpty && to.agentId.isEmpty && from.archiveId.isEmpty && to.archiveId.isEmpty &&
            from.connectionId == to.connectionId && from.computerId == to.computerId && !from.conversation.isEmpty &&
            from.conversation == to.conversation
    }

    public static func moved(_ state: MessageState, _ from: Target, _ to: Target) -> Bool {
        from != to && state.sessionActions.contains { proof($0, from, to) }
    }

    public static func destinationReady(_ state: MessageState, _ target: Target) -> Bool {
        !state.drafts.contains { draft in
            draft.target == target && draft.questionId.isEmpty && draft.detachedId.isEmpty &&
                (draft.status != "editing" || !draft.text.isEmpty || !draft.attachments.isEmpty || !draft.answers.isEmpty || !draft.requestId.isEmpty)
        }
    }

    public static func restore(_ state: MessageState, _ action: SessionAction, _ expected: Draft) -> MessageState {
        guard let current = state.drafts.first(where: { $0.key == expected.key && $0.generation == expected.generation &&
            $0.revision == expected.revision && $0.attachments == expected.attachments }),
              current.questionId.isEmpty, current.detachedId.isEmpty, current.status == "editing", current.target == action.target else { return state }
        return promote(state, action)
    }

    public static func promote(_ state: MessageState, _ action: SessionAction) -> MessageState {
        guard let to = action.resultTarget, proof(action, action.target, to),
              state.sessionActions.contains(where: { $0.requestId == action.requestId && proof($0, action.target, to) }),
              let editor = state.drafts.first(where: { $0.target == action.target && $0.questionId.isEmpty && $0.detachedId.isEmpty && $0.status == "editing" }),
              destinationReady(state, to) else { return state }
        var copy = state
        copy.drafts = state.drafts.filter { !($0.target == to && $0.questionId.isEmpty && $0.detachedId.isEmpty) }.map { draft in
            guard draft.key == editor.key else { return draft }
            var moved = draft
            moved.target = to
            return moved
        }
        return copy
    }
}
