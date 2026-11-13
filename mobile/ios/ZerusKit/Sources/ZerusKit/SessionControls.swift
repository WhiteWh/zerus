import Foundation

/// Native session actions: exact arguments, the evidence they were authorized against,
/// and receipts that must confirm this exact request. Uncertain attempts are never replayed.
public enum SessionActionPolicies {
    public static let lifecycle: Set<String> = ["pause", "resume", "archive", "rename", "fork", "terminate", "restore", "forget"]
    public static let operations: Set<String> = lifecycle.union(["send_now", "settings", "process_stop", "terminal_input", "launch", "worktree_create",
                                                                 "directory_create", RepositoryAcquisition.clone, "recovery_action", SessionNotificationPreferences.operation,
                                                                 SessionQuestionRouting.operation, SessionQuestionRouting.personOperation])
    public static let terminalKeys: Set<String> = ["Enter", "Escape", "Tab", "BTab", "BSpace", "Up", "Down", "Left", "Right", "Home", "End", "PPage",
                                                   "NPage", "DC", "C-c", "C-d", "C-l", "C-a", "C-e", "C-u", "C-w"]
    /// The relay's fixed effort levels; other native variants need `effortVariants`.
    static let efforts: Set<String> = ["", "off", "none", "on", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"]
    public static let effortUpdateRequired = "This effort needs an updated relay and Zerus on this computer."

    /// Native variants beyond the relay's fixed levels: only for an OpenCode session when the relay and this computer
    /// both advertise `LaunchPresentation.effortFeature`.
    public static func effortVariants(agent: String, relay: Set<String>?, computer: Set<String>?) -> Bool {
        agent == "opencode" && LaunchPresentation.effortSupported(relay: relay, computer: computer)
    }

    static func control(_ text: String) -> Bool { text.unicodeScalars.contains(where: PairingInvite.isISOControl) }
    static func blank(_ text: String) -> Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    static func string(_ raw: JSONObject, _ key: String) throws -> String {
        guard case .string(let value)? = raw[key] else { throw ZerusError.invalid("Missing action argument.") }
        return value
    }

    /// Validated exact arguments for one operation; unknown keys are refused. A settings effort outside the relay's
    /// fixed levels needs `effortVariants` for this session, relay and computer.
    public static func arguments(_ operation: String, _ raw: JSONObject, effortVariants: Bool = false) throws -> JSONObject {
        if operation == RepositoryAcquisition.clone {
            let checked = try RepositoryAcquisition.arguments(machineId: string(raw,"expected_machine_id"), url: string(raw,"url"), path: RepositoryAcquisition.destination(string(raw,"parent"),string(raw,"name")))
            guard Set(raw.keys) == Set(checked.keys) else { throw ZerusError.invalid("Unexpected checkout argument.") }
            return checked
        }
        if operation == SessionNotificationPreferences.operation { return try SessionNotificationPreferences.arguments(raw) }
        if operation == SessionQuestionRouting.operation { return try SessionQuestionRouting.arguments(raw) }
        if operation == SessionQuestionRouting.personOperation { return try SessionQuestionRouting.personArguments(raw) }
        guard operations.contains(operation) else { throw ZerusError.invalid("Unsupported session action.") }
        let allowed: Set<String>
        switch operation {
        case "rename": allowed = ["new_name"]
        case "fork": allowed = ["tag"]
        case "send_now": allowed = ["queue_id"]
        case "launch": allowed = ["agent", "directory", "tag", "account_id", "model", "effort", "swarm_id", "project_id", "project_folder_id", "add_folder",
                                  "worktree_folder_id", "worktree_common_dir"]
        case "worktree_create": allowed = ["path", "common_dir", "destination", "branch", "base"]
        case "directory_create": allowed = ["path"]
        case "recovery_action": allowed = ["job_id", "action"]
        case "terminal_input": allowed = ["terminal_binding_id", "text", "enter", "key"]
        case "settings": allowed = ["model", "effort", "expected_pending_id"]
        case "process_stop": allowed = ["process_id", "generation"]
        default: allowed = []
        }
        guard raw.keys.allSatisfy(allowed.contains) else { throw ZerusError.invalid("Unexpected action argument.") }
        func require(_ condition: Bool, _ message: String = "Invalid action argument.") throws {
            if !condition { throw ZerusError.invalid(message) }
        }
        switch operation {
        case "directory_create":
            try require(DirectoryCreation.pathError(try string(raw, "path")).isEmpty, "Enter a valid absolute path for a new folder.")
        case "rename", "fork":
            let rename = operation == "rename"
            let name = try string(raw, rename ? "new_name" : "tag")
            try require(!blank(name) && name.utf8.count <= (rename ? 512 : 120) && !control(name) && !name.contains("\\") && (rename || !name.contains("/")),
                        "Enter a valid session name.")
        case "recovery_action":
            try require(AttachmentPolicy.validId(try string(raw, "job_id")))
            try require(["now", "cancel"].contains(try string(raw, "action")))
        case "send_now":
            try require((try string(raw, "queue_id")).range(of: "^[0-9a-f]{64}$", options: .regularExpression) != nil, "The queued message identity changed.")
        case "settings":
            try require(raw.has("model") || raw.has("effort"), "Choose a model or effort.")
            if raw.has("model") {
                let model = try string(raw, "model")
                try require(!blank(model) && model.utf16.count <= 120 && !control(model), "Invalid model.")
            }
            // OpenCode variants are open-ended; `inspectReason` checks the model's fresh `effort_options`.
            if raw.has("effort") {
                let effort = try string(raw, "effort")
                if !efforts.contains(effort) {
                    try require(EffortOption.valid(effort), "Invalid effort.")
                    try require(effortVariants, effortUpdateRequired)
                }
            }
            if raw.has("expected_pending_id") { try require(AttachmentPolicy.validId(try string(raw, "expected_pending_id")), "The pending settings identity changed.") }
        case "worktree_create":
            for key in ["path", "common_dir", "destination"] { try require(LaunchPresentation.absoluteFolder(try string(raw, key))) }
            try require(WorktreePresentation.createError(branch: try string(raw, "branch"), base: try string(raw, "base"),
                                                         destination: try string(raw, "destination")).isEmpty)
        case "launch":
            let agent = try string(raw, "agent")
            try require(LaunchPresentation.agentIds.contains(agent))
            if raw.has("model") { try require(agent == "opencode" && LaunchPresentation.validModel(try string(raw, "model")), "Choose a valid native OpenCode model.") }
            if raw.has("effort") {
                try require(agent == "opencode" && raw.has("model") && EffortOption.valid(try string(raw, "effort")), "Choose an effort offered for this model.")
            }
            let directory = try string(raw, "directory")
            try require(directory.hasPrefix("/") && directory.utf16.count <= 4096 && !control(directory))
            let tag = try string(raw, "tag")
            try require(!blank(tag) && tag.trimmingCharacters(in: .whitespacesAndNewlines) == tag && tag.utf8.count <= 120 &&
                        !tag.unicodeScalars.contains(where: { "/\\:.".unicodeScalars.contains($0) || PairingInvite.isISOControl($0) }))
            if raw.has("account_id") {
                let account = try string(raw, "account_id")
                try require(!blank(account) && account.utf16.count <= 256 && !control(account))
            }
            if ["swarm_id", "project_id", "project_folder_id", "add_folder", "worktree_folder_id", "worktree_common_dir"].contains(where: raw.has) {
                try require(!blank(try string(raw, "swarm_id")) && !blank(try string(raw, "project_id")))
                guard case .bool(let add)? = raw["add_folder"] else { throw ZerusError.invalid("Invalid action argument.") }
                if raw.has("project_folder_id") { try require(!blank(try string(raw, "project_folder_id")) && !add) }
            }
        case "terminal_input":
            try require(!blank(try string(raw, "terminal_binding_id")))
            try require(raw.has("text") != raw.has("key"))
            if raw.has("text") {
                try require((try string(raw, "text")).utf8.count <= 65536)
                guard case .bool? = raw["enter"] else { throw ZerusError.invalid("Invalid action argument.") }
            } else {
                try require(!raw.has("enter") && terminalKeys.contains(try string(raw, "key")))
            }
        case "process_stop":
            let id = try string(raw, "process_id")
            try require(!blank(id) && id.utf16.count <= 256 && !control(id), "Invalid process identity.")
            if raw.has("generation") {
                let generation = try string(raw, "generation")
                try require(!blank(generation) && generation.utf16.count <= 256 && !control(generation), "Invalid process generation.")
            }
        default: break
        }
        return raw
    }

    /// The native fields an action was authorized against; a changed value invalidates the confirmation.
    public static func evidence(_ operation: String, _ raw: JSONObject, _ args: JSONObject = [:]) -> String {
        if operation == "process_stop" {
            let process = (raw.object("processes")?.objects("items") ?? []).first { $0.string("id") == args.string("process_id") }
            var value: JSONObject = ["generation": raw.object("processes")?["generation"] ?? raw["generation"] ?? .null]
            for field in ["id", "run_id", "conversation_id", "status", "capabilities"] { value[field] = process?[field] ?? .null }
            return JSON.object(value).compact
        }
        let fields: [String]
        switch operation {
        case SessionNotificationPreferences.operation: fields = ["run_id","conversation_id","archive_id","notification_preferences"]
        case SessionQuestionRouting.operation: fields = ["run_id","conversation_id","archive_id","question_routing","parent","state"]
        case SessionQuestionRouting.personOperation: fields = ["run_id","conversation_id","archive_id","question_routing","parent","state","session_id"]
        case "settings": fields = ["model", "effort", "model_options", "effort_options", "pending_model", "pending_effort", "pending_settings_id",
                                   "settings_apply_when", "settings_change_supported"]
        case "send_now": fields = ["input_queue"]
        case "recovery_action": fields = ["recovery"]
        default: fields = ["state", "runtime_state", "process_state", "run_id", "conversation_id", "archive_id"]
        }
        var value: JSONObject = [:]
        for field in fields { value[field] = raw[field] ?? .null }
        return JSON.object(value).compact
    }

    /// Why a fresh inspection no longer permits this action, or "" when it does.
    public static func inspectReason(_ target: Target, _ operation: String, _ args: JSONObject, _ raw: JSONObject) -> String {
        if operation == SessionQuestionRouting.operation || operation == SessionQuestionRouting.personOperation {
            let reason = SessionQuestionRouting.reason(target,raw)
            if !reason.isEmpty { return reason }
            if SessionQuestionRouting.parse(raw)?.revision != args.int64("expected_revision") { return SessionQuestionRouting.stale }
            if operation == SessionQuestionRouting.personOperation && raw["session_id"] != args["session_id"] {
                return SessionQuestionRouting.refusals["stale_identity"] ?? ""
            }
            return ""
        }
        if operation == SessionNotificationPreferences.operation {
            let reason = SessionNotificationPreferences.reason(target,raw)
            if !reason.isEmpty { return reason }
            return SessionNotificationPreferences.parse(raw,target)?.revision == args.int64("expected_revision") ? "" : "Shared notification settings changed. Refresh before applying."
        }
        if lifecycle.contains(operation) && !raw.strings("allowed_actions").contains(operation) {
            let reason = raw.object("action_reasons")?.string(operation) ?? ""
            return reason.isEmpty ? "The native session no longer allows this action." : reason
        }
        switch operation {
        case "rename":
            let parts = args.string("new_name").components(separatedBy: "/"), old = target.session.components(separatedBy: "/")
            if parts.count != 3 || !(2...3).contains(old.count) || Array(parts.prefix(2)) != Array(old.prefix(2)) ||
                parts.contains(where: { blank($0) || $0.trimmingCharacters(in: .whitespacesAndNewlines) != $0 ||
                    $0.unicodeScalars.contains(where: { $0 == ":" || $0 == "." || PairingInvite.isISOControl($0) }) }) {
                return "Keep the original provider and project; choose a valid session tag."
            }
        case "recovery_action":
            guard let recovery = raw.object("recovery") else { return "No native recovery job is present." }
            let identity = recovery.array("identity") ?? []
            if recovery.string("id") != args.string("job_id") || recovery.string("state") != "waiting" || identity.count < 2 ||
                identity[0] != .string(target.run) || identity[1] != .string(target.conversation) {
                return "The native recovery job changed or is no longer waiting."
            }
        case "send_now":
            let queue = raw.object("input_queue")
            if queue?.string("id") != args.string("queue_id") || queue?.bool("can_send_now") != true { return "The queued message changed or cannot be sent now." }
        case "settings":
            if !raw.bool("settings_change_supported") {
                let reason = raw.string("settings_change_reason")
                return reason.isEmpty ? "Native settings are unavailable." : reason
            }
            if args.has("expected_pending_id") && raw.string("pending_settings_id") != args.string("expected_pending_id") { return "The pending settings changed." }
            let model = args.has("model") ? args.string("model") : raw.string("model")
            let choice = raw.objects("model_options").first { $0.string("id") == model }
            if args.has("model") && choice == nil { return "The selected model is no longer in the native catalog." }
            if args.has("effort") && !args.string("effort").isEmpty {
                let options = choice?.array("effort_options") ?? raw.array("effort_options")
                if options == nil || !(options ?? []).contains(.string(args.string("effort"))) { return "The selected effort is unavailable for this model." }
            }
        case "process_stop":
            guard let process = (raw.object("processes")?.objects("items") ?? []).first(where: { $0.string("id") == args.string("process_id") }) else {
                return "This process is no longer present."
            }
            if process.string("run_id") != target.run || process.string("conversation_id") != target.conversation || process.object("capabilities")?.bool("stop") != true {
                return "This process cannot be stopped under the selected conversation."
            }
            let generation = raw.object("processes")?.string("generation") ?? ""
            if args.has("generation") && args.string("generation") != (generation.isEmpty ? raw.string("generation") : generation) {
                return "The process generation changed."
            }
        default: break
        }
        return ""
    }

    /// Settles one action from its original receipt. Anything not proven is uncertain.
    public static func result(_ action: SessionAction, _ receipt: JSONObject) -> SessionAction {
        if action.operation == RepositoryAcquisition.clone { return RepositoryAcquisition.result(action,receipt) }
        var copy = action
        func uncertain(_ message: String) -> SessionAction {
            copy.status = "uncertain"
            copy.error = message
            return copy
        }
        if receipt.string("request_id") != action.requestId { return uncertain("The receipt belongs to another request.") }
        if receipt.string("state") == "failed" {
            let message = receipt.string("error")
            copy.status = "failed"
            copy.error = message.isEmpty ? "The computer rejected this action." : message
            return copy
        }
        if action.operation == SessionNotificationPreferences.operation { return SessionNotificationPreferences.result(action,receipt) }
        if action.operation == SessionQuestionRouting.operation { return SessionQuestionRouting.result(action,receipt) }
        if action.operation == SessionQuestionRouting.personOperation { return SessionQuestionRouting.personResult(action,receipt) }
        if action.operation == MobileDelivery.operation { return MobileDelivery.result(action, receipt) }
        if action.operation == DirectoryCreation.operation { return DirectoryCreation.result(action, receipt) }
        let native = receipt.object("result")
        let args = action.arguments
        if action.operation == "worktree_create" {
            guard receipt.string("state") == "completed", let native, native.string("request_id") == action.requestId, native.string("status") == "created",
                  native.string("common_dir") == args.string("common_dir"), native.string("branch") == args.string("branch"),
                  LaunchPresentation.absoluteFolder(native.string("path")) else {
                return uncertain("Worktree creation is unconfirmed. Check the original receipt; it has not been retried.")
            }
            copy.status = "completed"
            copy.error = ""
            copy.resultPath = native.string("path")
            return copy
        }
        if action.operation == "launch" {
            guard receipt.string("state") == "completed", let native, native.string("request_id") == action.requestId, native.string("status") == "created",
                  let created = native.object("result_target"), !created.string("name").isEmpty, !created.string("run_id").isEmpty,
                  created.string("archive_id").isEmpty else {
                return uncertain("The new session was not confirmed. Check this original launch receipt; it has not been retried.")
            }
            let assignment = native.object("project_assignment")
            let confirmed = !args.has("project_id") || (assignment?.string("status") == "assigned" && assignment?.string("swarm_id") == args.string("swarm_id") &&
                                                         assignment?.string("project_id") == args.string("project_id"))
            copy.status = "completed"
            copy.error = confirmed ? "" : "Session created; project assignment was not confirmed. Review Projects. This launch will not be repeated."
            copy.resultTarget = action.target.with(session: created.string("name"), run: created.string("run_id"), conversation: created.string("conversation_id"))
            return copy
        }
        guard receipt.string("state") == "completed", let native, native.string("request_id") == action.requestId, native.string("name") == action.target.session,
              native.string("run_id") == action.target.run, native.string("conversation_id") == action.target.conversation,
              native.string("archive_id") == action.target.archiveId else {
            return uncertain("The native result did not confirm this exact request.")
        }
        let status = native.string("status")
        let accepted: Bool
        switch action.operation {
        case "terminal_input": accepted = status == "submitted" && native.string("terminal_binding_id") == args.string("terminal_binding_id")
        case "recovery_action":
            accepted = status == (args.string("action") == "now" ? "scheduled" : "cancelled") && native.string("job_id") == args.string("job_id") &&
                native.string("action") == args.string("action")
        case "send_now": accepted = status == "submitted" && native.string("queue_id") == args.string("queue_id")
        case "settings":
            accepted = ["applied", "scheduled"].contains(status) && (status != "scheduled" || AttachmentPolicy.validId(native.string("pending_settings_id"))) &&
                ["model", "effort"].allSatisfy { !args.has($0) || (native.has($0) && native[$0] == args[$0]) }
        case "process_stop": accepted = status == "requested" && native.string("id") == args.string("process_id")
        default: accepted = status == "completed"
        }
        if !accepted { return uncertain("The native outcome was not confirmed. Nothing has been retried.") }
        var result: Target?
        if let value = native.object("result_target"), !value.string("name").isEmpty, !value.string("run_id").isEmpty {
            result = action.target.with(session: value.string("name"), run: value.string("run_id"), conversation: value.string("conversation_id"),
                                        archiveId: value.string("archive_id"))
        }
        copy.status = action.operation == "settings" ? status : "completed"
        copy.error = ""
        copy.resultTarget = result
        copy.pendingId = native.string("pending_settings_id")
        return copy
    }
}

/// A confirmed receipt may change presentation, never replay its native command.
public enum SessionActionFollowUp {
    public enum Kind: Sendable { case none, refresh, sessions, openResult }

