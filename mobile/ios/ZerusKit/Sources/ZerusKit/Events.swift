import CryptoKit
import Foundation

/// One displayed conversation row, ported from Android `Event`.
public struct Event: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var role: String
    public var text: String
    public var time: String
    public var at: Double
    public var requestId: String
    public var attachments: [DisplayedAttachment]
    public var source: String
    public var delivery: String
    public var outgoingId: String
    public var nativeType: String
    public var submittedText: String
    public var originalId: String
    public var historyId: String
    public var historyCursor: String
    public var historyEpoch: String
    public var incomingSeq: Int64?
    public var originalIds: [String]
    public var detailTruncated: Bool
    public var voice: VoiceMessage?
    /// A message the native agent has not admitted yet: "queued" or "not_delivered" (OpenCode). It is shown only
    /// from the current inspection and never becomes history. Optional so older cached rows still decode.
    public var deliveryState: String?
    /// A notice's journal origin, its declared sender and a subagent's collapsed report
    /// (ActivityNotices). Optional so older cached rows still decode.
    public var origin: String?
    public var sender: String?
    public var report: String?
    /// Exact provider identity, kept separately from receipt and presentation IDs.
    /// Optional so older cached rows still decode.
    public var nativeMessageId: String?
    /// A fork's read-only row from the session it began from (that session's name); never incoming.
    public var inheritedFrom: String?

    public init(id: String, role: String, text: String, time: String, at: Double = 0, requestId: String = "", attachments: [DisplayedAttachment] = [],
                source: String = "", delivery: String = "", outgoingId: String = "", nativeType: String = "", submittedText: String = "",
                originalId: String = "", historyId: String = "", historyCursor: String = "", historyEpoch: String = "", incomingSeq: Int64? = nil,
                originalIds: [String] = [], detailTruncated: Bool = false, voice: VoiceMessage? = nil, deliveryState: String? = nil,
                origin: String? = nil, sender: String? = nil, report: String? = nil, inheritedFrom: String? = nil, nativeMessageId: String? = nil) {
        self.id = id
        self.role = role
        self.text = text
        self.time = time
        self.at = at
        self.requestId = requestId
        self.attachments = attachments
        self.source = source
        self.delivery = delivery
        self.outgoingId = outgoingId
        self.nativeType = nativeType
        self.submittedText = submittedText
        self.originalId = originalId
        self.historyId = historyId
        self.historyCursor = historyCursor
        self.historyEpoch = historyEpoch
        self.incomingSeq = incomingSeq
        self.originalIds = originalIds
        self.detailTruncated = detailTruncated
        self.voice = voice
        self.deliveryState = deliveryState
        self.origin = origin
        self.sender = sender
        self.report = report
        self.inheritedFrom = inheritedFrom
        self.nativeMessageId = nativeMessageId
    }

    /// The request of a row Zerus recorded itself: a delivery or voice receipt, or this phone's outbox. A native OpenCode
    /// prompt names the request that sent it only so `ConversationMerge` can match this phone's message; it stays a native row.
    public var receiptRequestId: String { role != "You" || source == "opencode_native" ? "" : requestId }

    /// Serialized-size estimate used for the bounded history window.
    public var approximateBytes: Int {
        // Separate statements: one long `+` chain takes the type checker tens of seconds.
        var size = 260
        let fields: [String] = [id, role, text, time, requestId, source, delivery, outgoingId, nativeType, submittedText, originalId,
                                historyId, historyCursor, historyEpoch, deliveryState ?? "", origin ?? "", sender ?? "", report ?? "", nativeMessageId ?? ""]
        for field in fields { size += field.utf8.count }
        for value in originalIds { size += value.utf8.count + 3 }
        for file in attachments {
            let names: [String] = [file.name, file.mime, file.reference, file.messageId]
            size += 90
            for name in names { size += name.utf8.count }
        }
        if let voice {
            let texts: [String] = [voice.transcript, voice.error, voice.nativeBinding]
            size += 300 + voice.waveform.count * 4
            for value in texts { size += value.utf8.count }
        }
        return size
    }
}

/// The label of a message the native agent has not admitted. Phones offer no removal in phase 1.
public enum QueuedDelivery {
    public static let states: Set<String> = ["queued", "not_delivered"]

