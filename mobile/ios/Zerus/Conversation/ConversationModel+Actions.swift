import Foundation
import ZerusKit

/// Native session actions, context commands, scheduled settings and process output.
/// Every action is durable before transport, re-verified against fresh evidence, and settled
/// only from its original receipt.
extension ConversationModel {
    /// Saved actions, with interrupted sends shown as unconfirmed.
    var sessionActions: [SessionAction] { messageState.sessionActions.map { actionFlights.contains($0.requestId) ? $0 : $0.recover() } }
    var contextOperations: [ContextOperation] { messageState.contextOperations.map { contextFlights.contains($0.requestId) ? $0 : $0.recover() } }
    var compactContinuationActive: Bool { continuation != nil && continuation?.draft.target == selected?.target }

    func contextOperation(_ target: Target) -> ContextOperation? {
        contextOperations.last { ContextPolicies.sameSessionRun($0.target, target) && $0.blocksSending } ??
            contextOperations.last { ContextPolicies.sameSessionRun($0.target, target) }
    }

    func cancelCompactContinuation(_ reason: String = "") {
        if continuation != nil && !reason.isEmpty { contextNotice = reason }
        continuation = nil
        continuationEnqueuing = ""
    }

    func setContextNotice(_ requestId: String, _ text: String) {
        if contextNoticeOwner == requestId { contextNotice = text }
    }

    // MARK: Actions

    func actionReason(_ target: Target, _ operation: String) -> String {
        if operation == "terminal_input" { return terminalReason(target) }
        if terminalVisible { return "Return to Activity before using session controls." }
        if !target.agentId.isEmpty { return "Session controls belong to the parent conversation." }
        guard app.storageReady, !demo, selected?.target == target, activityVerified else { return "Refresh this exact session before taking an action." }
        guard let raw = activity else { return "Refresh this session." }
        if raw.string("name") != target.session || raw.string("run_id") != target.run || raw.string("conversation_id") != target.conversation ||
            raw.string("archive_id") != target.archiveId { return "The session identity changed. Refresh before taking an action." }
        if !SessionActionPolicies.operations.contains(operation) || app.relayOperations[target.connectionId]?.contains(operation) != true ||
            app.machineOperations[target.machineKey]?.contains(operation) != true { return "This gateway or computer does not support this action." }
        if !target.archiveId.isEmpty && !["restore", "forget", "rename", "fork"].contains(operation) { return "This archived conversation is read-only." }
        if target.archiveId.isEmpty && operation == "restore" { return "Select an archived version first." }
        if operation == SessionNotificationPreferences.operation {
            let reason = SessionNotificationPreferences.reason(target,raw)
            if !reason.isEmpty { return reason }
        }
        if operation == SessionQuestionRouting.operation {
            let reason = SessionQuestionRouting.reason(target,raw)
            if !reason.isEmpty { return reason }
        }
        if SessionActionPolicies.lifecycle.contains(operation) && !raw.strings("allowed_actions").contains(operation) {
            let reason = raw.object("action_reasons")?.string(operation) ?? ""
            return reason.isEmpty ? "Native session controls are unavailable for this action." : reason
        }
        if contextBlocked(target) || normalizedOutgoing.contains(where: { $0.belongs(to: target) && $0.blocksSending }) || preparingTargets.contains(target.key) ||
            compactContinuationActive { return "Resolve the pending delivery or action first." }
        if messageState.pickerDraft?.target == target || drafts.contains(where: { $0.target == target && ["submitting", "uncertain"].contains($0.status) }) {
            return "Wait for the current answer or file selection."
        }
        return ""
    }

    func sendNow(_ target: Target, queueId: String) { sessionAction(target, "send_now", ["queue_id": .string(queueId)]) }

    func updateSettings(_ target: Target, model: String? = nil, effort: String? = nil, expectedPendingId: String = "") {
        var args: JSONObject = [:]
        if let model { args["model"] = .string(model) }
        if let effort { args["effort"] = .string(effort) }
        if !expectedPendingId.isEmpty { args["expected_pending_id"] = .string(expectedPendingId) }
        sessionAction(target, "settings", args)
    }

    func processStop(_ target: Target, processId: String, generation: String?) {
        var args: JSONObject = ["process_id": .string(processId)]
        if let generation { args["generation"] = .string(generation) }
        sessionAction(target, "process_stop", args)
    }

    func sessionAction(_ target: Target, _ operation: String, _ args: JSONObject = [:]) {
        submitSessionAction(target, operation, args)
    }

