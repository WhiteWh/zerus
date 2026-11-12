package app.zerus.mobile

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.CancellationException
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Response
import java.io.IOException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.TimeUnit
import java.io.ByteArrayOutputStream
import java.net.URI
import java.security.MessageDigest

class RelayException(val status: Int, message: String) : Exception(message)
class RelayApi internal constructor(private val client: OkHttpClient = OkHttpClient.Builder()
    .dispatcher(okhttp3.Dispatcher().apply { maxRequests = 64; maxRequestsPerHost = 64 })
    .connectTimeout(10, TimeUnit.SECONDS).readTimeout(35, TimeUnit.SECONDS)
    .followRedirects(false).followSslRedirects(false).retryOnConnectionFailure(false).build(),
    internal val workspace: WorkspaceJournal? = null,
    internal val catalogNowSeconds: () -> Long = { System.currentTimeMillis()/1000 },
    /** Test seam: a deterministic local seal failure before any mutation POST (zerus-a4mub.6). */
    internal val sealEnvelope: (WorkspaceIdentity, WorkspaceDevice, WorkspaceHeader, ByteArray) -> WorkspaceEnvelope =
        { identity, recipient, header, plaintext -> WorkspaceEnvelope.seal(identity, recipient, header, plaintext) }) {
    private val resultStream = RelayResultStream(client)
    internal fun closeResultStreams(connectionId: String? = null) = resultStream.close(connectionId)
    suspend fun call(url: String, token: String, path: String, body: JSONObject? = null, delete: Boolean = false,
        attempt: SendAttempt? = null): JSONObject = RelayReadTiming.measure(RelayReadTiming.httpStage(path, body != null)) { withContext(Dispatchers.IO) {
        val request = Request.Builder().url(ManagedRelay.transport(url) + path).header("Accept", "application/json")
        if (token.isNotBlank()) request.header("Authorization", "Bearer $token")
        if (delete) request.delete() else if (body != null) request.post(JsonRequestBody(body, when(path) { "/v1/requests" -> 30_408_704L; "/v2/requests" -> 41L*1024*1024; else -> 1_048_576L }))
        val call = client.newCall(request.build())
        suspendCancellableCoroutine { continuation ->
            continuation.invokeOnCancellation { call.cancel() }
            // Already cancelled before the client saw it: nothing can leave.
            if (!continuation.isActive) return@suspendCancellableCoroutine
            // The mutation request is handed to the client here: from this point
            // delivery may have begun, so settlement may only call it unknown.
            attempt?.enqueued()
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, error: IOException) {
                    continuation.resumeWithException(error)
                }
                override fun onResponse(call: Call, response: Response) {
                    try {
                        val result = response.use {
                            val value = response.body?.byteStream()?.use { stream ->
                                val bytes=ByteArrayOutputStream();val buffer=ByteArray(8192)
                                while(true) {
                                    val count=stream.read(buffer)
                                    if(count<0) break
                                    val ceiling=when { path.substringBefore('?')=="/v2/computers" -> 3*1024*1024;path.startsWith("/v2/") -> 2*1024*1024;else -> 1024*1024 }
                                    require(bytes.size()+count <= ceiling) { "Gateway response exceeded its safety limit." }
                                    bytes.write(buffer,0,count)
                                }
                                bytes.toString(Charsets.UTF_8.name())
                            }.orEmpty()
                            if(!response.isSuccessful) throw RelayException(response.code,
                                runCatching { JSONObject(value).string("error","message") }.getOrDefault("").ifBlank { "Server returned ${response.code}." })
                            if(value.isBlank()) JSONObject() else JSONObject(value)
                        }
                        continuation.resume(result)
                    } catch(error:Exception) { continuation.resumeWithException(error) }
                }
            })
        }
    } }

    suspend fun pair(url: String, code: String): Connection {
        require(!WorkspaceProtocol.encryptedInvitationCode(code.trim())) { "Use the encrypted phone invitation pairing flow." }
        val normalized = EndpointPolicy.normalize(url)
        require(code.isNotBlank()) { "Enter the invitation code shown on your computer." }
        val result = call(normalized, "", "/v1/pair", JSONObject().put("code", code.trim()).put("device_name", "Zerus Android"))
        return Connection(UUID.randomUUID().toString(), result.string("workspace_name").ifBlank { URI(normalized).host ?: "Workspace" }, normalized, result.getString("device_token"))
    }
    /** Safe on the UI dispatcher: encrypted CPU work and blocking journal access choose their workers. */
    suspend fun computers(connection: Connection) = RelayReadTiming.read(RelayReadTiming.Operation.Catalog) {
        if(connection.protocolVersion==2) encryptedComputers(connection) else call(connection.url, connection.token, "/v1/computers")
    }
    suspend fun capabilities(connection: Connection) = call(connection.url, connection.token, if(connection.protocolVersion==2) "/v2/capabilities" else "/v1/capabilities").also { value ->
        val stream = value.optJSONObject("encrypted_transport")?.optJSONObject("result_stream")
        resultStream.configure(connection, connection.protocolVersion == 2 && stream?.optInt("version") == 1 &&
            stream.optString("path") == "/v2/stream" && stream.optInt("max_requests") >= 16)
    }
    suspend fun revoke(connection:Connection):JSONObject {
        check(connection.protocolVersion==1) { "Encrypted connections are forgotten locally. Remove this phone from an enrolled computer to revoke it globally." }
        return call(connection.url,connection.token,"/v1/device",delete=true)
    }
    suspend fun registerPush(connection:Connection,provider:String,token:String) = call(connection.url,connection.token,
        if(connection.protocolVersion==2) "/v2/push" else "/v1/push",JSONObject().put("provider",provider).put("token",token))
    suspend fun post(connection:Connection,computerId:String,session:String="",operation:String,payload:JSONObject,id:String,
        attempt: SendAttempt):JSONObject {
        val body=JSONObject().put("request_id",id).put("computer_id",computerId).put("session",session).put("operation",operation).put("payload",payload)
        return (if(connection.protocolVersion==2) encryptedPost(connection,body,attempt) else call(connection.url,connection.token,"/v1/requests",body,attempt = attempt)).also {
            check(it.string("request_id")==id) { "Gateway returned a mismatched receipt. Delivery remains unconfirmed." }
        }
    }
    /** One mutation, with its transport evidence. The evidence is required by
     *  type (no default): the outbox send cannot silently drop it and fall back
     *  to "before the marker" (zerus-a4mub.21). */
    suspend fun submit(connection: Connection, target: Target, operation: String, payload: JSONObject, id: String,
        attempt: SendAttempt): JSONObject {
        require(target.agentId.isBlank() || operation in setOf("inspect", "history", "history_projection", "send", "send_receipt", "attachment_read", "file_read")) { "This control belongs to the parent session." }
        require(target.archiveId.isBlank() && payload.string("archive_id").isBlank() || operation in setOf("inspect", "history", "history_projection", "process_output", "restore", "forget", "rename", "fork", "attachment_read", "file_read") && target.archiveId.isNotBlank() && payload.string("archive_id") == target.archiveId) { "Archived conversations are read-only." }
        return post(connection,target.computerId,target.session,operation,payload,id,attempt)
    }
    internal suspend fun speech(connection: Connection, host: MachineKey, operation: String, payload: JSONObject,
        id: String = UUID.randomUUID().toString()): JSONObject {
        require(host.connectionId == connection.id && operation in SpeechProtocol.operations + "speech_stats")
        val initial = post(connection,host.computerId,"",operation,payload,id,SendAttempt())
        check(initial.string("request_id") == id) { "Gateway returned a different voice receipt." }
        val receipt = await(connection, initial)
        check(receipt.string("state") == "completed") {
            receipt.string("error").ifBlank { "The computer has not confirmed this voice request. Check its status." }
        }
        return receipt.getJSONObject("result")
    }
    internal suspend fun voice(connection: Connection, target: Target, operation: String, payload: JSONObject, id: String): JSONObject {
        require(connection.id == target.connectionId && operation in VoiceProtocol.operations && target.session.isNotBlank())
        require(payload.string("request_id") == id && payload.string("expected_run_id") == target.run &&
            payload.string("expected_conversation_id") == target.conversation)
        val receipt = await(connection, submit(connection, target, operation, payload, id, SendAttempt()))
        check(receipt.string("request_id") == id && receipt.string("state") == "completed") {
            receipt.string("error").ifBlank { "The computer has not confirmed this voice request. Check its status." }
        }
        return receipt.getJSONObject("result")
    }
    internal suspend fun speechConfiguration(connection: Connection, host: SpeechConfigHost, operation: String,
        payload: JSONObject, id: String = UUID.randomUUID().toString()): JSONObject {
        require(host.key.connectionId == connection.id && operation in SpeechConfigProtocol.operations)
        if (operation in SpeechConfigProtocol.mutations) require(payload.string("request_id") == id)
        if (operation != "speech_config") require(payload.string("expected_machine_id") == host.machineId)
        val initial = post(connection,host.key.computerId,"",operation,payload,id,SendAttempt())
        check(initial.string("request_id") == id) { "The gateway returned a different configuration receipt." }
        val receipt = await(connection, initial)
        check(receipt.string("state") == "completed") { "The computer has not confirmed this configuration operation. Check its status." }
        return receipt.getJSONObject("result")
    }
    /** Releases the exact sealed reservation of an outbox send proven never
     *  enqueued. Call only while the same live SendAttempt is Local: a marked
     *  request keeps its reservation as the unknown/no-replay evidence. */
    internal suspend fun releaseReservedOutgoing(connection: Connection, id: String) = withContext(Dispatchers.IO) {
        if (connection.protocolVersion == 2) workspace?.release(connection, id)
    }
    suspend fun receipt(connection: Connection, id: String) = (if(connection.protocolVersion==2) encryptedReceipt(connection,id) else call(connection.url, connection.token, "/v1/requests/$id")).also {
        check(it.string("request_id") == id) { "Gateway returned a mismatched receipt. Delivery remains unconfirmed." }
    }
    /** A fresh READ UUID observes the frozen original; it never submits that send again. */
    suspend fun sendReceipt(connection:Connection,message:OutgoingMessage,intent:SendIntent):SendReceiptProof? {
        require(connection.id==message.target.connectionId && SendReceiptRecovery.matches(message,intent))
        val id=UUID.randomUUID().toString()
        val receipt=await(connection,submit(connection,message.target,SendReceiptRecovery.operation,SendReceiptRecovery.request(id,intent),id,SendAttempt()),firstPollDelayMs=100)
        if(receipt.string("request_id")!=id || receipt.string("state")!="completed") return null
        return withContext(Dispatchers.Default) { receipt.optJSONObject("result")?.let { SendReceiptRecovery.proof(id,intent,it) } }
    }
    /** One unchanged campaign budget covers the original receipt and its exact native fallback. */
    suspend fun checkDelivery(connection:Connection,message:OutgoingMessage,intent:SendIntent?,limitMs:Long=45_000):SendReceiptCheck {
        require(limitMs in 1..45_000)
        val deadline=SendReceiptRecovery.nowMs()+limitMs
        return withTimeout(limitMs) {
            val original=try { await(connection,receipt(connection,message.requestId)) }
                catch(e:CancellationException) { throw e } catch(_:Exception) { null }
            if(original?.string("state")=="failed") return@withTimeout SendReceiptCheck.Original(original,deadline)
            if(original?.string("state")=="completed" && original.optJSONObject("result")?.string("status") in listOf("submitted","queued")) {
                if(message.sendIntent==null || intent==null) return@withTimeout SendReceiptCheck.Original(original,deadline)
                val proof=original.optJSONObject("result")?.let { SendReceiptRecovery.acknowledgement(message.sendIntent,it) }
                if(proof!=null) return@withTimeout SendReceiptCheck.Native(message.sendIntent,proof,deadline)
            }
            if(intent==null || !SendReceiptRecovery.matches(message,intent)) return@withTimeout SendReceiptCheck.Unconfirmed
            val proof=try { sendReceipt(connection,message,intent) }
                catch(e:CancellationException) { throw e } catch(_:Exception) { null }
            if(proof==null || SendReceiptRecovery.nowMs()>=deadline) SendReceiptCheck.Unconfirmed else SendReceiptCheck.Native(intent,proof,deadline)
        }
    }
    suspend fun await(connection: Connection, initial: JSONObject, maxPolls: Int = 35, pollDelayMs:Long = 800,
        firstPollDelayMs:Long = pollDelayMs): JSONObject {
        if (connection.protocolVersion == 2 && resultStream.available(connection) && maxPolls > 0 && initial.string("state") in listOf("queued", "claimed")) {
            val id = initial.getString("request_id")
            prepareReceiptWatch(connection, id)
            return coroutineScope {
                val ordinary = pollDelayMs.coerceIn(50, 1000)
                // A hint may accelerate a receipt, but a connected silent socket
                // must never postpone ordinary receipt recovery. One watcher is
                // shared by this bounded polling campaign and consumed at most once.
                val readiness = async {
                    val signaled = RelayReadTiming.measure(RelayReadTiming.Stage.ResultSignalWait) {
                        resultStream.ready(connection, id, minOf(25_000L, maxPolls * ordinary))
                    }
                    RelayReadTiming.point(if (signaled) RelayReadTiming.Stage.ResultSignalReady else RelayReadTiming.Stage.ResultSignalFallback)
                    signaled
                }
                var consumed = false
                try {
                    ReceiptPolling.await(initial,maxPolls,pollDelayMs,firstPollDelayMs,
                        pause = { pause -> RelayReadTiming.measure(RelayReadTiming.Stage.PollDelay) {
                            if (consumed) delay(pause)
                            else if (withTimeoutOrNull(pause) { readiness.await() } != null) consumed = true
                        } },
                        // The ordinary path still verifies fresh membership,
                        // signature, original request/route and journal settlement.
                        receipt = { receipt(connection,it) })
                } finally {
                    readiness.cancel()
                }
            }
        }
        return ReceiptPolling.await(initial,maxPolls,pollDelayMs,firstPollDelayMs,
            pause={ RelayReadTiming.measure(RelayReadTiming.Stage.PollDelay) { delay(it) } },
            receipt={ receipt(connection,it) })
    }
    suspend fun inspect(connection: Connection, target: Target): JSONObject = RelayReadTiming.read(RelayReadTiming.Operation.Inspect) {
        val payload = JSONObject().also {
            if (target.archiveId.isNotBlank()) it.put("archive_id", target.archiveId)
            if (target.agentId.isNotBlank()) it.put("agent_id",target.agentId).put("expected_run_id",target.run).put("expected_conversation_id",target.parentConversation)
        }
        val result = await(connection, submit(connection, target, "inspect", payload, UUID.randomUUID().toString(), SendAttempt()),firstPollDelayMs=100)
        check(result.string("state") == "completed") { result.string("error").ifBlank { "Computer has not returned its activity yet. Refresh to check again." } }
        result.getJSONObject("result").also { activity ->
            if(target.agentId.isNotBlank()) check(activity.string("agent_id") == target.agentId && activity.string("parent_conversation_id") == target.parentConversation && activity.string("run_id") == target.run && activity.string("name") == target.session) { "Computer returned a different child conversation." }
            else check(activity.string("agent_id").isBlank()) { "Computer returned a child conversation for the parent." }
            if (target.archiveId.isNotBlank()) check(activity.string("archive_id") == target.archiveId && activity.string("name") == target.session) { "Computer returned a different archived conversation." }
            else check(activity.string("archive_id").isBlank() && activity.string("state") != "archived" &&
                (activity.string("name").isBlank() || activity.string("name") == target.session)) { "Computer returned a different conversation for this session." }
        }
    }
    suspend fun history(connection:Connection,target:Target,direction:String="",cursor:String="",limit:Int=100):HistoryPage = RelayReadTiming.read(RelayReadTiming.Operation.History) {
        require(target.run.isNotBlank() && target.conversation.isNotBlank() && limit in 1..100)
        require(direction in setOf("","before","after","around","unread") && (direction.isEmpty() == cursor.isEmpty()) && cursor.length<=8192)
        val id=UUID.randomUUID().toString()
        val payload=HistoryRequests.payload(target,id,direction,cursor,limit)
        val receipt=await(connection,submit(connection,target,"history",payload,id, SendAttempt()),firstPollDelayMs=100)
        check(receipt.string("state")=="completed") { receipt.string("error").ifBlank { "History is not available yet. Try again." } }
        withContext(Dispatchers.Default) { HistoryPage.parse(target,id,receipt.getJSONObject("result")) }
    }
    /** Read-only projection repair. The applied display marker belongs to the client cache only. */
    suspend fun historyProjection(connection:Connection,target:Target,epoch:String,after:Long,until:Long):HistoryProjection.Page = RelayReadTiming.read(RelayReadTiming.Operation.Projection) {
        val id=UUID.randomUUID().toString()
        val payload=HistoryProjection.payload(target,id,epoch,after,until)
        val receipt=await(connection,submit(connection,target,"history_projection",payload,id, SendAttempt()),firstPollDelayMs=100)
        check(receipt.string("state")=="completed") { receipt.string("error").ifBlank { "History changes are not available yet." } }
        withContext(Dispatchers.Default) { HistoryProjection.parse(target,id,epoch,after,until,receipt.getJSONObject("result")) }
    }
    /** Shared read mark; the computer applies only current values, so each attempt has its own request ID. */
    suspend fun markRead(connection: Connection, target: Target, payload: JSONObject): JSONObject {
        require(target.archiveId.isBlank() && target.agentId.isBlank()) { "Read marks belong to the live parent conversation." }
        val id = UUID.randomUUID().toString()
        return await(connection, submit(connection, target, "mark_read", payload, id, SendAttempt()))
    }
    suspend fun events(connection: Connection, after: Long, wait: Int = 25) = if(connection.protocolVersion==2) encryptedEvents(connection,after,wait)
        else call(connection.url, connection.token, "/v1/events?after=$after&wait=$wait")
}