    public static func label(_ state: String?) -> String {
        switch state {
        case "queued": "Queued"
        case "not_delivered": "Not delivered"
        default: ""
        }
    }
}

public struct Choice: Codable, Hashable, Sendable {
    public var id: String
    public var label: String
    public var description: String
    /// An approval option's reach: "once" for this request or "broader" for more; empty elsewhere.
    public var scope = ""
}

public struct Prompt: Codable, Hashable, Sendable {
    public var id: String
    public var text: String
    public var choices: [Choice]
    public var multi: Bool
    public var required: Bool
    public var other: Bool
    public var header: String
    public var body: String
    public var otherLabel: String
    public var otherDescription: String
    /// Form item fields (OpenCode): "choice" or "text"; empty for other agents, whose rules are unchanged.
    public var inputKind = ""
    public var minLength: Int?
    public var maxLength: Int?
    public var minItems: Int?
    public var maxItems: Int?
    public var defaultOptionIds: [String] = []
    /// The prefilled answer of a native OpenCode text item (`default_text`), bounded like a typed answer; "" otherwise.
    public var defaultText = ""
    /// An item of an OpenCode form (not a permission): its text reaches the native form reply, never a terminal, so text
    /// starting with / or ! is an ordinary answer. Every other card keeps such text on the computer.
    public var literalText = false
}

public struct NativeQuestionAnswer: Codable, Hashable, Sendable {
    public var questionId: String
    public var optionIds: [String]
    public var text: String
    public var skip: Bool

    public init(questionId: String, optionIds: [String], text: String, skip: Bool) {
        self.questionId = questionId
        self.optionIds = optionIds
        self.text = text
        self.skip = skip
    }
}

public struct NativeQuestionDelivery: Codable, Hashable, Sendable {
    public var requestId: String
    public var questionId: String
    public var questionHash: String
    public var run: String
    public var conversation: String
    public var status: String
    public var answers: [NativeQuestionAnswer]
}

public struct Question: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var hash: String
    public var prompts: [Prompt]
    public var canAnswer: Bool
    public var canSkip: Bool
    public var optional: Bool
    public var delivery: String
    public var createdAt: Double
    public var source: String
    public var approval: Bool
    public var trustRequest: Bool
    public var unavailableReason: String
    public var run: String
    public var conversation: String
    public var toolCallId: String
    public var answerDelivery: NativeQuestionDelivery?
    /// An approval answered with the agent's own options (`approval_choices`), such as a Claude terminal panel or a Kimi
    /// tool approval; see `QuestionPolicies.nativeChoices`.
    public var approvalChoices: Bool

    public init(id: String, hash: String, prompts: [Prompt], canAnswer: Bool, canSkip: Bool, optional: Bool, delivery: String,
                createdAt: Double = 0, source: String = "", approval: Bool = false, trustRequest: Bool = false, unavailableReason: String = "",
                run: String = "", conversation: String = "", toolCallId: String = "", answerDelivery: NativeQuestionDelivery? = nil,
                approvalChoices: Bool = false) {
        self.id = id
        self.hash = hash
        self.prompts = prompts
        self.canAnswer = canAnswer
        self.canSkip = canSkip
        self.optional = optional
        self.delivery = delivery
        self.createdAt = createdAt
        self.source = source
        self.approval = approval
        self.trustRequest = trustRequest
        self.unavailableReason = unavailableReason
        self.run = run
        self.conversation = conversation
        self.toolCallId = toolCallId
        self.answerDelivery = answerDelivery
        self.approvalChoices = approvalChoices
    }
}

// MARK: Voice and video metadata

public struct VideoMessage: Codable, Hashable, Sendable {
    public var file: DisplayedAttachment
    public var durationMs: Int
    public var width: Int
    public var height: Int
    public var posterBase64: String

    public init(file: DisplayedAttachment, durationMs: Int, width: Int, height: Int, posterBase64: String = "") {
        self.file = file
        self.durationMs = durationMs
        self.width = width
        self.height = height
        self.posterBase64 = posterBase64
    }
}