    public static func kind(_ action: SessionAction, selected: Target?, sameNavigation: Bool) -> Kind {
        guard selected == action.target, sameNavigation else { return .none }
        guard action.status == "completed" else { return .refresh }
        switch action.operation {
        case "archive", "forget": return .sessions
        case "restore": return openable(action) ? .openResult : .none
        case "rename", "resume":
            return action.resultTarget.map { ActionDraftPolicies.proof(action, action.target, $0) } == true ? .openResult : .refresh
        default: return .refresh
        }
    }

    public static func openable(_ action: SessionAction) -> Bool {
        guard let destination = action.resultTarget else { return false }
        return action.status == "completed" && destination != action.target && destination.connectionId == action.target.connectionId &&
            destination.computerId == action.target.computerId && destination.agentId.isEmpty &&
            (ActionDraftPolicies.proof(action, action.target, destination) ||
             (action.operation == "restore" && !action.target.archiveId.isEmpty && destination.archiveId.isEmpty && !destination.run.isEmpty))
    }
}

public enum SessionControlPresentation {
    /// Native presentation decisions; exact-identity authorization remains with the model.
    public static func reason(_ target: Target, _ operation: String, _ raw: JSONObject?) -> String {
        if operation == SessionNotificationPreferences.operation { return SessionNotificationPreferences.reason(target,raw) }
        if operation == SessionQuestionRouting.operation { return SessionQuestionRouting.reason(target,raw) }
        guard let raw else { return "Refresh native session details." }
        if !target.agentId.isEmpty { return "Session controls belong to the parent conversation." }
        guard let allowed = raw.array("allowed_actions") else {
            return "The computer did not report native action availability. Refresh its session details."
        }
        if allowed.contains(.string(operation)) { return "" }
        let reason = raw.object("action_reasons")?.string(operation) ?? ""
        return reason.isEmpty ? "This native action is unavailable in the current session state." : reason
    }
}

/// The original scheduled authorization keeps its one attempt proof even if terminal receipts are pruned.
public enum ScheduledSettingsAttempt {
    public static func begin(_ state: MessageState, authorized: SessionAction, attempt: SessionAction) throws -> MessageState {
        guard var source = state.sessionActions.first(where: { $0.requestId == authorized.requestId && $0.status == "scheduled" && $0.applyAttemptId.isEmpty &&
            $0.pendingId == authorized.pendingId }) else { throw ZerusError.invalid("This authorized settings intent already has an attempt.") }
        let from = source.target, to = attempt.target
        guard source.operation == "settings", attempt.operation == "settings", from.connectionId == to.connectionId, from.computerId == to.computerId,
              from.session == to.session, !from.conversation.isEmpty, from.conversation == to.conversation, to.agentId.isEmpty, to.archiveId.isEmpty,
              ["model", "effort"].allSatisfy({ !source.arguments.has($0) || source.arguments[$0] == attempt.arguments[$0] }),
              attempt.arguments.string("expected_pending_id") == source.pendingId else {
            throw ZerusError.invalid("This authorized settings intent does not match the attempt.")
        }
        source.applyAttemptId = attempt.requestId
        return state.action(source).action(attempt)
    }
}

// MARK: - Context commands

/// A continuation is an in-memory intent, never reconstructed from durable records.
public struct CompactContinuation: Hashable, Sendable {
    public let requestId: String
    public let draft: Draft
    public let navigation: Int64
    public let startedAt: Double

