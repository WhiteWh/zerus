package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject

/** Bounded frozen header, not a second copy of the composition. Unknown versions round-trip. */
data class SendCorrelation(val encoded: String) {
    init { require(encoded.toByteArray(Charsets.UTF_8).size <= 8192) }
    override fun toString() = "SendCorrelation(retained)"
    fun json() = JSONObject(encoded)
    fun intent(row: OutgoingMessage): SendIntent? = runCatching {
        val data = json()
        require(data.keys().asSequence().toSet() == setOf("schema", "header", "intent_sha256", "native_digest", "row_sha256"))
        require(data.opt("schema") is Int && data.getInt("schema") == 1)
        require(data.getString("row_sha256") == OutboxRetention.binding(row))
        val value = JSONObject(data.getJSONObject("header").toString())
        val payload = value.getJSONObject("payload")
        require(!payload.has("text"))
        payload.put("text", row.text)
        SendReceiptRecovery.validateIntent(value)
        val canonical = SendReceiptRecovery.encode(value)
        require(SendReceiptRecovery.hash(canonical) == data.getString("intent_sha256"))
        require(SendReceiptRecovery.nativeDigest(value) == data.getString("native_digest"))
        SendIntent(canonical, data.getString("intent_sha256")).also { require(SendReceiptRecovery.matches(row, it)) }
    }.getOrNull()
    companion object {
        fun capture(row: OutgoingMessage, intent: SendIntent): SendCorrelation? = runCatching {
            require(SendReceiptRecovery.matches(row, intent))
            val header = intent.value() ?: return null
            val native = SendReceiptRecovery.nativeDigest(header)
            header.getJSONObject("payload").remove("text")
            SendCorrelation(JSONObject().put("schema", 1).put("header", header)
                .put("intent_sha256", intent.intentSha256).put("native_digest", native)
                .put("row_sha256", OutboxRetention.binding(row)).toString())
        }.getOrNull()
    }
}

/** Immutable evidence of complete representation, distinct from presentation-only `recorded`. */
data class OutgoingCoverage(val kind: String, val binding: String, val identity: String, val schema: Int = 1) {
    fun json() = JSONObject().put("schema", schema).put("kind", kind).put("binding", binding).put("identity", identity)
    fun valid(row: OutgoingMessage) = schema == 1 && kind in setOf("native", "draft") && identity.isNotEmpty() &&
        identity.toByteArray(Charsets.UTF_8).size <= 2048 && binding == OutboxRetention.binding(row)
    companion object {
        fun decode(value: JSONObject): OutgoingCoverage {
            require(value.opt("schema") is Int)
            return OutgoingCoverage(value.getString("kind"), value.getString("binding"), value.getString("identity"), value.getInt("schema"))
        }
        fun native(row: OutgoingMessage, event: Event): OutgoingCoverage? {
            if (event.id.isEmpty() || event.role != "You" || event.inheritedFrom.isNotEmpty() || event.detailTruncated || event.text != row.text) return null
            val exactRequest = event.source in setOf("hgs_delivery", "opencode_native") && event.requestId == row.requestId
            val exactId = SendReceiptRecovery.exactNativeId(row).takeIf { it.isNotEmpty() }
                ?.let { (event.requestId.isEmpty() || event.requestId == row.requestId) && event.nativeMessageId == it && event.source == row.originalSendIntent()?.value()?.string("provider") + "_native" } == true
            if (!exactRequest && !exactId) return null
            if (event.attachments.size != row.attachments.size) return null
            if (!row.attachments.indices.all { i ->
                val local = row.attachments[i]; val remote = event.attachments[i]
                AttachmentIdentity.remote(remote) && remote.messageId == row.requestId && remote.index == i &&
                    remote.name == local.name && remote.mime == local.mime && remote.bytes == local.bytes && remote.sha256 == local.sha256
            }) return null
            return OutgoingCoverage("native", OutboxRetention.binding(row), event.source + ":" + event.id)
                .takeIf { it.valid(row) }
        }
        fun draft(row: OutgoingMessage, draft: Draft): OutgoingCoverage? =
            if (draft.target == row.target && draft.text == row.text && draft.attachments == row.attachments)
                OutgoingCoverage("draft", OutboxRetention.binding(row), draft.generation).takeIf { it.valid(row) } else null
    }
}

fun OutgoingMessage.originalSendIntent(): SendIntent? = sendIntent ?: sendCorrelation?.intent(this)
val OutgoingMessage.hasOriginalSendIdentity get() = sendIntent != null || sendCorrelation != null