/// A session-owned voice message. Audio identity is independent of the speech worker.
public struct VoiceMessage: Codable, Hashable, Sendable {
    public var id: String
    public var runId: String
    public var conversationId: String
    public var createdAt: Double
    public var revision: Int64
    public var state: String
    public var audio: DisplayedAttachment
    public var durationMs: Int
    public var transcript: String
    public var error: String
    public var errorCode: String
    public var nativeBinding: String
    public var waveform: [Int]
    public var video: VideoMessage?

    public init(id: String, runId: String, conversationId: String, createdAt: Double, revision: Int64, state: String, audio: DisplayedAttachment,
                durationMs: Int, transcript: String = "", error: String = "", errorCode: String = "", nativeBinding: String = "", waveform: [Int] = [],
                video: VideoMessage? = nil) {
        self.id = id
        self.runId = runId
        self.conversationId = conversationId
        self.createdAt = createdAt
        self.revision = revision
        self.state = state
        self.audio = audio
        self.durationMs = durationMs
        self.transcript = transcript
        self.error = error
        self.errorCode = errorCode
        self.nativeBinding = nativeBinding
        self.waveform = waveform
        self.video = video
    }

    public var terminal: Bool { ["submitted", "failed", "cancelled", "uncertain"].contains(state) }
}

public enum VoiceWaveform {
    public static let bins = 64
    public static func valid(_ values: [Int]) -> Bool { values.count == bins && values.allSatisfy { (0...255).contains($0) } }
    /// Optional display metadata must never prevent the original recording from opening.
    public static func decode(_ raw: JSON?) -> [Int] {
        guard let array = raw?.array, array.count == bins else { return [] }
        var values: [Int] = []
        for item in array {
            guard case .int(let number) = item, (0...255).contains(number) else { return [] }
            values.append(Int(number))
        }
        return values
    }
}

public enum VideoProtocol {
    public static let feature = "voice_video"
    public static let maxBytes = 10 * 1024 * 1024
    public static let maxDurationMs = 60_000

    public static func message(_ raw: JSONObject, id: String) throws -> VideoMessage {
        guard (100...maxDurationMs).contains(raw.int("duration_ms")), (16...1920).contains(raw.int("width")), (16...1920).contains(raw.int("height")),
              raw.string("poster_base64").count <= 10_924, raw.string("request_id") == id, raw.int("index") == 1, raw.string("mime") == "video/mp4",
              raw.string("name") == "video.mp4", (1...Int64(maxBytes)).contains(raw.int64("bytes")) else {
            throw ZerusError.invalid("Invalid video message.")
        }
        return VideoMessage(file: DisplayedAttachment(name: "video.mp4", mime: "video/mp4", bytes: raw.int64("bytes"), messageId: id, index: 1),
                            durationMs: raw.int("duration_ms"), width: raw.int("width"), height: raw.int("height"), posterBase64: raw.string("poster_base64"))
    }
}

public enum VoiceProtocol {
    public static let operations: Set<String> = ["voice_profiles", "voice_submit", "voice_status", "voice_cancel"]
    public static let states: Set<String> = ["queued", "transcribing", "ready", "waiting_agent", "delivering", "submitted", "failed", "cancelled", "uncertain"]
    public static let maxPcmBytes = 3_840_000

    public static func message(_ raw: JSONObject) throws -> VoiceMessage {
        let id = raw.string("id")
        let state = raw.string("state")
        let run = raw.string("run_id")
        let conversation = raw.string("conversation_id")
        let created = raw.double("created_at")
        guard AttachmentPolicy.validId(id), states.contains(state), !run.isEmpty, run.count <= 256, raw.has("conversation_id"),
              conversation.count <= 256, created.isFinite, created > 0, case .int(let revision)? = raw["revision"], revision >= 0,
              let audio = raw.object("audio"), audio.string("request_id") == id, audio.int("index") == 0, audio.string("mime") == "audio/wav" else {
            throw ZerusError.invalid("Invalid voice message.")
        }
        let name = audio.string("name")
        let bytes = audio.int64("bytes")
        let duration = audio.int("duration_ms")
        let transcript = raw.string("transcript")
        guard name == AttachmentPolicy.filename(name), (45...Int64(maxPcmBytes + 44)).contains(bytes), (1...120_000).contains(duration),
              transcript.utf8.count <= 65_536 else { throw ZerusError.invalid("Invalid voice message.") }
        let binding = raw.object("native_binding").map { JSON.object($0).compact } ?? ""
        guard binding.count <= 4096 else { throw ZerusError.invalid("Invalid voice message.") }
        return VoiceMessage(id: id, runId: run, conversationId: conversation, createdAt: created, revision: revision, state: state,
                            audio: DisplayedAttachment(name: name, mime: "audio/wav", bytes: bytes, messageId: id, index: 0),
                            durationMs: duration, transcript: transcript, error: String(raw.string("error").prefix(2000)),
                            errorCode: String(raw.string("error_code").prefix(128)), nativeBinding: binding,
                            waveform: VoiceWaveform.decode(audio["waveform"]), video: try raw.object("video").map { try VideoProtocol.message($0, id: id) })
    }