object NativeParser {
    fun sessions(connection: Connection, machine: JSONObject): List<Session> {
        val snapshot = machine.optJSONObject("snapshot") ?: return emptyList()
        return snapshot.optJSONArray("sessions")?.objects().orEmpty().mapNotNull { raw ->
            val archive = raw.string("archive_id")
            if (raw.string("state") == "archived" || archive.isNotBlank()) {
                if (archive.isBlank() || runCatching { UUID.fromString(archive).toString() == archive }.getOrDefault(false).not()) return@mapNotNull null
            }
            Session(Target(machine.getString("id"), raw.string("name"), raw.string("run_id"), raw.string("conversation_id"), connection.id, archive),
                raw.string("title", "label", "name"), raw.string("agent", "cmd"),
                if (raw.string("phase") in listOf("input", "approval", "error")) raw.string("phase") else raw.string("activity", "phase", "state"),
                raw.string("project", "cwd"), SessionPreview.render(raw.string("last_message", "prompt")), QuestionPolicies.blockingRequests(raw), raw)
        }
    }
    fun events(raw: JSONObject): List<Event> {
        val providers = raw.optJSONArray("provider_messages")?.objects().orEmpty()
        // Native child inspection may normalize its own journal agent_id to blank.
        // Explicit child rows must still match that exact inspected child.
        val inspectedAgent = raw.string("agent_id")
        fun inScope(value: JSONObject) = value.string("agent_id") in
            if (inspectedAgent.isBlank()) listOf("", "main") else listOf("", inspectedAgent)
        // message_events is the separately bounded main-message window. Keep
        // the journal namespace so matching native seq IDs deduplicate safely.
        val seenJournal = mutableSetOf<String>()
        val journal = (raw.optJSONArray("events")?.objects().orEmpty() + raw.optJSONArray("message_events")?.objects().orEmpty()).filter { value ->
            val seq = value.string("seq")
            seq.isBlank() || seenJournal.add("${value.string("history_stream")}:$seq")
        }
        val occurrences = mutableMapOf<String, Int>()
        fun parse(value: JSONObject, streamArgument: String): Event? {
            val stream = value.string("history_stream").takeIf { it in listOf("journal", "provider", "attachment", "voice") } ?: streamArgument
            // A fork's inherited row carries its original type; older apps drop the unknown type.
            val inheritedFrom = if (value.string("type") == "Inherited") value.string("inherited_from") else ""
            if (value.string("type") == "Inherited" && inheritedFrom.isBlank()) return null
            val type = if (inheritedFrom.isNotEmpty()) value.string("inherited_type") else value.string("type")
            if (type !in listOf("UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted", "QuestionAnswered", "AgentMessage", "Stop") ||
                !inScope(value)) return null
            // A native prompt the person did not write is a notice, as on desktop.
            val origin = value.string("origin").takeIf { inheritedFrom.isEmpty() && it in ActivityNotices.origins && type in
                listOf("UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted") }.orEmpty()
            val role = when (type) {
                "QuestionAnswered" -> "You (answer)"
                "UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted" -> "You"
                else -> "AgentMessage"
            }
            // Identities keep the native role, so cached rows stay the same rows.
            val shown = if (origin.isNotEmpty()) ActivityNotices.ROLE else role
            val source = value.string("source").ifBlank { stream }
            // An inherited voice belongs to the source session: its text stays, but
            // no voice card, playback or status acts on it through this fork.
            val voice = if (source == "hgs_voice" && inheritedFrom.isEmpty()) value.optJSONObject("voice")?.let {
                runCatching { VoiceProtocol.message(it) }.getOrNull()
            } else null
            val files = value.optJSONArray("attachments")?.objects().orEmpty().map {
                DisplayedAttachment(it.string("name"), it.string("mime"), it.optLong("bytes"), it.string("reference"),
                    it.string("request_id"), (it.opt("index") as? Int) ?: -1)
            }
            // Fallback identity belongs to the original native row, not its display label/body.
            val identityText = if (source == "hgs_delivery" && files.isNotEmpty()) value.optString("detail", "")
                else value.string("detail", "text", "message", "prompt", "submitted_text")
            val text = if (origin.isNotEmpty()) ActivityNotices.body(origin, value.string("detail"), value.string("report")) else identityText
            if (text.isBlank() && files.isEmpty() && voice == null) return null
            val at = value.optDouble("at", 0.0).takeIf { it.isFinite() } ?: 0.0
            val nativeId = value.string("message_id", "seq", "source_offset")
            // Count only indistinguishable rows, rather than the whole array.
            // Prepending unrelated history therefore preserves existing keys.
            val fallback = if (nativeId.isBlank()) {
                val identity = JSONArray(listOf(type, role, source, at, value.string("timestamp"), identityText,
                    files.map { listOf(it.name, it.mime, it.bytes, it.reference) })).toString()
                val digest = MessageDigest.getInstance("SHA-256").digest(identity.toByteArray(Charsets.UTF_8))
                    .joinToString("") { "%02x".format(it) }
                val key = "$stream:$source:$role:$digest"
                val occurrence = occurrences[key] ?: 0
                occurrences[key] = occurrence + 1
                "fallback:$digest:$occurrence"
            } else "native:$nativeId"
            val originalId = value.string("original_id").ifBlank { "$stream:$source:$role:$fallback" }
            val historyId = value.string("history_id")
            val requestId = when {
                shown != "You" || inheritedFrom.isNotEmpty() -> ""
                source in setOf("hgs_delivery", "hgs_voice") -> value.string("message_id")
                // OpenCode dates a queued prompt by its delivery; hgs names the Zerus send
                // that admitted it, so the phone can match its own message exactly.
                source == "opencode_native" -> value.string("request_id").takeIf {
                    runCatching { UUID.fromString(it).toString() == it }.getOrDefault(false) }.orEmpty()
                else -> ""
            }
            return Event(if (historyId.isBlank()) originalId else "history:$historyId", shown,
                text, value.string("at", "timestamp"), at, requestId, files, source,
                delivery = if (shown == "You" && inheritedFrom.isEmpty()) MessageDeliveryPresentation.inboxState(value) else "",
                nativeType = type, submittedText = if (source == "hgs_delivery") value.string("submitted_text") else "",
                originalId = originalId, historyId = historyId, historyCursor = value.string("history_cursor"),
                historyEpoch = raw.string("history_epoch"), incomingSeq = (value.opt("incoming_seq") as? Number)?.toLong()?.takeIf { it >= 0 },
                originalIds = value.optJSONArray("original_ids")?.let { ids -> (0 until minOf(ids.length(),8)).mapNotNull { ids.opt(it) as? String } }.orEmpty(), detailTruncated = value.optBoolean("detail_truncated"), voice = voice,
                origin = origin, sender = if (origin == "peer_message") value.string("sender") else "",
                report = ActivityNotices.report(origin, value.string("report")), inheritedFrom = inheritedFrom,
                nativeMessageId = if(source in setOf("claude_native","codex_native","kimi_native","dsh_native","opencode_native")) value.string("message_id").takeIf {
                    it.toByteArray(Charsets.UTF_8).size<=256 && it.none(Char::isISOControl) }.orEmpty() else "")
        }
        val rows = (journal.mapNotNull { item ->
            fun replaces(provider: JSONObject, hook: JSONObject) = provider.string("type") == "AgentMessage" && inScope(provider) && inScope(hook) &&
                hook.string("type") == "Stop" && NativeStopExcerpt.matches(provider.string("detail"), hook.string("detail")) &&
                kotlin.math.abs(provider.optDouble("at") - hook.optDouble("at")) < 10
            val replacement = if (item.string("type") == "Stop") providers.filter { replaces(it,item) }.singleOrNull() else null
            if (replacement != null && journal.count { replaces(replacement,it) } == 1) null else parse(item,"journal")
        } + providers.mapNotNull { item -> parse(item, "provider") })
            .distinctBy { it.id }.toMutableList()
        val used = mutableSetOf<Int>()
        val receipts = raw.optJSONArray("attachment_messages")?.objects().orEmpty().mapNotNull { parse(it, "attachment") }
        receipts.forEach { metadata ->
            val candidate = rows.indices.filter { it !in used && rows[it].role == "You" && AttachmentReceiptMatch.replaceable(metadata, rows[it]) &&
                rows[it].nativeType in listOf("UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted") &&
                AttachmentReceiptMatch.matches(metadata, rows[it]) }.singleOrNull()
            val exact = candidate?.takeIf { index -> receipts.count { AttachmentReceiptMatch.matches(it, rows[index]) } == 1 }
            if (exact != null) {
                used += exact
                rows[exact] = metadata.copy(at = rows[exact].at, originalIds = (metadata.originalIds + rows[exact].id).distinct().take(8))
            }
            else if (rows.none { it.role == "You" && it.receiptRequestId == metadata.requestId && metadata.requestId.isNotBlank() }) rows += metadata
        }
        raw.optJSONArray("voice_messages")?.objects().orEmpty().forEach { item ->
            val metadata = parse(item, "voice") ?: return@forEach
            val voice = metadata.voice ?: return@forEach
            if (metadata.requestId != voice.id) return@forEach
            // A persisted native binding is authoritative; equal text alone never removes a row.
            val binding = item.optJSONObject("voice")?.optJSONObject("native_binding")
            val bound = if (binding == null) emptyList() else (journal.map { it to "journal" } + providers.map { it to "provider" }).filter { (native, stream) ->
                val seq = binding.opt("journal_seq") as? Number
                val providerId = binding.string("provider_message_id")
                inScope(native) && native.string("type") in listOf("UserPromptSubmit", "UserPromptQueued", "UserMessage", "TurnStarted") &&
                    if (stream == "journal") seq != null && (native.opt("seq") as? Number)?.toLong() == seq.toLong()
                    else providerId.isNotBlank() && native.string("message_id") == providerId &&
                        (binding.string("provider_source").isBlank() || native.string("source") == binding.string("provider_source")) &&
                        (!binding.has("provider_source_offset") || native.opt("source_offset")?.toString() == binding.opt("provider_source_offset")?.toString())
            }.mapNotNull { (native, stream) -> parse(native, stream) }.distinctBy { it.id }
            // Admission of the voice job does not consume the native inbox. Keep its
            // live status row, including after Stop, outside voice suppression aliases.
            val accepted = bound.singleOrNull()?.takeIf { it.delivery !in MessageDeliveryPresentation.inboxStates }
            if (accepted != null) rows.removeAll { it.id == accepted.id && it.role == "You" && it.voice == null }
            val old = rows.indexOfFirst { it.voice?.id == voice.id }
            val next = metadata.copy(originalIds = (metadata.originalIds + listOfNotNull(accepted?.id)).distinct().take(8))
            if (old < 0) rows += next else if (rows[old].voice!!.revision < voice.revision) rows[old] = next
        }
        return if (raw.string("history_epoch").isNotBlank()) rows.toList() else rows.sortedBy { it.at }
    }
    fun messageBlockReason(raw: JSONObject?): String {
        if (raw == null) return "Refresh to verify the session before sending."
        if(raw.string("agent_id").isNotBlank() && raw.string("agent_id") != "main") return if(raw.optBoolean("can_send")) "" else raw.string("send_reason","can_send_reason").ifBlank { "This child conversation is read-only." }
        if (!raw.optBoolean("tracked")) return "This session has no native tracking."
        if (raw.string("run_id").isBlank()) return "Waiting for the agent to finish starting."
        if (raw.string("runtime_state") != "live" || raw.string("process_state") != "running") return "The agent is not running. Resume it on your computer."
        if (raw.optBoolean("auth_required") || raw.string("error").isNotBlank()) return "Complete login or setup on your computer."
        if (raw.string("expected_id").isNotBlank()) return if (raw.optBoolean("resume_message_can_send")) "" else raw.string("resume_message_reason").ifBlank { "Waiting for the restored conversation." }
        if (raw.string("conversation_id").isBlank()) return if (raw.optBoolean("first_message_can_send")) "" else raw.string("first_message_reason").ifBlank { "Waiting for the new conversation." }
        val phase = raw.string("phase")
        if (phase in listOf("approval", "input")) return "Answer the pending request or continue on your computer."
        if (phase == "interrupted") return if (raw.optBoolean("interrupted_message_can_send")) "" else raw.string("interrupted_message_reason").ifBlank { "Waiting for the input prompt after interruption." }
        if (phase == "error" && raw.optJSONObject("provider_error") != null) return if (raw.optBoolean("error_message_can_send")) "" else raw.string("error_message_reason").ifBlank { "Check the agent before retrying." }
        if (raw.string("activity") == "idle" && phase == "idle" || raw.string("activity") == "busy" && phase in listOf("idle", "working", "tool", "compacting")) return ""
        return "Waiting for the agent to confirm its input state."
    }
    fun questions(raw: JSONObject): List<Question> = raw.optJSONArray("pending_questions")?.objects().orEmpty().map { card ->
        val nativeDelivery = card.optJSONObject("answer_delivery")?.let { delivery ->
            NativeQuestionDelivery(delivery.string("request_id"), delivery.string("question_id"), delivery.string("question_hash"),
                delivery.string("run_id"), delivery.string("conversation_id"), delivery.string("status"),
                delivery.optJSONArray("answers")?.objects().orEmpty().map { answer ->
                    NativeQuestionAnswer(answer.string("question_id"), answer.optJSONArray("selected_option_ids")?.let { ids ->
                        (0 until ids.length()).mapNotNull { ids.opt(it) as? String }
                    }.orEmpty(), answer.string("text"), answer.optBoolean("skip"))
                })
        }
        Question(card.string("question_id"), card.string("question_hash"), card.optJSONArray("questions")?.objects().orEmpty().map { prompt ->
            fun bound(key: String, limit: Int) = (prompt.opt(key) as? Number)?.toLong()?.takeIf { it in 0..limit }?.toInt()
            Prompt(prompt.string("id"), prompt.string("question", "prompt"), prompt.optJSONArray("options")?.objects().orEmpty().map {
                Choice(it.string("id"), it.string("label"), it.string("description"), it.string("scope"))
            }, prompt.optBoolean("multi_select"), prompt.optBoolean("required", true), prompt.optBoolean("allow_other", false),
                prompt.string("header"), prompt.string("body"), prompt.string("other_label"), prompt.string("other_description"),
                prompt.string("input_kind"), bound("min_length", 1_000_000), bound("max_length", 1_000_000), bound("min_items", 1000), bound("max_items", 1000),
                prompt.optJSONArray("default_option_ids")?.let { ids -> (0 until minOf(ids.length(), 64)).mapNotNull { ids.opt(it) as? String } }.orEmpty(),
                // Only an explicit OpenCode text item seeds its field; it is never sent without the user's Send.
                if (prompt.string("input_kind") == "text" && card.string("source") == "opencode_native" && prompt.optBoolean("allow_other", false))
                    QuestionPolicies.defaultText(prompt.opt("default_text"), bound("max_length", 1_000_000)) else "")
        }, card.optBoolean("can_answer"), card.optBoolean("can_skip"), card.optBoolean("optional"),
            nativeDelivery?.status.orEmpty(), (card.opt("created_at") as? Number)?.toDouble()?.takeIf { it.isFinite() && it > 0 && it < 253402300800.0 } ?: 0.0,
            card.string("source"), card.optBoolean("approval"), card.optBoolean("trust_request"), card.string("answer_unavailable_reason", "unavailable_reason"),
            card.string("run_id"), card.string("conversation_id"), card.string("tool_call_id"), nativeDelivery,
            card.optBoolean("approval_choices"))
    }
}
