package app.zerus.mobile

import org.json.JSONObject

/** The child's switch as its computer reports it: whether its blocking questions go to its parent session. */
data class QuestionRouting(val sendToParent: Boolean, val revision: Long)

/**
 * The person's per-child switch of the owner-approved question route (zerus-sa9qn.33), kept on the child's
 * computer beside its notification preference so every desktop and phone agrees. The computer refuses a change
 * from an agent's own process tree; this phone's request through the connector is the person's.
 * Shared contract: `question-routing-contract.json`.
 */
object SessionQuestionRouting {
    const val operation = "session_question_routing"
    const val unavailable = "Question routing is unavailable. Update and refresh Zerus on this computer."
    const val notChild = "Question routing belongs to a live Claude child session."
    const val hint = "Applies to this child session on all devices."
    private fun integer(value: Any?): Long? = when(value) { is Int -> value.toLong();is Long -> value;else -> null }?.takeIf { it >= 0 }

    /** Exactly `send_to_parent` and its `revision`; anything else is unknown, never a default. */
    fun parse(raw: JSONObject?): QuestionRouting? {
        val value = raw?.optJSONObject("question_routing") ?: return null
        if(value.length() != 2 || value.opt("send_to_parent") !is Boolean) return null
        val revision = integer(value.opt("revision")) ?: return null
        return QuestionRouting(value.getBoolean("send_to_parent"),revision)
    }
    private fun agent(target: Target, raw: JSONObject) =
        (raw.opt("cmd") as? String).orEmpty().ifBlank { (raw.opt("agent") as? String).orEmpty() }.ifBlank { target.session.substringBefore('/') }
    /** A live Claude session with a parent link: only those route their questions in this version. */
    fun applies(target: Target, raw: JSONObject?): Boolean = raw != null && target.agentId.isBlank() && target.archiveId.isBlank() &&
        raw.optJSONObject("parent") != null && raw.opt("state") != "archived" && agent(target,raw) == "claude"
    fun parentName(raw: JSONObject?): String =
        (raw?.optJSONObject("parent")?.opt("name") as? String)?.substringAfterLast('/')?.takeIf { it.isNotBlank() } ?: "parent"
    /** What choosing it does: turn sending off, turn it on, or (unknown) nothing yet. */
    fun label(raw: JSONObject?): String = when(parse(raw)?.sendToParent) {
        true -> "Stop sending questions to ${parentName(raw)}"
        false -> "Send questions to ${parentName(raw)}"
        null -> "Questions to ${parentName(raw)}"
    }
    fun reason(target: Target, raw: JSONObject?): String = when {
        !applies(target,raw) -> notChild
        target.run.isBlank() || target.conversation.isBlank() || parse(raw) == null -> unavailable
        else -> ""
    }
    /** The confirmed change turns the current value the other way at its revision. */
    fun toggle(routing: QuestionRouting?): JSONObject =
        JSONObject().put("send_to_parent",routing?.sendToParent != true).put("expected_revision",routing?.revision ?: -1)
    fun arguments(raw: JSONObject): JSONObject {
        require(raw.length()==2 && raw.opt("send_to_parent") is Boolean && integer(raw.opt("expected_revision")) != null) { "Invalid question routing setting." }
        return JSONObject(raw.toString())
    }
    fun payload(target: Target, args: JSONObject): JSONObject = JSONObject().put("expected_run_id",target.run)
        .put("expected_conversation_id",target.conversation).put("expected_revision",args.get("expected_revision")).put("send_to_parent",args.get("send_to_parent"))
    val refusals = mapOf(
        "conflict" to "Where this child's questions go changed on another device. Refresh and try again.",
        "stale_identity" to "The conversation changed. Refresh its exact identity before applying.",
        "not_found" to "This conversation is no longer available.",
        "unsupported" to "This session has no durable identity, so its questions cannot be routed.",
        "agent_refused" to "Only you can change where a child's questions go.",
        "capacity" to "The computer's question routing storage is full. The setting was not changed.",
        "revision_exhausted" to "The question routing revision is exhausted. The setting was not changed.")
    const val uncertain = "Where this child's questions go was not confirmed. Check its original receipt; it has not been retried."

