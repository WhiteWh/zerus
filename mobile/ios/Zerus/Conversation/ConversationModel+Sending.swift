import Foundation
import ZerusKit

/// Drafts, messages, answers, receipts and interruptions. Every attempt is durable
/// before transport; an uncertain attempt is only ever checked by its original receipt.
extension ConversationModel {
    static let rejectedStatuses: Set<Int> = [400, 401, 403, 404, 409, 413, 429]

    func draft(_ target: Target, _ question: Question? = nil) -> Draft {
        let questionId = question?.id ?? "", hash = question?.hash ?? ""
        if let existing = drafts.first(where: { $0.target == target && $0.questionId == questionId && $0.questionHash == hash && $0.detachedId.isEmpty }) {
            return existing
        }
        let key = JSON.array([.string(target.key), .string(questionId), .string(hash)]).compact
        if let empty = emptyDrafts[key] { return empty }
        let created = Draft(target: target, text: "", questionId: questionId, questionHash: hash)
        emptyDrafts[key] = created
        return created
    }

    func save(_ draft: Draft) { app.editMessages { MessageChanges.draft($0, draft) } }

    func contextBlocked(_ target: Target) -> Bool {
        messageState.sessionActions.contains { $0.target == target && $0.blocksSending } ||
            clearExpectation.map { ContextPolicies.sameSessionRun($0.target, target) } == true ||
            messageState.contextOperations.contains { ContextPolicies.sameSessionRun($0.target, target) && $0.blocksSending }
    }

    func sendingBlocked(_ target: Target) -> Bool {
        app.voice.recording || contextBlocked(target) || preparingTargets.contains(target.key) || normalizedOutgoing.contains { $0.belongs(to: target) && $0.blocksSending } ||
            messageState.pickerDraft?.target == target
    }

    func ownSendId(_ target: Target) -> String {
        guard let send = lastOwnSend, send.target == target || send.target.resolves(to: target) else { return "" }
        return send.id
    }

    func edit(_ target: Target, text: String, question: Question? = nil, answers: String = "") {
        let current = draft(target, question)
        guard target.archiveId.isEmpty, current.status == "editing", app.storageReady else { return }
        var editor = current
        editor.text = text
        editor.answers = answers
        editor.updatedAt = Clock.milliseconds()
        editor.revision = UUID().uuidString.lowercased()
        let updated = editor
        app.editMessages { MessageChanges.text($0, updated) }
    }

    func discard(_ draft: Draft) {
        guard !receiptFlights.contains(draft.key), !(drafts.first { $0.key == draft.key }.map(QuestionTracking.locked) ?? false), !answerInFlight(draft) else { return }
        emptyDrafts[JSON.array([.string(draft.target.key), .string(draft.questionId), .string(draft.questionHash)]).compact] = nil
        app.editMessages { MessageChanges.discard($0, draft) }
        app.flushPrivateState()
    }

    /// Returns a failed or unconfirmed answer to editing after the person reviewed it.
    func review(_ draft: Draft) {
        guard !receiptFlights.contains(draft.key), !(drafts.first { $0.key == draft.key }.map(QuestionTracking.locked) ?? false), !answerInFlight(draft),
              !(draft.status == "submitting" && draft.questionId.isEmpty) else { return }
        app.editMessages { state in
            var copy = state
            copy.drafts = state.drafts.map { existing in
                guard existing.key == draft.key, existing.requestId == draft.requestId else { return existing }
                var updated = existing
                updated.status = "editing"
                updated.requestId = ""
                return updated
            }
            return copy
        }
        app.flushPrivateState()
    }

    func answerInFlight(_ draft: Draft) -> Bool { !draft.requestId.isEmpty && answerFlights[draft.key] == draft.requestId }
    func answerStatus(_ draft: Draft) -> String { draft.status == "submitting" && !answerInFlight(draft) ? "uncertain" : draft.status }

    // MARK: Answers

    func questionSendNotice(_ target: Target, _ question: Question) -> String { questionSendRefusal?.text(for: target, question) ?? "" }

