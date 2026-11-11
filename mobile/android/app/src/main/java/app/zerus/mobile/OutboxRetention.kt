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
    fun normalize(state: MessageState, pinned: Set<String> = emptySet()): MessageState {
        val rows = state.outgoing.map(::compact)
        var count = 0; var bytes = 2 // JSON array delimiters and one separator per retained row.
        val retained = BooleanArray(rows.size) { true }
        for (i in rows.indices.reversed()) {
            val row = rows[i]
            if (!evictable(row, pinned)) continue
            val size = MessageCodec.outgoingJson(row).toString().toByteArray(Charsets.UTF_8).size + 1
            if (count >= MAX_ROWS || size > MAX_BYTES - bytes) retained[i] = false
            else { count++; bytes += size }
        }
        val outgoing = rows.filterIndexed { i, _ -> retained[i] }
        return if (outgoing == state.outgoing) state else state.copy(outgoing = outgoing)
    }
    /** Called only after the caller has verified this fresh frame's scope. */
    fun recorded(state: MessageState, ids: Set<String>, exact: Map<String, OutgoingCoverage>): MessageState =
        state.copy(outgoing = state.outgoing.map { row ->
            if (row.requestId !in ids || row.status == "reviewed") row
            else row.copy(status = "recorded", error = "", coverage = exact[row.requestId]?.takeIf { it.valid(row) } ?: row.coverage)
        })
}
