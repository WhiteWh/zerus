package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID
import kotlin.math.abs

data class OutgoingMessage(val requestId: String, val target: Target, val text: String,
    val attachments: List<Attachment>, val createdAt: Double, val status: String = "sending",
    val submittedAt: Double = 0.0, val submittedText: String = "", val error: String = "",
    val deliveryDismissed: Boolean = false, val sendIntent: SendIntent? = null, val sendReceiptProof: SendReceiptProof? = null, val sendCorrelation: SendCorrelation? = null, val coverage: OutgoingCoverage? = null) {
    fun recover() = if (status == "sending") copy(status = "uncertain", error = "Delivery was interrupted. Check its receipt before sending again.", deliveryDismissed = false) else this
    fun belongsTo(current: Target): Boolean = target == current || target.resolvesTo(current)
    val blocksSending get() = status == "sending" || status == "uncertain" && !deliveryDismissed
}

/** Only the initial blank conversation can acquire an identity under the same verified run. */
fun Target.resolvesTo(current: Target): Boolean = agentId.isBlank() && current.agentId.isBlank() && archiveId.isBlank() && current.archiveId.isBlank() && conversation.isBlank() && current.conversation.isNotBlank() &&
    connectionId == current.connectionId && computerId == current.computerId && session == current.session && run == current.run

/** The entire draft -> immutable outbox -> empty composer transition is one encrypted commit. */
data class MessageState(val drafts: List<Draft> = emptyList(), val outgoing: List<OutgoingMessage> = emptyList(), val pickerDraft: Draft? = null, val pickerId: String = "", val contextOperations: List<ContextOperation> = emptyList(), val sessionActions: List<SessionAction> = emptyList(), val conversationIndex: List<IndexedConversation> = emptyList(), val terminalBuffers:List<TerminalBuffer> = emptyList(), val machineAliases:List<MachineAlias> = emptyList(), val machineColors:List<MachineColorOverride> = emptyList(), val speechAppliedIds:List<String> = emptyList()) {
    fun action(action: SessionAction): MessageState {
        val next = sessionActions.filterNot { it.requestId == action.requestId } + action
        // Only confirmed scheduled apply children enter the terminal retention pool.
        // Keep the latest child referenced by a live original authorization.
        val referenced=next.filter { it.operation=="settings" && it.status=="scheduled" && ScheduledSettingsAttempt.expectedPendingId(it)==null }.map { it.applyAttemptId }.toSet()
        fun retain(value:SessionAction)=value.blocksSending || value.needsProjectReview || value.status=="scheduled" &&
            (value.operation!="settings" || ScheduledSettingsAttempt.expectedPendingId(value)==null || value.requestId in referenced)
        return copy(sessionActions = SessionNotificationPreferences.boundedProofs(next.filter(::retain) + next.filterNot(::retain).takeLast(100)))
    }
    fun context(operation: ContextOperation): MessageState {
        val next = contextOperations.filterNot { it.requestId == operation.requestId } + operation
        return copy(contextOperations = next.filter { it.blocksSending } + next.filterNot { it.blocksSending }.takeLast(50))
    }
    fun enqueue(draft: Draft, requestId: String, at: Double, sendIntent: SendIntent? = null): MessageState {
        require(draft.target.archiveId.isBlank()) { "Archived conversations are read-only." }
        require(draft.detachedId.isBlank() && draft.questionId.isBlank() && draft.status == "editing") { "Review this draft before sending." }
        require(draft.text.isNotBlank() || draft.attachments.isNotEmpty()) { "Write a message or attach a file." }
        require(draft.text.toByteArray(Charsets.UTF_8).size <= 65_536) { "Messages can contain at most 64 KiB of text." }
        require(sessionActions.none { it.target == draft.target && it.blocksSending }) { "Resolve the pending session action first." }
        require(contextOperations.none { ContextPolicies.sameSessionRun(it.target, draft.target) && it.blocksSending }) { "Check the pending context command before sending." }
        require(outgoing.none { it.belongsTo(draft.target) && it.blocksSending }) { "Check the previous delivery before sending again." }
        require(pickerDraft?.target != draft.target) { "Wait for the selected files to finish importing." }
        require(outgoing.none { it.requestId == requestId }) { "This request already exists." }
        val current = drafts.find { it.key == draft.key }
        require(current == null || current.revision == draft.revision) { "The draft changed. Review it before sending." }
        AttachmentPolicy.selection(draft.attachments)
        val message = OutgoingMessage(requestId, draft.target, draft.text, draft.attachments.toList(), at, sendIntent=sendIntent)
        require(sendIntent==null || SendReceiptRecovery.matches(message,sendIntent)) { "The saved message intent changed before transport." }
        val cleared = draft.copy(text = "", attachments = emptyList(), revision = UUID.randomUUID().toString(), generation = UUID.randomUUID().toString(), updatedAt = (at * 1000).toLong())
        return copy(drafts = drafts.filterNot { it.key == draft.key } + cleared, outgoing = outgoing + message)
    }
    fun resolveConversation(from: Target, to: Target): MessageState {
        if (!from.resolvesTo(to)) return this
        val editor = drafts.find { it.target == from && it.questionId.isBlank() && it.detachedId.isBlank() && it.status == "editing" } ?: return this
        if (drafts.any { it.target == to && it.questionId.isBlank() && it.detachedId.isBlank() }) return this
        return copy(drafts = drafts.map { if (it.key == editor.key) it.copy(target = to) else it })
    }
    fun selectionResult(origin: Draft, added: List<Attachment>): MessageState {
        require(pickerDraft?.generation == origin.generation && pickerDraft?.target == origin.target) { "File selection was canceled. The original draft is preserved." }
        if (added.isEmpty()) return copy(pickerDraft = null, pickerId = "")
        val current = drafts.find { it.generation == origin.generation && it.detachedId.isBlank() && it.status == "editing" &&
            (it.target == origin.target || origin.target.resolvesTo(it.target) || (ContextPolicies.clearMoved(this, origin.target, it.target) || ActionDraftPolicies.moved(this,origin.target,it.target))) }
        val restored = if (current != null) current.copy(attachments = current.attachments + added, revision = UUID.randomUUID().toString())
            else origin.copy(attachments = origin.attachments + added, detachedId = UUID.randomUUID().toString(), revision = UUID.randomUUID().toString())
        AttachmentPolicy.selection(restored.attachments)
        return copy(drafts = drafts.filterNot { it.key == restored.key } + restored, pickerDraft = null, pickerId = "")
    }
    fun update(message: OutgoingMessage): MessageState = copy(outgoing = outgoing.map { if (it.requestId == message.requestId) message else it })
}