    func refuseQuestionSend(_ intent: QuestionSendIntent, _ text: String) {
        questionSendRefusal = QuestionSendRefusal(target: intent.target, questionId: intent.question.id, questionHash: intent.question.hash, text: text)
        contextNotice = text
        if selected?.target == intent.target && !questions.contains(where: { $0.id == intent.question.id && $0.hash == intent.question.hash }) {
            app.error = text
        }
    }

    func questionSendValid(_ intent: QuestionSendIntent) -> Bool {
        fresh(intent.target) && !demo && app.storageReady && !contextBlocked(intent.target) && !preparingTargets.contains(intent.target.key) &&
            app.connections.contains { $0.id == intent.target.connectionId } &&
            intent.matches(target: selected?.target, navigation: Int(navigationId),
                           current: questions.first { $0.id == intent.question.id && $0.hash == intent.question.hash },
                           draft: draft(intent.target, intent.question)) && answerFlights[intent.draft.key] == nil
    }

    func cancelQuestionSend(_ intent: QuestionSendIntent) {
        questionSendGate.cancel(intent)
        pendingQuestionSend = questionSendGate.pending
    }

    func confirmQuestionSend(_ intent: QuestionSendIntent) {
        guard questionSendGate.active(intent) else { return }
        let confirmed = questionSendGate.confirm(intent, valid: questionSendValid(intent))
        pendingQuestionSend = questionSendGate.pending
        guard let confirmed else {
            refuseQuestionSend(intent, "The question or draft changed. Nothing was sent. Review it before submitting.")
            return
        }
        Task { await sendQuestion(intent.target, intent.question, answers: intent.answers, consent: confirmed) }
    }

    /// Answers one native question with the exact captured bytes, at most once.
    func sendQuestion(_ target: Target, _ question: Question, answers answerText: String?, consent: QuestionSendIntent? = nil) async {
        guard target.agentId.isEmpty, fresh(target), !demo, app.storageReady, !contextBlocked(target), !preparingTargets.contains(target.key),
              let native = questions.first(where: { $0.id == question.id && $0.hash == question.hash }),
              QuestionSendPolicies.schema(question) == QuestionSendPolicies.schema(native),
              let connection = app.connections.first(where: { $0.id == target.connectionId }) else { return }
        let original = draft(target, question)
        guard original.status == "editing", !target.run.isEmpty, answerFlights[original.key] == nil else { return }
        let frozen = answerText ?? original.answers
        guard QuestionSendPolicies.available(native, target, frozen) else { return }
        let intent = consent ?? QuestionSendPolicies.capture(target, navigation: Int(navigationId), question: native, draft: original, answers: frozen)
        guard questionSendValid(intent), intent.answers == frozen else { return }
        questionSendRefusal = nil
        if consent == nil {
            let authorized = questionSendGate.request(intent, cold: ContextPresentation.isCold(activity))
            pendingQuestionSend = questionSendGate.pending
            guard authorized != nil else { return }
        }
        var sending = original.begin()
        sending.answers = frozen
        let attempt = sending
        var attempted = false, refused = false
        answerFlights[attempt.key] = attempt.requestId
        preparingTargets.insert(target.key)
        defer {
            if answerFlights[attempt.key] == attempt.requestId { answerFlights[attempt.key] = nil }
            preparingTargets.remove(target.key)
        }
        do {
            save(attempt)
            _ = try await app.flushMessages()
            if consent == nil && QuestionSendPolicies.requiresCacheConsent(native, cold: ContextPresentation.isCold(activity)) {
                _ = try await app.durableMessages { intent.restoreAfterExpiry($0, requestId: attempt.requestId) }
                refuseQuestionSend(intent, "The prompt cache expired before sending. Review your answer and confirm full context; nothing was sent.")
                return
            }
            let currentQuestion = questions.first { $0.id == question.id && $0.hash == question.hash }
            guard fresh(target), !contextBlocked(target), !demo, app.storageReady, app.connections.contains(connection),
                  intent.permitsPrepared(target: selected?.target, navigation: Int(navigationId), current: currentQuestion, prepared: draft(target, question),
                                         requestId: attempt.requestId, cold: ContextPresentation.isCold(activity), consent: consent != nil) else {
                throw ZerusError.invalid("The selected question changed before sending.")
            }
            preparingTargets.remove(target.key)
            lastOwnSend = (target, attempt.requestId)
            var payload = try target.json()
            payload["request_id"] = .string(attempt.requestId)
            payload["question_id"] = .string(question.id)
            payload["expected_question_hash"] = .string(question.hash)
            payload["answers"] = try JSON.parse(frozen)
            attempted = true
            let initial: JSONObject
            do { initial = try await app.client.submit(connection, target: target, operation: "answer", payload: payload, id: attempt.requestId) }
            catch let failure as RelayError {
                refused = Self.rejectedStatuses.contains(failure.status)
                throw failure
            }
            let receipt = try await app.client.waitForReceipt(connection, initial: initial)
            await settle(attempt, receipt)
        } catch {
            let status = !attempted || refused ? "failed" : "uncertain"
            do {
                _ = try await app.durableMessages { state in
                    var copy = state
                    copy.drafts = state.drafts.map { existing in
                        guard existing.key == attempt.key, existing.requestId == attempt.requestId, existing.generation == attempt.generation,
                              !QuestionTracking.locked(existing) else { return existing }
                        var updated = existing
                        updated.status = status
                        return updated
                    }
                    return copy
                }
            } catch {
                app.error = "Could not save the answer outcome. Its original request remains recoverable."
            }
            app.error = status == "failed" ? "Answer not sent. \(AppModel.message(error, fallback: ""))"
                : "Answer delivery is unknown. Check the original receipt; nothing has been retried."
        }
    }

