package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject
import java.security.MessageDigest
import java.util.UUID

private fun exactString(value:JSONObject,key:String) = value.get(key) as? String ?: error("Invalid send receipt text")

/** Immutable bytes inside the encrypted outbox row, never preferences or logs. */
data class SendIntent(val canonical: String, val intentSha256: String) {
    override fun toString() = "SendIntent(retained)"
    fun json() = JSONObject().put("canonical", canonical).put("intent_sha256", intentSha256)
    fun value(): JSONObject? = runCatching {
        require(canonical.toByteArray(Charsets.UTF_8).size <= 256 * 1024)
        val value = JSONObject(canonical)
        SendReceiptRecovery.validateIntent(value)
        require(canonical == SendReceiptRecovery.encode(value) && intentSha256 == SendReceiptRecovery.hash(canonical))
        value
    }.getOrNull()
    companion object {
        fun decode(value: JSONObject) = SendIntent(exactString(value,"canonical"), exactString(value,"intent_sha256")).also {
            require(it.canonical.toByteArray(Charsets.UTF_8).size <= 256 * 1024)
        }
    }
}

data class SendReceiptProof(val messageRequestId: String, val intentSha256: String, val nativeDigest: String,
    val submittedAt: Double, val queued: Boolean, val nativeMessageId: String = "") {
    fun json() = JSONObject().put("message_request_id",messageRequestId).put("intent_sha256",intentSha256)
        .put("native_digest",nativeDigest).put("submitted_at",submittedAt).put("queued",queued).put("native_message_id",nativeMessageId)
    companion object {
        fun decode(value: JSONObject) = SendReceiptProof(exactString(value,"message_request_id"),exactString(value,"intent_sha256"),
            exactString(value,"native_digest"),value.getDouble("submitted_at"),value.getBoolean("queued"),value.string("native_message_id"))
    }
}

sealed interface SendReceiptCheck {
    data class Original(val receipt:JSONObject,val deadlineMs:Long):SendReceiptCheck
    data class Native(val intent:SendIntent,val proof:SendReceiptProof,val deadlineMs:Long):SendReceiptCheck
    data object Unconfirmed:SendReceiptCheck
}

/** Exact intent/native digests have different encodings and are never compared to each other. */
object SendReceiptRecovery {
    const val operation = "send_receipt"
    private val providers = setOf("claude","codex","kimi","dsh","opencode")
    private fun uuid(value:String) = runCatching { UUID.fromString(value).toString()==value && value!="00000000-0000-0000-0000-000000000000" }.getOrDefault(false)
    private fun keys(value:JSONObject) = value.keys().asSequence().toSet()
    private fun identity(value:String,max:Int,empty:Boolean=false) = (empty || value.isNotEmpty()) && value.toByteArray(Charsets.UTF_8).size<=max && value.none { it.isISOControl() }

    fun validateIntent(value:JSONObject) {
        require(keys(value)==setOf("schema","computer_id","machine_id","session_id","session","provider","native_encoding","payload"))
        require(value.opt("schema") is Int && value.getInt("schema")==1)
        require(identity(exactString(value,"computer_id"),36) && uuid(exactString(value,"machine_id")) && uuid(exactString(value,"session_id")))
        require(identity(exactString(value,"session"),512) && exactString(value,"provider") in providers && exactString(value,"native_encoding") in setOf("python_default","compact"))
        val p=value.getJSONObject("payload");val fields=keys(p)
        require(fields.containsAll(setOf("request_id","expected_run_id","expected_conversation_id","text")) &&
            fields.all { it in setOf("request_id","expected_run_id","expected_conversation_id","text","attachments","agent_id","expected_compaction_id") })
        require(uuid(exactString(p,"request_id")) && identity(exactString(p,"expected_run_id"),128) && identity(exactString(p,"expected_conversation_id"),256,true))
        val text=exactString(p,"text")
        require(text.toByteArray(Charsets.UTF_8).size<=65536 && text.isNotBlank() && text.none { it.isISOControl() && it!='\n' && it!='\t' } && !text.trimStart().startsWith('/') && !text.trimStart().startsWith('!'))
        if(p.has("attachments")) require(p.getJSONArray("attachments").length()==0)
        if(p.has("agent_id")) require(identity(exactString(p,"agent_id"),160) && exactString(p,"agent_id").none { it=='/' || it=='\\' } && exactString(p,"expected_conversation_id").isNotEmpty())
        if(p.has("expected_compaction_id")) require(exactString(p,"expected_compaction_id").let { it.isEmpty() || uuid(it) })
        require(!p.has("agent_id") || p.optString("expected_compaction_id").isEmpty())
    }

