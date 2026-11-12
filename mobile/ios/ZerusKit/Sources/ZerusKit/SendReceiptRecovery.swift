import CryptoKit
import Foundation

/// Frozen exact bytes, stored only inside the encrypted outbox row.
public struct SendIntent: Codable, Hashable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let canonical: String
    public let intentSha256: String
    public var description: String { "SendIntent(retained)" }
    public var debugDescription: String { description }
    public static func == (lhs: Self, rhs: Self) -> Bool {
        SendReceiptRecovery.exact(lhs.canonical, rhs.canonical) && lhs.intentSha256 == rhs.intentSha256
    }
    public func hash(into hasher: inout Hasher) { hasher.combine(Data(canonical.utf8)); hasher.combine(intentSha256) }
    public func value() -> JSONObject? {
        guard canonical.utf8.count <= 256 * 1024, let value = try? JSON.parse(canonical).object,
              SendReceiptRecovery.valid(value), let encoded = try? SendReceiptRecovery.encode(.object(value)),
              SendReceiptRecovery.exact(encoded, canonical), SendReceiptRecovery.hash(canonical) == intentSha256 else { return nil }
        return value
    }
}

public struct SendReceiptProof: Codable, Hashable, Sendable {
    public let messageRequestId: String
    public let intentSha256: String
    public let nativeDigest: String
    public let submittedAt: Double
    public let queued: Bool
    public let nativeMessageId: String
}

public enum SendReceiptCheck: Sendable {
    case original(JSONObject, deadline: Double)
    case native(SendIntent, SendReceiptProof, deadline: Double)
    case unconfirmed
}

public enum SendReceiptRecovery {
    public static let operation = "send_receipt"
    static let providers: Set<String> = ["claude", "codex", "kimi", "dsh", "opencode"]
    public static func exact(_ a: String, _ b: String) -> Bool { a.utf8.elementsEqual(b.utf8) }
    static func text(_ v: JSONObject, _ key: String) -> String? { if case .string(let s)? = v[key] { s } else { nil } }
    static func identity(_ s: String, _ max: Int, empty: Bool = false) -> Bool {
        (empty || !s.isEmpty) && s.utf8.count <= max && !s.unicodeScalars.contains { $0.properties.generalCategory == .control }
    }
    static func uuid(_ s: String) -> Bool { AttachmentPolicy.validId(s) && s != "00000000-0000-0000-0000-000000000000" }
    public static func valid(_ v: JSONObject) -> Bool {
        guard Set(v.keys) == Set(["schema", "computer_id", "machine_id", "session_id", "session", "provider", "native_encoding", "payload"]),
              v["schema"] == .int(1), let computer = text(v, "computer_id"), identity(computer, 36),
              let machine = text(v, "machine_id"), uuid(machine), let session = text(v, "session_id"), uuid(session),
              let name = text(v, "session"), identity(name, 512), let provider = text(v, "provider"), providers.contains(provider),
              let encoding = text(v, "native_encoding"), ["compact", "python_default"].contains(encoding), let p = v.object("payload"),
              Set(p.keys).isSuperset(of: ["request_id", "expected_run_id", "expected_conversation_id", "text"]),
              Set(p.keys).isSubset(of: ["request_id", "expected_run_id", "expected_conversation_id", "text", "attachments", "agent_id", "expected_compaction_id"]),
              let id = text(p, "request_id"), uuid(id), let run = text(p, "expected_run_id"), identity(run, 128),
              let cid = text(p, "expected_conversation_id"), identity(cid, 256, empty: true), let body = text(p, "text"),
              body.utf8.count <= 65_536, !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !body.unicodeScalars.contains(where: { $0.properties.generalCategory == .control && $0.value != 10 && $0.value != 9 }),
              !["/", "!"].contains(String(body.trimmingCharacters(in: .whitespacesAndNewlines).prefix(1))) else { return false }
        if p.has("attachments"), p["attachments"] != .array([]) { return false }
        if p.has("agent_id") {
            guard let agent = text(p, "agent_id"), identity(agent, 160), !agent.contains("/"), !agent.contains("\\"), !cid.isEmpty,
                  !p.has("expected_compaction_id") else { return false }
        }
        if p.has("expected_compaction_id") {
            guard let id = text(p, "expected_compaction_id"), id.isEmpty || uuid(id) else { return false }
        }
        return true
    }

