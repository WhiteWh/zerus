package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject
import java.net.URI
import java.util.UUID

data class Connection(val id: String, val label: String, val url: String, val token: String, val protocolVersion: Int = 1) {
    init { require(protocolVersion in setOf(1,2)) { "Unsupported workspace transport." } }
    override fun toString() = "Connection(id=$id, protocolVersion=$protocolVersion)"
    val endpoint: String get() = ManagedRelay.transport(url)
    val displayName: String get() = if (label.isBlank() || runCatching { UUID.fromString(label) }.isSuccess ||
        endpoint != url && label.equals(runCatching { URI(url).host }.getOrNull(),ignoreCase=true))
        runCatching { URI(endpoint).host }.getOrNull().orEmpty().ifBlank { "Workspace" } else label
}
data class Machine(val connectionId: String, val id: String, val name: String, val online: Boolean,
    val nativeName: String = name, val serverAliases: Set<String> = emptySet(),
    val route: MachineRoute = MachineRoute.Unknown, val lastKnown: Boolean = false,
    val hgsVersion: String = "", val mobileDelivery: String = "", val lastSeenAt: Double? = null,
    val machineId: String = "", val nativeHost: String = "")
data class Target(val computerId: String, val session: String, val run: String, val conversation: String, val connectionId: String = "demo", val archiveId: String = "", val agentId: String = "", val parentConversation: String = "") {
    val key: String get() = JSONArray(listOf(connectionId, computerId, session, run, conversation) + (if (archiveId.isBlank()) emptyList() else listOf(archiveId)) + (if(agentId.isBlank()) emptyList() else listOf("agent",agentId,parentConversation))).toString()
    fun json(): JSONObject {
        require(archiveId.isBlank()) { "Archived conversations are read-only." }
        return JSONObject().put("expected_run_id", run).put("expected_conversation_id", if(agentId.isBlank()) conversation else parentConversation).also { if(agentId.isNotBlank()) it.put("agent_id",agentId) }
    }
}
data class Draft(val target: Target, val text: String, val status: String = "editing",
    val requestId: String = "", val questionId: String = "", val questionHash: String = "",
    val answers: String = "", val updatedAt: Long = System.currentTimeMillis(), val attachments: List<Attachment> = emptyList(),
    val revision: String = UUID.randomUUID().toString(), val detachedId: String = "", val generation: String = UUID.randomUUID().toString()) {
    val key: String get() = target.key + ":" + questionId + ":" + questionHash + if (detachedId.isBlank()) "" else ":$detachedId"
    fun begin() = copy(status = "submitting", requestId = UUID.randomUUID().toString())
    fun recover() = if (status == "submitting") copy(status = "uncertain") else this
}
data class Session(val target: Target, val title: String, val agent: String, val status: String,
    val project: String, val preview: String, val pendingCount: Int, val raw: JSONObject, val projectKey: ProjectKey? = null)
data class Event(val id: String, val role: String, val text: String, val time: String, val at: Double = 0.0,
    val requestId: String = "", val attachments: List<DisplayedAttachment> = emptyList(), val source: String = "",
    val delivery: String = "", val outgoingId: String = "", val nativeType: String = "", val submittedText: String = "",
    val originalId: String = "", val historyId: String = "", val historyCursor: String = "",
    val historyEpoch: String = "", val incomingSeq: Long? = null, val originalIds: List<String> = emptyList(), val detailTruncated: Boolean = false,
    val voice: VoiceMessage? = null, val origin: String = "", val sender: String = "", val report: String = "",
    /** A fork's read-only row from the session it began from (that session's name); never incoming. */
    val inheritedFrom: String = "", val nativeMessageId: String = "") {
    /** The request this row stands for as a receipt or a local send. An OpenCode prompt only
     * names the request that sent it, which ConversationMerge alone matches. */
    val receiptRequestId: String get() = if (role != "You" || source == "opencode_native") "" else requestId
}
/** Display metadata plus the identities that can open the exact bytes: a native
 * receipt position (`messageId`, `index`) and/or this phone's private copy. */
data class DisplayedAttachment(val name: String, val mime: String, val bytes: Long, val reference: String = "",
    val messageId: String = "", val index: Int = -1, val localId: String = "", val sha256: String = "")