    public init(requestId: String, draft: Draft, navigation: Int64, startedAt: Double) {
        self.requestId = requestId
        self.draft = draft
        self.navigation = navigation
        self.startedAt = startedAt
    }

    public func unchanged(_ current: Draft) -> Bool { ContextPolicies.sameDraft(draft, current) }
    public func allowed(target: Target?, navigation: Int64, now: Double) -> Bool {
        target == draft.target && navigation == self.navigation && (0...600).contains(now - startedAt)
    }
    public func permitsTransport(target: Target?, navigation: Int64, now: Double, written: Draft, current: Draft) -> Bool {
        allowed(target: target, navigation: navigation, now: now) && written.key == draft.key && current.key == written.key &&
            current.generation == written.generation && current.revision == written.revision && current.text.isEmpty && current.attachments.isEmpty
    }
}

extension ContextPolicies {
    /// The native outcome of this exact compaction request, once the session is idle again.
    public static func compactStatus(_ operation: ContextOperation, _ raw: JSONObject) -> String? {
        let target = operation.target
        guard operation.operation == "compact_context", target.archiveId.isEmpty, raw.string("archive_id").isEmpty, raw.string("state") != "archived",
              raw.string("run_id") == target.run, raw.string("conversation_id") == target.conversation, raw.string("name") == target.session,
              let request = raw.object("compact_context_request"), request.string("request_id") == operation.requestId,
              request.string("run_id") == target.run, request.string("conversation_id") == target.conversation else { return nil }
        let status = request.string("status")
        if status == "completed" && (raw.string("activity") != "idle" || raw.string("phase") != "idle") { return nil }
        return ["completed", "failed", "cancelled", "unchanged", "uncertain"].contains(status) ? status : nil
    }