    // MARK: Messages

    func sendReason(_ target: Target) -> String {
        if demo { return "Connect a computer to send messages." }
        if !app.storageReady { return "Private storage is not ready." }
        if !target.archiveId.isEmpty { return "Archived conversations are read-only." }
        let child = childSendReason(target)
        if !child.isEmpty { return child }
        if !fresh(target) { return "Refresh this conversation before sending." }
        let block = NativeEvents.messageBlockReason(activity)
        if !block.isEmpty { return block }
        if sendingBlocked(target) { return "Resolve the pending delivery before sending again." }
        return ""
    }

    /// Persists the outgoing record first, then submits it once with its immutable request ID.
    /// A compaction continuation sends only its captured, unchanged message after native compaction completed.
    func sendMessage(_ target: Target, expectedDraft: Draft? = nil, expectedCompactionId: String = "", allowColdCache: Bool = false,
                     continuationId: String = "") async {
        guard childSendReason(target).isEmpty, fresh(target), !demo, app.storageReady, NativeEvents.messageBlockReason(activity).isEmpty, !sendingBlocked(target),
              let connection = app.connections.first(where: { $0.id == target.connectionId }) else {
            if !continuationId.isEmpty { cancelCompactContinuation("The conversation is not ready. Your draft was kept without sending.") }
            return
        }
        let original = draft(target)
        if !original.attachments.isEmpty, let reason = attachmentReason(target).nonEmpty ?? (app.attachmentStore == nil ? "Private attachment storage is unavailable." : nil) {
            contextNotice = reason
            if !continuationId.isEmpty { cancelCompactContinuation("The draft was kept without sending. \(reason)") }
            return
        }
        if let expectedDraft, !ContextPolicies.sameDraft(expectedDraft, original) {
            contextNotice = "Your composition changed. Review it before sending."
            return
        }
        if !expectedCompactionId.isEmpty {
            guard continuationId == expectedCompactionId, continuation?.requestId == continuationId,
                  let operation = messageState.contextOperations.first(where: { $0.requestId == continuationId }), let raw = activity,
                  ContextPolicies.compactStatus(operation, raw) == "completed" else { return }
        }
        if expectedCompactionId.isEmpty && !allowColdCache && ContextPresentation.isCold(activity) {
            contextNotice = "Choose how to continue with the cold cache before sending."
            return
        }
        let intent = continuation.flatMap { $0.requestId == continuationId && !continuationId.isEmpty ? $0 : nil }
        if let intent { continuationEnqueuing = intent.requestId }
        let id = UUID().uuidString.lowercased(), at = Clock.seconds()
        preparingTargets.insert(target.key)
        deliveryFlights.insert(id)
        defer {
            if intent != nil {
                continuationEnqueuing = ""
                continuation = nil
            }
            deliveryFlights.remove(id)
            syncOutgoing()
        }
        let frozen = original.attachments.isEmpty ? captureSendIntent(target, id: id, text: original.text,
            compactionId: expectedCompactionId.isEmpty ? nil : expectedCompactionId) : nil
        let written: MessageState
        do {
            written = try await app.durableMessages({ try $0.enqueue(original, requestId: id, at: at, sendIntent: frozen) },
                                                    acknowledge: { current, committed in MessageChanges.enqueueAcknowledged(current, original: original, written: committed, requestId: id) })
        } catch {
            app.error = (error as? ZerusError).map(\.message) ?? "Could not save the outgoing message. It was not sent. Your composition is preserved."
            preparingTargets.remove(target.key)
            continuationEnqueuing = ""
            cancelCompactContinuation()
            return
        }
        preparingTargets.remove(target.key)
        guard let message = written.outgoing.first(where: { $0.requestId == id }) else { return }
        lastOwnSend = (target, id)
        var attempted = false
        var rejected: RelayError?
        do {
            var files: JSON = .array([])
            if !message.attachments.isEmpty {
                guard let store = app.attachmentStore else { throw ZerusError.invalid("Private attachment storage is unavailable.") }
                let attachments = message.attachments
                files = try await Task.detached(priority: .userInitiated) { try store.payload(attachments) }.value
            }
            guard fresh(target), app.connections.contains(where: { $0.id == connection.id }) else {
                throw ZerusError.invalid("The session changed before transport. Review the saved message.")
            }
            if let intent {
                guard let cleared = written.drafts.first(where: { $0.key == original.key }), continuation?.requestId == intent.requestId,
                      intent.permitsTransport(target: selected?.target, navigation: navigationId, now: ProcessInfo.processInfo.systemUptime, written: cleared, current: draft(target)),
                      let operation = messageState.contextOperations.first(where: { $0.requestId == intent.requestId }), let raw = activity,
                      ContextPolicies.compactStatus(operation, raw) == "completed" else {
                    throw ZerusError.invalid("Compact continuation was canceled. The captured message was not sent and remains recoverable.")
                }
            }
            var payload = try target.json()
            payload["request_id"] = .string(id)
            payload["text"] = .string(message.text)
            payload["attachments"] = files
            if !expectedCompactionId.isEmpty { payload["expected_compaction_id"] = .string(expectedCompactionId) }
            if let saved = message.sendIntent {
                guard let retained = saved.value()?.object("payload") else { throw ZerusError.invalid("The saved message details are unavailable. Review this message.") }
                payload = retained
            }
            attempted = true
            let initial: JSONObject
            do { initial = try await app.client.submit(connection, target: target, operation: "send", payload: payload, id: id) }
            catch let failure as RelayError {
                if Self.rejectedStatuses.contains(failure.status) { rejected = failure }
                throw failure
            }
            await settleOutgoing(message, try await app.client.waitForReceipt(connection, initial: initial))
        } catch {
            let failure = rejected?.message ?? (attempted ? "Delivery is unconfirmed. Check its receipt before sending again."
                : (error as? ZerusError)?.message ?? "The saved message was not sent. Review it before trying again.")
            let status = attempted && rejected == nil ? "uncertain" : "failed"
            do {
                _ = try await app.durableMessages { state in
                    MessageChanges.result(state, id) { existing in
                        var updated = existing
                        updated.status = status
                        updated.error = failure
                        return updated
                    }
                }
            } catch {
                app.error = "Could not save the delivery result. The original outgoing message remains recoverable. Check its receipt."
            }
        }
    }