/** [scope]: an approval option's reach, "once" for this request or "broader" for more; empty elsewhere. */
data class Choice(val id: String, val label: String, val description: String, val scope: String = "")
/** One question item. The additive OpenCode form fields are optional bounds and defaults. */
data class Prompt(val id: String, val text: String, val choices: List<Choice>, val multi: Boolean,
    val required: Boolean, val other: Boolean, val header: String = "", val body: String = "",
    val otherLabel: String = "", val otherDescription: String = "", val inputKind: String = "",
    val minLength: Int? = null, val maxLength: Int? = null, val minItems: Int? = null, val maxItems: Int? = null,
    val defaults: List<String> = emptyList(), val defaultText: String = "")
data class Question(val id: String, val hash: String, val prompts: List<Prompt>, val canAnswer: Boolean,
    val canSkip: Boolean, val optional: Boolean, val delivery: String, val createdAt: Double = 0.0,
    val source: String = "", val approval: Boolean = false, val trustRequest: Boolean = false,
    val unavailableReason: String = "", val run: String = "", val conversation: String = "",
    val toolCallId: String = "", val answerDelivery: NativeQuestionDelivery? = null, val approvalChoices: Boolean = false)
data class NativeQuestionAnswer(val questionId: String, val optionIds: List<String>, val text: String, val skip: Boolean = false)
data class NativeQuestionDelivery(val requestId: String, val questionId: String, val questionHash: String,
    val run: String, val conversation: String, val status: String, val answers: List<NativeQuestionAnswer>)

fun JSONArray.objects(): List<JSONObject> = (0 until length()).mapNotNull { optJSONObject(it) }
fun JSONObject.string(vararg keys: String): String = keys.firstNotNullOfOrNull { key ->
    optString(key).takeIf { it.isNotEmpty() && it != "null" }
} ?: ""

object EndpointPolicy {
    fun normalize(value: String): String {
        val uri = URI(value.trim())
        require(uri.scheme == "https") { "Use an HTTPS URL for your computer or gateway." }
        require(!uri.host.isNullOrBlank() && uri.userInfo == null && uri.query == null && uri.fragment == null) {
            "Enter a server URL without credentials, query parameters, or fragments."
        }
        return uri.toASCIIString().trimEnd('/')
    }
}

object Demo {
    val computer = Connection("demo", "Design workstation", "https://workstation.example", "")
    val sessions: List<Session> = listOf(
        Triple("API migration", "Needs your input", "Should I keep the previous endpoint available during migration?"),
        Triple("Dashboard polish", "Working", "Checking spacing and keyboard navigation across the main views."),
        Triple("Release notes", "Idle", "The changelog is ready for review.")
    ).mapIndexed { index, (title, status, preview) -> Session(Target("demo", "example-$index", "demo-run", "demo-conversation"),
        title, listOf("Codex", "Claude Code", "Kimi Code")[index], status, "Zerus", preview,
        if (index == 0) 1 else 0, JSONObject().put("state", "running").put("process_state", "running").put("tracked", true)
            .put("tag", title).put("activity", if (index < 2) "busy" else "idle").put("phase", if (index == 0) "input" else if (index == 1) "working" else "idle")
            .put("git_metadata_state", "ok").put("git_branch", listOf("api-migration", "ui-polish", "main")[index])
            .also { raw -> if (index == 1) raw.put("git_worktree", true).put("git_worktree_name", "zerus-ui-polish")
                .put("subagent_source", "hooks").put("subagent_count", 1).put("subagent_total_count", 1).put("subagent_counts_complete", true)
                .put("subagents", JSONObject().put("reviewer", JSONObject().put("label", "Accessibility review").put("state", "working")
                    .put("task", "Checking focus order and contrast in the session list."))) }) }
    val events = listOf(Event("1", "You", "Review the API migration and call out any compatibility risks.", "10:42"),
        Event("2", "Codex", "I found two clients using the previous endpoint. I can keep a compatibility route while the new API rolls out.", "10:43"),
        Event("3", "Codex", "Should I keep the previous endpoint available during migration?", "10:44"))
}