    public static func result(_ operation: ContextOperation, _ receipt: JSONObject) -> ContextOperation {
        var copy = operation
        if receipt.string("request_id") != operation.requestId {
            copy.status = "uncertain"
            copy.error = "Gateway returned a different request. Nothing has been retried."
            return copy
        }
        if receipt.string("state") == "failed" {
            copy.status = "failed"
            copy.error = "The computer rejected the context command."
            return copy
        }
        guard receipt.string("state") == "completed", let native = receipt.object("result"), native.string("request_id") == operation.requestId,
              native.string("name") == operation.target.session, native.string("run_id") == operation.target.run,
              native.string("conversation_id") == operation.target.conversation else {
            copy.status = "uncertain"
            copy.error = "Context delivery is unconfirmed. Check the original receipt."
            return copy
        }
        switch native.string("status") {
        case "submitted":
            copy.status = "submitted"
            copy.error = ""
        case "confirmed" where operation.operation == "clear_context":
            copy.status = "completed"
            copy.error = ""
        case "confirmed":
            copy.status = "uncertain"
        default:
            copy.status = "uncertain"
            copy.error = "The native context command did not confirm its outcome."
        }
        return copy
    }

    /// Moves the composition to the new conversation that a confirmed clear created.
    public static func promoteClearDraft(_ state: MessageState, _ operation: ContextOperation, to: Target) -> MessageState {
        let from = operation.target
        guard var confirmed = state.contextOperations.first(where: { $0.requestId == operation.requestId && $0.status == "completed" }),
              operation.operation == "clear_context", operation.status == "completed", from.archiveId.isEmpty, to.archiveId.isEmpty,
              from.connectionId == to.connectionId, from.computerId == to.computerId, from.session == to.session, from.run == to.run,
              !to.conversation.isEmpty, from.conversation != to.conversation,
              let editor = state.drafts.first(where: { $0.target == from && $0.questionId.isEmpty && $0.detachedId.isEmpty && $0.status == "editing" }),
              !state.drafts.contains(where: { $0.target == to && $0.questionId.isEmpty && $0.detachedId.isEmpty }) else { return state }
        confirmed.resolvedConversation = to.conversation
        var next = state.context(confirmed)
        next.drafts = state.drafts.map { draft in
            guard draft.key == editor.key else { return draft }
            var moved = draft
            moved.target = to
            return moved
        }
        return next
    }
}

// MARK: - Recovery

public struct RecoveryAttempt: Hashable, Sendable {
    public let at: Double?
    public let status: String
    public let attempt: Int?
    public let reason: String
}

public struct NativeRecovery: Hashable, Sendable {
    public let id: String
    public let state: String
    public let reason: String
    public let action: String
    public let acknowledged: Bool
    public let attempt: Int?
    public let classAttempt: Int?
    public let delays: [Int]
    public let dueAt: Double?
    public let notBefore: Double?
    public let history: [RecoveryAttempt]
    public let recorded: Bool
    public let resumed: Bool