    private func captureSendIntent(_ target: Target, id: String, text: String, compactionId: String? = nil) -> SendIntent? {
        guard let connection = app.connections.first(where: { $0.id == target.connectionId }),
              let machine = SendReceiptRecovery.machine(app.machines, target: target),
              let encoding = SendReceiptRecovery.encoding(version: connection.protocolVersion, route: machine.route) else { return nil }
        let candidates = app.sessions.filter { $0.target.connectionId == target.connectionId && $0.target.computerId == target.computerId &&
            $0.target.run == target.run && $0.target.agentId.isEmpty && $0.target.archiveId.isEmpty }
        guard candidates.count == 1, let source = candidates.first else { return nil }
        return SendReceiptRecovery.freeze(target: target, id: id, text: text, provider: source.agent, machineId: machine.machineId,
            sessionId: source.raw.string("session_id"), encoding: encoding, compactionId: compactionId)
    }

    func settleOutgoing(_ message: OutgoingMessage, _ receipt: JSONObject, deadline: Double? = nil) async {
        let result = receipt.object("result"), state = receipt.string("state")
        let proof = message.originalSendIntent().flatMap { intent in result.flatMap { SendReceiptRecovery.acknowledgement(intent, $0) } }
        let strict = message.hasOriginalSendIdentity && SendReceiptRecovery.supported(relay: app.relayOperations[message.target.connectionId],
            native: app.machineOperations[MachineKey(connectionId: message.target.connectionId, computerId: message.target.computerId)])
        let confirmed = receipt.string("request_id") == message.requestId && state == "completed" && ["submitted", "queued"].contains(result?.string("status") ?? "") && (!strict || proof != nil) && (!message.hasOriginalSendIdentity || message.originalSendIntent() != nil)
        let rejected = state == "failed" && receipt.string("request_id") == message.requestId
        let submittedAt = result?.number("submitted_at"), submittedText = result?.string("submitted_text") ?? ""
        do {
            _ = try await app.durableMessages { current in
                guard current.outgoing.filter({ $0.requestId == message.requestId }).count == 1 else { return current }
                return MessageChanges.result(current, message.requestId) { existing in
                    guard existing.target == message.target, SendReceiptRecovery.exact(existing.text, message.text),
                          existing.attachments == message.attachments, existing.createdAt == message.createdAt,
                          existing.originalSendIntent() == message.originalSendIntent() else { return existing }
                    if let deadline, SendReceiptRecovery.now >= deadline { return existing }
                    if existing.sendReceiptProof != nil && ["submitted", "recorded"].contains(existing.status) { return existing }
                    var updated = existing
                    if confirmed {
                        updated.sendReceiptProof = proof
                        updated.status = "submitted"
                        updated.submittedAt = submittedAt ?? existing.createdAt
                        updated.submittedText = submittedText.isEmpty ? existing.text.trimmingCharacters(in: .whitespacesAndNewlines) : submittedText
                        updated.error = ""
                    } else {
                        updated.status = rejected ? "failed" : "uncertain"
                        updated.error = rejected ? "The computer rejected this message. Review it before trying again."
                            : "Delivery is unconfirmed. Check its receipt before sending again."
                    }
                    return updated
                }
            }
        } catch {
            app.error = "Could not save the delivery result. Check its receipt; the outgoing message remains recoverable."
            return
        }
        if let target = selected?.target, message.belongs(to: target) { refreshActivity() }
    }

