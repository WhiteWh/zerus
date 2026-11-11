package app.zerus.mobile

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import java.util.UUID

/** Process lifetime prevents Activity/ViewModel disposal from cancelling accepted saves. */
class MessagePersistence private constructor(context: Context) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    val error = MutableStateFlow<Throwable?>(null)
    private val outboxPins = OutboxPins()
    fun retainOutbox(delivery: Set<String>, receipts: Set<String>, events: List<Event>) { outboxPins.retain(delivery, receipts, events) }
    private val initialized = scope.async {
        val store = PrivateStore(context.applicationContext)
        val writer = OutboxRetention.Normalizer(); val publication = OutboxRetention.Normalizer()
        val initial = publication.normalize(writer.normalize(store.messageState(), outboxPins.snapshot()), outboxPins.snapshot())
        OrderedStatePersistence(initial, scope, { store.saveMessageState(it) }, onResult = { error.value = it },
            normalize = { writer.normalize(it, outboxPins.snapshot()) }, normalizePublished = { publication.normalize(it, outboxPins.snapshot()) })
    }
    suspend fun open() = initialized.await()
    companion object {
        @Volatile private var instance: MessagePersistence? = null
        fun get(context: Context): MessagePersistence = instance ?: synchronized(this) {
            instance ?: MessagePersistence(context).also { instance = it }
        }
    }
}

/** Keyed changes preserve attachment additions and never replace the rest of the private state. */
object MessageChanges {
    fun draft(state: MessageState, value: Draft): MessageState {
        val current = state.drafts.find { it.key == value.key }
        val recovery = current?.takeIf { it.status == "editing" && it.generation != value.generation &&
            it.attachments.any { file -> value.attachments.none { it.id == file.id } } }
            ?.let { it.copy(detachedId = "selection-" + it.revision) }
        return state.copy(drafts = state.drafts.filterNot { it.key == value.key || (recovery != null && it.key == recovery.key) } + listOfNotNull(recovery) + value)
    }
    fun discard(state: MessageState, captured: Draft): MessageState {
        val current = state.drafts.find { it.key == captured.key } ?: return state
        if (current.generation != captured.generation || current.requestId != captured.requestId) return state
        if (current.revision != captured.revision) {
            if (current.status != "editing" || current.attachments.isEmpty()) return state
            val recovery = current.copy(detachedId = "selection-" + current.revision)
            return state.copy(drafts = state.drafts.filterNot { it.key == current.key || it.key == recovery.key } + recovery)
        }
        return state.copy(drafts = state.drafts.filterNot { it.key == captured.key })
    }
    fun text(state: MessageState, editor: Draft): MessageState {
        val current = state.drafts.find { it.key == editor.key } ?: state.drafts.find { moved ->
            editor.questionId.isBlank() && editor.detachedId.isBlank() && moved.questionId.isBlank() && moved.detachedId.isBlank() &&
                moved.generation == editor.generation && (ContextPolicies.clearMoved(state, editor.target, moved.target) || ActionDraftPolicies.moved(state,editor.target,moved.target))
        }
        if (current != null && current.status != "editing") return state
        val updated = current?.copy(text = editor.text, answers = editor.answers, revision = editor.revision, updatedAt = editor.updatedAt) ?: editor
        return draft(state, updated)
    }
    fun enqueueAcknowledged(state: MessageState, original: Draft, written: MessageState, requestId: String): MessageState {
        val message = written.outgoing.first { it.requestId == requestId }
        val cleared = written.drafts.first { it.key == original.key }
        val current = state.drafts.find { it.key == original.key }
        val acknowledged = when {
            current == null -> null
            current.revision == original.revision -> cleared
            current.generation != original.generation -> current
            else -> cleared.copy(text = current.text, answers = current.answers, revision = current.revision,
                updatedAt = current.updatedAt, attachments = current.attachments.filterNot { file -> original.attachments.any { it.id == file.id } })
        }
        val nextDrafts = state.drafts.filterNot { it.key == original.key } + listOfNotNull(acknowledged)
        return state.copy(drafts = nextDrafts, outgoing = if (state.outgoing.any { it.requestId == message.requestId }) state.outgoing else state.outgoing + message)
    }
    fun selectionAcknowledged(state: MessageState, origin: Draft, written: MessageState, addedIds: Set<String>, selectionId: String): MessageState {
        val committed = written.drafts.firstOrNull { it.attachments.any { file -> file.id in addedIds } } ?: return state
        val current = state.drafts.firstOrNull { it.generation == origin.generation && it.detachedId.isBlank() && it.status == "editing" &&
            (it.target == origin.target || origin.target.resolvesTo(it.target) || (ContextPolicies.clearMoved(state, origin.target, it.target) || ActionDraftPolicies.moved(state,origin.target,it.target))) }
        val recovered = if (current != null) current.copy(attachments = current.attachments + committed.attachments.filter { file -> file.id in addedIds && current.attachments.none { it.id == file.id } }, revision = committed.revision)
            else committed.copy(detachedId = committed.detachedId.ifBlank { "selection-" + committed.revision })
        return state.copy(drafts = state.drafts.filterNot { it.key == recovered.key } + recovered,
            pickerDraft = if (state.pickerId == selectionId) null else state.pickerDraft,
            pickerId = if (state.pickerId == selectionId) "" else state.pickerId)
    }
    /** Local dismissal keeps the original outcome checkable and never changes the editor or payload. */
    fun dismissOutgoing(state: MessageState, captured: OutgoingMessage, recoverInterruptedSending: Boolean = false): MessageState {
        if (captured.requestId.isBlank()) return state
        val saved = state.outgoing.singleOrNull { it.requestId == captured.requestId } ?: return state
        // Only a caller that verified the original send has no live flight may recover this projection.
        val current = if (recoverInterruptedSending && saved.status == "sending" && captured.status == "uncertain") saved.recover() else saved
        if (current != captured || current.status !in setOf("uncertain", "failed") || current.deliveryDismissed) return state
        return state.copy(outgoing = state.outgoing.map {
            if (it.requestId == current.requestId) current.copy(deliveryDismissed = true) else it
        })
    }
    fun result(state: MessageState, requestId: String, update: (OutgoingMessage) -> OutgoingMessage): MessageState = state.copy(outgoing = state.outgoing.map {
        if (it.requestId == requestId && it.status !in listOf("recorded", "reviewed")) update(it) else it
    })
}