object MessageCodec {
    fun target(value: JSONObject) = Target(value.getString("computer"), value.getString("session"), value.getString("run"), value.getString("conversation"), value.getString("connection"), value.string("archive"), value.string("agent"), value.string("parent_conversation"))
    fun targetJson(target: Target) = JSONObject().put("connection", target.connectionId).put("computer", target.computerId)
        .put("session", target.session).put("run", target.run).put("conversation", target.conversation).also {
            if (target.archiveId.isNotBlank()) it.put("archive", target.archiveId)
            if (target.agentId.isNotBlank()) it.put("agent",target.agentId).put("parent_conversation",target.parentConversation)
        }
    fun draft(value: JSONObject): Draft = Draft(target(value), value.getString("text"), value.optString("status", "editing"),
        value.optString("request"), value.optString("question"), value.optString("hash"), value.optString("answers"), value.optLong("updated"),
        value.optJSONArray("attachments")?.objects().orEmpty().map(Attachment::fromJson), value.string("revision").ifBlank { UUID.randomUUID().toString() }, value.string("detached"), value.string("generation").ifBlank { UUID.randomUUID().toString() }).recover()
    fun draftJson(draft: Draft) = targetJson(draft.target).put("text", draft.text).put("status", draft.status).put("request", draft.requestId)
        .put("question", draft.questionId).put("hash", draft.questionHash).put("answers", draft.answers).put("updated", draft.updatedAt)
        .put("attachments", JSONArray(draft.attachments.map(Attachment::toJson))).put("revision", draft.revision).put("detached", draft.detachedId).put("generation", draft.generation)
    fun outgoing(value: JSONObject) = OutgoingMessage(value.getString("request"), target(value), value.getString("text"),
        value.optJSONArray("attachments")?.objects().orEmpty().map(Attachment::fromJson), value.getDouble("created"), value.getString("status"),
        value.optDouble("submitted", 0.0), value.string("submitted_text"), value.string("error"), value.opt("delivery_dismissed") == true,
        if(value.has("send_intent")) SendIntent.decode(value.getJSONObject("send_intent")) else null,
        if(value.has("send_receipt_proof")) SendReceiptProof.decode(value.getJSONObject("send_receipt_proof")) else null,
        if(value.has("send_correlation")) SendCorrelation(value.getJSONObject("send_correlation").toString()) else null,
        if(value.has("coverage")) OutgoingCoverage.decode(value.getJSONObject("coverage")) else null).recover()
    fun outgoingJson(message: OutgoingMessage) = targetJson(message.target).put("request", message.requestId).put("text", message.text)
        .put("attachments", JSONArray(message.attachments.map(Attachment::toJson))).put("created", message.createdAt).put("status", message.status)
        .put("submitted", message.submittedAt).put("submitted_text", message.submittedText).put("error", message.error)
        .put("delivery_dismissed", message.deliveryDismissed).also { value ->
            message.sendIntent?.let { value.put("send_intent",it.json()) }
            message.sendReceiptProof?.let { value.put("send_receipt_proof",it.json()) }
            message.sendCorrelation?.let { value.put("send_correlation",it.json()) }
            message.coverage?.let { value.put("coverage",it.json()) }
        }
    private fun contextOperation(value: JSONObject): ContextOperation {
        val operation = value.getString("operation")
        val dismiss = if (value.has("dismiss_prompt")) {
            val flag = value.opt("dismiss_prompt")
            require(flag is Boolean && operation == "clear_context") { "Invalid saved context dismissal metadata." }
            flag
        } else null
        return ContextOperation(value.getString("request"), target(value), operation, value.getString("status"), value.optLong("created"),
            value.string("error"), value.string("resolved_conversation"), dismiss).recover()
    }
    private fun contextOperationJson(operation: ContextOperation): JSONObject {
        require(operation.dismissPrompt == null || operation.operation == "clear_context") { "Only clear context can dismiss a prompt." }
        return targetJson(operation.target).put("request", operation.requestId).put("operation", operation.operation).put("status", operation.status)
            .put("created", operation.createdAt).put("error", operation.error).put("resolved_conversation", operation.resolvedConversation).also { value ->
                operation.dismissPrompt?.let { value.put("dismiss_prompt", it) }
            }
    }
    fun state(value: JSONObject) = MessageState(value.optJSONArray("drafts")?.objects().orEmpty().map(::draft),
        value.optJSONArray("outgoing")?.objects().orEmpty().map(::outgoing), value.optJSONObject("picker")?.let(::draft), value.string("picker_id"),
        value.optJSONArray("context_operations")?.objects().orEmpty().map(::contextOperation),
        SessionNotificationPreferences.boundedProofs(value.optJSONArray("session_actions")?.objects().orEmpty().map { SessionAction(it.getString("request"), target(it), it.getString("operation"), it.getString("arguments"), it.getString("status"), it.optLong("created"), it.string("error"), it.optJSONObject("result_target")?.let(::target), it.string("pending_id"),it.string("apply_attempt_id"),it.string("result_path"),it.string("confirmed_notification_preference")).recover() }), value.optJSONArray("conversation_index")?.objects().orEmpty().take(1000).map(ConversationIndex::decode),value.optJSONArray("terminal_buffers")?.objects().orEmpty().map(TerminalBuffers::decode),value.optJSONArray("machine_aliases")?.objects().orEmpty().map(MachineNames::decode),MachineColors.decodeArray(value.optJSONArray("machine_colors")), value.optJSONArray("speech_applied_ids")?.let { ids -> (0 until ids.length()).map { ids.getString(it) }.takeLast(100) }.orEmpty())
    fun stateJson(state: MessageState) = JSONObject().put("drafts", JSONArray(state.drafts.map(::draftJson)))
        .put("outgoing", JSONArray(state.outgoing.map(::outgoingJson))).put("picker", state.pickerDraft?.let(::draftJson) ?: JSONObject.NULL).put("picker_id", state.pickerId).put("context_operations", JSONArray(state.contextOperations.map(::contextOperationJson)))
        .put("session_actions", JSONArray(state.sessionActions.map { action ->
            targetJson(action.target).put("request", action.requestId).put("operation", action.operation).put("arguments", action.arguments)
                .put("status", action.status).put("created", action.createdAt).put("error", action.error)
                .put("result_target", action.resultTarget?.let(::targetJson) ?: JSONObject.NULL).put("pending_id", action.pendingId).put("apply_attempt_id",action.applyAttemptId).put("result_path",action.resultPath)
                .put("confirmed_notification_preference",action.confirmedNotificationPreference)
        })).put("conversation_index", JSONArray(state.conversationIndex.map(ConversationIndex::encode))).put("terminal_buffers",JSONArray(state.terminalBuffers.map(TerminalBuffers::encode))).put("machine_aliases",JSONArray(state.machineAliases.map(MachineNames::encode))).put("machine_colors",JSONArray(state.machineColors.map(MachineColors::encode))).put("speech_applied_ids", JSONArray(state.speechAppliedIds))
}