    /// Reads the original receipt of an unconfirmed message; it never sends again. An automatic check stays silent
    /// when the receipt is unavailable, and both give up after 45 seconds.
    func checkOutgoing(_ message: OutgoingMessage, automatic: Bool = false) async {
        guard let connection = app.connections.first(where: { $0.id == message.target.connectionId }) else { return }
        let flight = "outgoing:\(message.requestId)"
        guard !receiptFlights.contains(flight), message.status != "sending" else { return }
        receiptFlights.insert(flight)
        defer { receiptFlights.remove(flight) }
        do {
            let supported = SendReceiptRecovery.supported(relay: app.relayOperations[message.target.connectionId],
                native: app.machineOperations[MachineKey(connectionId: message.target.connectionId, computerId: message.target.computerId)])
            let intent = supported ? (message.hasOriginalSendIdentity ? message.originalSendIntent() : captureSendIntent(message.target, id: message.requestId, text: message.text)) : nil
            switch try await app.client.checkDelivery(connection, message: message, intent: intent) {
            case .original(let receipt, let deadline): await settleOutgoing(message, receipt, deadline: deadline)
            case .native(let intent, let proof, let deadline):
                _ = try await app.durableMessages { current in
                    SendReceiptRecovery.settle(current, captured: message, intent: intent, proof: proof, deadline: deadline, now: SendReceiptRecovery.now)
                }
                if let target = selected?.target, message.belongs(to: target) { refreshActivity() }
            case .unconfirmed:
                if !automatic { app.error = "Delivery is still unconfirmed. The original message remains saved." }
            }
        } catch {
            if !automatic { app.error = "Receipt unavailable. The outgoing message remains saved." }
        }
    }