    /// Presentation for one Sessions row's long-press actions; the opened conversation re-verifies before any request.
    func sessionMenu(_ session: Session) -> [[SessionMenuItem]] {
        let target = session.target
        let online = app.online(session)
        let blocked = demo ? "Unavailable in preview." : (!app.storageReady ? "Private storage is still loading." : "")
        let busy = contextBlocked(target) || normalizedOutgoing.contains { $0.belongs(to: target) && $0.blocksSending } || preparingTargets.contains(target.key)
        let read = SessionMenuRead(unavailable: demo ? "Unavailable in preview." : (!readTrackingReady || readWriter == nil ? "Read state is still loading on this phone." : ""),
                                   canMark: canReadAll(target), partial: readAllPartial(target),
                                   replies: ReadAllPolicies.unreadReplies(readState.record(target), loadedRows(target) ?? []) ||
                                       session.raw.bool("unread_reply") || session.raw.bool("mobile_unread_reply"),
                                   reminder: reviewLater(target), checking: online && historyNetworkReason(target).isEmpty)
        let reference = SessionReference.resolve(target, app.machines)
        let supported = (app.relayOperations[target.connectionId] ?? []).intersection(app.machineOperations[target.machineKey] ?? [])
        return SessionActionsMenu.items(SessionMenuFacts(target: target, row: session.raw, state: SessionFilters.state(session), archived: SessionFilters.archived(session),
                                                         online: online, supported: supported, blocked: blocked,
                                                         busy: busy ? "Resolve the pending delivery or action first." : "",
                                                         needsAttention: SessionFilters.needsAttention(session, online), read: read, reference: reference.uri,
                                                         folder: session.raw.string("cwd"), referenceReason: reference.reason))
    }

    /// A Sessions action opens its exact session; the conversation then confirms it against fresh native details.
    func openWithSessionAction(_ session: Session, _ operation: String) {
        guard operation == "terminal" || operation == SessionNotificationPreferences.operation || operation == SessionQuestionRouting.operation
            || SessionActionPolicies.lifecycle.contains(operation) else { return }
        open(session, preserveProject: true)
        sessionMenuRequest = (session.target, operation, navigationId)
    }

    /// Hands the request over once, only to the session and navigation that it opened.
    func takeSessionMenuRequest(_ target: Target) -> String? {
        guard let request = sessionMenuRequest else { return nil }
        if request.navigation == navigationId && request.target != target { return nil }
        sessionMenuRequest = nil
        return request.navigation == navigationId && request.target == target ? request.operation : nil
    }

    /// OpenCode effort variants beyond the relay's fixed levels need the feature on the relay and this computer.
    func effortVariants(_ target: Target, _ raw: JSONObject) -> Bool {
        SessionActionPolicies.effortVariants(agent: raw.string("agent"), relay: app.relayFeatures[target.connectionId], computer: app.machineFeatures[target.machineKey])
    }

    func submitSessionAction(_ target: Target, _ operation: String, _ args: JSONObject, authorized: SessionAction? = nil, frame: ConversationFrame? = nil,
                             terminalBuffer: TerminalBuffer? = nil) {
        let inspected = frame?.raw ?? activity ?? [:]
        let reason = authorized.map { authorizedSettingsReason(target, args, $0, frame) } ?? actionReason(target, operation)
        guard reason.isEmpty else {
            app.error = reason
            return
        }
        let arguments: JSONObject
        do { arguments = try SessionActionPolicies.arguments(operation, args, effortVariants: effortVariants(target, inspected)) } catch {
            app.error = AppModel.message(error, fallback: "Invalid action.")
            return
        }
        let eligibility = SessionActionPolicies.inspectReason(target, operation, arguments, inspected)
        guard eligibility.isEmpty else {
            app.error = eligibility
            return
        }
        guard let connection = app.connections.first(where: { $0.id == target.connectionId }) else { return }
        let action = SessionAction(requestId: UUID().uuidString.lowercased(), target: target, operation: operation, arguments: arguments)
        let evidence = SessionActionPolicies.evidence(operation, inspected, arguments)
        let navigation = navigationId
        preparingTargets.insert(target.key)
        actionFlights.insert(action.requestId)
        Task {
            var attempted = false, rejected = false
            defer {
                preparingTargets.remove(target.key)
                actionFlights.remove(action.requestId)
            }
            do {
                _ = try await app.durableMessages({ state in
                    if let terminalBuffer { return try TerminalBuffers.enqueue(state, terminalBuffer, action) }
                    if let authorized { return try ScheduledSettingsAttempt.begin(state, authorized: authorized, attempt: action) }
                    return state.action(action)
                }, acknowledge: { current, written in
                    var next = terminalBuffer.map { TerminalBuffers.acknowledge(current, written, $0, action) }
                        ?? written.sessionActions.first { $0.requestId == action.requestId }.map(current.action) ?? current
                    if let authorized, let source = written.sessionActions.first(where: { $0.requestId == authorized.requestId }) { next = next.action(source) }
                    return next
                })
                preparingTargets.remove(target.key)
                let live = authorized == nil ? activity ?? [:] : inspected
                if authorized == nil {
                    guard selected?.target == target, navigationId == navigation, operation == "terminal_input" || activityVerified else {
                        throw ZerusError.invalid("The session changed before the action was sent.")
                    }
                } else {
                    guard let frame, frame.target == target, (0...10_000).contains(Clock.milliseconds() - frame.observedAt),
                          app.connections.contains(where: { $0.id == target.connectionId }),
                          messageState.sessionActions.contains(where: { $0.requestId == authorized!.requestId && $0.applyAttemptId == action.requestId }) else {
                        throw ZerusError.invalid("The authorized native settings changed before delivery.")
                    }
                }
                if operation == "terminal_input" {
                    guard terminalCurrent(target, binding: arguments.string("terminal_binding_id")) else {
                        throw ZerusError.invalid("Terminal binding expired before delivery. Input was not sent.")
                    }
                } else {
                    guard SessionActionPolicies.evidence(operation, live, arguments) == evidence else {
                        throw ZerusError.invalid("The displayed native action details changed before delivery.")
                    }
                }
                guard SessionActionPolicies.inspectReason(target, operation, arguments, live).isEmpty else {
                    throw ZerusError.invalid("The native action target or catalog changed before delivery.")
                }
                var payload = arguments
                if operation == SessionNotificationPreferences.operation { payload = SessionNotificationPreferences.payload(target,arguments) }
                else if operation == SessionQuestionRouting.operation { payload = SessionQuestionRouting.payload(target,arguments) }
                else {
                    payload["request_id"] = .string(action.requestId)
                    payload["expected_run_id"] = .string(target.run)
                    payload["expected_conversation_id"] = .string(target.conversation)
                }
                if !target.archiveId.isEmpty { payload["archive_id"] = .string(target.archiveId) }
                attempted = true
                let initial: JSONObject
                do { initial = try await app.client.submit(connection, target: target, operation: operation, payload: payload, id: action.requestId) }
                catch let failure as RelayError {
                    rejected = Self.rejectedStatuses.contains(failure.status)
                    throw failure
                }
                if operation == "terminal_input" {
                    await settleSessionAction(action, try await app.client.waitForReceipt(connection, initial: initial, maxPolls: 50, pollDelay: .milliseconds(100)))
                    if selected?.target == target && terminalVisible { terminalSnapshot(target) }
                    return
                }
                await settleSessionAction(action, try await app.client.waitForReceipt(connection, initial: initial))
                guard let completed = messageState.sessionActions.first(where: { $0.requestId == action.requestId }) else { return }
                switch SessionActionFollowUp.kind(completed, selected: selected?.target, sameNavigation: navigationId == navigation) {
                case .sessions:
                    close()
                    returnToSessions += 1
                    app.refresh()
                case .openResult:
                    activityVerified = false
                    activeFrameVerified = false
                    historyCached = activity != nil
                    await resolveActionResult(completed, navigation: navigation, original: target)
                case .refresh:
                    refreshActivity()
                case .none:
                    break
                }
            } catch {
                let status = !attempted || rejected ? "failed" : "uncertain"
                let message = status == "failed" ? AppModel.message(error, fallback: "The action was not sent.")
                    : "Action delivery is unconfirmed. Check its original receipt; it has not been retried."
                do {
                    _ = try await app.durableMessages { state in
                        guard var current = state.sessionActions.first(where: { $0.requestId == action.requestId }),
                              ["sending", "uncertain"].contains(current.status) else { return state }
                        current.status = status
                        current.error = message
                        return state.action(current)
                    }
                } catch {
                    app.error = "Could not save the action result. Its original request remains recoverable."
                }
            }
        }
    }