    public func status(now: Double) -> String { RecoveryPresentation.status(state, acknowledged: acknowledged, due: dueAt, now: now, resumed: resumed) }

    public var attempts: String {
        guard let attempt else { return "Attempts not reported" }
        if delays.last == 0 { return "\(attempt) attempts (repeats until stopped)" }
        if delays.isEmpty { return "\(attempt) attempts" }
        return "\(classAttempt ?? attempt)/\(delays.count - (delays.last! <= 0 ? 1 : 0)) attempts"
    }

    public func retryTimeAllowed(now: Double) -> Bool { notBefore.map { now >= $0 } ?? true }
}

/// The literal native recovery job. No provider retry schedule or success is inferred.
public enum RecoveryPresentation {
    public static func from(_ raw: JSONObject?, _ target: Target, verified: Bool) -> NativeRecovery? {
        guard let raw, let job = raw.object("recovery"), case .string(let state)? = job["state"], !state.trimmingCharacters(in: .whitespaces).isEmpty else { return nil }
        let identity = job.array("identity") ?? []
        let exact = raw.string("name") == target.session && raw.string("run_id") == target.run && raw.string("conversation_id") == target.conversation &&
            job.string("name") == target.session && identity.first == .string(target.run) && identity.dropFirst().first == .string(target.conversation)
        let delays = (job.array("delays") ?? []).prefix(16).compactMap { integer($0, -1, 86400) }
        let history = job.objects("history").suffix(32).map { entry in
            RecoveryAttempt(at: time(entry["at"]), status: status(entry.string("state"), acknowledged: entry.bool("acknowledged"), due: nil, now: 0),
                            attempt: integer(entry["attempt"], 0, Int(Int32.max)), reason: entry.string("reason"))
        }
        return NativeRecovery(id: job.string("id"), state: state, reason: job.string("reason"), action: job.string("action"), acknowledged: job.bool("acknowledged"),
                              attempt: integer(job["attempt"], 0, Int(Int32.max)), classAttempt: integer(job["class_attempt"], 0, Int(Int32.max)), delays: delays,
                              dueAt: time(job["due_at"]), notBefore: time(job["not_before"]), history: Array(history),
                              recorded: !verified || !exact || !target.archiveId.isEmpty || !target.agentId.isEmpty, resumed: resumed(job))
    }