    /// Only clears this phone's sending block; the original receipt can still settle later.
    func dismissOutgoing(_ message: OutgoingMessage) async {
        guard message.status == "uncertain", !deliveryFlights.contains(message.requestId) else { return }
        do {
            // A failed result write can leave a settled send stored as "sending";
            // recover only this exact row, after the original flight is absent.
            _ = try await app.durableMessages { state in
                OutgoingRecoveryPolicies.dismiss(state, message, recoverInterruptedSending: true)
            }
        } catch {
            app.error = "Could not save the dismissal. The original message and your composition are kept."
        }
    }

    /// Restores a failed message to the composer for explicit review; the current text is parked as a saved draft.
    func prepareOutgoing(_ message: OutgoingMessage) {
        guard message.status == "failed" else { return }
        guard !receiptFlights.contains("outgoing:\(message.requestId)") else { return }
        let restored = Draft(target: message.target, text: message.text, attachments: message.attachments)
        let parkedId = UUID().uuidString.lowercased()
        app.editMessages { state in
            guard let current = state.outgoing.first(where: { $0.requestId == message.requestId }), current.status == "failed" else { return state }
            let editor = state.drafts.first { $0.target == message.target && $0.questionId.isEmpty && $0.detachedId.isEmpty }
            var parked: Draft?
            if let editor, !editor.text.isEmpty || !editor.attachments.isEmpty {
                var copy = editor
                copy.detachedId = parkedId
                parked = copy
            }
            var copy = state
            copy.drafts = state.drafts.filter { $0.key != restored.key } + (parked.map { [$0] } ?? []) + [restored]
            copy.outgoing = state.outgoing.map { existing in
                guard existing.requestId == message.requestId else { return existing }
                var updated = existing
                updated.status = "reviewed"
                updated.coverage = OutgoingCoverage.draft(existing, restored)
                return updated
            }
            return copy
        }
        app.flushPrivateState()
    }

    /// Brings a saved draft back into its conversation; the current composition is parked.
    func restoreDetached(_ draft: Draft) {
        guard !QuestionTracking.locked(draft), !draft.detachedId.isEmpty else { return }
        var restored = draft
        restored.detachedId = ""
        restored.generation = UUID().uuidString.lowercased()
        restored.revision = UUID().uuidString.lowercased()
        let candidate = restored
        let parkedId = UUID().uuidString.lowercased()
        app.editMessages { state in
            var parked: Draft?
            if let editor = state.drafts.first(where: { $0.key == candidate.key }), !editor.text.isEmpty || !editor.attachments.isEmpty {
                var copy = editor
                copy.detachedId = parkedId
                parked = copy
            }
            var copy = state
            copy.drafts = state.drafts.filter { $0.key != draft.key && $0.key != candidate.key } + (parked.map { [$0] } ?? []) + [candidate]
            return copy
        }
        app.flushPrivateState()
    }

