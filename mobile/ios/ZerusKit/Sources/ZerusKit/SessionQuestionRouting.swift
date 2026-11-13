import Foundation

/// The child's switch as its computer reports it: whether its blocking questions go to its parent session, and (marker v2)
/// the display-only mirror of the person's latch in that computer's service, which is the authority (zerus-sa9qn.50).
public struct QuestionRouting: Equatable, Sendable {
    public let sendToParent: Bool
    public let revision: Int64
    public let personOff: Bool
    public init(sendToParent: Bool, revision: Int64, personOff: Bool = false) {
        self.sendToParent = sendToParent
        self.revision = revision
        self.personOff = personOff
    }
    /// Whether its questions go to the parent: the host says so and the person did not latch it off.
    public var sends: Bool { sendToParent && !personOff }
}

/// The person's per-child switch of the owner-approved question route (zerus-sa9qn.33), kept on the child's
/// computer beside its notification preference so every desktop and phone agrees. The computer refuses a change
/// from an agent's own process tree; this phone's request through the connector is the person's.
/// Shared contract: `question-routing-contract.json`.
public enum SessionQuestionRouting {
    public static let operation = "session_question_routing"
    public static let unavailable = "Question routing is unavailable. Update and refresh Zerus on this computer."
    public static let notChild = "Question routing belongs to a live Claude child session."
    public static let hint = "Applies to this child session on all devices."
    public static let uncertain = "Where this child's questions go was not confirmed. Check its original receipt; it has not been retried."
    public static let stale = "Where this child's questions go changed. Refresh before applying."
    public static let refusals: [String: String] = [
        "conflict": "Where this child's questions go changed on another device. Refresh and try again.",
        "stale_identity": "The conversation changed. Refresh its exact identity before applying.",
        "not_found": "This conversation is no longer available.",
        "unsupported": "This session has no durable identity, so its questions cannot be routed.",
        "agent_refused": "Only you can change where a child's questions go.",
        "capacity": "The computer's question routing storage is full. The setting was not changed.",
        "revision_exhausted": "The question routing revision is exhausted. The setting was not changed.",
    ]

    /// The person's latch through this phone's signed statement (zerus-sa9qn.50).
    public static let personOperation = "session_question_route_person"
    /// Only an encrypted workspace connection can sign, and only where both the relay and the computer offer it.
    public static func person(_ connection: Connection?, relay: Set<String>, machine: Set<String>) -> Bool {
        connection?.protocolVersion == 2 && relay.contains(personOperation) && machine.contains(personOperation)
    }
    /// The toggled switch at the row's revision, for the child's durable session ID.
    public static func personArguments(_ raw: JSONObject) throws -> JSONObject {
        guard raw.count == 3, case .string(let session)? = raw["session_id"], (try? WorkspaceProtocol.identifier(session)) != nil else {
            throw ZerusError.invalid("Invalid question routing setting.")
        }
        var plain = raw
        plain["session_id"] = nil
        _ = try arguments(plain)
        return raw
    }
    public static let personRefusals: [String: String] = [
        "not_verified": "The computer did not accept this phone's signature. Refresh this workspace and try again.",
        "stale_decision": "A newer choice for this child was made. Refresh and choose again.",
        "stale_identity": "The conversation changed. Refresh its exact identity before applying.",
        "not_found": "This conversation is no longer available.",
        "capacity": "The computer's question routing storage is full. The setting was not changed.",
        "invalid_request": "The computer refused this request. Refresh and try again.",
    ]
    /// Only the service's exact receipt of this statement settles it; a closed refusal fails; anything else is uncertain.
    public static func personResult(_ action: SessionAction, _ receipt: JSONObject) -> SessionAction {
        var copy = action
        let value = receipt.object("result")
        let args = try? personArguments(action.arguments)
        let completed = receipt["request_id"] == .string(action.requestId) && receipt["state"] == .string("completed") && args != nil
            && value?["request_id"] == .string(action.requestId) && value?["op"] == .string("question_route_child")
        if completed, let value, value["accepted"] == .bool(false), value.count == 5, value["durable"] == .bool(false),
           case .string(let code)? = value["code"], let message = personRefusals[code] {
            copy.status = "failed"
            copy.error = message
            return copy
        }
        guard completed, let value, let args, value.count == 9, value["accepted"] == .bool(true), value["durable"] == .bool(true),
              value["session_id"] == args["session_id"], case .bool(let send)? = args["send_to_parent"], value["person_off"] == .bool(!send),
              integer(value["revision"]) != nil, case .bool(_)? = value["applied"], value["code"] == .string("") else {
            copy.status = "uncertain"
            copy.error = uncertain
            return copy
        }
        copy.status = "completed"
        copy.error = ""
        return copy
    }