    public static func newer(_ previous: VoiceMessage?, _ next: VoiceMessage) -> VoiceMessage {
        guard let previous, previous.id == next.id, previous.runId == next.runId,
              previous.conversationId.isEmpty || next.conversationId == previous.conversationId else { return next }
        return next.revision > previous.revision ? next : previous
    }

    public static func label(_ state: String) -> String {
        switch state {
        case "uploading": "Sending recording…"
        case "queued": "Waiting to transcribe…"
        case "transcribing": "Transcribing…"
        case "ready", "waiting_agent": "Waiting for the agent…"
        case "delivering": "Delivering to the agent…"
        case "submitted": "Accepted by native app"
        case "failed": "Voice message failed"
        case "cancelled": "Cancelled"
        default: "Delivery unconfirmed"
        }
    }
}

// MARK: Native inspection parsing

/// Text comparison only; callers must prove scope, native types, time and a unique pair.
public enum NativeStopExcerpt {
    static let journalDetailLimit = 1200

    public static func matches(_ providerText: String, _ hookText: String) -> Bool {
        let hook = hookText.trimmingCharacters(in: .whitespacesAndNewlines)
        if hook.isEmpty || hook.allSatisfy({ $0 == "…" }) { return false }
        if providerText.contains(hookText) { return true }
        // journal::clipped takes Unicode scalar characters, then adds U+2026 only
        // when more text remains. Short, meaningful literal ellipses stay literal.
        guard hookText.hasSuffix("…"), hookText.unicodeScalars.count == journalDetailLimit + 1 else { return false }
        var scalars = hookText.unicodeScalars
        scalars.removeLast()
        let excerpt = String(scalars)
        return !excerpt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && providerText.contains(excerpt)
    }
}

/// Match the exact transport expansion, never strip attachment-looking user text.
public enum AttachmentReceiptMatch {
    static let instruction = try! NSRegularExpression(pattern: #"(?:Inspect the attached image at|Read the attached file at) ("(?:[^"\\]|\\.)+")"#)

    public static func matches(_ receipt: Event, _ native: Event) -> Bool {
        // A receipt of this session never stands for a fork's inherited row.
        if !(native.inheritedFrom ?? "").isEmpty { return false }
        let submitted = receipt.submittedText.trimmingCharacters(in: .whitespacesAndNewlines)
        if submitted.isEmpty || native.text.trimmingCharacters(in: .whitespacesAndNewlines) != submitted { return false }
        if abs(native.at - receipt.at) <= 10 { return true }
        // Full expansions contain distinct immutable staged paths; a queued
        // acknowledgement can occur long after its accepted terminal input.
        let matches = instruction.matches(in: submitted, range: NSRange(submitted.startIndex..., in: submitted))
        let paths = matches.compactMap { Range($0.range(at: 1), in: submitted).map { String(submitted[$0]) } }
        return !receipt.attachments.isEmpty && submitted != receipt.text.trimmingCharacters(in: .whitespacesAndNewlines) &&
            paths.count >= receipt.attachments.count && Set(paths).count == paths.count
    }
}

public enum NativeEvents {
    static let userTypes: Set<String> = ["UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted"]
    static let eventTypes: Set<String> = userTypes.union(["QuestionAnswered", "AgentMessage", "Stop"])