    // MARK: Receipts and interruption

    func settle(_ draft: Draft, _ receipt: JSONObject) async {
        do { _ = try await app.durableMessages { QuestionTracking.settle($0, draft, receipt) } }
        catch {
            app.error = "Could not save the delivery outcome. Its original request remains recoverable."
            return
        }
        let latest = drafts.first { $0.key == draft.key && $0.requestId == draft.requestId }
        if latest?.status == "submitted" || (draft.questionId == "__interrupt" && latest == nil) {
            refreshActivity()
        } else {
            let message = receipt.string("error")
            app.error = message.isEmpty ? "Delivery is unknown. Check the original receipt and saved answer." : message
        }
    }

    func checkReceipt(_ draft: Draft) async {
        guard let connection = app.connections.first(where: { $0.id == draft.target.connectionId }), !draft.requestId.isEmpty,
              !receiptFlights.contains(draft.key), !answerInFlight(draft) else { return }
        receiptFlights.insert(draft.key)
        defer { receiptFlights.remove(draft.key) }
        do {
            let receipt = try await app.client.waitForReceipt(connection, initial: try await app.client.receipt(connection, id: draft.requestId))
            await settle(draft, receipt)
        } catch {
            app.error = "Receipt unavailable. The answer remains saved."
        }
    }

    func interruptReason(_ target: Target) -> String {
        guard let raw = activity, raw.bool("interrupt_supported") else { return "The native agent cannot be interrupted from this phone." }
        if !target.agentId.isEmpty { return "Stop belongs to the parent session." }
        if !fresh(target) { return "Refresh this conversation first." }
        if contextBlocked(target) || preparingTargets.contains(target.key) { return "Wait for the pending request." }
        if drafts.contains(where: { $0.target == target && $0.questionId == "__interrupt" && $0.status != "editing" }) { return "An interruption is already pending." }
        return ""
    }

    /// Stops the exact running turn; a changed turn is never interrupted.
    func interrupt(_ target: Target) async {
        guard target.agentId.isEmpty, let raw = activity, fresh(target), !contextBlocked(target), raw.bool("interrupt_supported"),
              !preparingTargets.contains(target.key), let turn = raw.number("turn_started"),
              let connection = app.connections.first(where: { $0.id == target.connectionId }),
              !drafts.contains(where: { $0.target == target && $0.questionId == "__interrupt" && $0.status != "editing" }) else { return }
        let attempt = Draft(target: target, text: "Stop current turn", questionId: "__interrupt", questionHash: JSON.double(turn).compact).begin()
        preparingTargets.insert(target.key)
        save(attempt)
        do { _ = try await app.flushMessages() } catch {
            app.error = "Could not save the interruption request. It was not sent."
            preparingTargets.remove(target.key)
            return
        }
        preparingTargets.remove(target.key)
        do {
            guard fresh(target), activity?.number("turn_started") == turn else { throw CancellationError() }
            var payload = try target.json()
            payload["request_id"] = .string(attempt.requestId)
            payload["expected_turn_started"] = .double(turn)
            let initial = try await app.client.submit(connection, target: target, operation: "interrupt", payload: payload, id: attempt.requestId)
            await settle(attempt, try await app.client.waitForReceipt(connection, initial: initial))
        } catch {
            app.editMessages { state in
                var copy = state
                copy.drafts = state.drafts.map { existing in
                    guard existing.key == attempt.key, existing.requestId == attempt.requestId else { return existing }
                    var updated = existing
                    updated.status = "uncertain"
                    return updated
                }
                return copy
            }
            app.flushPrivateState()
            app.error = "Interruption is unconfirmed. Check the conversation before trying again."
        }
    }
}