    private static func integer(_ value: JSON?) -> Int64? {
        guard case .int(let number)? = value, number >= 0 else { return nil }
        return number
    }

    /// Exactly `send_to_parent` and its `revision`, with `person_off` under v2; anything else is unknown, never a default.
    public static func parse(_ raw: JSONObject?) -> QuestionRouting? {
        guard let value = raw?.object("question_routing"), case .bool(let send)? = value["send_to_parent"],
              let revision = integer(value["revision"]) else { return nil }
        if value.count == 2 { return QuestionRouting(sendToParent: send, revision: revision) }
        guard value.count == 3, case .bool(let off)? = value["person_off"] else { return nil }
        return QuestionRouting(sendToParent: send, revision: revision, personOff: off)
    }
    public static let personLatched = "A person turned this child's questions off. Turn them on again from its own computer or a phone on an encrypted workspace."

    private static func agent(_ target: Target, _ raw: JSONObject) -> String {
        let cmd = raw.string("cmd")
        if !cmd.isEmpty { return cmd }
        let agent = raw.string("agent")
        if !agent.isEmpty { return agent }
        return String(target.session.split(separator: "/", omittingEmptySubsequences: false).first ?? "")
    }

    /// A live Claude session with a parent link: only those route their questions in this version.
    public static func applies(_ target: Target, _ raw: JSONObject?) -> Bool {
        guard let raw, target.agentId.isEmpty, target.archiveId.isEmpty, raw.object("parent") != nil,
              raw.string("state") != "archived" else { return false }
        return agent(target, raw) == "claude"
    }

    public static func parentName(_ raw: JSONObject?) -> String {
        let name = raw?.object("parent")?.string("name") ?? ""
        let last = name.split(separator: "/", omittingEmptySubsequences: false).last.map(String.init) ?? ""
        return last.isEmpty ? "parent" : last
    }

    /// What choosing it does: turn sending off, turn it on, or (unknown) nothing yet.
    public static func label(_ raw: JSONObject?) -> String {
        let parent = parentName(raw)
        switch parse(raw)?.sends {
        case true?: return "Stop sending questions to \(parent)"
        case false?: return "Send questions to \(parent)"
        case nil: return "Questions to \(parent)"
        }
    }

    public static func reason(_ target: Target, _ raw: JSONObject?) -> String {
        if !applies(target, raw) { return notChild }
        if target.run.isEmpty || target.conversation.isEmpty || parse(raw) == nil { return unavailable }
        return ""
    }

    /// The confirmed change turns the current value the other way at its revision.
    public static func toggle(_ routing: QuestionRouting?) -> JSONObject {
        ["send_to_parent": .bool(routing?.sends != true), "expected_revision": .int(routing?.revision ?? -1)]
    }

    public static func arguments(_ raw: JSONObject) throws -> JSONObject {
        guard raw.count == 2, case .bool(_)? = raw["send_to_parent"], integer(raw["expected_revision"]) != nil else {
            throw ZerusError.invalid("Invalid question routing setting.")
        }
        return raw
    }

    public static func payload(_ target: Target, _ args: JSONObject) -> JSONObject {
        ["expected_run_id": .string(target.run), "expected_conversation_id": .string(target.conversation),
         "expected_revision": args["expected_revision"] ?? .null, "send_to_parent": args["send_to_parent"] ?? .null]
    }

    /// Only the computer's exact reply settles it: a closed refusal fails, the exact confirmation completes,
    /// anything else is uncertain and never retried.
    public static func result(_ action: SessionAction, _ receipt: JSONObject) -> SessionAction {
        var copy = action
        let completed = receipt["request_id"] == .string(action.requestId) && receipt["state"] == .string("completed")
        if completed, let value = receipt.object("result"), value["ok"] == .bool(false), !value.has("applied"),
           case .string(let error)? = value["error"], let message = refusals[error] {
            copy.status = "failed"
            copy.error = message
            return copy
        }
        guard completed, let value = receipt.object("result"), value["ok"] == .bool(true), value.count == 5,
              value["run_id"] == .string(action.target.run), value["conversation_id"] == .string(action.target.conversation),
              case .bool(let applied)? = value["applied"], let args = try? arguments(action.arguments),
              case .bool(let send)? = args["send_to_parent"], let revision = integer(args["expected_revision"]),
              !(applied && revision == Int64.max),
              let routing = parse(["question_routing": value["question_routing"] ?? .null]),
              routing.sendToParent == send, routing.revision == revision + (applied ? 1 : 0) else {
            copy.status = "uncertain"
            copy.error = uncertain
            return copy
        }
        copy.status = "completed"
        copy.error = ""
        return copy
    }
}