    /// The agent accepted the continuation and the provider served it again. The episode stays
    /// open until the turn ends only to keep its attempt budget; the turn is ordinary work.
    public static func resumed(_ job: JSONObject?) -> Bool {
        guard let job, job.string("state") == "retrying", job.bool("acknowledged") else { return false }
        return (time(job["resumed_at"]) ?? 0) > 0
    }

    static func integer(_ value: JSON?, _ min: Int, _ max: Int) -> Int? {
        guard let number = value?.numberValue, number.isFinite, number >= Double(min), number <= Double(max), number.truncatingRemainder(dividingBy: 1) == 0 else { return nil }
        return Int(number)
    }

    static func time(_ value: JSON?) -> Double? {
        guard let number = value?.numberValue, number.isFinite, (0...253_402_300_799).contains(number) else { return nil }
        return number
    }

    public static func status(_ state: String, acknowledged: Bool, due: Double?, now: Double, resumed: Bool = false) -> String {
        switch state {
        case "waiting":
            if let due, Int64(max(due - now, 0).rounded(.up)) > 0 { return "Retry in \(Int64(max(due - now, 0).rounded(.up))) s" }
            return "Waiting to retry"
        case "dispatching": return "Sending recovery request…"
        case "retrying": return !acknowledged ? "Waiting for delivery confirmation…" : resumed ? "Agent resumed after recovery" : "Recovery in progress"
        case "exhausted": return "Automatic attempts exhausted"
        case "uncertain": return "Check the agent before retrying"
        case "blocked": return "Automatic recovery needs attention"
        case "cancelled": return "Automatic recovery cancelled"
        case "succeeded": return "Recovered"
        default:
            let text = state.replacingOccurrences(of: "_", with: " ")
            return text.prefix(1).uppercased() + text.dropFirst()
        }
    }
}

// MARK: - Phone delivery setting

extension MobileDelivery {
    public static let unconfirmed = "The change was not confirmed. Check its original receipt in Drafts; it has not been repeated."
    static let confirmationMs: Int64 = 60_000

    public static func arguments(_ value: String) throws -> JSONObject {
        guard values.contains(value) else { throw ZerusError.invalid("Choose a supported notification timing.") }
        return ["value": .string(value)]
    }

    public static func requested(_ action: SessionAction) -> String { action.arguments.string("value") }

    /// Only gateway connectors advertise the operation; an older one still shows its value read-only.
    public static func gateway(_ machines: [Machine], supported: (Machine) -> Bool) -> Machine? {
        let candidates = machines.filter(supported).sorted { a, b in
            let left = (!a.online || a.lastKnown, a.route != .direct, a.id), right = (!b.online || b.lastKnown, b.route != .direct, b.id)
            if left.0 != right.0 { return !left.0 }
            if left.1 != right.1 { return !left.1 }
            return left.2 < right.2
        }
        if let first = candidates.first { return first }
        return machines.filter { $0.route == .direct && !$0.mobileDelivery.isEmpty }.min { $0.id < $1.id }
    }

    /// Without a connection the last known machines carry no operations; that is not an older gateway.
    public static func reason(_ gateway: Machine?, gatewaySupports: Bool, relaySupports: Bool, pending: Bool) -> String {
        guard let gateway else { return "No gateway computer reports this setting yet." }
        if !gateway.online || gateway.lastKnown { return "\(gateway.name) is offline. Try again when it is connected." }
        if !gatewaySupports { return "Update Zerus on \(gateway.name) to change this from the phone." }
        if !relaySupports { return "Update the relay to change this from the phone." }
        if pending { return "Check the previous change before choosing again." }
        return ""
    }

    /// Called after the generic identity and failure checks of `SessionActionPolicies.result`.
    public static func result(_ action: SessionAction, _ receipt: JSONObject) -> SessionAction {
        var copy = action
        guard receipt.string("state") == "completed", let native = receipt.object("result"), native["ok"] == .bool(true),
              native["mobile_delivery"] == .string(requested(action)) else {
            copy.status = "uncertain"
            copy.error = unconfirmed
            return copy
        }
        copy.status = "completed"
        copy.error = ""
        return copy
    }

    /// The requested value only while it is being saved or was just confirmed; an unconfirmed one is not selected.
    public static func shown(reported: String, action: SessionAction?, now: Int64) -> String {
        guard let action else { return reported }
        let requested = requested(action)
        if requested.isEmpty { return reported }
        if action.status == "sending" { return requested }
        if action.status == "completed" && (0..<confirmationMs).contains(now - action.createdAt) { return requested }
        return reported
    }