    func settleSessionAction(_ action: SessionAction, _ receipt: JSONObject) async {
        let result = SessionActionPolicies.result(action, receipt)
        do {
            _ = try await app.durableMessages({ state in
                guard let current = state.sessionActions.first(where: { $0.requestId == action.requestId }), ["sending", "uncertain"].contains(current.status) else { return state }
                var next = state.action(result)
                if result.operation == "settings" && result.status == "applied", var source = state.sessionActions.first(where: { $0.applyAttemptId == action.requestId }) {
                    source.status = "applied"
                    next = next.action(source)
                }
                return next
            }, acknowledge: { current, written in
                var next = written.sessionActions.first { $0.requestId == action.requestId }.map(current.action) ?? current
                if let source = written.sessionActions.first(where: { $0.applyAttemptId == action.requestId }) { next = next.action(source) }
                return next
            })
        } catch {
            app.error = "Could not save the action result. Its original request remains recoverable."
            if action.operation == SessionNotificationPreferences.operation { return }
        }
        if ["archive", "terminate", "restore", "forget", "rename"].contains(action.operation) { app.archive.refresh() }
        // The computer's listing and inspection show the confirmed switch.
        if action.operation == SessionQuestionRouting.operation { app.refresh(explicit:true) }
        if action.operation == SessionNotificationPreferences.operation {
            if result.status == "completed" { await app.confirmNotificationPreference(result) }
            app.refresh(explicit:true)
            await app.notifications.syncAll(forceCatalog:true)
        }
    }

    func pendingActionResult(_ target: Target) -> SessionAction? {
        messageState.sessionActions.last { $0.target == target && SessionActionFollowUp.openable($0) }
    }

    func openSessionActionResult(_ action: SessionAction) {
        guard let stored = messageState.sessionActions.first(where: { $0.requestId == action.requestId && $0.status == "completed" && $0.resultTarget != nil }),
              actionResultTask == nil else { return }
        let original = selected?.target, navigation = navigationId
        actionResultTask = Task {
            await resolveActionResult(stored, navigation: navigation, original: original)
            actionResultTask = nil
        }
    }