    /** The person's latch through this phone's signed statement (zerus-sa9qn.50). */
    const val personOperation = "session_question_route_person"
    /** Only an encrypted workspace connection can sign, and only where both the relay and the computer offer it. */
    fun person(connection: Connection?, relay: Set<String>, machine: Set<String>) =
        connection?.protocolVersion == 2 && personOperation in relay && personOperation in machine
    /** The toggled switch at the row's revision, for the child's durable session ID. */
    fun personArguments(raw: JSONObject): JSONObject {
        require(raw.length()==3 && (raw.opt("session_id") as? String)?.let { runCatching { WorkspaceProtocol.identifier(it) }.isSuccess } == true) { "Invalid question routing setting." }
        arguments(JSONObject(raw.toString()).apply { remove("session_id") })
        return JSONObject(raw.toString())
    }
    val personRefusals = mapOf(
        "not_verified" to "The computer did not accept this phone's signature. Refresh this workspace and try again.",
        "stale_identity" to "The conversation changed. Refresh its exact identity before applying.",
        "not_found" to "This conversation is no longer available.",
        "capacity" to "The computer's question routing storage is full. The setting was not changed.",
        "invalid_request" to "The computer refused this request. Refresh and try again.")
    /** Only the service's exact receipt of this statement settles it; a closed refusal fails; anything else is uncertain. */
    fun personResult(action: SessionAction, receipt: JSONObject): SessionAction {
        val value = receipt.optJSONObject("result")
        val args = runCatching { personArguments(JSONObject(action.arguments)) }.getOrNull()
        val completed = receipt.opt("request_id")==action.requestId && receipt.opt("state")=="completed" && args != null && value != null &&
            value.opt("request_id")==action.requestId && value.opt("op")=="question_route_child"
        if(completed && value!!.opt("accepted")==false && value.length()==5 && value.opt("durable")==false)
            personRefusals[value.opt("code") as? String]?.let { return action.copy(status="failed",error=it) }
        val valid = completed && value!!.length()==10 && value.opt("accepted")==true && value.opt("durable")==true &&
            value.opt("session_id")==args!!.getString("session_id") && value.opt("person_off")==!args.getBoolean("send_to_parent") &&
            (value.opt("revision") as? Number)?.toLong()?.let { it >= 0 } == true && value.opt("applied") is Boolean &&
            value.opt("host") is Boolean && value.opt("code")==""
        return if(valid) action.copy(status="completed",error="") else action.copy(status="uncertain",error=uncertain)
    }
    /** Only the computer's exact reply settles it: a closed refusal fails, the exact confirmation completes, anything else is uncertain. */
    fun result(action: SessionAction, receipt: JSONObject): SessionAction {
        val value = receipt.optJSONObject("result")
        val completed = receipt.opt("request_id")==action.requestId && receipt.opt("state")=="completed"
        if(completed && value?.opt("ok")==false && !value.has("applied"))
            refusals[value.opt("error") as? String]?.let { return action.copy(status="failed",error=it) }
        val args = runCatching { arguments(JSONObject(action.arguments)) }.getOrNull()
        val routing = value?.let { parse(JSONObject().put("question_routing",it.opt("question_routing") ?: JSONObject.NULL)) }
        val applied = value?.opt("applied") as? Boolean
        val revision = args?.let { integer(it.opt("expected_revision")) }
        val expected = if(applied == true && revision != null && revision < Long.MAX_VALUE) revision+1 else revision
        val valid = completed && value?.opt("ok")==true && value.length()==5 && value.opt("run_id")==action.target.run &&
            value.opt("conversation_id")==action.target.conversation && applied != null && args != null && routing != null &&
            routing.sendToParent==args.getBoolean("send_to_parent") && routing.revision==expected && !(applied && revision==Long.MAX_VALUE)
        return if(valid) action.copy(status="completed",error="") else action.copy(status="uncertain",error=uncertain)
    }
}