    /// An inline error about one request ends when that request is reviewed or confirmed.
    public static func error(_ message: String, requestId: String, actions: [SessionAction]) -> String {
        requestId.isEmpty || !actions.contains(where: { $0.requestId == requestId && ["reviewed", "completed"].contains($0.status) }) ? message : ""
    }

    public static func status(gateway: String, reported: String, action: SessionAction?, reason: String, now: Int64) -> String {
        let recent = action.map { (0..<confirmationMs).contains(now - $0.createdAt) } ?? false
        if action?.status == "sending" { return "Saving on \(gateway)…" }
        if action?.status == "uncertain" { return unconfirmed }
        if action?.status == "failed" && recent { return action!.error.isEmpty ? "Not changed. Try again." : action!.error }
        if !reason.isEmpty { return reason }
        if reported.isEmpty { return "Not reported by \(gateway) yet." }
        if let action, action.status == "completed", recent, requested(action) != reported {
            return "Saved on \(gateway). The computers report it with their next update."
        }
        return "Shared by your computers through \(gateway)."
    }
}

// MARK: - Session actions menu

/// One long-press action. A non-empty `reason` disables it and is shown as its supporting line.
public struct SessionMenuItem: Hashable, Sendable, Identifiable {
    public let id: String
    public let label: String
    public var reason = ""
    public var hint = ""
    public init(_ id: String, _ label: String, reason: String = "", hint: String = "") {
        self.id = id
        self.label = label
        self.reason = reason
        self.hint = hint
    }
    public var enabled: Bool { reason.isEmpty }
}

/// Phone-local read facts for one row; `unavailable` is non-empty while read state cannot change.
public struct SessionMenuRead: Hashable, Sendable {
    public var unavailable = ""
    public var canMark = false
    public var partial = false
    public var replies = false
    public var reminder = false
    public var checking = false
    public init(unavailable: String = "", canMark: Bool = false, partial: Bool = false, replies: Bool = false, reminder: Bool = false, checking: Bool = false) {
        self.unavailable = unavailable
        self.canMark = canMark
        self.partial = partial
        self.replies = replies
        self.reminder = reminder
        self.checking = checking
    }
}

/// Exact row facts captured by the model. `supported` is the gateway and computer operation intersection.
public struct SessionMenuFacts: Hashable, Sendable {
    public var target: Target
    public var row: JSONObject
    public var state: String
    public var archived: Bool
    public var online: Bool
    public var supported: Set<String>
    public var blocked = ""
    public var busy = ""
    public var needsAttention = false
    public var read = SessionMenuRead()
    public var reference: String?
    public var folder = ""
    public var referenceReason = ""

    public init(target: Target, row: JSONObject, state: String, archived: Bool, online: Bool, supported: Set<String>, blocked: String = "", busy: String = "",
                needsAttention: Bool = false, read: SessionMenuRead = SessionMenuRead(), reference: String? = nil, folder: String = "", referenceReason: String = "") {
        self.target = target
        self.row = row
        self.state = state
        self.archived = archived
        self.online = online
        self.supported = supported
        self.blocked = blocked
        self.busy = busy
        self.needsAttention = needsAttention
        self.read = read
        self.reference = reference
        self.folder = folder
        self.referenceReason = referenceReason
    }
}

/// The Sessions long-press menu, mirroring the desktop session context menu. Native items are presentation
/// gates from the catalog row only; the opened conversation confirms them after a fresh inspection.
public enum SessionActionsMenu {
    public static func items(_ facts: SessionMenuFacts) -> [[SessionMenuItem]] {
        let archived = facts.archived || !facts.target.archiveId.isEmpty
        let running = !archived && facts.state == "running"
        func native(_ operation: String, _ label: String) -> SessionMenuItem { SessionMenuItem(operation, label, reason: operationReason(facts, operation)) }
        var lifecycle = [archived ? native("restore", "Restore session…") : running ? native("pause", "Pause session…") : native("resume", "Resume session…"),
                         native("rename", "Rename…"), native("fork", "Fork session…")]
        if !archived && ["paused", "stopped"].contains(facts.state) { lifecycle.append(native("archive", "Move to archive…")) }
        let sections: [[SessionMenuItem]] = [
            archived ? [] : [SessionMenuItem("terminal", "Open terminal", reason: terminalReason(facts))],
            archived || !facts.target.agentId.isEmpty ? [] : [readItem(facts)],
            [SessionMenuItem(SessionNotificationPreferences.operation,SessionNotificationPreferences.label(SessionNotificationPreferences.parse(facts.row,facts.target)),
                reason:notificationReason(facts),hint:"Applies to this conversation on all devices.")],
            // The person's per-child question route switch (zerus-sa9qn.33): live Claude children only.
            SessionQuestionRouting.applies(facts.target,facts.row) ? [SessionMenuItem(SessionQuestionRouting.operation,SessionQuestionRouting.label(facts.row),
                reason:questionRoutingReason(facts),hint:SessionQuestionRouting.hint)] : [],
            lifecycle,
            [SessionMenuItem("copy_name", "Copy session name", reason: facts.target.session.isEmpty ? "The session name is not reported." : ""),
             SessionMenuItem("copy_reference", "Copy session reference", reason: facts.reference == nil
                ? (facts.referenceReason.isEmpty ? "A reference needs the computer's native identity and the session's identity." : facts.referenceReason) : ""),
             SessionMenuItem("copy_folder", "Copy folder path", reason: facts.folder.isEmpty ? "The session folder is not reported." : "")],
            [archived ? native("forget", "Forget archived version…") : running ? native("terminate", "Terminate session…") : native("forget", "Forget saved session…")],
        ]
        return sections.filter { !$0.isEmpty }
    }