    func confirmedDraftAction(_ draft: Draft) -> SessionAction? {
        messageState.sessionActions.last { action in
            action.resultTarget.map { ActionDraftPolicies.proof(action, draft.target, $0) } == true && draft.status == "editing" &&
                draft.questionId.isEmpty && draft.detachedId.isEmpty
        }
    }

    func restoreDraftToConfirmedResult(_ draft: Draft, _ action: SessionAction) {
        guard let stored = confirmedDraftAction(draft), stored.requestId == action.requestId,
              let source = messageState.drafts.first(where: { $0.key == draft.key && $0.generation == draft.generation && $0.revision == draft.revision &&
                  $0.attachments == draft.attachments }), let destination = stored.resultTarget else { return }
        guard ActionDraftPolicies.destinationReady(messageState, destination) else {
            actionResultNotice = "The confirmed session already has a composition. Your original draft remains in Saved drafts."
            return
        }
        guard actionResultTask == nil else { return }
        let navigation = navigationId, original = selected?.target
        actionResultTask = Task {
            await resolveActionResult(stored, navigation: navigation, original: original, restore: source)
            actionResultTask = nil
        }
    }

    /// Opens a confirmed action's destination; reads may retry, the action never does.
    func resolveActionResult(_ action: SessionAction, navigation: Int64, original: Target?, restore: Draft? = nil) async {
        guard let destination = action.resultTarget, let connection = app.connections.first(where: { $0.id == destination.connectionId }),
              resolvingActionResults.insert(action.requestId).inserted else { return }
        defer { resolvingActionResults.remove(action.requestId) }
        do {
            let frame = try await ConfirmedTargetResolution.read(active: {
                navigationId == navigation && selected?.target == original && app.connections.contains { $0.id == destination.connectionId }
            }, pause: {
                try? await Task.sleep(for: .milliseconds(750))
                _ = try? await app.client.computers(connection)
            }, inspect: {
                let frame = try await inspectOnce(connection, destination).value
                guard frame.target == destination || destination.resolves(to: frame.target) else {
                    throw ZerusError.mismatch("The confirmed destination has changed. The original draft remains saved.")
                }
                return frame
            })
            guard navigationId == navigation, selected?.target == original else { return }
            let messages = normalizedOutgoing.filter { $0.belongs(to: frame.target) }, state = messageState
            let rows = frame.events, frameQuestions = frame.questions, frameTarget = frame.target
            let prepared = await Task.detached(priority: .userInitiated) {
                ConversationFrame.Presentation(messages: messages, events: ConversationMerge.merge(target: frameTarget, native: rows, outgoing: messages).events,
                                               questions: QuestionTracking.restore(state, frameTarget, frameQuestions))
            }.value
            guard navigationId == navigation, selected?.target == original else { return }
            if (original == action.target || restore != nil) && ActionDraftPolicies.proof(action, action.target, frame.target) {
                app.editMessages { restore == nil ? ActionDraftPolicies.promote($0, action) : ActionDraftPolicies.restore($0, action, restore!) }
                editRead { $0.promote(action) }
            }
            selected = Session(target: frame.target, title: frame.raw.string("title", "label", "name"), agent: frame.raw.string("agent"),
                               status: frame.raw.string("activity", "state"), project: frame.raw.string("project", "cwd"), preview: "", pendingCount: 0, raw: frame.raw)
            frame.presentation = prepared
            installFrame(frame, verified: true)
            actionResultNotice = restore != nil && drafts.contains(where: { $0.key == restore!.key })
                ? "Your original draft remains in Saved drafts because the composition changed or the destination is occupied." : ""
        } catch is CancellationError {
        } catch {
            if navigationId == navigation && selected?.target == original {
                actionResultNotice = "The computer confirmed the action. Its destination is not reachable yet. Open the confirmed result when connected; your original draft remains saved."
            }
        }
    }

    func recoveryActionReason(_ target: Target, jobId: String, choice: String) -> String {
        let reason = actionReason(target, "recovery_action")
        if !reason.isEmpty { return reason }
        guard let args = try? SessionActionPolicies.arguments("recovery_action", ["job_id": .string(jobId), "action": .string(choice)]) else {
            return "Invalid native recovery action."
        }
        return SessionActionPolicies.inspectReason(target, "recovery_action", args, activity ?? [:])
    }

    func recoveryAction(_ target: Target, jobId: String, choice: String) {
        let reason = recoveryActionReason(target, jobId: jobId, choice: choice)
        guard reason.isEmpty else {
            app.error = reason
            return
        }
        sessionAction(target, "recovery_action", ["job_id": .string(jobId), "action": .string(choice)])
    }

    /// Reads the original action receipt; nothing is retried.
    func checkSessionAction(_ action: SessionAction) {
        guard !actionFlights.contains(action.requestId), let connection = app.connections.first(where: { $0.id == action.target.connectionId }) else { return }
        actionFlights.insert(action.requestId)
        Task {
            defer { actionFlights.remove(action.requestId) }
            do {
                await settleSessionAction(action, try await app.client.waitForReceipt(connection, initial: try await app.client.receipt(connection, id: action.requestId)))
            } catch {
                app.error = "The original action receipt could not be checked. Nothing has been retried."
            }
        }
    }