    /** Sorted ASCII keys; UTF-16 escapes preserve normalization and exact submitted bytes. */
    fun encode(value:Any?,ascii:Boolean=true,spaced:Boolean=false):String = when(value) {
        null,JSONObject.NULL -> "null"
        is JSONObject -> keys(value).sorted().joinToString(if(spaced) ", " else ",","{","}") { encode(it,ascii)+if(spaced) ": "+encode(value.get(it),ascii,spaced) else ":"+encode(value.get(it),ascii,spaced) }
        is JSONArray -> (0 until value.length()).joinToString(if(spaced) ", " else ",","[","]") { encode(value.get(it),ascii,spaced) }
        is String -> buildString { append('"');value.forEach { c -> append(when(c) {
            '"' -> "\\\""; '\\' -> "\\\\"; '\b' -> "\\b"; '\u000c' -> "\\f"; '\n' -> "\\n"; '\r' -> "\\r"; '\t' -> "\\t"
            else -> if(c<' ' || ascii && c>'~') "\\u"+c.code.toString(16).padStart(4,'0') else c.toString()
        }) };append('"') }
        is Int,is Long -> value.toString()
        is Boolean -> value.toString()
        else -> error("Unsupported send intent value")
    }
    fun hash(value:String) = MessageDigest.getInstance("SHA-256").digest(value.toByteArray(Charsets.UTF_8)).joinToString("") { (it.toInt() and 255).toString(16).padStart(2,'0') }
    fun nativeDigest(value:JSONObject):String {
        val p=value.getJSONObject("payload")
        return hash(if(exactString(value,"provider") in setOf("dsh","opencode")) encode(JSONArray().put(exactString(value,"session")).put(p),false)
            else encode(p,false,exactString(value,"native_encoding")=="python_default"))
    }
    fun encoding(version:Int,route:MachineRoute):String? = when {
        route==MachineRoute.Unknown -> null
        version==1 || route is MachineRoute.Via -> "compact"
        version==2 && route==MachineRoute.Direct -> "python_default"
        else -> null
    }
    fun machine(machines:List<Machine>,target:Target):Machine? = machines.singleOrNull {
        it.connectionId==target.connectionId && it.id==target.computerId && it.online && !it.lastKnown
    }
    fun freeze(target:Target,id:String,text:String,provider:String,machineId:String,sessionId:String,encoding:String,compactionId:String?=null):SendIntent? = runCatching {
        val p=target.json().put("request_id",id).put("text",text).put("attachments",JSONArray())
        compactionId?.let { p.put("expected_compaction_id",it) }
        val value=JSONObject().put("schema",1).put("computer_id",target.computerId).put("machine_id",machineId).put("session_id",sessionId)
            .put("session",target.session).put("provider",provider).put("native_encoding",encoding).put("payload",p)
        validateIntent(value);val canonical=encode(value);SendIntent(canonical,hash(canonical))
    }.getOrNull()
    fun matches(message:OutgoingMessage,intent:SendIntent):Boolean {
        val value=intent.value() ?: return false;val p=value.getJSONObject("payload");val target=message.target
        return message.attachments.isEmpty() && exactString(p,"request_id")==message.requestId && exactString(p,"text")==message.text &&
            exactString(value,"computer_id")==target.computerId && exactString(value,"session")==target.session && target.archiveId.isEmpty() &&
            exactString(p,"expected_run_id")==target.run && exactString(p,"expected_conversation_id")==(if(target.agentId.isEmpty()) target.conversation else target.parentConversation) &&
            p.string("agent_id")==target.agentId
    }
    fun request(id:String,intent:SendIntent) = JSONObject().put("schema",1).put("request_id",id)
        .put("intent",intent.value() ?: error("Original intent is unavailable")).put("intent_sha256",intent.intentSha256)
    fun proof(id:String,intent:SendIntent,result:JSONObject):SendReceiptProof? = runCatching {
        val value=intent.value() ?: return null;val p=value.getJSONObject("payload")
        require(result.opt("schema") is Int && result.getInt("schema")==1 && exactString(result,"request_id")==id && exactString(result,"message_request_id")==exactString(p,"request_id"))
        for((field,expected) in listOf("computer_id" to exactString(value,"computer_id"),"machine_id" to exactString(value,"machine_id"),"session_id" to exactString(value,"session_id"),
            "session" to exactString(value,"session"),"provider" to exactString(value,"provider"),"run_id" to exactString(p,"expected_run_id"),"conversation_id" to exactString(p,"expected_conversation_id"),"agent_id" to p.string("agent_id"))) require(exactString(result,field)==expected)
        require(exactString(result,"intent_sha256")==intent.intentSha256 && exactString(result,"native_digest")==nativeDigest(value))
        require(exactString(result,"status")=="submitted" && exactString(result,"proof")=="original_receipt" && result.opt("queued") is Boolean)
        require(result.opt("submitted_at") is Number);val at=result.getDouble("submitted_at");require(at.isFinite() && at>=0)
        val nativeId=if(result.has("native_message_id")) exactString(result,"native_message_id").also { require(validNativeId(it,exactString(value,"provider"))) }
            else "".also { require(exactString(value,"provider")!="opencode") }
        SendReceiptProof(exactString(p,"request_id"),intent.intentSha256,exactString(result,"native_digest"),at,result.getBoolean("queued"),nativeId)
    }.getOrNull()
    /** The ordinary acknowledgement prints the persisted original, with its producer digest. */
    fun acknowledgement(intent:SendIntent,result:JSONObject):SendReceiptProof? = runCatching {
        if(result.has("queued") && result.opt("queued") !is Boolean) return null
        val binding=result.getJSONObject("delivery_binding")
        require(binding.opt("schema") is Int && binding.getInt("schema")==1)
        val normalized=JSONObject().put("schema",1).put("request_id",result.get("request_id"))
            .put("message_request_id",result.get("request_id")).put("computer_id",intent.value()?.get("computer_id"))
            .put("machine_id",binding.get("machine_id")).put("session_id",binding.get("session_id")).put("provider",binding.get("provider"))
            .put("session",result.get("name")).put("run_id",result.get("run_id")).put("conversation_id",result.get("conversation_id"))
            .put("agent_id",result.string("agent_id")).put("intent_sha256",intent.intentSha256).put("native_digest",result.get("digest"))
            .put("status",result.get("status")).put("proof","original_receipt").put("submitted_at",result.get("submitted_at"))
            .put("queued",result.opt("queued") as? Boolean ?: false)
        if(result.has("native_message_id")) normalized.put("native_message_id",result.get("native_message_id"))
        proof(exactString(result,"request_id"),intent,normalized)
    }.getOrNull()
    fun settle(state:MessageState,captured:OutgoingMessage,intent:SendIntent,proof:SendReceiptProof,deadlineMs:Long,nowMs:Long):MessageState {
        if(nowMs>=deadlineMs || !proof.submittedAt.isFinite() || proof.submittedAt<0 || proof.nativeMessageId.isNotEmpty() && !validNativeId(proof.nativeMessageId,intent.value()?.string("provider").orEmpty())) return state
        val index=state.outgoing.indices.singleOrNull { state.outgoing[it].requestId==captured.requestId } ?: return state
        val current=state.outgoing[index]
        if(current.status!="uncertain" || current.target!=captured.target || current.text!=captured.text || current.attachments!=captured.attachments || current.createdAt!=captured.createdAt || current.originalSendIntent()!=captured.originalSendIntent() || current.hasOriginalSendIdentity && current.originalSendIntent()!=intent ||
            !matches(current,intent) || proof.messageRequestId!=current.requestId || proof.intentSha256!=intent.intentSha256 || proof.nativeDigest!=nativeDigest(intent.value() ?: return state)) return state
        return state.copy(outgoing=state.outgoing.mapIndexed { at,row -> if(at!=index) row else row.copy(status="submitted",submittedAt=proof.submittedAt,submittedText=row.text,error="",sendIntent=intent,sendReceiptProof=proof) })
    }
    private fun validNativeId(id:String,provider:String) = provider=="opencode" && id.startsWith("msg_") && id.length in 5..128 && id.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it=='_' || it=='-' }
    fun exactNativeId(message:OutgoingMessage):String {
        val intent=message.originalSendIntent() ?: return "";val proof=message.sendReceiptProof ?: return ""
        val value=intent.value() ?: return ""
        return proof.nativeMessageId.takeIf { matches(message,intent) && proof.messageRequestId==message.requestId && proof.intentSha256==intent.intentSha256 &&
            proof.nativeDigest==nativeDigest(value) && proof.submittedAt.isFinite() && proof.submittedAt>=0 && validNativeId(it,value.string("provider")) }.orEmpty()
    }
    fun nowMs()=System.nanoTime()/1_000_000L
    fun supported(relay:Set<String>?,native:Set<String>?) = relay?.contains(operation)==true && native?.contains(operation)==true
}