    /// Conversation rows of one native inspection, ported from Android `NativeParser.events`.
    public static func events(_ raw: JSONObject) -> [Event] {
        let providers = raw.objects("provider_messages")
        // Native child inspection may normalize its own journal agent_id to blank.
        let inspectedAgent = raw.string("agent_id")
        func inScope(_ value: JSONObject) -> Bool {
            let agent = value.string("agent_id")
            return inspectedAgent.isEmpty ? (agent.isEmpty || agent == "main") : (agent.isEmpty || agent == inspectedAgent)
        }
        // message_events is the separately bounded main-message window. Keep
        // the journal namespace so matching native seq IDs deduplicate safely.
        var seenJournal = Set<String>()
        let journal = (raw.objects("events") + raw.objects("message_events")).filter { value in
            let seq = value.string("seq")
            return seq.isEmpty || seenJournal.insert("\(value.string("history_stream")):\(seq)").inserted
        }
        var occurrences: [String: Int] = [:]
        func parse(_ value: JSONObject, _ streamArgument: String) -> Event? {
            let declared = value.string("history_stream")
            let stream = ["journal", "provider", "attachment", "voice"].contains(declared) ? declared : streamArgument
            // A fork's inherited row carries its original type; older apps drop the unknown type.
            let inheritedFrom = value.string("type") == "Inherited" ? value.string("inherited_from") : ""
            if value.string("type") == "Inherited" && inheritedFrom.isEmpty { return nil }
            let type = inheritedFrom.isEmpty ? value.string("type") : value.string("inherited_type")
            guard eventTypes.contains(type), inScope(value) else { return nil }
            let role = type == "QuestionAnswered" ? "You (answer)" : (userTypes.contains(type) ? "You" : "AgentMessage")
            // A native prompt the person did not write is a notice, as on desktop. Identities
            // keep the native role, so cached rows stay the same rows.
            let marked = value.string("origin")
            let origin = inheritedFrom.isEmpty && ActivityNotices.origins.contains(marked) && userTypes.contains(type) ? marked : ""
            let shown = origin.isEmpty ? role : ActivityNotices.role
            let sourceValue = value.string("source")
            let source = sourceValue.isEmpty ? stream : sourceValue
            // An inherited voice belongs to the source session: its text stays, but
            // no voice card, playback or status acts on it through this fork.
            let voice = source == "hgs_voice" && inheritedFrom.isEmpty ? value.object("voice").flatMap { try? VoiceProtocol.message($0) } : nil
            let files = value.objects("attachments").map { file -> DisplayedAttachment in
                var index = -1
                if case .int(let number)? = file["index"], let value = Int(exactly: number) { index = value }
                return DisplayedAttachment(name: file.string("name"), mime: file.string("mime"), bytes: file.int64("bytes"),
                                           reference: file.string("reference"), messageId: file.string("request_id"), index: index)
            }
            // Fallback identity belongs to the original native row, not its display label/body.
            let identityText = source == "hgs_delivery" && !files.isEmpty ? (value["detail"]?.textValue ?? "")
                : value.string("detail", "text", "message", "prompt", "submitted_text")
            let text = !origin.isEmpty ? ActivityNotices.body(origin: origin, detail: value.string("detail"), report: value.string("report")) : identityText
            if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && files.isEmpty && voice == nil { return nil }
            let atValue = value.double("at", default: 0)
            let at = atValue.isFinite ? atValue : 0
            let nativeId = value.string("message_id", "seq", "source_offset")
            let fallback: String
            if nativeId.isEmpty {
                // Count only indistinguishable rows, rather than the whole array.
                let identity: JSON = [.string(type), .string(role), .string(source), .double(at), .string(value.string("timestamp")), .string(identityText),
                                      .array(files.map { [.string($0.name), .string($0.mime), .int($0.bytes), .string($0.reference)] })]
                let digest = SHA256.hash(data: Data(identity.compact.utf8)).map { String(format: "%02x", $0) }.joined()
                let key = "\(stream):\(source):\(role):\(digest)"
                let occurrence = occurrences[key] ?? 0
                occurrences[key] = occurrence + 1
                fallback = "fallback:\(digest):\(occurrence)"
            } else {
                fallback = "native:\(nativeId)"
            }
            let originalValue = value.string("original_id")
            let originalId = originalValue.isEmpty ? "\(stream):\(source):\(role):\(fallback)" : originalValue
            let historyId = value.string("history_id")
            var incoming: Int64?
            if let number = value["incoming_seq"]?.numberValue, number >= 0, number.isFinite { incoming = Int64(number) }
            let originalIds = (value.array("original_ids") ?? []).prefix(8).compactMap { item -> String? in
                if case .string(let id) = item { return id }
                return nil
            }
            // A prompt Zerus sent to OpenCode names its request: OpenCode dates a queued prompt by its delivery, so text and time cannot match it.
            // An inherited row is never this session's own send.
            var requestId = ""
            if shown == "You" && inheritedFrom.isEmpty {
                if ["hgs_delivery", "hgs_voice"].contains(source) { requestId = value.string("message_id") }
                if source == "opencode_native" && AttachmentPolicy.validId(value.string("request_id")) { requestId = value.string("request_id") }
            }
            return Event(id: historyId.isEmpty ? originalId : "history:\(historyId)", role: shown, text: text, time: value.string("at", "timestamp"), at: at,
                         requestId: requestId,
                         attachments: files, source: source, nativeType: type,
                         submittedText: source == "hgs_delivery" ? value.string("submitted_text") : "", originalId: originalId, historyId: historyId,
                         historyCursor: value.string("history_cursor"), historyEpoch: raw.string("history_epoch"), incomingSeq: incoming,
                         originalIds: originalIds, detailTruncated: value.bool("detail_truncated"), voice: voice,
                         deliveryState: shown == "You" && inheritedFrom.isEmpty && QueuedDelivery.states.contains(value.string("delivery_state")) ? value.string("delivery_state") : nil,
                         origin: origin.isEmpty ? nil : origin, sender: origin == "peer_message" ? value.string("sender") : nil,
                         report: origin.isEmpty ? nil : ActivityNotices.report(origin: origin, report: value.string("report")),
                         inheritedFrom: inheritedFrom.isEmpty ? nil : inheritedFrom,
                         nativeMessageId: ["claude_native", "codex_native", "kimi_native", "dsh_native", "opencode_native"].contains(source) &&
                            !value.string("message_id").isEmpty && value.string("message_id").utf8.count <= 256 &&
                            !value.string("message_id").unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) ? value.string("message_id") : nil)
        }
        func replaces(_ provider: JSONObject, _ hook: JSONObject) -> Bool {
            provider.string("type") == "AgentMessage" && inScope(provider) && inScope(hook) && hook.string("type") == "Stop" &&
                NativeStopExcerpt.matches(provider.string("detail"), hook.string("detail")) && abs(provider.double("at") - hook.double("at")) < 10
        }
        var rows: [Event] = []
        for item in journal {
            let candidates = item.string("type") == "Stop" ? providers.filter { replaces($0, item) } : []
            if candidates.count == 1, journal.filter({ replaces(candidates[0], $0) }).count == 1 { continue }
            if let event = parse(item, "journal") { rows.append(event) }
        }
        rows += providers.compactMap { parse($0, "provider") }
        var seenIds = Set<String>()
        rows = rows.filter { seenIds.insert($0.id).inserted }
        var used = Set<Int>()
        let receipts = raw.objects("attachment_messages").compactMap { parse($0, "attachment") }
        for metadata in receipts {
            let candidates = rows.indices.filter { index in
                !used.contains(index) && rows[index].role == "You" && rows[index].receiptRequestId.isEmpty && userTypes.contains(rows[index].nativeType) &&
                    AttachmentReceiptMatch.matches(metadata, rows[index])
            }
            if candidates.count == 1, receipts.filter({ AttachmentReceiptMatch.matches($0, rows[candidates[0]]) }).count == 1 {
                let exact = candidates[0]
                used.insert(exact)
                var replaced = metadata
                replaced.at = rows[exact].at
                replaced.originalIds = Array(unique(metadata.originalIds + [rows[exact].id]).prefix(8))
                rows[exact] = replaced
            } else if !rows.contains(where: { $0.role == "You" && $0.receiptRequestId == metadata.requestId && !metadata.requestId.isEmpty }) {
                rows.append(metadata)
            }
        }
        for item in raw.objects("voice_messages") {
            guard let metadata = parse(item, "voice"), let voice = metadata.voice, metadata.requestId == voice.id else { continue }
            // A persisted native binding is authoritative; equal text alone never removes a row.
            var bound: [Event] = []
            if let binding = item.object("voice")?.object("native_binding") {
                let natives = journal.map { ($0, "journal") } + providers.map { ($0, "provider") }
                for (native, stream) in natives {
                    guard inScope(native), userTypes.contains(native.string("type")) else { continue }
                    let matched: Bool
                    if stream == "journal" {
                        if let seq = binding["journal_seq"]?.numberValue, let nativeSeq = native["seq"]?.numberValue { matched = Int64(nativeSeq) == Int64(seq) }
                        else { matched = false }
                    } else {
                        let providerId = binding.string("provider_message_id")
                        let providerSource = binding.string("provider_source")
                        matched = !providerId.isEmpty && native.string("message_id") == providerId &&
                            (providerSource.isEmpty || native.string("source") == providerSource) &&
                            (!binding.has("provider_source_offset") || native["source_offset"]?.compact == binding["provider_source_offset"]?.compact)
                    }
                    if matched, let row = parse(native, stream), !bound.contains(where: { $0.id == row.id }) { bound.append(row) }
                }
            }
            // Admission of the voice job does not consume the native inbox. Keep its
            // live status row, including after Stop, outside voice suppression aliases.
            let accepted = bound.count == 1 && bound[0].deliveryState == nil ? bound[0] : nil
            if let accepted { rows.removeAll { $0.id == accepted.id && $0.role == "You" && $0.voice == nil } }
            var next = metadata
            next.originalIds = Array(unique(metadata.originalIds + (accepted.map { [$0.id] } ?? [])).prefix(8))
            if let old = rows.firstIndex(where: { $0.voice?.id == voice.id }) {
                if rows[old].voice!.revision < voice.revision { rows[old] = next }
            } else {
                rows.append(next)
            }
        }
        if !raw.string("history_epoch").isEmpty { return rows }
        return rows.enumerated().sorted { $0.element.at != $1.element.at ? $0.element.at < $1.element.at : $0.offset < $1.offset }.map(\.element)
    }

