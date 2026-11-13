package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject

/** Public display rows only. Cached history never verifies native input identity. */
object RollingConversationHistory {
    data class Result(val events: List<Event>, val truncated: Boolean, val supersededIds: Map<String, String> = emptyMap())
    fun merge(target: Target, previousTarget: Target?, previous: List<Event>, incoming: List<Event>,
              maxEvents: Int = 500, maxBytes: Int = 2 * 1024 * 1024): Result {
        require(maxEvents >= 0 && maxBytes >= 2)
        val rows = linkedMapOf<String, Event>()
        if (target == previousTarget) previous.forEach { rows[it.id] = it.copy(attachments = it.attachments.toList()) }
        incoming.forEach { incomingRow ->
            val previousRow = rows[incomingRow.id]
            val voice = incomingRow.voice?.let { VoiceProtocol.newer(previousRow?.voice, it) }
            rows[incomingRow.id] = if (voice == null) incomingRow.copy(attachments = incomingRow.attachments.toList())
                else incomingRow.copy(voice = voice, text = voice.transcript, attachments = listOf(voice.audio))
        }
        val canonicalAliases = mutableMapOf<String, String>()
        incoming.filter { it.historyId.isNotBlank() }.forEach { canonical ->
            (canonical.originalIds + canonical.originalId).filter(String::isNotBlank).distinct().forEach { original ->
                val legacy = rows.values.filter { it.id == original && it.id != canonical.id }.singleOrNull()
                if (legacy != null) { rows.remove(legacy.id); canonicalAliases[legacy.id] = canonical.id }
            }
        }
        incoming.filter { it.voice != null }.forEach { voice ->
            voice.originalIds.forEach { original ->
                val legacy = rows[original]
                if (legacy != null && legacy.voice == null && legacy.role == "You" && original != voice.id) {
                    rows.remove(original); canonicalAliases[original] = voice.id
                }
            }
        }
        val reconciled = rows.values.toMutableList()
        // Native Stop is a hook excerpt of a later public provider reply, not
        // a second message. Preserve distinct native provider IDs even when
        // their contents repeat; only demonstrated cross-source supersession.
        val superseded = canonicalAliases.toMutableMap()
        fun replaces(provider: Event, hook: Event) = hook.nativeType == "Stop" && hook.id.startsWith("journal:") && hook.text.isNotBlank() &&
            provider.nativeType == "AgentMessage" && provider.id.startsWith("provider:") && provider.at.isFinite() && hook.at.isFinite() &&
            NativeStopExcerpt.matches(provider.text, hook.text) && kotlin.math.abs(provider.at - hook.at) < 10
        reconciled.removeAll { hook ->
            val candidates = rows.values.filter { provider -> replaces(provider, hook) }
            val provider = candidates.singleOrNull()
            val demonstrated = provider != null && rows.values.count { other -> replaces(provider, other) } == 1
            if (demonstrated) superseded[hook.id] = provider!!.id
            demonstrated
        }
        val replaced = mutableSetOf<String>()
        reconciled.filter { it.source == "hgs_delivery" && it.role == "You" && it.requestId.isNotBlank() && it.submittedText.isNotBlank() }.forEach { receipt ->
            val candidates = reconciled.filter { it.id !in replaced && it.role == "You" && AttachmentReceiptMatch.replaceable(receipt, it) &&
                it.nativeType in listOf("UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted") &&
                AttachmentReceiptMatch.matches(receipt, it) }
            val candidate = candidates.singleOrNull()
            val competing = reconciled.count { other -> other.source == "hgs_delivery" && other.role == "You" && other.requestId.isNotBlank() &&
                candidate != null && AttachmentReceiptMatch.matches(other, candidate) }
            if (candidate != null && competing == 1) {
                replaced += candidate.id
                superseded[candidate.id] = receipt.id
            }
        }
        reconciled.removeAll { it.id in replaced }
        val ordered = reconciled.sortedBy { it.at.takeIf(Double::isFinite) ?: 0.0 }
        val retained = ArrayDeque<Event>()
        var bytes = 2
        var truncated = false
        // Include JSON escaping and metadata in the actual serialized budget.
        val sizes = ArrayDeque<Int>()
        ordered.forEach { event ->
            val size = EventHistoryCodec.row(event).toString().toByteArray(Charsets.UTF_8).size
            retained.addLast(event); sizes.addLast(size); bytes += size + if (retained.size > 1) 1 else 0
            while (retained.size > maxEvents || bytes > maxBytes) {
                bytes -= sizes.removeFirst() + if (retained.size > 1) 1 else 0
                retained.removeFirst(); truncated = true
            }
        }
        val events = retained.toList()
        val kept = events.map { it.id }.toSet()
        return Result(events, truncated, superseded.filterValues { it in kept }.toMap())
    }
}