    /// Mark as read reuses Read all's evidence rules; otherwise the row offers a phone-local reminder.
    public static func readItem(_ facts: SessionMenuFacts) -> SessionMenuItem {
        let read = facts.read
        guard facts.needsAttention else {
            return SessionMenuItem("review_later", "Review later", reason: read.unavailable, hint: "Keep a reminder on this phone to review this session later.")
        }
        let label = read.canMark && read.partial ? "Mark loaded replies read" : "Mark as read"
        if !read.unavailable.isEmpty { return SessionMenuItem("mark_read", label, reason: read.unavailable) }
        if read.canMark { return SessionMenuItem("mark_read", label, hint: read.partial ? "Only replies loaded on this phone are marked read." : "") }
        if read.reminder {
            return SessionMenuItem("clear_reminder", "Clear review reminder",
                                   hint: read.replies ? "Replies stay unread until this phone has verified history or loaded replies." : "")
        }
        if read.replies {
            return SessionMenuItem("mark_read", label, reason: read.checking
                ? "Waiting for verified history from its machine. Open the session if its replies are not loaded on this phone."
                : "Needs verified history or loaded replies on this phone. Open the session to read them.")
        }
        return SessionMenuItem("mark_read", label, reason: "Questions, approvals and errors clear when you open the session. Mark as read covers replies only.")
    }

    public static func nativeTerminalReason(_ target: Target, _ row: JSONObject?) -> String {
        if let row, row.has("terminal_supported"), row["terminal_supported"] != .bool(true) {
            let reason = row.string("terminal_reason")
            return reason.isEmpty ? "This native adapter does not provide remote Terminal." : reason
        }
        if row?["terminal_supported"] == .bool(true) { return "" }
        if target.session.components(separatedBy: "/").first == "opencode" {
            return "OpenCode runs through its native server. Use the conversation instead of Terminal."
        }
        return ""
    }
    public static func questionRoutingReason(_ facts: SessionMenuFacts) -> String {
        if !facts.blocked.isEmpty { return facts.blocked }
        if !SessionQuestionRouting.applies(facts.target,facts.row) || facts.archived { return SessionQuestionRouting.notChild }
        if !facts.online { return "The machine is offline." }
        if !facts.supported.contains(SessionQuestionRouting.operation) { return SessionQuestionRouting.unavailable }
        if !facts.busy.isEmpty { return facts.busy }
        // A person's latch is cleared only by the person's latch statement (zerus-sa9qn.50).
        if SessionQuestionRouting.parse(facts.row)?.personOff == true && !facts.supported.contains(SessionQuestionRouting.personOperation) {
            return SessionQuestionRouting.personLatched
        }
        return SessionQuestionRouting.reason(facts.target,facts.row)
    }
    public static func notificationReason(_ facts: SessionMenuFacts) -> String {
        if !facts.blocked.isEmpty { return facts.blocked }
        if !facts.target.agentId.isEmpty || facts.archived || !facts.target.archiveId.isEmpty { return "Notification controls belong to the live parent conversation." }
        if !facts.online { return "The machine is offline." }
        if !facts.supported.contains(SessionNotificationPreferences.operation) { return SessionNotificationPreferences.unavailable }
        if !facts.busy.isEmpty { return facts.busy }
        return SessionNotificationPreferences.reason(facts.target,facts.row)
    }

    public static func terminalReason(_ facts: SessionMenuFacts) -> String {
        if !facts.blocked.isEmpty { return facts.blocked }
        if !facts.target.agentId.isEmpty || facts.archived || !facts.target.archiveId.isEmpty { return "Terminal is available only for the live parent session." }
        if !facts.online { return "The machine is offline." }
        if !facts.supported.contains("terminal_snapshot") { return "This native adapter does not provide remote Terminal." }
        return nativeTerminalReason(facts.target, facts.row)
    }

    public static func operationReason(_ facts: SessionMenuFacts, _ operation: String) -> String {
        let target = facts.target
        let archived = facts.archived || !target.archiveId.isEmpty
        if !SessionActionPolicies.lifecycle.contains(operation) { return "Unsupported session action." }
        if !facts.blocked.isEmpty { return facts.blocked }
        if !target.agentId.isEmpty { return "Session controls belong to the parent conversation." }
        if !facts.online { return "The machine is offline." }
        if !facts.supported.contains(operation) { return "This gateway or computer does not support this action." }
        if archived && !["restore", "forget", "rename", "fork"].contains(operation) { return "This archived conversation is read-only." }
        if !archived && operation == "restore" { return "Select an archived version first." }
        if !facts.busy.isEmpty { return facts.busy }
        return SessionControlPresentation.reason(target, operation, facts.row)
    }
}

/// Retries reads only after the original mutation receipt is already confirmed.
public enum ConfirmedTargetResolution {
    public static func read<T>(isolation: isolated (any Actor)? = #isolation, attempts: Int = 4, active: () -> Bool, pause: () async -> Void,
                               inspect: () async throws -> T) async throws -> T {
        precondition((1...8).contains(attempts))
        var last: Error?
        for index in 0..<attempts {
            guard active() else { throw CancellationError() }
            do { return try await inspect() }
            catch is CancellationError { throw CancellationError() }
            catch { last = error }
            if index + 1 < attempts { await pause() }
        }
        throw last ?? ZerusError(.unavailable, "The confirmed destination is not reachable yet.")
    }
}
