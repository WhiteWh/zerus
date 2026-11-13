package app.zerus.mobile

import org.json.JSONObject
import java.util.UUID

/** One immutable native action attempt. Uncertain attempts are checked, never replayed. */
data class SessionAction(val requestId: String, val target: Target, val operation: String, val arguments: String,
    val status: String = "sending", val createdAt: Long = System.currentTimeMillis(), val error: String = "",
    val resultTarget: Target? = null, val pendingId: String = "", val applyAttemptId:String = "", val resultPath:String = "",
    val confirmedNotificationPreference: String = "") {
    val blocksSending get() = status in listOf("sending", "uncertain")
    val needsProjectReview get() = operation=="launch" && status=="completed" && error.isNotBlank() && resultTarget!=null
    fun recover() = if (status == "sending") copy(status = "uncertain", error = "The action was interrupted. Check its original receipt; it has not been retried.") else this
}

object SessionActionPolicies {
    val lifecycle = setOf("pause", "resume", "archive", "rename", "fork", "terminate", "restore", "forget")
    val operations = lifecycle + setOf("send_now", "settings", "process_stop", "terminal_input", "launch", "worktree_create", "directory_create", RepositoryAcquisition.clone, "recovery_action", SessionNotificationPreferences.operation, SessionQuestionRouting.operation, SessionQuestionRouting.personOperation, SessionProjectProtocol.assign)
    val terminalKeys=setOf("Enter","Escape","Tab","BTab","BSpace","Up","Down","Left","Right","Home","End","PPage","NPage","DC","C-c","C-d","C-l","C-a","C-e","C-u","C-w")
    /** Effort names every relay accepts in `settings`. */
    val relayEfforts = setOf("", "off", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra", "on")
    /** Other native names (OpenCode variants) need `opencode_launch_effort` on both the relay and the connector. */
    fun effortReason(operation: String, args: JSONObject, nativeNames: Boolean): String =
        if (operation == "settings" && !nativeNames && args.has("effort") && args.optString("effort") !in relayEfforts)
            "Choose an effort offered for this session." else ""
    private fun uuid(value: String) = runCatching { UUID.fromString(value).toString() == value }.getOrDefault(false)
    /**
     * Model-specific native effort names belong only to an OpenCode session: an `opencode/` name whose current
     * inspection reports agent `opencode`, on a relay and computer that both advertise `opencode_launch_effort`.
     * Every other provider keeps [relayEfforts].
     */
    fun nativeEfforts(target: Target, inspected: JSONObject?, advertised: Boolean) =
        advertised && target.session.startsWith("opencode/") && inspected?.string("agent") == "opencode"
    fun arguments(operation: String, raw: JSONObject, nativeEfforts: Boolean = false): JSONObject {
        if(operation==RepositoryAcquisition.clone) {
            val checked=RepositoryAcquisition.arguments(raw.getString("expected_machine_id"),raw.getString("url"),RepositoryAcquisition.destination(raw.getString("parent"),raw.getString("name")))
            require(raw.keys().asSequence().toSet()==checked.keys().asSequence().toSet())
            return checked
        }
        if(operation == SessionNotificationPreferences.operation) return SessionNotificationPreferences.arguments(raw)
        if(operation == SessionQuestionRouting.operation) return SessionQuestionRouting.arguments(raw)
        if(operation == SessionQuestionRouting.personOperation) return SessionQuestionRouting.personArguments(raw)
        require(operation in operations) { "Unsupported session action." }
        if(operation==SessionProjectProtocol.assign) return SessionProjectProtocol.arguments(raw)
        val allowed = when(operation) { "rename" -> setOf("new_name"); "fork" -> setOf("tag"); "send_now" -> setOf("queue_id")
            "launch" -> setOf("agent","directory","tag","account_id","model","effort","swarm_id","project_id","project_folder_id","add_folder","worktree_folder_id","worktree_common_dir")
            "worktree_create" -> setOf("path","common_dir","destination","branch","base")
            "directory_create" -> setOf("path")
            "recovery_action" -> setOf("job_id","action")
            "terminal_input" -> setOf("terminal_binding_id", "text", "enter", "key")
            "settings" -> setOf("model", "effort", "expected_pending_id"); "process_stop" -> setOf("process_id", "generation"); else -> emptySet() }
        require(raw.keys().asSequence().all { it in allowed }) { "Unexpected action argument." }
        val args = JSONObject(raw.toString())
        when(operation) {
            "directory_create" -> require(args.opt("path") is String && DirectoryCreation.pathError(args.getString("path")).isBlank()) { "Enter a valid absolute path for a new folder." }
            "rename", "fork" -> {
                val field = if(operation == "rename") "new_name" else "tag"
                val name = args.getString(field)
                require(name.isNotBlank() && name.toByteArray(Charsets.UTF_8).size <= (if(operation == "rename") 512 else 120) && name.none(Char::isISOControl) && !name.contains('\\') && (operation == "rename" || !name.contains('/'))) { "Enter a valid session name." }
            }
            "recovery_action" -> { require(uuid(args.getString("job_id")));require(args.getString("action") in setOf("now","cancel")) }
            "send_now" -> require(args.getString("queue_id").matches(Regex("[0-9a-f]{64}"))) { "The queued message identity changed." }
            "settings" -> {
                require(args.has("model") || args.has("effort")) { "Choose a model or effort." }
                if(args.has("model")) require(args.getString("model").let { it.isNotBlank() && it.length <= 120 && it.none(Char::isISOControl) }) { "Invalid model." }
                // The fixed names; OpenCode variants only when [nativeEfforts]. inspectReason checks the model's own options.
                if(args.has("effort")) require(args.getString("effort").let { it in relayEfforts || nativeEfforts && LaunchPresentation.validEffort(it) }) { "Invalid effort." }
                if(args.has("expected_pending_id")) require(uuid(args.getString("expected_pending_id"))) { "The pending settings identity changed." }
            }
            "worktree_create" -> {
                listOf("path","common_dir","destination").forEach { require(LaunchPresentation.absoluteFolder(args.getString(it))) }
                require(WorktreePresentation.createError(args.getString("branch"),args.getString("base"),args.getString("destination")).isBlank())
            }
            "launch" -> {
                require(args.getString("agent") in setOf("codex","claude","kimi","dsh","opencode"))
                if(args.has("model")) require(args.getString("agent")=="opencode" && LaunchPresentation.validModel(args.getString("model"))) { "Choose a valid native OpenCode model." }
                if(args.has("effort")) require(args.getString("agent")=="opencode" && args.has("model") && LaunchPresentation.validEffort(args.getString("effort"))) { "Choose an offered OpenCode effort." }
                require(args.getString("directory").let { it.startsWith('/') && it.length <= 4096 && it.none(Char::isISOControl) })
                require(args.getString("tag").let { it.isNotBlank() && it.trim() == it && it.toByteArray(Charsets.UTF_8).size <= 120 && it.none { c -> c == '/' || c == '\\' || c == ':' || c == '.' || c.isISOControl() } })
                if(args.has("account_id")) require(args.getString("account_id").let { it.isNotBlank() && it.length <= 256 && it.none(Char::isISOControl) })
                if(listOf("swarm_id","project_id","project_folder_id","add_folder","worktree_folder_id","worktree_common_dir").any(args::has)) {
                    require(args.getString("swarm_id").isNotBlank() && args.getString("project_id").isNotBlank())
                    require(args.get("add_folder") is Boolean)
                    if(args.has("project_folder_id")) require(args.getString("project_folder_id").isNotBlank() && args.get("add_folder")==false)
                }
            }
            "terminal_input" -> {
                require(args.getString("terminal_binding_id").isNotBlank())
                require(args.has("text") != args.has("key"))
                if(args.has("text")) { require(args.get("text") is String && args.getString("text").toByteArray(Charsets.UTF_8).size <= 65536); require(args.get("enter") is Boolean) }
                else { require(!args.has("enter"));require(args.getString("key") in terminalKeys) }
            }
            "process_stop" -> {
                require(args.getString("process_id").let { it.isNotBlank() && it.length <= 256 && it.none(Char::isISOControl) }) { "Invalid process identity." }
                if(args.has("generation")) require(args.getString("generation").let { it.isNotBlank() && it.length <= 256 && it.none(Char::isISOControl) }) { "Invalid process generation." }
            }
        }
        return args
    }
    fun evidence(operation: String, raw: JSONObject, args: JSONObject = JSONObject()): String {
        if(operation == "process_stop") {
            val process=raw.optJSONObject("processes")?.optJSONArray("items")?.objects().orEmpty().find { it.string("id") == args.string("process_id") }
            return JSONObject().put("generation",raw.optJSONObject("processes")?.opt("generation") ?: raw.opt("generation") ?: JSONObject.NULL).also { value ->
                listOf("id","run_id","conversation_id","status","capabilities").forEach { value.put(it,process?.opt(it) ?: JSONObject.NULL) }
            }.toString()
        }
        val fields=when(operation) {
            SessionNotificationPreferences.operation -> listOf("run_id","conversation_id","archive_id","notification_preferences")
            SessionQuestionRouting.operation -> listOf("run_id","conversation_id","archive_id","question_routing","parent","state")
            SessionQuestionRouting.personOperation -> listOf("run_id","conversation_id","archive_id","question_routing","parent","state","session_id")
            "settings" -> listOf("model", "effort", "model_options", "effort_options", "pending_model", "pending_effort", "pending_settings_id", "settings_apply_when", "settings_change_supported")
            "send_now" -> listOf("input_queue")
            "recovery_action" -> listOf("recovery")
            "process_stop" -> listOf("generation", "processes")
            else -> listOf("state", "runtime_state", "process_state", "run_id", "conversation_id", "archive_id")
        }
        return JSONObject().also { value -> fields.forEach { field -> value.put(field,raw.opt(field) ?: JSONObject.NULL) } }.toString()
    }
    fun inspectReason(target: Target, operation: String, args: JSONObject, raw: JSONObject): String {
        if(operation == SessionQuestionRouting.operation || operation == SessionQuestionRouting.personOperation) return SessionQuestionRouting.reason(target,raw).ifBlank {
            if(SessionQuestionRouting.parse(raw)?.revision != args.getLong("expected_revision")) "Where this child's questions go changed. Refresh before applying."
            else if(operation == SessionQuestionRouting.personOperation && raw.opt("session_id") != args.opt("session_id")) "The conversation changed. Refresh its exact identity before applying."
            else ""
        }
        if(operation == SessionNotificationPreferences.operation) return SessionNotificationPreferences.reason(target,raw).ifBlank {
            if(SessionNotificationPreferences.parse(raw,target)?.revision != args.getLong("expected_revision")) "Shared notification settings changed. Refresh before applying." else ""
        }
        if(operation in lifecycle && raw.optJSONArray("allowed_actions")?.let { values -> (0 until values.length()).any { values.optString(it) == operation } } != true)
            return raw.optJSONObject("action_reasons")?.string(operation).orEmpty().ifBlank { "The native session no longer allows this action." }
        when(operation) {
            "rename" -> {
                val parts=args.string("new_name").split('/')
                val old=target.session.split('/')
                if(parts.size != 3 || old.size !in 2..3 || parts.take(2) != old.take(2) || parts.any { it.isBlank() || it.trim() != it || it.any { c -> c == ':' || c == '.' || c.isISOControl() } })
                    return "Keep the original provider and project; choose a valid session tag."
            }
            "recovery_action" -> {
                val recovery=raw.optJSONObject("recovery") ?: return "No native recovery job is present."
                val identity=recovery.optJSONArray("identity")
                if(recovery.string("id")!=args.string("job_id") || recovery.string("state")!="waiting" || identity==null || identity.length()<2 || identity.optString(0)!=target.run || identity.optString(1)!=target.conversation)
                    return "The native recovery job changed or is no longer waiting."
            }
            "send_now" -> {
                val queue=raw.optJSONObject("input_queue")
                if(queue?.string("id") != args.string("queue_id") || queue.optBoolean("can_send_now") != true) return "The queued message changed or cannot be sent now."
            }
            "settings" -> {
                if(!raw.optBoolean("settings_change_supported")) return raw.string("settings_change_reason").ifBlank { "Native settings are unavailable." }
                if(args.has("expected_pending_id") && raw.string("pending_settings_id") != args.string("expected_pending_id")) return "The pending settings changed."
                val model=if(args.has("model")) args.string("model") else raw.string("model")
                val choice=raw.optJSONArray("model_options")?.objects().orEmpty().find { it.string("id") == model }
                if(args.has("model") && choice == null) return "The selected model is no longer in the native catalog."
                if(args.has("effort") && args.string("effort").isNotBlank()) {
                    val options=choice?.optJSONArray("effort_options") ?: raw.optJSONArray("effort_options")
                    if(options == null || (0 until options.length()).none { options.optString(it) == args.string("effort") }) return "The selected effort is unavailable for this model."
                }
            }
            "process_stop" -> {
                val process=raw.optJSONObject("processes")?.optJSONArray("items")?.objects().orEmpty().find { it.string("id") == args.string("process_id") }
                    ?: return "This process is no longer present."
                if(process.string("run_id") != target.run || process.string("conversation_id") != target.conversation || process.optJSONObject("capabilities")?.optBoolean("stop") != true)
                    return "This process cannot be stopped under the selected conversation."
                if(args.has("generation") && args.string("generation") != raw.optJSONObject("processes")?.string("generation").orEmpty().ifBlank { raw.string("generation") }) return "The process generation changed."
            }
        }
        return ""
    }
    fun result(action: SessionAction, receipt: JSONObject): SessionAction {
        if(receipt.string("request_id") != action.requestId) return action.copy(status="uncertain",error="The receipt belongs to another request.")
        if(action.operation==RepositoryAcquisition.clone) return RepositoryAcquisition.result(action,receipt)
        if(action.operation==SessionProjectProtocol.assign) return SessionProjectProtocol.result(action,receipt)
        if(receipt.string("state") == "failed") return action.copy(status="failed",error=receipt.string("error").ifBlank { "The computer rejected this action." })
        if(action.operation == SessionNotificationPreferences.operation) return SessionNotificationPreferences.result(action,receipt)
        if(action.operation == SessionQuestionRouting.operation) return SessionQuestionRouting.result(action,receipt)
        if(action.operation == SessionQuestionRouting.personOperation) return SessionQuestionRouting.personResult(action,receipt)
        if(action.operation == MobileDelivery.OPERATION) return MobileDelivery.result(action,receipt)
        if(action.operation == DirectoryCreation.operation) return DirectoryCreation.result(action,receipt)
        val native = receipt.optJSONObject("result")
        if(action.operation == "worktree_create") {
            val args=JSONObject(action.arguments)
            if(receipt.string("state") != "completed" || native == null || native.string("request_id") != action.requestId ||
                native.string("status") != "created" || native.string("common_dir") != args.string("common_dir") ||
                native.string("branch") != args.string("branch") || !LaunchPresentation.absoluteFolder(native.string("path")))
                return action.copy(status="uncertain",error="Worktree creation is unconfirmed. Check the original receipt; it has not been retried.")
            return action.copy(status="completed",error="",resultPath=native.string("path"))
        }
        if(action.operation == "launch") {
            val created=native?.optJSONObject("result_target")
            if(receipt.string("state") != "completed" || native == null || native.string("request_id") != action.requestId || native.string("status") != "created" ||
                created == null || created.string("name").isBlank() || created.string("run_id").isBlank() || created.string("archive_id").isNotBlank()) return action.copy(status="uncertain",error="The new session was not confirmed. Check this original launch receipt; it has not been retried.")
            val arguments=JSONObject(action.arguments)
            val assignment=native.optJSONObject("project_assignment")
            val projectConfirmed=!arguments.has("project_id") || assignment!=null && assignment.string("status")=="assigned" &&
                assignment.string("swarm_id")==arguments.string("swarm_id") && assignment.string("project_id")==arguments.string("project_id")
            return action.copy(status="completed",error=if(projectConfirmed) "" else "Session created; project assignment was not confirmed. Review Projects. This launch will not be repeated.",
                resultTarget=action.target.copy(session=created.string("name"),run=created.string("run_id"),conversation=created.string("conversation_id")))
        }
        if(receipt.string("state") != "completed" || native == null || native.string("request_id") != action.requestId ||
            native.string("name") != action.target.session || native.string("run_id") != action.target.run || native.string("conversation_id") != action.target.conversation ||
            native.string("archive_id") != action.target.archiveId)
            return action.copy(status="uncertain",error="The native result did not confirm this exact request.")
        val args = JSONObject(action.arguments)
        val accepted = when(action.operation) {
            "terminal_input" -> native.string("status") == "submitted" && native.string("terminal_binding_id") == args.string("terminal_binding_id")
            "recovery_action" -> native.string("status") == (if(args.string("action")=="now") "scheduled" else "cancelled") && native.string("job_id")==args.string("job_id") && native.string("action")==args.string("action")
            "send_now" -> native.string("status") == "submitted" && native.string("queue_id") == args.string("queue_id")
            "settings" -> native.string("status") in listOf("applied", "scheduled") && (native.string("status") != "scheduled" || uuid(native.string("pending_settings_id"))) && listOf("model", "effort").all { !args.has(it) || native.has(it) && native.get(it) == args.get(it) }
            "process_stop" -> native.string("status") == "requested" && native.string("id") == args.string("process_id")
            else -> native.string("status") == "completed"
        }
        if(!accepted) return action.copy(status="uncertain",error="The native outcome was not confirmed. Nothing has been retried.")
        val result = native.optJSONObject("result_target")?.let { value ->
            val name = value.string("name"); val run = value.string("run_id"); val conversation = value.string("conversation_id")
            if(name.isBlank() || run.isBlank()) null else action.target.copy(session=name,run=run,conversation=conversation,archiveId=value.string("archive_id"))
        }
        return action.copy(status=if(action.operation == "settings") native.string("status") else "completed",error="",resultTarget=result,
            pendingId=native.string("pending_settings_id"))
    }
}