    static func unique(_ values: [String]) -> [String] {
        var seen = Set<String>()
        return values.filter { seen.insert($0).inserted }
    }

    /// Why a message cannot be sent now, from verified native fields; empty when it can.
    public static func messageBlockReason(_ raw: JSONObject?) -> String {
        guard let raw else { return "Refresh to verify the session before sending." }
        let agent = raw.string("agent_id")
        if !agent.isEmpty && agent != "main" {
            if raw.bool("can_send") { return "" }
            let reason = raw.string("send_reason", "can_send_reason")
            return reason.isEmpty ? "This child conversation is read-only." : reason
        }
        if !raw.bool("tracked") { return "This session has no native tracking." }
        if raw.string("run_id").isEmpty { return "Waiting for the agent to finish starting." }
        if raw.string("runtime_state") != "live" || raw.string("process_state") != "running" { return "The agent is not running. Resume it on your computer." }
        if raw.bool("auth_required") || !raw.string("error").isEmpty { return "Complete login or setup on your computer." }
        func fallback(_ flag: String, _ reason: String, _ text: String) -> String {
            if raw.bool(flag) { return "" }
            let value = raw.string(reason)
            return value.isEmpty ? text : value
        }
        if !raw.string("expected_id").isEmpty { return fallback("resume_message_can_send", "resume_message_reason", "Waiting for the restored conversation.") }
        if raw.string("conversation_id").isEmpty { return fallback("first_message_can_send", "first_message_reason", "Waiting for the new conversation.") }
        let phase = raw.string("phase")
        if ["approval", "input"].contains(phase) { return "Answer the pending request or continue on your computer." }
        if phase == "interrupted" { return fallback("interrupted_message_can_send", "interrupted_message_reason", "Waiting for the input prompt after interruption.") }
        if phase == "error" && raw.object("provider_error") != nil { return fallback("error_message_can_send", "error_message_reason", "Check the agent before retrying.") }
        let activity = raw.string("activity")
        if activity == "idle" && phase == "idle" || activity == "busy" && ["idle", "working", "tool", "compacting"].contains(phase) { return "" }
        return "Waiting for the agent to confirm its input state."
    }