/** Match the exact transport expansion, never strip attachment-looking user text. */
internal object AttachmentReceiptMatch {
    private val instruction = Regex("""(?:Inspect the attached image at|Read the attached file at) ("(?:[^"\\]|\\.)+")""")
    /** A receipt stands only for a row that is no receipt itself; an OpenCode prompt that
     * names its sending request must name the receipt's. */
    fun replaceable(receipt: Event, native: Event) = native.receiptRequestId.isBlank() &&
        (native.requestId.isBlank() || native.requestId == receipt.requestId)
    fun matches(receipt: Event, native: Event): Boolean {
        // A receipt of this session never stands for a fork's inherited row.
        if (native.inheritedFrom.isNotBlank()) return false
        val submitted = receipt.submittedText.trim()
        if (submitted.isBlank() || native.text.trim() != submitted) return false
        if (kotlin.math.abs(native.at - receipt.at) <= 10) return true
        // Full expansions contain distinct immutable staged paths; a queued
        // acknowledgement can occur long after its accepted terminal input.
        val paths = instruction.findAll(submitted).map { it.groupValues[1] }.toList()
        return receipt.attachments.isNotEmpty() && submitted != receipt.text.trim() &&
            paths.size >= receipt.attachments.size && paths.distinct().size == paths.size
    }
}

/** Versioned private encrypted display cache; native inspect JSON stays unchanged. */
object EventHistoryCodec {
    fun row(event: Event): JSONObject = storedRow(ActivityNotices.cached(event))
    private fun storedRow(event: Event): JSONObject = JSONObject().put("id", event.id).put("role", event.role)
        .put("text", event.text).put("time", event.time).put("at", event.at.takeIf(Double::isFinite) ?: 0.0)
        .put("request_id", event.requestId).put("source", event.source).put("delivery", event.delivery)
        .put("native_type", event.nativeType).put("submitted_text", event.submittedText)
        .also { if(event.nativeMessageId.isNotEmpty()) it.put("native_message_id",event.nativeMessageId) }
        .put("original_id", event.originalId).put("history_id", event.historyId).put("history_cursor", event.historyCursor)
        .put("history_epoch", event.historyEpoch).put("incoming_seq", event.incomingSeq ?: JSONObject.NULL)
        .put("original_ids", JSONArray(event.originalIds)).put("detail_truncated",event.detailTruncated)
        .put("origin", event.origin).put("sender", event.sender).put("report", event.report).put("inherited_from", event.inheritedFrom)
        .put("voice", event.voice?.takeIf { it.state in VoiceProtocol.states }?.let(VoiceProtocol::json))
        .put("outgoing_id", event.outgoingId).put("attachments", JSONArray(event.attachments.map { file ->
            JSONObject().put("name", file.name).put("mime", file.mime).put("bytes", file.bytes).put("reference", file.reference).also {
                if (file.messageId.isNotBlank() && file.index >= 0) it.put("message_id", file.messageId).put("index", file.index)
            }
        }))
    fun encode(events: List<Event>): JSONArray = JSONArray(events.map(::row))
    fun decode(raw: JSONArray): List<Event> {
        require(raw.length() <= 500)
        require(raw.toString().toByteArray(Charsets.UTF_8).size <= 2 * 1024 * 1024)
        return (0 until raw.length()).map { index ->
            val value = raw.getJSONObject(index)
            val at = value.getDouble("at"); require(at.isFinite())
            val files = value.getJSONArray("attachments"); require(files.length() <= 8)
            Event(value.getString("id"), value.getString("role"), value.getString("text"), value.getString("time"), at,
                value.getString("request_id"), (0 until files.length()).map { position ->
                    val file = files.getJSONObject(position)
                    DisplayedAttachment(file.getString("name"), file.getString("mime"), file.getLong("bytes"), file.getString("reference"),
                        file.optString("message_id"), file.optInt("index", -1))
                }, value.getString("source"), value.getString("delivery"), value.getString("outgoing_id"), value.optString("native_type"), value.optString("submitted_text"), value.optString("original_id"),
                value.optString("history_id"), value.optString("history_cursor"), value.optString("history_epoch"),
                (value.opt("incoming_seq") as? Number)?.toLong()?.takeIf { it >= 0 },
                value.optJSONArray("original_ids")?.let { ids -> (0 until minOf(ids.length(),8)).mapNotNull { ids.opt(it) as? String } }.orEmpty(), value.optBoolean("detail_truncated"),
                value.optJSONObject("voice")?.let(VoiceProtocol::message),
                value.optString("origin").takeIf { it in ActivityNotices.origins }.orEmpty(), value.optString("sender"), value.optString("report"),
                inheritedFrom = value.optString("inherited_from").take(512),
                nativeMessageId = value.string("native_message_id").takeIf { it.toByteArray(Charsets.UTF_8).size<=256 && it.none(Char::isISOControl) }.orEmpty())
                .let(ActivityNotices::cached)
        }
    }
}