    func reviewSessionAction(_ action: SessionAction) {
        guard !actionFlights.contains(action.requestId) else { return }
        app.editMessages { state in
            guard var current = state.sessionActions.first(where: { $0.requestId == action.requestId && ($0.blocksSending || $0.needsProjectReview) }) else { return state }
            current.status = "reviewed"
            return state.action(current)
        }
        app.flushPrivateState()
    }

    // MARK: Context commands

    func contextActionReason(_ target: Target, _ operation: String) -> String {
        if !target.agentId.isEmpty || !target.archiveId.isEmpty || !fresh(target) { return "Refresh the live conversation before changing its context." }
        if target.run.isEmpty || target.conversation.isEmpty { return "Wait for the conversation to finish starting." }
        if app.relayOperations[target.connectionId]?.contains(operation) != true || app.machineOperations[target.machineKey]?.contains(operation) != true {
            return "This gateway or computer does not support native context commands."
        }
        if compactContinuationActive || contextBlocked(target) || normalizedOutgoing.contains(where: { $0.belongs(to: target) && $0.blocksSending }) ||
            preparingTargets.contains(target.key) { return "Resolve the pending delivery or context command first." }
        if messageState.pickerDraft?.target == target { return "Wait for the selected files to finish importing." }
        if drafts.contains(where: { $0.target == target && ["submitting", "uncertain"].contains($0.status) }) { return "Wait for the pending answer or interrupt." }
        if activity?.bool(operation + "_supported") != true || activity?.string("activity") != "idle" || activity?.string("phase") != "idle" {
            return "Native context changes are unavailable while the agent is busy or waiting for input."
        }
        return ""
    }

    func compactContext(_ target: Target, continueDraft: Bool = false, expectedDraft: Draft? = nil) {
        Task { await startContext(target, "compact_context", continueDraft: continueDraft, expectedDraft: expectedDraft) }
    }

    func clearContext(_ target: Target) {
        Task { await startContext(target, "clear_context", continueDraft: false, expectedDraft: nil) }
    }

    func startContext(_ target: Target, _ operation: String, continueDraft: Bool, expectedDraft: Draft?) async {
        let reason = contextActionReason(target, operation)
        guard !demo, app.storageReady, reason.isEmpty else {
            if !reason.isEmpty { contextNotice = reason }
            return
        }
        let original = draft(target)
        if let expectedDraft, !ContextPolicies.sameDraft(expectedDraft, original) {
            contextNotice = "Your composition changed. Review it before choosing a context action."
            return
        }
        if continueDraft && original.text.isEmpty && original.attachments.isEmpty { return }
        guard let connection = app.connections.first(where: { $0.id == target.connectionId }) else { return }
        let op = ContextOperation(requestId: UUID().uuidString.lowercased(), target: target, operation: operation)
        let navigation = navigationId
        preparingTargets.insert(target.key)
        contextFlights.insert(op.requestId)
        defer { preparingTargets.remove(target.key) }
        if continueDraft { continuation = CompactContinuation(requestId: op.requestId, draft: original, navigation: navigation, startedAt: ProcessInfo.processInfo.systemUptime) }
        contextNoticeOwner = op.requestId
        contextNotice = ""
        contextNoticeOwner = op.requestId
        if operation == "clear_context" { clearExpectation = (op.requestId, target, navigation, false) }
        var attempted = false
        var rejection: RelayError?
        do {
            _ = try await app.durableMessages({ state in
                guard !state.contextOperations.contains(where: { ContextPolicies.sameSessionRun($0.target, target) && $0.blocksSending }) else {
                    throw ZerusError.invalid("A context command is already pending.")
                }
                return state.context(op)
            }, acknowledge: { current, written in
                written.contextOperations.first { $0.requestId == op.requestId }.map(current.context) ?? current
            })
            guard fresh(target), navigationId == navigation, !continueDraft || continuation?.requestId == op.requestId,
                  operation != "clear_context" || clearExpectation?.requestId == op.requestId else {
                throw ZerusError.invalid("The selected conversation changed before transport.")
            }
            var payload = try target.json()
            payload["request_id"] = .string(op.requestId)
            attempted = true
            let initial: JSONObject
            do { initial = try await app.client.submit(connection, target: target, operation: operation, payload: payload, id: op.requestId) }
            catch let failure as RelayError {
                if Self.rejectedStatuses.contains(failure.status) { rejection = failure }
                throw failure
            }
            let result = ContextPolicies.result(op, try await app.client.waitForReceipt(connection, initial: initial))
            _ = try await app.durableMessages { state in
                Self.updateContext(state, op.requestId) { ["completed", "failed", "cancelled", "unchanged", "reviewed"].contains($0.status) ? $0 : result }
            }
            if result.status == "submitted" { acknowledgedContexts.insert(op.requestId) }
            if !["submitted", "completed"].contains(result.status) {
                contextFlights.remove(op.requestId)
                cancelCompactContinuation("Context delivery was not confirmed. Your message was not sent.")
            }
            if operation == "clear_context", clearExpectation?.requestId == op.requestId {
                if result.status == "completed" { clearExpectation?.confirmed = true } else if result.status != "submitted" { clearExpectation = nil }
            }
            if result.status == "completed" { contextFlights.remove(op.requestId) }
            setContextNotice(op.requestId, result.status == "submitted" ? "Native command submitted. Waiting for the agent to confirm the result." : result.error)
            if let selected = selected?.target, ContextPolicies.sameSessionRun(selected, target) { refreshActivity() }
            if let frame = activeFrame, frame.target == target, activityVerified { observeContext(frame) }
        } catch {
            let status = attempted && rejection == nil ? "uncertain" : "failed"
            do {
                _ = try await app.durableMessages { state in
                    Self.updateContext(state, op.requestId) { current in
                        guard !["completed", "reviewed", "cancelled", "unchanged"].contains(current.status) else { return current }
                        var updated = current
                        updated.status = status
                        updated.error = status == "uncertain" ? "Delivery is unconfirmed. Check the original context receipt before trying again."
                            : "The context command was not sent or was rejected. Your draft is preserved."
                        return updated
                    }
                }
            } catch {
                contextNotice = "Could not save the command outcome. Its original identity remains recoverable."
            }
            cancelCompactContinuation("Compaction was not confirmed. Your message was not sent.")
            clearExpectation = nil
            contextFlights.remove(op.requestId)
        }
    }