    /// A reported non-negative whole-number bound; anything else is no bound.
    static func limit(_ raw: JSONObject, _ key: String) -> Int? {
        guard case .int(let value)? = raw[key], value >= 0, value <= 1_000_000 else { return nil }
        return Int(value)
    }

    /// Only an explicit native OpenCode text item seeds its text: one line, clipped to `max_length` characters and the
    /// 4096-byte answer bound. Anything else has no default text.
    static func defaultText(_ item: JSONObject, source: String) -> String {
        guard source == "opencode_native", item.string("input_kind") == "text", case .string(let text)? = item["default_text"],
              !text.unicodeScalars.contains(where: PairingInvite.isISOControl) else { return "" }
        let characters = limit(item, "max_length") ?? Int.max
        var clipped = String.UnicodeScalarView()
        var count = 0, bytes = 0
        for scalar in text.unicodeScalars {
            let size = String(scalar).utf8.count
            if count == characters || bytes + size > 4096 { break }
            clipped.append(scalar)
            count += 1
            bytes += size
        }
        return String(clipped)
    }

    public static func questions(_ raw: JSONObject) -> [Question] {
        raw.objects("pending_questions").map { card in
            let delivery = card.object("answer_delivery").map { delivery in
                NativeQuestionDelivery(requestId: delivery.string("request_id"), questionId: delivery.string("question_id"),
                                       questionHash: delivery.string("question_hash"), run: delivery.string("run_id"),
                                       conversation: delivery.string("conversation_id"), status: delivery.string("status"),
                                       answers: delivery.objects("answers").map { answer in
                                           NativeQuestionAnswer(questionId: answer.string("question_id"), optionIds: answer.strings("selected_option_ids"),
                                                                text: answer.string("text"), skip: answer.bool("skip"))
                                       })
            }
            var created = 0.0
            if let value = card["created_at"]?.numberValue, value.isFinite, value > 0, value < 253_402_300_800 { created = value }
            return Question(id: card.string("question_id"), hash: card.string("question_hash"),
                            prompts: card.objects("questions").map { prompt in
                                Prompt(id: prompt.string("id"), text: prompt.string("question", "prompt"),
                                       choices: prompt.objects("options").map {
                                           Choice(id: $0.string("id"), label: $0.string("label"), description: $0.string("description"), scope: $0.string("scope"))
                                       },
                                       multi: prompt.bool("multi_select"), required: prompt.bool("required", default: true),
                                       other: prompt.bool("allow_other"), header: prompt.string("header"), body: prompt.string("body"),
                                       otherLabel: prompt.string("other_label"), otherDescription: prompt.string("other_description"),
                                       inputKind: ["choice", "text"].contains(prompt.string("input_kind")) ? prompt.string("input_kind") : "",
                                       minLength: limit(prompt, "min_length"), maxLength: limit(prompt, "max_length"),
                                       minItems: limit(prompt, "min_items"), maxItems: limit(prompt, "max_items"),
                                       defaultOptionIds: prompt.strings("default_option_ids"), defaultText: defaultText(prompt, source: card.string("source")),
                                       literalText: card.string("source") == "opencode_native" && !card.bool("approval"))
                            },
                            canAnswer: card.bool("can_answer"), canSkip: card.bool("can_skip"), optional: card.bool("optional"),
                            delivery: delivery?.status ?? "", createdAt: created, source: card.string("source"), approval: card.bool("approval"),
                            trustRequest: card.bool("trust_request"), unavailableReason: card.string("answer_unavailable_reason", "unavailable_reason"),
                            run: card.string("run_id"), conversation: card.string("conversation_id"), toolCallId: card.string("tool_call_id"),
                            answerDelivery: delivery, approvalChoices: card.bool("approval_choices"))
        }
    }
}