object ConversationMerge {
    data class Result(val events: List<Event>, val recordedRequestIds: Set<String>, val exactRepresentations: Map<String,OutgoingCoverage> = emptyMap())
    fun merge(target: Target, native: List<Event>, outgoing: List<OutgoingMessage>): Result {
        val result = native.toMutableList()
        val used = mutableSetOf<Int>()
        val recorded = mutableSetOf<String>()
        val exact = mutableMapOf<String,OutgoingCoverage>()
        outgoing.filter { it.belongsTo(target) && it.status != "reviewed" }.sortedBy { it.createdAt }.forEach { message ->
            // Exact receipt-backed read evidence reconciles its own request.
            // OpenCode dates queued prompts by delivery, outside the time window.
            // Text/time matching below is only presentation deduplication.
            val confirmed=message.status in listOf("submitted","recorded")
            fun named(source: String) = result.indices.firstOrNull { index -> index !in used && result[index].role == "You" &&
                result[index].source == source && result[index].requestId.isNotBlank() && result[index].requestId == message.requestId }
            var match = named("hgs_delivery") ?: named("opencode_native")
            val nativeId=SendReceiptRecovery.exactNativeId(message)
            if(match==null && nativeId.isNotEmpty()) match=result.indices.singleOrNull { index -> index !in used && result[index].role=="You" &&
                result[index].inheritedFrom.isEmpty() && result[index].source==message.originalSendIntent()?.value()?.string("provider")+"_native" && result[index].nativeMessageId==nativeId }
            if (match == null && confirmed && message.submittedAt > 0) {
                val text = message.submittedText.ifBlank { message.text }.trim()
                // A fork's inherited row was never sent from this session: it is
                // no candidate, so it neither takes the send nor makes it ambiguous.
                if (text.isNotBlank()) match = result.indices.filter { index ->
                    val event = result[index]
                    index !in used && event.role == "You" && event.inheritedFrom.isBlank() && event.requestId.isBlank() &&
                        event.text.trim() == text && abs(event.at - message.submittedAt) <= 10
                }.singleOrNull()?.takeIf { index ->
                    outgoing.count { other -> other.belongsTo(target) && other.status in listOf("submitted", "recorded") &&
                        other.submittedAt > 0 && other.submittedText.ifBlank { other.text }.trim() == result[index].text.trim() &&
                        abs(result[index].at - other.submittedAt) <= 10 } == 1
                }
            }
            if (match != null) {
                OutgoingCoverage.native(message, result[match])?.let { exact[message.requestId] = it }
                used += match
                recorded += message.requestId
                result[match] = result[match].copy(id = "outgoing:${message.requestId}", outgoingId = message.requestId,
                    attachments = if (result[match].attachments.isNotEmpty()) AttachmentIdentity.withLocal(result[match].attachments, message.attachments)
                        else message.attachments.map(Attachment::displayed),
                    // The computer accepted it; a native inbox state says whether the agent has it yet.
                    delivery = result[match].delivery.takeIf { it in MessageDeliveryPresentation.inboxStates } ?: "recorded")
            } else result += Event("outgoing:${message.requestId}", "You", message.submittedText.ifBlank { message.text }, "", message.submittedAt.takeIf { it > 0 } ?: message.createdAt, message.requestId,
                message.attachments.map(Attachment::displayed), "mobile_outbox", if(message.status in listOf("submitted","recorded") && message.sendReceiptProof?.queued==true) "queued" else message.status, message.requestId)
        }
        val ordered = result.take(native.size).toMutableList()
        result.drop(native.size).forEach { fallback ->
            val position = ordered.indexOfFirst { it.at > fallback.at }
            if (position < 0) ordered += fallback else ordered.add(position, fallback)
        }
        return Result(ordered.toList(), recorded, exact)
    }
}