    nonisolated static func updateContext(_ state: MessageState, _ requestId: String, _ transform: (ContextOperation) -> ContextOperation) -> MessageState {
        guard let current = state.contextOperations.first(where: { $0.requestId == requestId }) else { return state }
        return state.context(transform(current))
    }

    /// Follows native compaction; a completed, unchanged continuation sends the captured message once.
    func observeContext(_ frame: ConversationFrame) {
        if let intent = continuation, !intent.allowed(target: selected?.target, navigation: navigationId, now: ProcessInfo.processInfo.systemUptime) {
            cancelCompactContinuation("Continuation was canceled. Your draft has not been sent.")
        }
        for op in messageState.contextOperations where op.target == frame.target && op.operation == "compact_context" && op.status != "reviewed" {
            guard let status = ContextPolicies.compactStatus(op, frame.raw) else { continue }
            if status != "completed" && continuation?.requestId == op.requestId { cancelCompactContinuation("Compaction did not complete. Your draft was kept without sending.") }
            guard contextObservations.insert(op.requestId).inserted else { continue }
            Task {
                defer { contextObservations.remove(op.requestId) }
                do {
                    if op.status != status {
                        _ = try await app.durableMessages { state in
                            Self.updateContext(state, op.requestId) { current in
                                guard current.status != "reviewed" else { return current }
                                var updated = current
                                updated.status = status
                                updated.error = status == "completed" ? "" : "Native compaction is \(status). Your draft was not sent."
                                return updated
                            }
                        }
                    }
                    contextFlights.remove(op.requestId)
                    setContextNotice(op.requestId, status == "completed" ? "" : "Native compaction is \(status). Your draft was not sent.")
                    guard let current = continuation, current.requestId == op.requestId else { return }
                    if status == "completed" && acknowledgedContexts.contains(op.requestId) && continuationEnqueuing.isEmpty && fresh(current.draft.target) &&
                        current.allowed(target: selected?.target, navigation: navigationId, now: ProcessInfo.processInfo.systemUptime) &&
                        current.unchanged(draft(current.draft.target)) && !sendingBlocked(current.draft.target) {
                        continuationEnqueuing = current.requestId
                        await sendMessage(current.draft.target, expectedDraft: current.draft, expectedCompactionId: current.requestId, allowColdCache: true,
                                          continuationId: current.requestId)
                    }
                } catch {
                    cancelCompactContinuation("Could not save compaction confirmation. Your message was not sent.")
                }
            }
        }
    }

    /// Reads the original context receipt and the exact session; nothing is retried.
    func checkContextOperation(_ operation: ContextOperation) {
        guard let connection = app.connections.first(where: { $0.id == operation.target.connectionId }) else { return }
        let flight = "context:\(operation.requestId)"
        guard receiptFlights.insert(flight).inserted else { return }
        Task {
            defer { receiptFlights.remove(flight) }
            do {
                let result = ContextPolicies.result(operation, try await app.client.waitForReceipt(connection, initial: try await app.client.receipt(connection, id: operation.requestId)))
                _ = try await app.durableMessages { state in Self.updateContext(state, operation.requestId) { ["completed", "reviewed"].contains($0.status) ? $0 : result } }
                let frame = try await inspectOnce(connection, operation.target).value
                if let status = ContextPolicies.compactStatus(operation, frame.raw) {
                    _ = try await app.durableMessages { state in
                        Self.updateContext(state, operation.requestId) { current in
                            guard current.status != "reviewed" else { return current }
                            var updated = current
                            updated.status = status
                            updated.error = ""
                            return updated
                        }
                    }
                }
                contextNotice = "Checked the original command. No command or message was retried."
                if let selected = selected?.target, ContextPolicies.sameSessionRun(selected, operation.target) { refreshActivity() }
            } catch is CancellationError {
            } catch {
                contextNotice = "The original receipt is unavailable. Its saved identity has been preserved."
            }
        }
    }