    /// Python canonical ASCII JSON; native JSON uses UTF-8 and its original spacing.
    public static func encode(_ value: JSON, ascii: Bool = true, spaced: Bool = false) throws -> String {
        let separator = spaced ? ", " : ","
        switch value {
        case .null: return "null"
        case .bool(let b): return b ? "true" : "false"
        case .int(let n): return String(n)
        case .string(let s):
            var result = "\""
            for scalar in s.unicodeScalars {
                switch scalar.value {
                case 34: result += "\\\""
                case 92: result += "\\\\"
                case 8: result += "\\b"
                case 12: result += "\\f"
                case 10: result += "\\n"
                case 13: result += "\\r"
                case 9: result += "\\t"
                default:
                    if scalar.value < 32 || ascii && scalar.value > 126 {
                        for unit in String(scalar).utf16 { result += String(format: "\\u%04x", unit) }
                    } else { result.unicodeScalars.append(scalar) }
                }
            }
            return result + "\""
        case .array(let a): return "[" + (try a.map { try encode($0, ascii: ascii, spaced: spaced) }).joined(separator: separator) + "]"
        case .object(let o):
            return "{" + (try o.keys.sorted().map { key in
                try encode(.string(key), ascii: ascii) + (spaced ? ": " : ":") + encode(o[key]!, ascii: ascii, spaced: spaced)
            }).joined(separator: separator) + "}"
        case .double: throw ZerusError.invalid("Invalid send intent value.")
        }
    }
    public static func hash(_ value: String) -> String { SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined() }
    public static func nativeDigest(_ value: JSONObject) -> String? {
        guard let p = value["payload"] else { return nil }
        let native = ["dsh", "opencode"].contains(value.string("provider"))
        let input: JSON = native ? .array([.string(value.string("session")), p]) : p
        return (try? encode(input, ascii: false, spaced: !native && value.string("native_encoding") == "python_default")).map(hash)
    }
    public static func encoding(version: Int, route: MachineRoute) -> String? {
        if route == .unknown { return nil }
        if version == 1 { return "compact" }
        if version == 2 { return route == .direct ? "python_default" : "compact" }
        return nil
    }
    public static func machine(_ machines: [Machine], target: Target) -> Machine? {
        let matches = machines.filter { $0.connectionId == target.connectionId && $0.id == target.computerId && $0.online && !$0.lastKnown }
        return matches.count == 1 ? matches[0] : nil
    }
    public static func freeze(target: Target, id: String, text: String, provider: String, machineId: String, sessionId: String,
                              encoding: String, compactionId: String? = nil) -> SendIntent? {
        guard var p = try? target.json() else { return nil }
        p["request_id"] = .string(id); p["text"] = .string(text); p["attachments"] = .array([])
        if let compactionId { p["expected_compaction_id"] = .string(compactionId) }
        let v: JSONObject = ["schema": 1, "computer_id": .string(target.computerId), "machine_id": .string(machineId),
            "session_id": .string(sessionId), "session": .string(target.session), "provider": .string(provider),
            "native_encoding": .string(encoding), "payload": .object(p)]
        guard valid(v), let canonical = try? encode(.object(v)) else { return nil }
        return SendIntent(canonical: canonical, intentSha256: hash(canonical))
    }
    public static func matches(_ message: OutgoingMessage, _ intent: SendIntent) -> Bool {
        guard let v = intent.value(), let p = v.object("payload") else { return false }
        let t = message.target
        return message.attachments.isEmpty && t.archiveId.isEmpty && exact(p.string("request_id"), message.requestId) && exact(p.string("text"), message.text) &&
            exact(v.string("computer_id"), t.computerId) && exact(v.string("session"), t.session) && exact(p.string("expected_run_id"), t.run) &&
            exact(p.string("expected_conversation_id"), t.agentId.isEmpty ? t.conversation : t.parentConversation) && exact(p.string("agent_id"), t.agentId)
    }
    public static func request(_ id: String, _ intent: SendIntent) throws -> JSONObject {
        guard let value = intent.value() else { throw ZerusError.invalid("Original intent unavailable.") }
        return ["schema": 1, "request_id": .string(id), "intent_sha256": .string(intent.intentSha256), "intent": .object(value)]
    }
    public static func proof(_ id: String, _ intent: SendIntent, _ result: JSONObject) -> SendReceiptProof? {
        guard let v = intent.value(), let p = v.object("payload"), result["schema"] == .int(1) else { return nil }
        let expected = ["request_id": id, "message_request_id": p.string("request_id"), "computer_id": v.string("computer_id"),
            "machine_id": v.string("machine_id"), "session_id": v.string("session_id"), "session": v.string("session"), "provider": v.string("provider"),
            "run_id": p.string("expected_run_id"), "conversation_id": p.string("expected_conversation_id"), "agent_id": p.string("agent_id"),
            "intent_sha256": intent.intentSha256, "native_digest": nativeDigest(v) ?? "", "status": "submitted", "proof": "original_receipt"]
        for (key, value) in expected { guard let actual = text(result, key), exact(actual, value) else { return nil } }
        guard let at = result.number("submitted_at"), at.isFinite, at >= 0, case .bool(let queued)? = result["queued"] else { return nil }
        let nativeId: String
        if result.has("native_message_id") {
            guard let id = text(result, "native_message_id"), validNativeId(id, provider: v.string("provider")) else { return nil }
            nativeId = id
        } else {
            guard v.string("provider") != "opencode" else { return nil }
            nativeId = ""
        }
        return SendReceiptProof(messageRequestId: p.string("request_id"), intentSha256: intent.intentSha256,
                                nativeDigest: result.string("native_digest"), submittedAt: at, queued: queued, nativeMessageId: nativeId)
    }
    static func validNativeId(_ id: String, provider: String) -> Bool {
        provider == "opencode" && id.hasPrefix("msg_") && id.utf8.count > 4 && id.utf8.count <= 128 &&
            id.utf8.allSatisfy { (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 95 || $0 == 45 }
    }
    public static func acknowledgement(_ intent: SendIntent, _ result: JSONObject) -> SendReceiptProof? {
        guard let binding = result.object("delivery_binding"), binding["schema"] == .int(1) else { return nil }
        var normalized = result
        normalized["schema"] = 1; normalized["message_request_id"] = result["request_id"]
        normalized["computer_id"] = intent.value()?["computer_id"]; normalized["session"] = result["name"]
        for key in ["machine_id", "session_id", "provider"] { normalized[key] = binding[key] }
        normalized["agent_id"] = result["agent_id"] ?? ""; normalized["intent_sha256"] = .string(intent.intentSha256)
        normalized["native_digest"] = result["digest"]; normalized["proof"] = "original_receipt"; normalized["queued"] = result["queued"] ?? false
        return proof(result.string("request_id"), intent, normalized)
    }
    public static func settle(_ state: MessageState, captured: OutgoingMessage, intent: SendIntent, proof: SendReceiptProof,
                              deadline: Double, now: Double) -> MessageState {
        guard now < deadline, proof.submittedAt.isFinite, proof.submittedAt >= 0,
              let i = state.outgoing.indices.first(where: { state.outgoing[$0].requestId == captured.requestId }),
              state.outgoing.filter({ $0.requestId == captured.requestId }).count == 1 else { return state }
        let current = state.outgoing[i]
        guard current.status == "uncertain", current.target == captured.target, exact(current.text, captured.text), current.attachments == captured.attachments,
              current.createdAt == captured.createdAt, current.sendIntent == captured.sendIntent, matches(current, intent), proof.messageRequestId == current.requestId,
              proof.intentSha256 == intent.intentSha256, let v = intent.value(), proof.nativeDigest == nativeDigest(v),
              proof.nativeMessageId.isEmpty || validNativeId(proof.nativeMessageId, provider: v.string("provider")) else { return state }
        var copy = state
        copy.outgoing[i].status = "submitted"; copy.outgoing[i].submittedAt = proof.submittedAt; copy.outgoing[i].submittedText = current.text
        copy.outgoing[i].error = ""; copy.outgoing[i].sendIntent = intent; copy.outgoing[i].sendReceiptProof = proof
        return copy
    }
    public static func exactNativeId(_ message: OutgoingMessage) -> String? {
        guard let i = message.sendIntent, let p = message.sendReceiptProof, let v = i.value(), matches(message, i),
              p.messageRequestId == message.requestId, p.intentSha256 == i.intentSha256, p.nativeDigest == nativeDigest(v),
              p.submittedAt.isFinite, p.submittedAt >= 0, validNativeId(p.nativeMessageId, provider: v.string("provider")) else { return nil }
        return p.nativeMessageId
    }
    public static var now: Double { ProcessInfo.processInfo.systemUptime }
    public static func supported(relay: Set<String>?, native: Set<String>?) -> Bool { relay?.contains(operation) == true && native?.contains(operation) == true }
}
