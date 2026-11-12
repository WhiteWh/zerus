package app.zerus.mobile

import org.json.JSONObject

/**
 * An open conversation's `current_read` (`hgs inspect --current-read`): the reply and attention its
 * computer would accept now, from the same inspection this phone displays (zerus-a4mub.9).
 */
data class CurrentRead(val reply:String?,val attention:String?,val attentionComplete:Boolean)

/**
 * Read marks shared through the session's computer (`hgs read`, operation `mark_read`).
 * The computer applies a field only while it is current, so marks never hide newer work.
 */
object SharedReadPolicies {
    private val signature=Regex("[0-9a-f]{64}")
    private val currentKeys=setOf("schema","run_id","conversation_id","reply_id","attention_signature","attention_complete")
    /** The computer's mark for this row's current conversation. */
    fun mark(row:Session):JSONObject?=row.raw.optJSONObject("read")
        ?.takeIf { row.target.conversation.isNotBlank() && it.string("conversation_id")==row.target.conversation }
    /** Exactly the current reply was read on some device. */
    fun replyRead(row:Session):Boolean=row.raw.string("reply_id").let { it.isNotBlank() && mark(row)?.string("reply_id")==it }
    /** C1 owns the current reply badge. A missing reply identity cannot resurrect old
     * phone-local history as a new desktop reply; this never advances a history watermark. */
    fun canShowReplyBadge(row:Session):Boolean {
        if(replyRead(row)) return false
        val target=row.target
        val shared=row.raw.has("attention_signature") && target.run.isNotBlank() && target.conversation.isNotBlank() &&
            row.raw.string("run_id")==target.run && row.raw.string("conversation_id")==target.conversation
        return !shared || row.raw.string("reply_id").isNotBlank() || mark(row)?.string("reply_id").isNullOrBlank()
    }
    /** Fleet hints alone cannot bind a nonselected reply mark to a canonical prefix. */
    fun canAdoptReply(row:Session,selectedTarget:Target?,selectedRaw:JSONObject?,selectedVerified:Boolean):Boolean {
        if(!replyRead(row)) return false
        if(selectedTarget==null || ConversationReadPolicies.key(selectedTarget)!=ConversationReadPolicies.key(row.target)) return false
        val target=row.target
        return selectedTarget==target && selectedVerified && selectedRaw!=null &&
            row.raw.string("run_id")==target.run && row.raw.string("conversation_id")==target.conversation &&
            selectedRaw.string("run_id")==target.run && selectedRaw.string("conversation_id")==target.conversation &&
            selectedRaw.string("reply_id")==row.raw.string("reply_id")
    }
    /** Exactly the current request was acknowledged on some device. */
    fun attentionAcknowledged(row:Session):Boolean=
        row.raw.string("attention_signature").let { it.isNotBlank() && mark(row)?.string("attention_signature")==it }
    /**
     * C4 payload once this phone has shown the current reply: a fresh head fetched for this exact
     * reply and read through on this phone. Null when the computer already has everything.
     * Read all passes [acknowledge] false: like the desktop's Mark all read it marks replies only,
     * and pending approvals, questions and errors are acknowledged only by viewing the session.
     */
    fun payload(row:Session,freshHead:Boolean,localUnread:Int?,acknowledge:Boolean=true):JSONObject? {
        val t=row.target
        if(!freshHead || localUnread!=0 || t.archiveId.isNotBlank() || t.agentId.isNotBlank() || t.run.isBlank() || t.conversation.isBlank() ||
            row.raw.string("run_id")!=t.run || row.raw.string("conversation_id")!=t.conversation || SessionFilters.archived(row)) return null
        val reply=row.raw.string("reply_id").takeIf { it.isNotBlank() && !replyRead(row) }
        val attention=row.raw.string("attention_signature").takeIf { acknowledge && signature.matches(it) && !attentionAcknowledged(row) }
        if(reply==null && attention==null) return null
        return JSONObject().put("run_id",t.run).put("conversation_id",t.conversation)
            .put("reply_id",reply ?: JSONObject.NULL).put("attention_signature",attention ?: JSONObject.NULL)
    }
    /**
     * The verified open inspection's `current_read` when it names exactly [target]: schema 1, exactly the
     * native fields, and the target's and the inspection's own run and conversation. Null for archives,
     * subagents, cached frames, older computers or anything malformed; nothing is repaired.
     */
    fun current(raw:JSONObject?,target:Target,verified:Boolean):CurrentRead? {
        if(!verified || raw==null || target.archiveId.isNotBlank() || target.agentId.isNotBlank() ||
            target.run.isBlank() || target.conversation.isBlank() ||
            raw.opt("run_id")!=target.run || raw.opt("conversation_id")!=target.conversation) return null
        val value=raw.optJSONObject("current_read") ?: return null
        if(value.keys().asSequence().toSet()!=currentKeys || value.opt("schema")!=1 ||
            value.opt("run_id")!=target.run || value.opt("conversation_id")!=target.conversation) return null
        val complete=value.opt("attention_complete") as? Boolean ?: return null
        val reply=when(val v=value.opt("reply_id")) {
            JSONObject.NULL -> null
            is String -> v.takeIf { it.isNotEmpty() && it.length<=256 && it.none(Char::isISOControl) } ?: return null
            else -> return null
        }
        val attention=when(val v=value.opt("attention_signature")) {
            JSONObject.NULL -> null
            is String -> v.takeIf(signature::matches) ?: return null
            else -> return null
        }
        return CurrentRead(reply,attention,complete)
    }
    /**
     * What one settled viewport of the open conversation showed of its current inspection: the reply when it
     * shows the latest messages and the tail or the newest displayed row was visible; the attention when the
     * listed questions were visible or, without questions, when the reply was shown. Returns (reply, attention).
     */
    fun currentShown(latest:Boolean,questionsListed:Boolean,flags:ConversationViewport.CurrentFlags):Pair<Boolean,Boolean> {
        val reply=latest && (flags.tail || flags.newest)
        return reply to (latest && if(questionsListed) flags.questions else reply)
    }
    /**
     * The `mark_read` the open conversation may send for [current]: its reply once the conversation's tail
     * or newest row stayed visible, its attention once complete and its presentation stayed visible. Values
     * the computer already has are left out; null when nothing is new. This never moves the phone's private
     * read position, which only canonical history can advance.
     */
    fun currentPayload(row:Session?,target:Target,current:CurrentRead,replyShown:Boolean,attentionShown:Boolean):JSONObject? {
        if(row!=null && SessionFilters.archived(row)) return null
        val mark=row?.let { mark(it) }
        val reply=current.reply?.takeIf { replyShown && mark?.string("reply_id")!=it }
        val attention=current.attention?.takeIf { attentionShown && current.attentionComplete && mark?.string("attention_signature")!=it }
        if(reply==null && attention==null) return null
        return JSONObject().put("run_id",target.run).put("conversation_id",target.conversation)
            .put("reply_id",reply ?: JSONObject.NULL).put("attention_signature",attention ?: JSONObject.NULL)
    }
    /**
     * Whether a `mark_read` receipt shows the computer applied every field this phone sent. A
     * completed request can apply nothing: the value is no longer current, or the computer's
     * question index is not ready. That needs a newer snapshot, so the same mark waits.
     */
    fun applied(receipt:JSONObject,payload:JSONObject):Boolean {
        val result=receipt.optJSONObject("result")?.takeIf { receipt.string("state")=="completed" && it.opt("ok")==true } ?: return false
        val applied=result.optJSONObject("applied") ?: return false
        return listOf("reply_id","attention_signature").all { payload.isNull(it) || applied.opt(it)==true }
    }
    /** Delay before the same unapplied mark may be sent again: two minutes, doubling up to 16. */
    fun retryDelayMs(unapplied:Int):Long=120_000L shl unapplied.coerceIn(0,3)
    /**
     * Read all shares its marks one request at a time. Each request is prepared only when its turn
     * comes, so a target that no longer qualifies (or was shared meanwhile) is skipped.
     */
    suspend fun <T,R:Any> inOrder(targets:List<T>,prepare:(T)->R?,send:suspend (R)->Unit) {
        for(target in targets) prepare(target)?.let { send(it) }
    }
}