    func reviewContextOperation(_ operation: ContextOperation) {
        guard !receiptFlights.contains("context:\(operation.requestId)") else { return }
        if continuation?.requestId == operation.requestId { cancelCompactContinuation() }
        if clearExpectation?.requestId == operation.requestId { clearExpectation = nil }
        app.editMessages { state in
            Self.updateContext(state, operation.requestId) { current in
                guard current.status != "completed" else { return current }
                var updated = current
                updated.status = "reviewed"
                return updated
            }
        }
        app.flushPrivateState()
    }

    func contextDrafts(_ target: Target) -> [Draft] {
        drafts.filter { $0.target != target && ContextPolicies.sameSession($0.target, target) && $0.questionId.isEmpty && $0.detachedId.isEmpty &&
            $0.status == "editing" && (!$0.text.isEmpty || !$0.attachments.isEmpty) }
    }

    func canRestoreContextDraft(_ draft: Draft, _ target: Target) -> Bool {
        fresh(target) && ContextPolicies.sameSession(draft.target, target) && draft.target != target && draft.questionId.isEmpty && draft.detachedId.isEmpty &&
            draft.status == "editing"
    }

    func restoreContextDraft(_ draft: Draft, _ target: Target) {
        guard canRestoreContextDraft(draft, target), !preparingTargets.contains(target.key) else { return }
        cancelCompactContinuation()
        var restored = draft
        restored.target = target
        restored.generation = UUID().uuidString.lowercased()
        restored.revision = UUID().uuidString.lowercased()
        let candidate = restored, parkedId = UUID().uuidString.lowercased()
        app.editMessages { state in
            guard let original = state.drafts.first(where: { $0.key == draft.key && $0.revision == draft.revision && $0.generation == draft.generation }) else { return state }
            let editor = state.drafts.first { $0.target == target && $0.questionId.isEmpty && $0.detachedId.isEmpty }
            if let editor, editor.status != "editing" { return state }
            var parked: Draft?
            if let editor, !editor.text.isEmpty || !editor.attachments.isEmpty {
                var copy = editor
                copy.detachedId = parkedId
                parked = copy
            }
            var copy = state
            copy.drafts = state.drafts.filter { $0.key != original.key && $0.key != candidate.key } + (parked.map { [$0] } ?? []) + [candidate]
            return copy
        }
        app.flushPrivateState()
    }

    // MARK: Scheduled settings

    func authorizedSettingsReason(_ target: Target, _ args: JSONObject, _ authorized: SessionAction, _ frame: ConversationFrame?) -> String {
        guard let frame else { return "No fresh settings inspection." }
        let raw = frame.raw
        if frame.target != target || !target.agentId.isEmpty || !target.archiveId.isEmpty || authorized.operation != "settings" || authorized.status != "scheduled" ||
            !authorized.applyAttemptId.isEmpty || authorized.target.connectionId != target.connectionId || authorized.target.computerId != target.computerId ||
            authorized.target.session != target.session || authorized.target.conversation != target.conversation || raw.string("name") != target.session ||
            raw.string("run_id") != target.run || raw.string("conversation_id") != target.conversation || raw.string("pending_settings_id") != authorized.pendingId ||
            args.string("expected_pending_id") != authorized.pendingId || raw.string("settings_apply_when") != "now" || raw.string("phase") != "idle" ||
            raw.string("activity") != "idle" { return "The authorized settings identity changed." }
        if app.relayOperations[target.connectionId]?.contains("settings") != true || app.machineOperations[target.machineKey]?.contains("settings") != true ||
            contextBlocked(target) || preparingTargets.contains(target.key) || normalizedOutgoing.contains(where: { $0.belongs(to: target) && $0.blocksSending }) {
            return "Resolve pending delivery first."
        }
        // A scheduled OpenCode variant is refused here once the relay or computer no longer accepts it.
        do { _ = try SessionActionPolicies.arguments("settings", args, effortVariants: effortVariants(target, raw)) }
        catch { return AppModel.message(error, fallback: "Invalid settings.") }
        return SessionActionPolicies.inspectReason(target, "settings", args, raw)
    }

    /// Applies a confirmed "on next idle" settings intent exactly once, after a fresh inspection proves the same pending identity.
    func watchScheduledSettings() async {
        let authorized = messageState.sessionActions.filter {
            $0.operation == "settings" && $0.status == "scheduled" && $0.applyAttemptId.isEmpty && !$0.pendingId.isEmpty && $0.arguments.string("expected_pending_id").isEmpty
        }
        guard !authorized.isEmpty else { return }
        var candidates: [(Connection, SessionAction, Target)] = []
        for connection in app.connections where authorized.contains(where: { $0.target.connectionId == connection.id }) {
            guard let computers = try? await app.client.computers(connection).objects("computers") else { continue }
            for intent in authorized where intent.target.connectionId == connection.id {
                guard let computer = computers.first(where: { $0.string("id") == intent.target.computerId && $0.bool("online") }),
                      let row = computer.object("snapshot")?.objects("sessions").first(where: {
                          $0.string("name") == intent.target.session && $0.string("conversation_id") == intent.target.conversation && $0.string("archive_id").isEmpty
                      }), row.string("activity") == "idle", row.string("phase") == "idle" else { continue }
                candidates.append((connection, intent, intent.target.with(run: row.string("run_id"))))
            }
        }
        let chosen = scheduledReadScheduler.choose(candidates.map(\.2), now: ProcessInfo.processInfo.systemUptime)
        for target in chosen {
            guard let (connection, intent, _) = candidates.first(where: { $0.2 == target }) else { continue }
            let frame = try? await inspectOnce(connection, target).value
            scheduledReadScheduler.result(target, now: ProcessInfo.processInfo.systemUptime, success: frame != nil)
            guard let frame else { continue }
            if frame.raw.string("pending_settings_id") != intent.pendingId {
                app.editMessages { state in
                    guard var current = state.sessionActions.first(where: { $0.requestId == intent.requestId && $0.applyAttemptId.isEmpty }) else { return state }
                    current.status = "cancelled"
                    current.error = "Native pending settings changed; the old intent was not applied."
                    return state.action(current)
                }
            }
            observeScheduledSettings(frame)
        }
    }