/** Bounds redundant cache only. Protected compositions are durable user data (zerus-a4mub.47). */
object OutboxRetention {
    const val MAX_ROWS = 100
    const val MAX_BYTES = 1024 * 1024
    fun binding(row: OutgoingMessage): String {
        val value = MessageCodec.targetJson(row.target).put("request", row.requestId)
            .put("text_sha256", SendReceiptRecovery.hash(row.text))
            .put("attachments", JSONArray(row.attachments.map(Attachment::toJson)))
        return SendReceiptRecovery.hash(SendReceiptRecovery.encode(value))
    }
    fun compact(row: OutgoingMessage): OutgoingMessage {
        if (row.sendCorrelation != null) return row
        val intent = row.sendIntent ?: return row
        if (row.status !in setOf("submitted", "recorded", "failed", "reviewed")) return row
        val metadata = SendCorrelation.capture(row, intent) ?: return row
        // An invalid positive proof never becomes a trusted compact identity.
        if (row.sendReceiptProof != null && !validProof(row, intent)) return row
        return row.copy(sendIntent = null, sendCorrelation = metadata)
    }
    private fun validProof(row: OutgoingMessage, intent: SendIntent): Boolean {
        val proof = row.sendReceiptProof ?: return false
        val value = intent.value() ?: return false
        return proof.messageRequestId == row.requestId && proof.intentSha256 == intent.intentSha256 &&
            proof.nativeDigest == SendReceiptRecovery.nativeDigest(value) && proof.submittedAt.isFinite() && proof.submittedAt >= 0 &&
            (proof.nativeMessageId.isEmpty() && value.string("provider") != "opencode" || SendReceiptRecovery.exactNativeId(row).isNotEmpty())
    }
    fun evictable(row: OutgoingMessage, pinned: Set<String> = emptySet()): Boolean {
        if (row.requestId in pinned || row.status !in setOf("recorded", "reviewed")) return false
        val coverage = row.coverage ?: return false
        if (!coverage.valid(row) || (row.status == "reviewed") != (coverage.kind == "draft")) return false
        if (row.sendCorrelation != null && row.sendCorrelation.intent(row) == null) return false
        if (row.hasOriginalSendIdentity && row.originalSendIntent() == null) return false
        return true
    }
    /** Stateless entry point for migrations and contract tests. Live writers own a Normalizer. */
    fun normalize(state: MessageState, pinned: Set<String> = emptySet()) = Normalizer().normalize(state, pinned)

    /** One publication/writer lane. Memo entries never retain reconstructed canonical payloads. */
    class Normalizer {
        private data class Memo(val row: OutgoingMessage, val eligible: Boolean, val bytes: Int, var seen: Long)
        private val memo = java.util.LinkedHashMap<String, Memo>(512, .75f, true)
        private var outgoing: List<OutgoingMessage>? = null
        private var pins: Set<String>? = null
        internal val memoizedRows get() = synchronized(this) { memo.size }
        internal var walks = 0L; private set
        internal var rowEvaluations = 0L; private set
        @Synchronized fun normalize(state: MessageState, pinned: Set<String> = emptySet()): MessageState {
            // Draft reducers preserve this immutable list and the pin snapshot by reference.
            if (state.outgoing === outgoing && (pinned === pins || pinned == pins)) return state
            walks++
            val rows = state.outgoing.map { row ->
                val saved = memo[row.requestId]?.takeIf { it.row == row }
                val entry = saved ?: run {
                    rowEvaluations++
                    val normalized = compact(row)
                    val eligible = evictable(normalized)
                    Memo(normalized, eligible, if (eligible) MessageCodec.outgoingJson(normalized).toString().toByteArray(Charsets.UTF_8).size + 1 else 0, walks)
                        .also { memo[row.requestId] = it; if (memo.size > 512) memo.remove(memo.keys.first()) }
                }
                entry.seen = walks
                entry
            }
            var count = 0; var bytes = 2 // Array delimiters and separators.
            val retained = BooleanArray(rows.size) { true }
            for (i in rows.indices.reversed()) {
                val entry = rows[i]
                if (!entry.eligible || entry.row.requestId in pinned) continue
                if (count >= MAX_ROWS || entry.bytes > MAX_BYTES - bytes) {
                    retained[i] = false
                    if (memo[entry.row.requestId] === entry) memo.remove(entry.row.requestId)
                } else { count++; bytes += entry.bytes }
            }
            // Drop stale generations, including payloads removed from this lane's state.
            memo.entries.removeAll { it.value.seen != walks }
            val next = rows.mapIndexedNotNull { i, entry -> entry.row.takeIf { retained[i] } }
            val result = if (next == state.outgoing) state else state.copy(outgoing = next)
            outgoing = result.outgoing; pins = pinned
            return result
        }
    }
    /** Called only after the caller has verified this fresh frame's scope. */
    fun recorded(state: MessageState, ids: Set<String>, exact: Map<String, OutgoingCoverage>): MessageState =
        state.copy(outgoing = state.outgoing.map { row ->
            if (row.requestId !in ids || row.status == "reviewed") row
            else row.copy(status = "recorded", error = "", coverage = exact[row.requestId]?.takeIf { it.valid(row) } ?: row.coverage)
        })
}

/** Cache input identities before deriving the bounded presentation's pins. */
class OutboxPins {
    private val current = java.util.concurrent.atomic.AtomicReference<Set<String>>(emptySet())
    private var delivery: Set<String>? = null
    private var receipts: Set<String>? = null
    private var events: List<Event>? = null
    @Synchronized fun retain(delivery: Set<String>, receipts: Set<String>, events: List<Event>) {
        if (delivery === this.delivery && receipts === this.receipts && events === this.events) return
        this.delivery = delivery; this.receipts = receipts; this.events = events
        val next = delivery + receipts.map { it.substringAfterLast(':') } + events.mapNotNull { it.outgoingId.takeIf(String::isNotEmpty) }
        if (next != current.get()) current.set(next)
    }
    fun snapshot(): Set<String> = current.get()
}