    func observeScheduledSettings(_ frame: ConversationFrame) {
        let raw = frame.raw, pending = raw.string("pending_settings_id")
        guard !pending.isEmpty, raw.string("settings_apply_when") == "now", raw.string("activity") == "idle", raw.string("phase") == "idle" else { return }
        let key = frame.target.key + ":" + pending
        guard !pendingSettingsAttempts.contains(key) else { return }
        let target = frame.target
        func sameSession(_ action: SessionAction) -> Bool {
            action.target.connectionId == target.connectionId && action.target.computerId == target.computerId && action.target.session == target.session &&
                action.target.conversation == target.conversation
        }
        guard let authorized = messageState.sessionActions.last(where: {
            $0.operation == "settings" && $0.status == "scheduled" && $0.applyAttemptId.isEmpty && $0.pendingId == pending &&
                $0.arguments.string("expected_pending_id").isEmpty && sameSession($0)
        }), !messageState.sessionActions.contains(where: { $0.operation == "settings" && sameSession($0) && $0.arguments.string("expected_pending_id") == pending })
        else { return }
        var args = authorized.arguments
        if ["model", "effort"].contains(where: { args.has($0) && raw.string("pending_" + $0) != args.string($0) }) { return }
        args["expected_pending_id"] = .string(pending)
        guard authorizedSettingsReason(target, args, authorized, frame).isEmpty else { return }
        pendingSettingsAttempts.insert(key)
        submitSessionAction(target, "settings", args, authorized: authorized, frame: frame)
    }

    // MARK: Process output

    func processOutputReason(_ target: Target, _ processId: String) -> String {
        if selected?.target != target || !activityVerified || !target.agentId.isEmpty { return "Refresh the parent session before reading process output." }
        if app.relayOperations[target.connectionId]?.contains("process_output") != true || app.machineOperations[target.machineKey]?.contains("process_output") != true {
            return "This computer does not support remote process output."
        }
        guard let process = (activity?.object("processes")?.objects("items") ?? []).first(where: { $0.string("id") == processId }) else {
            return "This process is no longer present."
        }
        return process.object("capabilities")?.bool("output") == true ? "" : "This process does not provide output."
    }

    func processOutput(_ target: Target, _ processId: String, generation: String?) {
        let reason = processOutputReason(target, processId)
        guard reason.isEmpty else {
            processOutputError = reason
            return
        }
        guard !(processOutputLoading && processOutputId == processId), let connection = app.connections.first(where: { $0.id == target.connectionId }) else { return }
        let navigation = navigationId
        processOutputTask?.cancel()
        processOutputId = processId
        processOutputResult = nil
        processOutputError = ""
        processOutputLoading = true
        processOutputTask = Task {
            defer { if navigationId == navigation && processOutputId == processId { processOutputLoading = false } }
            do {
                let id = UUID().uuidString.lowercased()
                var payload: JSONObject = ["request_id": .string(id), "expected_run_id": .string(target.run), "expected_conversation_id": .string(target.conversation),
                                           "process_id": .string(processId)]
                if !target.archiveId.isEmpty { payload["archive_id"] = .string(target.archiveId) }
                if let generation { payload["generation"] = .string(generation) }
                let receipt = try await app.client.waitForReceipt(connection, initial: try await app.client.submit(connection, target: target, operation: "process_output",
                                                                                                                  payload: payload, id: id))
                guard receipt.string("state") == "completed", let result = receipt.object("result") else {
                    let message = receipt.string("error")
                    throw ZerusError(.unavailable, message.isEmpty ? "Process output is not available yet." : message)
                }
                guard result.string("request_id") == id, result.string("name") == target.session, result.string("id") == processId, result.string("run_id") == target.run,
                      result.string("conversation_id") == target.conversation, result.string("archive_id") == target.archiveId else {
                    throw ZerusError.mismatch("Computer returned output for another process.")
                }
                if navigationId == navigation && selected?.target == target && processOutputId == processId { processOutputResult = result }
            } catch is CancellationError {
            } catch {
                if navigationId == navigation && selected?.target == target { processOutputError = AppModel.message(error, fallback: "Process output is unavailable.") }
            }
        }
    }
}
