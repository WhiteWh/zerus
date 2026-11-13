package app.zerus.mobile

import android.app.Application
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.referentialEqualityPolicy
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import androidx.compose.runtime.mutableStateMapOf
import android.net.Uri
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject
import app.zerus.mobile.replica.SessionRef
import app.zerus.mobile.replica.TaskInfo
import app.zerus.mobile.replica.TaskPosition
import app.zerus.mobile.replica.TaskSettings
import app.zerus.mobile.replica.TaskStage
import app.zerus.mobile.replica.TaskStore
import app.zerus.mobile.replica.TaskTarget

class ZerusViewModel(application: Application) : AndroidViewModel(application) {
    private val store = PrivateStore(application)
    private val api = RelayApi(workspace = store.workspaceJournal)
    override fun onCleared() {
        api.closeResultStreams()
        super.onCleared()
    }
    private val gitStatusRelay = GitStatusRelay(api)
    private fun gitStatusCatalog() = GitStatusCatalog(connections, machines, relayOperations, machineOperations, demo, storageReady)
    private fun gitStatusAccess(key: MachineKey) = gitStatusCatalog().access(key)
    private fun gitStatusConnection(request: GitStatusRequest): Connection {
        val access = gitStatusAccess(request.key.machine)
        check(access?.available == true) { "Git status is no longer available on this computer." }
        return (access.epoch as GitStatusEpoch).connection
    }
    internal val gitStatus by lazy { GitStatusController(viewModelScope, android.os.SystemClock::elapsedRealtimeNanos,
        ::gitStatusAccess, start = { gitStatusRelay.start(gitStatusConnection(it), it) },
        receipt = { gitStatusRelay.receipt(gitStatusConnection(it), it) }) }
    internal val privateAnalytics by lazy { PrivateAnalyticsController(this, viewModelScope, api) }
    internal fun privateAnalyticsRoutes(): List<AnalyticsRoute> = machines.mapNotNull { machine ->
        val connection = connections.find { it.id == machine.connectionId } ?: return@mapNotNull null
        val key = MachineKey(machine.connectionId, machine.id)
        val state = privateAnalyticsAccess(machine.machineId, machine.online && !machine.lastKnown, !demo && storageReady,
            relayOperations[machine.connectionId].orEmpty(), machineOperations[key].orEmpty())
        AnalyticsRoute(connection, key, machine.machineId, machine.name, state, supportsSpeechStats(key))
    }
    internal fun privateAnalyticsRouteCurrent(route: AnalyticsRoute, agent: Boolean = true) = privateAnalyticsRoutes().any {
        it.key == route.key && it.machineId == route.machineId && it.connection == route.connection &&
            (if (agent) it.state == AnalyticsReadState.Ready else it.speech && it.state in setOf(AnalyticsReadState.Ready, AnalyticsReadState.Unsupported))
    }
    internal val speech by lazy { SpeechController(application, this, viewModelScope, api) }
    internal val defaultVoiceProviders by lazy { DefaultVoiceProviders(this, viewModelScope, api) }
    internal fun defaultVoiceProfilesReason(host: SpeechConfigHost): String = when {
        demo || !storageReady -> "Connect a computer to load its voice profiles."
        connections.none { it.id == host.key.connectionId } -> "The voice workspace is disconnected."
        DefaultVoiceRoutes.resolve(MachineKey(host.key.connectionId, host.machineId), machines, connections) != host ->
            "The voice computer is unavailable or its identity changed. Refresh Machines and try again."
        "speech_profiles" !in relayOperations[host.key.connectionId].orEmpty() -> "Update this gateway to load voice profiles."
        "speech_profiles" !in machineOperations[host.key].orEmpty() -> "Update Zerus on this computer to load voice profiles."
        else -> ""
    }
    internal val speechConfiguration by lazy { SpeechConfigurationController(application, this, viewModelScope, api) }
    internal fun speechConfigurationReason(host: SpeechConfigHost): String = when {
        demo || !storageReady -> "Connect a computer to manage its speech providers."
        updateInstallPreparing -> "Finish the app update first."
        connections.none { it.id == host.key.connectionId } -> "The workspace is disconnected."
        machines.none { it.connectionId == host.key.connectionId && it.id == host.key.computerId &&
            it.machineId == host.machineId && it.online && !it.lastKnown } -> "The selected computer is unavailable or its identity changed."
        !AttachmentPolicy.validId(host.machineId) -> "Refresh to verify this computer's identity."
        !relayOperations[host.key.connectionId].orEmpty().containsAll(SpeechConfigProtocol.operations) -> "Update this gateway to manage speech providers."
        !machineOperations[host.key].orEmpty().containsAll(SpeechConfigProtocol.operations) -> "Update Zerus on this computer to manage speech providers."
        else -> ""
    }
    internal val speechNavigation get() = navigationId
    internal fun voiceHostReason(target: Target): String = when {
        demo -> "Connect a computer to send voice messages."
        connections.none { it.id == target.connectionId } -> "The voice workspace is disconnected."
        machines.none { it.connectionId == target.connectionId && it.id == target.computerId && it.online && !it.lastKnown } -> "The conversation computer is offline."
        !relayOperations[target.connectionId].orEmpty().containsAll(VoiceProtocol.operations) -> "Update this gateway to send voice messages."
        !machineOperations[MachineKey(target.connectionId, target.computerId)].orEmpty().containsAll(VoiceProtocol.operations) -> "Update Zerus on the conversation computer to send voice messages."
        else -> ""
    }
    internal fun videoHostReason(target: Target): String = voiceHostReason(target).ifBlank {
        when {
            VideoProtocol.FEATURE !in relayFeatures[target.connectionId].orEmpty() -> "Update this gateway to send video messages."
            VideoProtocol.FEATURE !in machineFeatures[MachineKey(target.connectionId,target.computerId)].orEmpty() -> "Update Zerus on this conversation's computer to send video messages."
            else -> ""
        }
    }
    internal fun voiceSendReason(target: Target): String = when {
        !storageReady || updateInstallPreparing -> "Private storage is not ready."
        target.agentId.isNotBlank() || target.archiveId.isNotBlank() -> "Voice messages are available in live parent conversations."
        voiceHostReason(target).isNotBlank() -> voiceHostReason(target)
        !fresh(target) -> "Open and refresh the original conversation before sending."
        NativeParser.messageBlockReason(activity).isNotBlank() -> NativeParser.messageBlockReason(activity)
        sendingBlocked(target) -> "Resolve the pending delivery before sending this recording."
        else -> ""
    }
    internal fun voiceColdCache() = ContextPresentation.isCold(activity)
    internal fun voiceSent(target: Target, id: String) { lastOwnSend = target to id }
    internal fun speechHostReason(host: MachineKey): String = when {
        demo -> "Connect a computer to use voice input."
        connections.none { it.id == host.connectionId } -> "The voice workspace is disconnected."
        machines.none { it.connectionId == host.connectionId && it.id == host.computerId && it.online && !it.lastKnown } -> "The selected voice computer is offline."
        !relayOperations[host.connectionId].orEmpty().containsAll(SpeechProtocol.operations) -> "This gateway does not support voice input yet."
        !machineOperations[host].orEmpty().containsAll(SpeechProtocol.operations) -> "This computer does not support voice input yet."
        else -> ""
    }
    internal fun supportsSpeechStats(host: MachineKey) = "speech_stats" in relayOperations[host.connectionId].orEmpty() &&
        "speech_stats" in machineOperations[host].orEmpty()
    internal var speechComposer: SpeechComposerBridge? = null; private set
    internal fun bindSpeechComposer(bridge: SpeechComposerBridge) { speechComposer = bridge }
    internal fun unbindSpeechComposer(bridge: SpeechComposerBridge) {
        if (speechComposer === bridge) speechComposer = null
    }
    internal suspend fun insertSpeech(attempt: SpeechAttempt, explicit: Boolean) {
        check(storageReady && !updateInstallPreparing) { "Private storage is not ready." }
        if (attempt.id in messageState.speechAppliedIds) { persistence!!.flush(); return }
        check(selected?.target == attempt.origin.target) { "Open the original conversation to insert this transcription, or copy it." }
        val bridge = speechComposer
        check(bridge != null && bridge.isCurrent(draft(attempt.origin.target))) {
            "Wait for the original draft to open, then insert the transcription."
        }
        // The actual editor can be ahead of its snapshotFlow mirror. Reconcile without yielding
        // before checking the captured revision, including when insertion was explicitly reviewed.
        val editorText = bridge.editor.text.toString()
        if (draft(attempt.origin.target).text != editorText) edit(attempt.origin.target, editorText)
        val current = draft(attempt.origin.target)
        check(current.text == editorText) { "Could not save the latest draft. Copy the transcription instead." }
        check(explicit || SpeechDrafts.unchanged(attempt, current)) { "The draft changed. Review the transcription before inserting it." }
        if (continuation?.draft?.target == current.target) cancelCompactContinuation("Draft changed. Your message will not be sent automatically.")
        editState { SpeechDrafts.insert(it, attempt, current) }
        // Advance the real editor and its reconciliation generation in this same main-loop turn.
        // An IME edit during the persistence barrier must not be replaced by a later UI effect.
        bridge.apply(draft(current.target))
        persistence!!.flush()
    }
    var connections by mutableStateOf<List<Connection>>(emptyList()); private set
    var pendingEncryptedPairings by mutableStateOf<Set<String>>(emptySet());private set
    var machines by mutableStateOf<List<Machine>>(emptyList()); private set
    private var catalogSessions by mutableStateOf<List<Session>>(emptyList(),referentialEqualityPolicy())
    private var publishedSessions by mutableStateOf<List<Session>>(emptyList(),referentialEqualityPolicy())
    var sessions: List<Session>
        get() = publishedSessions
        private set(value) { catalogSessions = value }
    val archive by lazy { ArchivePaging(viewModelScope,reset={resetSessionScroll()},load=::loadArchivePage) }
    private var accountsWriter:OrderedStatePersistence<List<AccountCatalog>>?=null
    var accountCatalogs by mutableStateOf<List<AccountCatalog>>(emptyList()); private set
    var accountsError by mutableStateOf(""); private set
    val displayedAccounts get() = if(demo) previewAccounts else accountCatalogs
    private val previewAccounts by lazy { AccountSnapshots.demo(System.currentTimeMillis()/1000.0) }
    var projects by mutableStateOf<List<ProjectSummary>>(emptyList()); private set
    var projectWarnings by mutableStateOf<List<String>>(emptyList()); private set
    // The phone's task replicas and their sync (TaskSync.kt); tasks have no screen yet.
    private val taskReplicas by lazy { TaskReplicas(java.io.File(getApplication<Application>().noBackupFilesDir, "task-replicas-v1")) }
    private val taskSync by lazy { TaskSync({ api }, taskReplicas) }
    private var taskDigests: Map<ProjectKey, Map<String, String>> = emptyMap()
    private var taskDigestsAt = 0L
    private var taskSyncRunning = false
    private var taskSyncJob: Job? = null
    var selectedProject by mutableStateOf<ProjectSummary?>(null); private set
    var sessionProjectScope by mutableStateOf<ProjectSummary?>(null); private set
    var sessionFilter by mutableStateOf(SessionFilter.All); private set
    var sessionQuery by mutableStateOf(""); private set
    var selectedMachines by mutableStateOf<Set<MachineKey>>(emptySet()); private set
    var collapsedProjects by mutableStateOf<Set<String>>(emptySet()); private set
    var sessionListIndex = 0; private set
    var sessionListOffset = 0; private set
    var sessionScrollReset by mutableStateOf(0L); private set
    private var unscopedScroll = 0 to 0
    private var unscopedFilter = SessionFilter.All
    private var unscopedQuery = ""
    val archiveMachines get() = machines.filter { (selectedMachines.isEmpty() || MachineKey(it.connectionId,it.id) in selectedMachines) }
    val archiveSupportedMachines get() = archiveMachines.filter { it.online && "archive_list" in machineOperations[MachineKey(it.connectionId,it.id)].orEmpty() && "archive_list" in relayOperations[it.connectionId].orEmpty() }
    fun configureArchive() { archive.configure(archiveSupportedMachines.map { MachineKey(it.connectionId,it.id) }.toSet(),sessionQuery.trim(),sessionFilter==SessionFilter.Archive && !demo) }
    private suspend fun loadArchivePage(key:MachineKey,query:JSONObject):ArchivePage {
        val connection=connections.find { it.id==key.connectionId } ?: error("Workspace disconnected")
        val id=UUID.randomUUID().toString()
        val initial=api.post(connection,key.computerId,"","archive_list",query,id,SendAttempt())
        check(initial.string("request_id")==id){"Gateway returned another archive receipt"}
        val receipt=api.await(connection,initial,150,200)
        check(receipt.string("request_id")==id && receipt.string("state")=="completed"){receipt.string("error").ifBlank{"Archive page is not available. Refresh to retry."}}
        val result=receipt.getJSONObject("result")
        check(result.string("request_id")==id && result.optInt("version")==1 && result.optLong("from")==query.getLong("from") && result.optLong("until")==query.getLong("until") && result.string("query")==query.string("query")){"Computer returned another archive query"}
        val machine=JSONObject().put("id",key.computerId).put("name",machines.find{it.id==key.computerId&&it.connectionId==key.connectionId}?.name.orEmpty()).put("snapshot",result)
        val rows=NativeParser.sessions(connection,machine)
        check(rows.size==result.getJSONArray("sessions").length() && rows.all { SessionFilters.archived(it) && archive.range.let { r -> it.raw.optDouble("archived_at",-1.0)>=r.from && it.raw.optDouble("archived_at",-1.0)<r.until } }){"Invalid archived session identity"}
        val window=result.has("position") || result.has("previous_cursor")
        if(window)check(result.opt("previous_cursor") is String && result.opt("position") is Number && result.getDouble("position")==result.getInt("position").toDouble()){"Invalid archive window"}
        return ArchivePage(ProjectParser.parse(connection,listOf(machine),rows).sessions,result.getString("next_cursor"),result.getInt("total"),result.optString("previous_cursor",""),if(window)result.getInt("position")else null)
    }
    fun chooseSessionFilter(value: SessionFilter) { sessionFilter = value; resetSessionScroll() }
    fun changeSessionQuery(value: String) { sessionQuery = value; resetSessionScroll() }
    fun toggleMachine(key: MachineKey) { selectedMachines = if (key in selectedMachines) selectedMachines - key else selectedMachines + key; resetSessionScroll() }
    fun allMachines() { selectedMachines = emptySet(); resetSessionScroll() }
    fun toggleProject(key: ProjectKey) { collapsedProjects = if (key.key in collapsedProjects) collapsedProjects - key.key else collapsedProjects + key.key }
    /** Parents whose subagents and child sessions are hidden: one toggle, kept across relaunches (TeamCollapse). */
    var teamCollapse by mutableStateOf(TeamCollapse()); private set
    private var teamWriter: OrderedStatePersistence<List<String>>? = null
    /** The machine UUID of a target's computer, which parent links and collapsed parents name. */
    fun machineUuid(target: Target) = machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.machineId
    fun teamCollapsed(session: Session) = teamCollapse.collapsed(session, machineUuid(session.target))
    fun toggleTeam(session: Session) {
        val collapsed = !teamCollapsed(session)
        val saved = ChildSessions.teamId(machineUuid(session.target), session)?.let { id ->
            teamWriter?.edit { ChildSessions.withTeam(it, id, collapsed) } }
        teamCollapse = teamCollapse.choose(session.target.key, collapsed, saved)
    }
    /** Grouped accounts, which label the cards' account marks. */
    val groupedAccounts by derivedStateOf { AccountGrouping.group(displayedAccounts) }
    fun saveSessionScroll(index: Int, offset: Int, scope: String, reset: Long) {
        if (scope != sessionProjectScope?.key?.key.orEmpty() || reset != sessionScrollReset) return
        sessionListIndex = index; sessionListOffset = offset
    }
    private fun resetSessionScroll(index: Int = 0, offset: Int = 0) { sessionListIndex = index; sessionListOffset = offset; sessionScrollReset++ }
    fun viewProjectSessions(project: ProjectSummary) {
        if (sessionProjectScope == null) { unscopedFilter = sessionFilter; unscopedQuery = sessionQuery; unscopedScroll = sessionListIndex to sessionListOffset }
        sessionProjectScope = project; sessionFilter = SessionFilter.All; sessionQuery = ""; resetSessionScroll()
    }
    fun clearSessionScope() {
        if (sessionProjectScope != null) { sessionProjectScope = null; sessionFilter = unscopedFilter; sessionQuery = unscopedQuery; resetSessionScroll(unscopedScroll.first, unscopedScroll.second) }
    }
    private var continuation by mutableStateOf<CompactContinuation?>(null)
    private data class ClearExpectation(val requestId: String, val target: Target, val navigation: Long, val confirmed: Boolean = false)
    private var clearExpectation: ClearExpectation? = null
    private var continuationEnqueuing = ""
    private var actionFlights by mutableStateOf<Set<String>>(emptySet())
    val sessionActions get() = messageState.sessionActions.map { if(it.requestId !in actionFlights) it.recover() else it }
    private var contextFlights by mutableStateOf<Set<String>>(emptySet())
    private val pendingSettingsAttempts = mutableSetOf<String>()
    private val scheduledSettingsRetry = ScheduledSettingsRetry()
    private val scheduledReadScheduler=HistoryHeadScheduler()
    private val acknowledgedContexts = mutableSetOf<String>()
    private val contextObservations = mutableSetOf<String>()
    // Observable so capability-gated controls (the OpenCode Effort row) appear when discovery finishes.
    private var relayFeatures by mutableStateOf<Map<String,Set<String>>>(emptyMap())
    private var machineFeatures by mutableStateOf<Map<MachineKey,Set<String>>>(emptyMap())
    private var parentSelection:Session?=null
    private var relayOperations by mutableStateOf<Map<String, Set<String>>>(emptyMap())
    private var machineOperations by mutableStateOf<Map<MachineKey, Set<String>>>(emptyMap())
    private var contextNoticeValue by mutableStateOf("")
    private var contextNoticeOwner:String?=null
    var contextNotice:String
        get()=contextNoticeValue
        private set(value) { contextNoticeValue=value;contextNoticeOwner=null }
    private fun setContextNotice(requestId:String,text:String) { if(contextNoticeOwner == requestId) contextNoticeValue=text }
    val contextOperations get() = messageState.contextOperations.map { if (it.requestId !in contextFlights) it.recover() else it }
    val compactContinuationActive get() = continuation?.draft?.target == selected?.target && continuation != null
    fun contextOperation(target: Target) = contextOperations.lastOrNull { ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending }
        ?: contextOperations.lastOrNull { ContextPolicies.sameSessionRun(it.target, target) }
    fun contextBlocked(target: Target) = sessionActions.any { it.target == target && it.blocksSending } || clearExpectation?.let { ContextPolicies.sameSessionRun(it.target, target) } == true || contextOperations.any { ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending }
    // BEGIN QUESTION_AVAILABILITY_PROBE definitions
    private fun questionVerificationProbe(writer: QuestionUiProbe.VerificationWriter) {
        if (!QuestionUiProbe.enabled) return
        try {
            val frame = activeFrame
            QuestionUiProbe.verification(selected?.target, writer, activityVerified, activeFrameVerified,
                frame != null, frame?.presentation != null, frame != null && frame.target == selected?.target)
        } catch (_: Throwable) { QuestionUiProbe.observationFailed() }
    }
    internal fun questionAvailabilityProbe(target: Target, online: Boolean): QuestionUiProbe.Availability? {
        if (!QuestionUiProbe.enabled) return null
        return try {
            val verified = activityVerified
            val preview = demo
            val blocked = contextBlocked(target)
            val actionBlocked = sessionActions.any { it.target == target && it.blocksSending }
            val clearBlocked = clearExpectation?.let { ContextPolicies.sameSessionRun(it.target, target) } == true
            val operationBlocked = contextOperations.any { ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending }
            QuestionUiProbe.Availability(verified, preview, online, blocked, actionBlocked, clearBlocked,
                operationBlocked, selected?.target == target, online && verified && !blocked)
        } catch (_: Throwable) { QuestionUiProbe.observationFailed(); null }
    }
    // END QUESTION_AVAILABILITY_PROBE definitions
    private var messageState by mutableStateOf(MessageState())
    private val messageRepository = MessagePersistence.get(application)
    private var persistence: OrderedStatePersistence<MessageState>? = null
    private var readWriter:OrderedStatePersistence<ConversationReadState>?=null
    private var readState by mutableStateOf(ConversationReadState())
    private var readTrackingReady by mutableStateOf(false)
    var readPositionReady by mutableStateOf(false);private set
    var firstUnreadEventId by mutableStateOf("");private set
    private var unreadBoundaryCaptured=false
    // Marks submitted by this process; a failed submission is forgotten so the next view retries.
    private val sharedReadsSent=mutableSetOf<String>()
    // Marks completed without being applied wait in one bounded table, released by one owned timer.
    private val sharedReadRetries=SharedReadRetries()
    private var sharedReadRetryTimer:Job?=null
    private var readAllShares:Job?=null
    fun conversationViewport(target:Target)=readState.record(target)?.viewport
    fun saveConversationViewport(target:Target,eventId:String,offset:Int,following:Boolean=false) {
        if(selected?.target != target || activeFrame?.target != target || conversationEvents.none { it.id == eventId }) return
        val event=conversationEvents.find { it.id==eventId } ?: return
        readWriter?.let { readState=it.edit { state -> state.viewport(target,eventId,offset,event.historyCursor,event.historyEpoch,following) } }
        if(event.historyCursor.isNotBlank()) historyWindows[target.key]?.let { window ->
            viewportPageJob?.cancel();viewportPageJob=viewModelScope.launch {
                delay(250)
                runCatching {
                    val retained=withContext(Dispatchers.Default) { HistoryPage.retained(window,event.historyCursor) to HistoryPage.neighbors(window,event.historyCursor) }
                    retained.first?.let { pageCache.write(target,"around",event.historyCursor,it) }
                    retained.second.forEach { (direction,cursor,raw) -> pageCache.write(target,direction,cursor,raw) }
                }
            }
        }
    }
    private fun visibleReadRows(target:Target,eventIds:Set<String>):List<Event> {
        val visible=conversationEvents.filter { it.id in eventIds }
        val window=historyWindows[target.key]?.takeIf { it.target==target }
            ?: return visible.filter(ConversationReadPolicies::incoming)
        return ConversationReadPolicies.incomingThroughPosition(readState.record(target)?.epoch.orEmpty(),window.events,visible)
    }
    fun visibleReadEvidence(target:Target,eventIds:Set<String>):List<Triple<String,String,Long?>> {
        if(selected?.target!=target || activeFrame?.target!=target) return emptyList()
        return ConversationReadPolicies.visibilityEvidence(visibleReadRows(target,eventIds))
    }
    fun visibleMessages(target:Target,eventIds:Set<String>) {
        if(selected?.target != target || activeFrame?.target != target) return
        val incoming=visibleReadRows(target,eventIds)
        if(incoming.isEmpty()) return
        readWriter?.let { readState=it.edit { state -> state.visible(target,incoming) } };refreshReadAttention();shareRead(target)
    }
    /** Replies this phone has loaded for the conversation: the open window, else a retained page or frame. */
    private fun loadedRows(target:Target):List<Event>? =
        if(selected?.target==target) conversationEvents else historyWindows[target.key]?.events ?: frames[target.key]?.events
    private var readIndex:Pair<ConversationReadState,Map<String,ConversationReadRecord>>?=null
    /** Same result as readState.record(target), keyed once per read state for scope-wide checks. */
    private fun indexedReadRecord(target:Target):ConversationReadRecord? {
        val state=readState
        val index=readIndex?.takeIf { it.first===state }?.second
            ?: HashMap<String,ConversationReadRecord>().also { map -> state.records.forEach { map.putIfAbsent(ConversationReadPolicies.key(it.target),it) } }.also { readIndex=state to it }
        return index[ConversationReadPolicies.key(target)]
    }
    private var sessionIndex:Pair<List<Session>,Map<Target,Session>>?=null
    private fun indexedSession(target:Target):Session? {
        val rows=sessions
        val index=sessionIndex?.takeIf { it.first===rows }?.second
            ?: HashMap<Target,Session>().also { map -> rows.forEach { map.putIfAbsent(it.target,it) } }.also { sessionIndex=rows to it }
        return index[target]
    }
    fun loadedUnreadCount(target:Target):Int? {
        val record=indexedReadRecord(target)
        if(!readTrackingReady) return null
        if(record?.incomplete == true || readState.records.size >= 500 && record == null) return null
        // A saved canonical head may overlap these inspection IDs without a
        // proven alias. It is not a current count of the loaded messages.
        return loadedRows(target)?.count { ConversationReadPolicies.unread(record,it) }
    }
    fun unreadCount(target:Target):Int? {
        val record=indexedReadRecord(target) ?: return null
        if(!readTrackingReady || !record.headComplete || ConversationReadPolicies.key(target) !in currentHistoryHeads) return null
        return record.headIncoming?.let { (it-record.readThrough).coerceIn(0,Int.MAX_VALUE.toLong()).toInt() }
    }
    private fun captureUnreadBoundary(target:Target,events:List<Event>) {
        if(unreadBoundaryCaptured || !readPositionReady) return
        val decision=OpeningUnreadBoundary.decide(readTrackingReady,readState.record(target),events)
        if(!decision.captured) return
        firstUnreadEventId=decision.eventId
        unreadBoundaryCaptured=true
    }
    var olderLoading by mutableStateOf(false);private set
    var olderError by mutableStateOf("");private set
    var hasOlder by mutableStateOf(false);private set
    var hasNewer by mutableStateOf(false);private set
    var historyIndexing by mutableStateOf(false);private set
    var historyEpoch by mutableStateOf("");private set
    var historyComplete by mutableStateOf(false);private set
    private var lastHistoryRequest:Triple<Target,String,String>?=null
    private var lastHistoryWasLatest=false
    fun historyRetryIsLatest(target:Target)=lastHistoryWasLatest && selected?.target==target
    fun historyRetryLabel(target:Target)=if(historyRetryIsLatest(target)) "Retry latest messages" else "Retry history"
    fun retryHistory(target:Target) {
        if(historyRetryIsLatest(target)) { jumpToLatest(target);return }
        lastHistoryRequest?.takeIf { it.first==target }?.let { loadHistory(target,it.second,it.third) } ?: loadHistory(target,"")
    }
    private val historyWindows=LinkedHashMap<String,HistoryWindow>(20,0.75f,true)
    private var currentHistoryHeads by mutableStateOf<Set<String>>(emptySet())
    private val historyFlights=mutableMapOf<String,Pair<Target,Deferred<HistoryPage>>>()
    private val projectionJobs=mutableMapOf<String,Pair<Target,Job>>()
    private val projectionHeads=LinkedHashMap<String,Pair<String,Long>>()
    private var olderJob:Job?=null
    private var viewportPageJob:Job?=null
    private var historyHeadJob:Job?=null
    private val historySnapshotHints=mutableMapOf<String,String>()
    private val historyHeadScheduler=HistoryHeadScheduler()
    private var restoreHistoryAttempt=""
    private var pendingHistoryPosition=""
    private var pendingHistoryPublication=""
    private var historyViewIntent=0L
    private var latestNavigationIntent:Long?=null
    private var lastHistoryObservation=0L
    private var lastHistoryReadCompletedAt=0L
    private fun historyNetworkReason(target:Target):String = when {
        target.run.isBlank() || target.conversation.isBlank() -> "History is still starting."
        connections.none { it.id==target.connectionId } -> "No connection. Showing saved conversation."
        target.connectionId !in relayOperations || MachineKey(target.connectionId,target.computerId) !in machineOperations -> "Connect to check earlier history availability."
        "history" !in relayOperations[target.connectionId].orEmpty() || "history" !in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty() -> "This computer does not support earlier history yet."
        else -> ""
    }
    fun historyReason(target:Target):String {
        if(target.run.isBlank() || target.conversation.isBlank() || connections.none { it.id==target.connectionId }) return historyNetworkReason(target)
        if(readState.record(target)?.epoch?.isNotBlank()==true || historyWindows.containsKey(target.key)) return ""
        return historyNetworkReason(target)
    }
    private suspend fun historyOnce(target:Target,direction:String,cursor:String,limit:Int=100):HistoryPage {
        val key=target.key+JSONArray(listOf(direction,cursor,limit))
        historyFlights[key]?.let { return it.second.await() }
        while(historyFlights.size>=4) { runCatching { historyFlights.values.first().second.await() };kotlinx.coroutines.currentCoroutineContext().ensureActive();historyFlights.entries.removeAll { it.value.second.isCompleted } }
        val connection=connections.find { it.id==target.connectionId } ?: error("Workspace disconnected.")
        val flight=viewModelScope.async {
            val reason=historyNetworkReason(target)
            if(reason.isNotBlank()) throw java.io.IOException(reason)
            val page=api.history(connection,target,direction,cursor,limit)
            projectionHeads[target.key]=page.epoch to page.projectionApplied
            while(projectionHeads.size>20) projectionHeads.remove(projectionHeads.keys.first())
            scheduleProjection(target)
            viewModelScope.launch { runCatching { HistoryPageRetention.retain(limit,page,direction) { pageCache.write(target,direction,cursor,it.raw) } } }
            page
        }
        historyFlights[key]=target to flight
        flight.invokeOnCompletion { viewModelScope.launch { if(historyFlights[key]?.second===flight) historyFlights.remove(key) } }
        return flight.await()
    }
    private fun observeHistoryHead(page:HistoryPage,verified:Boolean=true) {
        if(connections.none { it.id==page.target.connectionId }) return
        readWriter?.let { readState=it.edit { state -> ConversationReadHead.observe(state,page) } }
        if(page.complete && verified) currentHistoryHeads+=ConversationReadPolicies.key(page.target) else currentHistoryHeads-=ConversationReadPolicies.key(page.target)
        refreshReadAttention()
    }
    private fun publishWindow(target:Target,page:HistoryWindow,complete:Boolean=false,indexing:Boolean=false,boundary:String?=null,aliases:Map<String,String> = emptyMap()) {
        // A page reaching the head shows the latest inspection's inbox rows, never an older copy.
        val inspected=(frames[target.key] ?: activeFrame?.takeIf { it.target==target })?.inspectedEvents
        val window=if(page.atHead && inspected!=null) page.copy(inbox=HistoryInspection.inbox(inspected)) else page
        historyWindows[target.key]=window
        while(historyWindows.size>20) historyWindows.remove(historyWindows.keys.first())
        scheduleProjection(target)
        if(selected?.target != target) return
        if(!readPositionReady) pendingHistoryPosition=target.key
        pendingHistoryPublication=target.key
        val head=frames[target.key] ?: activeFrame?.takeIf { it.target==target }
        if (head == null) {
            // The opening history and inspection run independently. Retain this
            // verified page's metadata until the matching inspection arrives.
            historyComplete=complete;historyIndexing=indexing
            return
        }
        installFrame(head.copy(events=window.displayed,presentation=null,truncated=head.truncated || window.hasBefore || window.hasAfter,window=window,indexComplete=complete,indexing=indexing,unreadBoundary=boundary,supersededIds=aliases),activityVerified)
    }
    /** Projection responses are display evidence only: never call observeHistoryHead,
     * readWriter.supersede, installFrame or the ordinary delivery-settling presentation path. */
    private fun scheduleProjection(target:Target) {
        val window=historyWindows[target.key] ?: return
        val head=projectionHeads[target.key]?.takeIf { it.first==window.epoch } ?: return
        if(head.second<=window.projectionApplied || target.key in projectionJobs || projectionJobs.size>=4) return
        fun supported()=connections.any { it.id==target.connectionId } &&
            HistoryProjection.supported(relayOperations[target.connectionId],machineOperations[MachineKey(target.connectionId,target.computerId)])
        if(!supported()) return
        val epoch=window.epoch;val start=window.projectionApplied;val until=head.second
        val job=viewModelScope.launch(start=kotlinx.coroutines.CoroutineStart.LAZY) {
            var progress=HistoryProjection.Run(start,until)
            try {
                while(progress.canRead) {
                    if(!supported() || historyWindows[target.key]?.epoch!=epoch) return@launch
                    val connection=connections.firstOrNull { it.id==target.connectionId } ?: return@launch
                    val page=api.historyProjection(connection,target,epoch,progress.after,until)
                    val applied=page.applied(progress.after)
                    val initial=historyWindows[target.key] ?: return@launch
                    val candidate=withContext(Dispatchers.Default) { HistoryProjection.apply(initial,target,epoch,start,page.prefix,applied) }
                    val rebased=HistoryInspection.rebase(initial,candidate,{ historyWindows[target.key] }) { latest ->
                        withContext(Dispatchers.Default) { latest?.let { HistoryProjection.apply(it,target,epoch,start,page.prefix,applied) } }
                    }
                    val next=rebased.value ?: return@launch
                    if(!supported()) return@launch
                    historyWindows[target.key]=next
                    // Reprepare the visible rows without settling outbox, changing the divider or read metadata.
                    val frame=activeFrame?.takeIf { it.target==target && selected?.target==target }
                    if(frame!=null) {
                        val revision=presentationRevision
                        val messages=outgoing.filter { it.belongsTo(target) }
                        val questions=conversationQuestions
                        val display=withContext(Dispatchers.Default) { if(target.archiveId.isBlank()) ConversationMerge.merge(target,next.displayed,messages).events else next.displayed }
                        if(activeFrame===frame && historyWindows[target.key]===next && selected?.target==target &&
                            presentationRevision==revision && outgoing.filter { it.belongsTo(target) }==messages && conversationQuestions==questions) {
                            presentationJob?.cancel();presentationRevision++
                            val updated=frame.copy(events=next.displayed,window=next,presentation=Presentation(messages,display,questions))
                            activeFrame=updated;rememberFrame(updated)
                            conversationEvents=display
                        }
                    }
                    // Rows and their coverage marker are always serialized in one encrypted page value.
                    val latest=historyWindows[target.key]?.takeIf { it.epoch==epoch } ?: return@launch
                    val saved=readState.record(target)?.viewport?.cursor.orEmpty()
                    val anchor=saved.takeIf { cursor -> latest.events.any { it.historyCursor==cursor } }
                        ?: latest.events.lastOrNull { it.historyCursor.isNotBlank() }?.historyCursor
                    if(anchor!=null) {
                        val raw=withContext(Dispatchers.Default) { HistoryPage.retained(latest,anchor) }
                        if(raw!=null) pageCache.write(target,"around",saved.ifBlank { anchor },raw)
                    }
                    progress=progress.accepted(page)
                }
            } catch(error:CancellationException) { throw error }
            catch(_:Exception) { /* A later verified history page can resume the unchanged display prefix. */ }
            finally { if(projectionJobs[target.key]?.second===kotlin.coroutines.coroutineContext[Job]) projectionJobs.remove(target.key) }
        }
        projectionJobs[target.key]=target to job;job.start()
    }
    private fun loadHistory(target:Target,direction:String,cursor:String="",latest:Boolean=false):Job? {
        if(selected?.target != target || olderLoading) return null
        if(target.run.isBlank() || target.conversation.isBlank() || connections.none { it.id==target.connectionId }) { olderError=historyReason(target);return null }
        val navigation=navigationId
        val intent=historyViewIntent
        lastHistoryWasLatest=false
        lastHistoryRequest=Triple(target,direction,cursor)
        olderLoading=true;olderError=""
        if(direction in setOf("","around","unread") && !unreadBoundaryCaptured) readPositionReady=false
        olderJob=viewModelScope.launch {
            var published=false
            try {
                val cursorEpoch=when(direction) { "before","after" -> historyWindows[target.key]?.epoch;"around" -> readState.record(target)?.viewport?.epoch;else -> null }
                val reopened=HistoryReadPaths.reopening(cursorEpoch,{ HistoryReadPaths.read(target,network={ historyOnce(target,direction,cursor) },cached={ pageCache.read(target,direction,cursor) }) }) { historyOnce(target,"","") }
                val result=reopened.result;val mode=if(reopened.head) "" else direction
                val page=result.page;val verifiedPage=result.verified
                if(!verifiedPage) currentHistoryHeads-=ConversationReadPolicies.key(target)
                val knownIds=readState.record(target)?.let { it.seen.toSet()+it.viewport?.eventId.orEmpty()+firstUnreadEventId }.orEmpty()
                val aliases=withContext(Dispatchers.Default) { RollingConversationHistory.merge(target,target,activeFrame?.takeIf { it.target==target }?.events.orEmpty(),page.events).supersededIds+HistoryAliases.exact(page.events,knownIds) }
                if(aliases.isNotEmpty()) readWriter?.let { readState=it.edit { state -> state.supersede(target,aliases) } }
                observeHistoryHead(page,verifiedPage)
                if(selected?.target != target || navigationId != navigation || historyViewIntent!=intent) return@launch
                if(verifiedPage && page.complete) lastHistoryReadCompletedAt=System.currentTimeMillis()
                if(mode=="" && verifiedPage && !page.complete) {
                    androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                        historyIndexing=page.indexing;historyComplete=false;olderLoading=false;readPositionReady=true
                        hasOlder=true
                        olderError=if(page.indexing) "Earlier history is still indexing. Recent messages remain available. Retry to continue."
                            else "This computer could not provide earlier history. Recent messages remain available. Retry to continue."
                        captureUnreadBoundary(target,conversationEvents)
                    }
                    return@launch
                }
                val marker=readState.record(target)
                val earliest=page.events.filter(ConversationReadPolicies::incoming).mapNotNull { it.incomingSeq }.minOrNull()
                val saved=marker?.viewport
                val noSavedAnchor=saved==null || saved.epoch!=page.epoch || saved.cursor.isBlank()
                val unreadBeforePage=page.latestIncoming!=null && marker!=null && page.latestIncoming>marker.readThrough && page.hasBefore && (earliest==null || earliest>marker.readThrough+1)
                if(!latest && mode=="" && verifiedPage && page.complete && marker?.established==true && noSavedAnchor && unreadBeforePage && !unreadBoundaryCaptured) {
                    readPositionReady=false
                    val unread=historyOnce(target,"unread",JSONArray(listOf(page.epoch,marker!!.readThrough)).toString())
                    observeHistoryHead(unread)
                    if(selected?.target!=target || navigationId!=navigation || historyViewIntent!=intent) return@launch
                    publishWindow(target,HistoryWindow.from(unread),unread.complete,unread.indexing);published=true
                    lastHistoryObservation=activeFrame?.observedAt ?: 0L
                    return@launch
                }
                val old=historyWindows[target.key]
                val window=withContext(Dispatchers.Default) { if(mode in setOf("before","after") && old?.epoch==page.epoch) old.merge(page,mode) else HistoryWindow.from(page) }
                publishWindow(target,window,page.complete && verifiedPage,page.indexing,aliases[firstUnreadEventId],aliases);lastHistoryObservation=activeFrame?.observedAt ?: 0L;published=true
            } catch(e:CancellationException) { throw e }
            catch(e:Exception) { if(selected?.target==target && navigationId==navigation && historyViewIntent==intent) { olderError=ReadFailure.message(e,"conversation");currentHistoryHeads-=ConversationReadPolicies.key(target) } }
            finally { if(!published && selected?.target==target && navigationId==navigation && historyViewIntent==intent) { olderLoading=false;if(pendingHistoryPosition!=target.key) readPositionReady=true } }
        }
        return olderJob
    }
    fun loadOlder(target:Target) {
        val window=historyWindows[target.key]
        if(window==null) loadHistory(target,"") else if(window.hasBefore && window.before!=null) loadHistory(target,"before",window.before)
    }
    fun loadNewer(target:Target) { historyWindows[target.key]?.takeIf { it.hasAfter && it.after!=null }?.let { loadHistory(target,"after",it.after!!) } }
    /** The recent native inspection is independent of historical indexing and exact unread totals. */
    fun jumpToLatest(target:Target):Job? {
        if(selected?.target!=target) return null
        val connection=connections.find { it.id==target.connectionId } ?: run { olderError="Workspace disconnected. Showing saved conversation.";return null }
        val navigation=navigationId
        val intent=++historyViewIntent
        val previous=olderJob
        latestNavigationIntent=intent
        lastHistoryWasLatest=true
        // Retire the old view's observers, retaining every shared native read's
        // original identity. They may finish but cannot publish an older window.
        detailGate.cancel();detailJob?.cancel();detailTimer?.cancel()
        detailBusy=false;detailProgress=false
        presentationRevision++;presentationJob?.cancel()
        previous?.cancel()
        olderLoading=true;olderError=""
        olderJob=viewModelScope.launch {
            fun current()=selected?.target==target && navigationId==navigation && historyViewIntent==intent && connections.any { it==connection }
            try {
                previous?.join()
                if(!current()) return@launch
                val inspected=inspectOnce(connection,target,fresh=true,current=::current).await()
                check(inspected.target==target) { "The conversation changed. Refresh before loading its latest messages." }
                if(!current()) return@launch
                val recent=withContext(Dispatchers.Default) {
                    val bounded=RollingConversationHistory.merge(target,target,emptyList(),inspected.inspectedEvents)
                    inspected.copy(events=bounded.events,window=null,indexComplete=false,indexing=false,presentation=null,
                        truncated=inspected.truncated || bounded.truncated)
                }
                if(!current()) return@launch
                check(recent.events.isNotEmpty() || conversationEvents.isEmpty()) {
                    "The computer returned no recent messages. The loaded conversation was kept."
                }
                // Explicit latest navigation leaves the older canonical window. Its
                // cursors remain in the page cache, never on these provisional rows.
                historyWindows.remove(target.key)
                currentHistoryHeads-=ConversationReadPolicies.key(target)
                historyEpoch="";historyComplete=false;historyIndexing=false;hasNewer=false
                recent.events.lastOrNull()?.let { last ->
                    readWriter?.let { readState=it.edit { state -> state.viewport(target,last.id,0,following=true) } }
                }
                restoreHistoryAttempt=target.key+readState.record(target)?.viewport?.cursor.orEmpty()
                pendingHistoryPublication=target.key
                rememberFrame(recent)
                installFrame(recent,true)
                // The navigation caller must receive the installed rows and paging
                // flags before choosing the scroll target, including identical reads.
                presentationJob?.join()
            } catch(error:CancellationException) { throw error }
            catch(error:Exception) { if(current()) olderError=ReadFailure.message(error,"conversation") }
            finally {
                if(latestNavigationIntent==intent) latestNavigationIntent=null
                if(current()) { olderLoading=false;readPositionReady=true }
            }
        }
        return olderJob
    }
    private fun restoreSavedWindow(target:Target,navigation:Long) {
        val saved=readState.record(target)?.viewport ?: return
        if(saved.following) return
        if(saved.cursor.isBlank()) return
        val originalWindow=historyWindows[target.key]
        val intent=historyViewIntent
        readPositionReady=false
        viewModelScope.launch {
            var published=false
            try {
                val page=HistoryReadPaths.saved(target) { pageCache.read(target,"around",saved.cursor) } ?: return@launch
                if(selected?.target==target && navigationId==navigation && historyViewIntent==intent && readState.record(target)?.viewport==saved &&
                    historyWindows[target.key]===originalWindow && conversationEvents.none { it.id==saved.eventId }) {
                    publishWindow(target,HistoryWindow.from(page));restoreHistoryAttempt=target.key+saved.cursor;published=true
                }
            } finally { if(!published && selected?.target==target && navigationId==navigation && historyViewIntent==intent &&
                readState.record(target)?.viewport==saved && !olderLoading && pendingHistoryPublication!=target.key) readPositionReady=true }
        }
    }
    private fun watchHistoryHeads(priority:Target?=null) {
        // Catalog-wide probes yield the serial relay queue to the visible read.
        // Explicit read-evidence preparation keeps its existing priority path.
        if(priority==null && selected!=null) return
        if(historyHeadJob?.isActive==true || !readTrackingReady) return
        historyHeadJob=viewModelScope.launch {
            val rows=sessions;val state=readState;val hosts=machines;val workspaces=connections
            val relay=relayOperations;val native=machineOperations
            val hints=historySnapshotHints.toMap();val heads=currentHistoryHeads
            val candidates=withContext(Dispatchers.Default) {
                val connected=workspaces.mapTo(hashSetOf()) { it.id }
                val online=hosts.filter { it.online }.mapTo(hashSetOf()) { MachineKey(it.connectionId,it.id) }
                val eligible=rows.filter { row ->
                    val target=row.target;val host=MachineKey(target.connectionId,target.computerId)
                    target.run.isNotBlank() && target.conversation.isNotBlank() && target.connectionId in connected && host in online &&
                        "history" in relay[target.connectionId].orEmpty() && "history" in native[host].orEmpty() && (priority==null || target==priority)
                }
                ReadAllHeadCandidates.candidates(eligible,state).mapNotNull { row ->
                    val hint=ReadAllHeadCandidates.hint(row)
                    if(hints[ConversationReadPolicies.key(row.target)]==hint && ConversationReadPolicies.key(row.target) in heads) null else row.target to hint
                }
            }
            if(rows!==sessions || state!==readState || hosts!==machines || workspaces!==connections || relay!==relayOperations || native!==machineOperations || heads!==currentHistoryHeads) return@launch
            candidates.forEach { currentHistoryHeads-=ConversationReadPolicies.key(it.first) }
            val chosen=historyHeadScheduler.choose(candidates.map { it.first },android.os.SystemClock.elapsedRealtime())
            if(chosen.isEmpty()) return@launch
            // Continue only when this pass selected work; an empty catalog must settle.
            if(priority==null) coroutineContext[Job]?.invokeOnCompletion { cause -> if(cause==null) viewModelScope.launch { watchHistoryHeads() } }
            chosen.forEach { target ->
                // Navigation may occur while the prior retained probe is in flight.
                if(priority==null && selected!=null) return@launch
                val result=runCatching {
                    val page=historyOnce(target,"","",1)
                    if(sessions.none { it.target==target } || historyNetworkReason(target).isNotBlank() ||
                        machines.none { it.connectionId==target.connectionId && it.id==target.computerId && it.online }) return@runCatching
                    observeHistoryHead(page,verified=priority!=null || selected?.target!=target || historyWindows[target.key]!=null)
                    // A retained metadata probe may finish after navigation. It
                    // never enters canonical history on behalf of a recent view.
                    if(selected?.target==target && historyWindows[target.key]==null && page.indexing) historyIndexing=true
                    historySnapshotHints[ConversationReadPolicies.key(target)]=candidates.first { it.first==target }.second
                    adoptSharedReads();if(selected?.target==target) shareRead(target)
                }
                historyHeadScheduler.result(target,android.os.SystemClock.elapsedRealtime(),result.isSuccess)
            }
        }
    }
    private fun restoreHistory(target:Target) {
        if(historyNetworkReason(target).isNotBlank() || olderLoading) return
        val saved=readState.record(target)?.viewport
        val key=target.key+saved?.cursor.orEmpty()
        if(restoreHistoryAttempt==key) return
        restoreHistoryAttempt=key
        if(saved?.following==false && saved.cursor.isNotBlank() && conversationEvents.none { it.id==saved.eventId }) {
            readPositionReady=false;loadHistory(target,"around",saved.cursor)
        }
    }
    fun reviewLater(target:Target)=indexedReadRecord(target)?.reviewLater==true
    fun captureReadAttention(target:Target):ReadAttentionEvidence {
        val record=indexedReadRecord(target)
        val raw=if(selected?.target==target) activity else indexedSession(target)?.raw
        val fingerprint=HistoryHeadHints.readFingerprint(target,raw,record)
        val rows=loadedRows(target).orEmpty()
        return ReadAttentionEvidence(target,fingerprint,record?.epoch.orEmpty(),record?.headIncoming,
            readTrackingReady && record?.headComplete==true && ConversationReadPolicies.key(target) in currentHistoryHeads,
            rows.filter(ConversationReadPolicies::incoming).map { it.id }.toSet(),
            rows.filter { ConversationReadPolicies.incoming(it) && record?.epoch?.isNotBlank()==true && it.historyEpoch==record.epoch }.mapNotNull { it.incomingSeq }.maxOrNull())
    }
    fun markRead(target:Target,evidence:ReadAttentionEvidence) {
        if(evidence.target!=target || !readTrackingReady || captureReadAttention(target)!=evidence) { error="The displayed reply evidence changed. Review it before marking read.";return }
        readWriter?.let { readState=it.edit { state -> ReadAttentionPolicies.mark(state,evidence) } };refreshReadAttention();shareRead(target)
        // The explicit choice covers what the open conversation's verified inspection names now.
        currentRead(target)?.let { shareCurrent(target,it,replyShown=true,attentionShown=true) }
        flushDrafts()
    }
    private fun readAllEvidence(target:Target):ReadAttentionEvidence? {
        if(!readTrackingReady || readWriter==null || demo || target.run.isBlank() || target.conversation.isBlank() ||
            connections.none { it.id==target.connectionId } ||
            indexedSession(target)==null && (selected?.target!=target || activeFrame?.target!=target)) return null
        val captured=captureReadAttention(target)
        val row=indexedSession(target)
        val currentHint=row?.let(ReadAllHeadCandidates::hint)
        val observedHint=historySnapshotHints[ConversationReadPolicies.key(target)]
        val hintMatches=currentHint!=null && observedHint==currentHint
        val freshHead=captured.authoritative && hintMatches && historyNetworkReason(target).isBlank() &&
            machines.any { it.connectionId==target.connectionId && it.id==target.computerId && it.online }
        return captured.copy(fingerprint=captured.fingerprint+JSONArray(listOf(currentHint,observedHint)).toString(),authoritative=freshHead)
    }
    fun canMarkRead(target:Target):Boolean=ReadAllPolicies.available(readAllEvidence(target))
    /** Something Read all could clear here once it has usable evidence: a reminder or replies counted unread. */
    fun readAllPending(target:Target):Boolean=readTrackingReady && !demo && ReadAllPolicies.pending(indexedReadRecord(target),loadedRows(target).orEmpty())
    /** Read all is enabled only when its current evidence would clear something this phone shows. */
    fun canReadAll(target:Target):Boolean {
        if(!readAllPending(target)) return false
        val record=indexedReadRecord(target)
        return ReadAllPolicies.changes(record,readAllEvidence(target),loadedRows(target).orEmpty(),record==null && readState.records.size>=500)
    }
    fun listReadAllCandidate(target:Target):Boolean = indexedSession(target)?.let { row ->
        ReadAllPolicies.listCandidate(row,SessionFilters.online(row,machines),reviewLater(target))
    } == true
    /** Whether marking this one conversation would use only loaded replies rather than a fresh complete head. */
    fun readAllPartial(target:Target):Boolean=readAllEvidence(target)?.authoritative!=true
    /** Clears only this phone's reminder; read markers stay and nothing is shared with the computer. */
    fun clearReviewLater(target:Target) {
        if(!readTrackingReady || demo || !reviewLater(target)) return
        readWriter?.let { readState=it.edit { state -> state.review(target,false) } };refreshReadAttention();flushDrafts()
    }
    /** Opening a session's actions asks the bounded head probe to check that row first. */
    fun prepareReadEvidence(target:Target) { if(!demo) watchHistoryHeads(target) }
    fun markAllRead(targets:List<Target>,visibleRepliesOnly:Boolean=false):ReadAllResult {
        // A list's captured scope can outlive its composition. Recheck visibility
        // before capturing read evidence; incoming work must not be silently cleared.
        val distinct=targets.distinctBy(ConversationReadPolicies::key).filter { !visibleRepliesOnly || listReadAllCandidate(it) }
        val captured=distinct.map { ReadAllCapture(it,readAllEvidence(it)) }
        val current=distinct.map { ReadAllCapture(it,readAllEvidence(it)) }
        val writer=readWriter ?: return ReadAllResult(skipped=distinct.size)
        var result=ReadAllResult(skipped=distinct.size)
        readState=writer.edit { state -> ReadAllPolicies.apply(state,captured,current).also { result=it.result }.state }
        refreshReadAttention();writer.flushAsync()
        // One mark_read at a time, after any earlier Read all, so a large scope never floods the
        // relay or the computer's queue ahead of the person's own requests.
        val previous=readAllShares
        readAllShares=viewModelScope.launch {
            previous?.join()
            SharedReadPolicies.inOrder(distinct,{ target -> sharedReadRequest(target,false)?.takeIf { sharedReadsSent.add(it.key) } }) { sendSharedRead(it) }
        }
        return result
    }
    fun reviewLater(target:Target,evidence:ReadAttentionEvidence) {
        if(evidence.target!=target || !readTrackingReady || captureReadAttention(target)!=evidence) { error="The displayed reply evidence changed. Review it again.";return }
        readWriter?.let { readState=it.edit { state -> state.review(target,true) } };refreshReadAttention();flushDrafts()
    }
    private var readAttentionJob:Job?=null
    private fun refreshReadAttention() {
        // Retire the opening divider once this exact current head is read.
        // Keep its capture latched so a later reply cannot resurrect an old divider.
        selected?.target?.let { if(unreadCount(it)==0) { firstUnreadEventId="";unreadBoundaryCaptured=true } }
        readAttentionJob?.cancel()
        val rows=catalogSessions;val state=readState;val ready=readTrackingReady;val heads=currentHistoryHeads
        // Retained windows/frames are bounded to twenty conversations. Copy references on main;
        // workers never read the mutable access-ordered caches or the open conversation state.
        val loaded=loadedReadRows()
        readAttentionJob=viewModelScope.launch {
            val prepared=withContext(Dispatchers.Default) { SessionReadAttention.prepare(rows,state,ready,heads,loaded) }
            if(catalogSessions===rows && readState===state && currentHistoryHeads===heads) catalogSessions=prepared
        }
    }
    private fun loadedReadRows():Map<Target,List<Event>> = frames.values.associate { it.target to it.events }.toMutableMap().also { loaded ->
        historyWindows.values.forEach { loaded[it.target]=it.events }
        selected?.target?.let { loaded[it]=conversationEvents }
    }
    private class SharedReadRequest(val connection:Connection,val target:Target,val payload:JSONObject,val key:String)
    /** Whether this target's gateway and computer accept `mark_read` now. */
    private fun sharedReadAvailable(target:Target)=!demo && readTrackingReady &&
        "mark_read" in relayOperations[target.connectionId].orEmpty() &&
        "mark_read" in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty()
    /** The C4 `mark_read` this phone may send now; [acknowledge] also covers the current request. */
    private fun sharedReadRequest(target:Target,acknowledge:Boolean):SharedReadRequest? {
        if(!sharedReadAvailable(target)) return null
        val row=sessions.find { it.target==target } ?: return null
        val payload=SharedReadPolicies.payload(row,readAllEvidence(target)?.authoritative==true,unreadCount(target),acknowledge) ?: return null
        val connection=connections.find { it.id==target.connectionId } ?: return null
        return SharedReadRequest(connection,target,payload,target.key+"\n"+payload)
    }
    private suspend fun sendSharedRead(request:SharedReadRequest) {
        fun observed(failure:Throwable?,completed:Boolean,applied:Boolean,retryDue:Long?) {
            val wall=System.currentTimeMillis()
            val retryAt=retryDue?.let { wall+(it-android.os.SystemClock.elapsedRealtime()).coerceAtLeast(0) }
            noteSharedRead(request.target,ConversationSyncStatus.sharedReadOutcome(wall,failure,completed,applied,retryAt))
        }
        val receipt=try { api.markRead(request.connection,request.target,request.payload) }
            catch(e:Exception) { sharedReadsSent.remove(request.key);if(e is CancellationException) throw e;observed(e,false,false,null);return }
        if(receipt.string("state")!="completed") { sharedReadsSent.remove(request.key);observed(null,false,false,null);return }
        if(SharedReadPolicies.applied(receipt,request.payload)) { sharedReadRetries.applied(request.key);observed(null,true,true,null);return }
        // Completed without applying: the same mark may go again only after a delay, without holding Read all.
        // At capacity it stays sent, so nothing is ever resent beyond the bound.
        val due=sharedReadRetries.unapplied(request.key,request.target.connectionId,android.os.SystemClock.elapsedRealtime())
        observed(null,true,false,due)
        if(due!=null) scheduleSharedReadRetries()
    }
    /**
     * The one owned timer: it wakes at the earliest due mark and lets exactly the released marks go again.
     * Rescheduling cancels it, and a late wakeup releases only marks that are due by the monotonic clock.
     */
    private fun scheduleSharedReadRetries() {
        sharedReadRetryTimer?.cancel()
        sharedReadRetryTimer=viewModelScope.launch { sharedReadRetries.releaseWhenDue(android.os.SystemClock::elapsedRealtime) { sharedReadsSent.remove(it) } }
    }
    /** zerus-a4mub.9: what the open conversation's verified inspection lets this phone acknowledge now. */
    private fun currentRead(target:Target):CurrentRead? =
        if(selected?.target!=target || activeFrame?.target!=target) null else SharedReadPolicies.current(activity,target,activityVerified)
    /** Changes with the current reply and attention values, so a visibility dwell restarts for new ones. */
    fun currentReadKey(target:Target):String=currentRead(target)?.let(::currentReadKey).orEmpty()
    private fun currentReadKey(current:CurrentRead)=JSONArray(listOf(current.reply,current.attention,current.attentionComplete)).toString()
    /** The newest row the open conversation displays, while it shows the latest messages. */
    fun newestDisplayedEventId(target:Target):String=
        if(selected?.target!=target || activeFrame?.target!=target || hasNewer) "" else conversationEvents.lastOrNull()?.id.orEmpty()
    /**
     * After the settled visibility dwell, acknowledges what the open conversation showed of its current
     * inspection: the reply once its tail or newest row was visible, the attention once its questions were
     * (without questions, once the same tail or newest row was). [key] is [currentReadKey] as the dwell saw it.
     */
    fun visibleCurrent(target:Target,key:String,flags:ConversationViewport.CurrentFlags) {
        val current=currentRead(target) ?: return
        if(key!=currentReadKey(current)) return
        val (replyShown,attentionShown)=SharedReadPolicies.currentShown(latest=!hasNewer,
            questionsListed=conversationQuestions.isNotEmpty(),flags=flags)
        shareCurrent(target,current,replyShown,attentionShown)
    }
    private fun shareCurrent(target:Target,current:CurrentRead,replyShown:Boolean,attentionShown:Boolean) {
        if(!sharedReadAvailable(target)) return
        val payload=SharedReadPolicies.currentPayload(sessions.find { it.target==target },target,current,replyShown,attentionShown) ?: return
        val connection=connections.find { it.id==target.connectionId } ?: return
        launchSharedRead(SharedReadRequest(connection,target,payload,target.key+"\n"+payload))
    }
    /**
     * Sends one prepared mark once. The canonical and current acknowledgements share this path, so their
     * delivery, retries and observed status (zerus-a4mub.10) stay one.
     */
    private fun launchSharedRead(request:SharedReadRequest) {
        if(!sharedReadsSent.add(request.key)) return
        noteSharedRead(request.target,SharedReadStatus.Pending(System.currentTimeMillis()))
        viewModelScope.launch { sendSharedRead(request) }
    }
    /** Tell the session's computer that this phone showed its current reply (C4 `mark_read`). */
    private fun shareRead(target:Target) {
        sharedReadRequest(target,true)?.let(::launchSharedRead)
    }
    /** A reply read on another device advances this phone's watermark, using only a fresh exact head. */
    private fun adoptSharedReads() {
        val writer=readWriter ?: return
        if(demo || !readTrackingReady) return
        val captured=sessions.filter { SharedReadPolicies.canAdoptReply(it,selected?.target,activity,activityVerified) && (unreadCount(it.target) ?: 0)>0 }
            .mapNotNull { row -> readAllEvidence(row.target)?.takeIf { it.authoritative }?.let { ReadAllCapture(row.target,it) } }
        if(captured.isEmpty()) return
        readState=writer.edit { state -> ReadAllPolicies.apply(state,captured,captured).state }
        refreshReadAttention()
    }
    private var preparingTargets by mutableStateOf<Set<String>>(emptySet())
    var attachmentPreparing by mutableStateOf(false); private set
    private var lastOwnSend by mutableStateOf<Pair<Target, String>?>(null)
    fun ownSendId(target: Target): String = lastOwnSend?.takeIf { it.first == target || it.first.resolvesTo(target) }?.second.orEmpty()
    var conversationEvents by mutableStateOf<List<Event>>(emptyList()); private set
    var conversationQuestions by mutableStateOf<List<Question>>(emptyList()); private set
    var conversationOutgoing by mutableStateOf<List<OutgoingMessage>>(emptyList()); private set
    var activityVerified by mutableStateOf(false); private set
    /** When the verified question projection was last installed; `0` means never. */
    var questionCheckedAt by mutableStateOf(0L); private set
    /** The projection's own completeness; an older CLI omits the field and stays complete. */
    var questionProjectionComplete by mutableStateOf(true); private set
    var historyCached by mutableStateOf(false); private set
    /** When the shown conversation's messages were observed (wall clock), null for a saved copy without a time (zerus-a4mub.10). */
    var conversationObservedAt by mutableStateOf<Long?>(null); private set
    /** The open conversation's own last read failed for a lost connection, not a refusal. */
    private var detailReadOffline by mutableStateOf(false)
    /** This conversation's own last read failure, while it is the shown error. Presentation only. */
    fun conversationReadFailure(target: Target): String = error.takeIf { detailReadErrorOwner?.first == target }.orEmpty()
    fun conversationReadOffline(target: Target): Boolean = conversationReadFailure(target).isNotBlank() && detailReadOffline
    /** The session's computer is reported offline by the workspace. */
    fun computerReportedOffline(target: Target): Boolean =
        machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.online == false
    /** Observed shared read outcomes for the most recent conversations; presentation only (zerus-a4mub.10). */
    private var sharedReadStatuses by mutableStateOf<Map<String, SharedReadStatus>>(emptyMap())
    fun sharedReadStatus(target: Target): SharedReadStatus = sharedReadStatuses[target.key] ?: SharedReadStatus.None
    /**
     * The narrow seam for shared read acknowledgement status. The paths that decide and send
     * `mark_read` (zerus-a4mub.9 owns that authority) report their observed outcome here; it is
     * shown and never consulted to allow reading, answering or sending. Bounded to 64 conversations.
     */
    internal fun noteSharedRead(target: Target, status: SharedReadStatus) {
        sharedReadStatuses = (sharedReadStatuses - target.key + (target.key to status)).entries.toList().takeLast(64).associate { it.toPair() }
    }
    var conversationHistoryTruncated by mutableStateOf(false); private set
    private data class Frame(val target: Target, val raw: JSONObject, val events: List<Event>, val questions: List<Question>, val observedAt: Long, val bytes: Int, val truncated: Boolean = false, var presentation: Presentation? = null,val window:HistoryWindow?=null,val indexComplete:Boolean=false,val indexing:Boolean=false,val unreadBoundary:String?=null,val inspectedEvents:List<Event> = emptyList(),val settingsInspectionStartNanos:Long=0,val supersededIds:Map<String,String> = emptyMap())
    internal val conversationSupersededIds:Map<String,String> get() = activeFrame?.supersededIds.orEmpty()
    private data class Presentation(val messages: List<OutgoingMessage>, val events: List<Event>, val questions: List<Question>)
    private data class FrameInstall(val source: Frame, val frame: Frame, val guarded: HistoryWindow?, val changed: Boolean)
    private data class FramePreparation(val install: FrameInstall, val state: MessageState,
        val messages: List<OutgoingMessage>, val window: HistoryWindow?, val revision: Long,
        val complete: Boolean, val indexing: Boolean, val selection: Session, val catalog: List<Session>)
    private val pageCache=ConversationPageCache(application)
    private val historyCache = ConversationCache(application)
    private val frames = LinkedHashMap<String, Frame>(24, 0.75f, true)
    private val inspections = mutableMapOf<String, Pair<Target, Deferred<Frame>>>()
    private var presentationJob: Job? = null
    private var presentationRevision = 0L
    private var activeFrame: Frame? = null
    private var activeFrameVerified = false
    val drafts get() = messageState.drafts
    val terminalBuffers get()=messageState.terminalBuffers
    private var deliveryFlights by mutableStateOf<Set<String>>(emptySet())
    private val outgoingReceiptProbe = OutgoingReceiptProbe()
    private var normalizedOutgoing by mutableStateOf<List<OutgoingMessage>>(emptyList())
    val outgoing get() = normalizedOutgoing
    private val emptyDrafts = mutableMapOf<Triple<Target, String, String>, Draft>()
    val pickerDraft get() = messageState.pickerDraft
    val pickerId get() = messageState.pickerId
    var attachmentImporting by mutableStateOf(false); private set
    private val attachmentStore = AttachmentStore(application)
    val attachments = AttachmentViewer(application, attachmentStore, api, viewModelScope,
        { id -> connections.find { it.id == id } }) { target ->
        AttachmentIdentity.access(relayOperations[target.connectionId], machineOperations[MachineKey(target.connectionId, target.computerId)],
            machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.online)
    }
    internal val computerFiles = ComputerFiles(application, api, viewModelScope,
        { id -> connections.find { it.id == id } }) { target ->
        ComputerFileProtocol.access(relayOperations[target.connectionId], machineOperations[MachineKey(target.connectionId, target.computerId)],
            machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.online)
    }
    private var importJob: Job? = null
    var selected by mutableStateOf<Session?>(null); private set
    var activity by mutableStateOf<JSONObject?>(null); private set
    var busy by mutableStateOf(false); private set
    var detailBusy by mutableStateOf(false); private set
    var catalogBusy by mutableStateOf(false); private set
    var catalogProgress by mutableStateOf(false); private set
    var catalogRefresh by mutableStateOf(CatalogRefreshState()); private set
    var detailProgress by mutableStateOf(false); private set
    private val catalogGate = LoadingGate(android.os.SystemClock::elapsedRealtime)
    private val detailGate = LoadingGate(android.os.SystemClock::elapsedRealtime)
    private var catalogJob: Job? = null
    private var detailJob: Job? = null
    private var catalogTimer: Job? = null
    private var detailTimer: Job? = null
    private var navigationId = 0L
    private var catalogErrorOwner: String? = null
    private var detailReadErrorOwner: Pair<Target,Long>? = null
    private var errorValue by mutableStateOf("")
    var error: String
        get() = errorValue
        private set(value) { errorValue = value; catalogErrorOwner = null;detailReadErrorOwner=null }
    var demo by mutableStateOf(false); private set
    var storageReady by mutableStateOf(false); private set
    var receiptFlights by mutableStateOf<Set<String>>(emptySet()); private set
    var notifications by mutableStateOf(store.notificationEnabled()); private set
    var pushStatuses by mutableStateOf<Map<String, String>>(emptyMap()); private set
    private var pendingNotification: Triple<String, String, String>? = null
    init {
        viewModelScope.launch {
            snapshotFlow { gitStatusCatalog() }
                .collectLatest { gitStatus.refresh() }
        }
        viewModelScope.launch {
            snapshotFlow { SessionCatalogProjection(catalogSessions,archive.rows,machineOperations,relayOperations) }
                .collectLatest { input ->
                    val prepared=withContext(Dispatchers.Default) { input.prepare() }
                    if(input.matches(catalogSessions,archive.rows,machineOperations,relayOperations)) {
                        publishedSessions=prepared
                        if(storageReady && !demo) watchHistoryHeads()
                    }
                }
        }
        viewModelScope.launch {
            try {
                connections = withContext(Dispatchers.IO) { store.connections() }
                pendingEncryptedPairings=withContext(Dispatchers.IO) { connections.filter { it.protocolVersion==2 && runCatching { store.workspaceJournal.load(it).enrollment==null }.getOrDefault(false) }.map { it.id }.toSet() }
                persistence = messageRepository.open()
                accountsWriter=AccountsPersistence.get(application).open()
                accountsWriter!!.edit { rows -> rows.filter { row -> connections.any { it.id==row.key.connectionId } } }
                accountCatalogs=accountsWriter!!.state.value
                launch { accountsWriter!!.state.collectLatest { accountCatalogs=it } }
                try {
                    teamWriter=OrderedStatePersistence(withContext(Dispatchers.IO) { store.collapsedTeams() },viewModelScope,{ store.saveCollapsedTeams(it) })
                    teamCollapse=teamCollapse.copy(stored=teamWriter!!.state.value)
                } catch(_:Exception) { }
                launch { AccountsPersistence.get(application).error.collectLatest { failure -> accountsError=if(failure!=null) "Account snapshots could not be saved. Existing private data is preserved." else "" } }
                try {
                    readWriter=ConversationReadPersistence.get(application).open();readState=readWriter!!.state.value;readTrackingReady=true
                    launch { readWriter!!.state.collectLatest { readState=it;refreshReadAttention() } }
                    launch { ConversationReadPersistence.get(application).error.collectLatest { failure -> if(failure!=null) error="Could not save private reading positions. They remain available here; restart may restore the earlier saved position." } }
                } catch(_:Exception) { error="Private read positions could not be opened. They have been preserved; unread totals remain unknown." }
                readPositionReady=true
                selected?.target?.let { restoreSavedWindow(it,navigationId) }
                activeFrame?.takeIf { selected?.target == it.target }?.presentation?.let { captureUnreadBoundary(selected!!.target,it.events) }
                updateMessageState(persistence!!.state.value)
                syncOutgoing()
                val indexed=messageState.conversationIndex.filter { row -> connections.any { it.id == row.target.connectionId } }
                catalogSessions=indexed.map(IndexedConversation::session)
                refreshReadAttention()
                machines=indexed.distinctBy { MachineKey(it.target.connectionId,it.target.computerId) }.map { MachineNames.apply(Machine(it.target.connectionId,it.target.computerId,it.computerName,false,lastKnown=true),messageState.machineAliases) }
                storageReady = true
                if (connections.isNotEmpty()) refresh()
                launch { while(true) { delay(10_000);runCatching { watchScheduledSettings() } } }
                launch { persistence!!.state.collectLatest { updateMessageState(it) } }
                launch { messageRepository.error.collectLatest { failure ->
                    if (failure != null) error = "Could not save private changes. Your text remains available here; a message is sent only after its outgoing record is saved."
                } }
            } catch (e: Exception) { AppTelemetry.failure(DiagnosticFailure.PrivateStorage); error = "Private storage could not be opened. Existing data has been preserved. ${e.message.orEmpty()}" }
        }
    }
    private fun updateMessageState(value: MessageState) {
        val changed = messageState.outgoing != value.outgoing || messageState.drafts.filter { it.status == "submitted" && it.questionId.isNotBlank() } != value.drafts.filter { it.status == "submitted" && it.questionId.isNotBlank() }
        val namesChanged = messageState.machineAliases != value.machineAliases
        messageState = value
        if(namesChanged) machines = machines.map { MachineNames.apply(it,value.machineAliases) }
        continuation?.let { intent ->
            if (continuationEnqueuing != intent.requestId && !intent.unchanged(draft(intent.draft.target))) cancelCompactContinuation("Draft changed. Compaction may finish, but your message has not been sent.")
        }
        if (changed) syncOutgoing()
    }
    private fun retainOutbox() {
        messageRepository.retainOutbox(deliveryFlights, receiptFlights, conversationEvents)
    }
    private fun syncOutgoing() {
        retainOutbox()
        normalizedOutgoing = messageState.outgoing.map { if (it.status == "sending" && it.requestId !in deliveryFlights) it.recover() else it }
        preparePresentation()
    }
    private fun editState(reduce: (MessageState) -> MessageState) {
        retainOutbox()
        updateMessageState(checkNotNull(persistence) { "Private storage is not ready." }.edit(reduce))
    }
    private suspend fun durable(reduce: (MessageState) -> MessageState,
        acknowledge: (MessageState, MessageState) -> MessageState = { current, _ -> reduce(current) }): MessageState {
        retainOutbox()
        val writer = checkNotNull(persistence) { "Private storage is not ready." }
        val written = writer.durable(reduce, acknowledge)
        updateMessageState(writer.state.value)
        return written
    }
    var machineNameSaving by mutableStateOf<Set<MachineKey>>(emptySet()); private set
    fun machineName(connectionId:String, id:String, fallback:String=""):String = machines.find { it.connectionId==connectionId && it.id==id }?.name
        ?: messageState.machineAliases.find { it.key==MachineKey(connectionId,id) }?.name ?: fallback
    fun machineAlias(key:MachineKey)=messageState.machineAliases.find { it.key==key }?.name.orEmpty()
    fun renameMachine(key:MachineKey,name:String?,onSaved:()->Unit={}) {
        if(!storageReady || key in machineNameSaving || demo) return
        if(machines.none { MachineKey(it.connectionId,it.id)==key } || connections.none { it.id==key.connectionId }) { error="This machine is no longer connected to this workspace.";return }
        val checked=try { name?.let(MachineNames::checked) } catch(e:IllegalArgumentException) { error=e.message.orEmpty();return }
        machineNameSaving=machineNameSaving+key
        viewModelScope.launch {
            try {
                durable({ state -> check(connections.any { it.id==key.connectionId }) { "Workspace disconnected before saving the name." };MachineNames.set(state,key,checked) },
                    { current,_ -> MachineNames.set(current,key,checked) })
                onSaved()
            } catch(e:Exception) { error="Could not save the machine name. ${e.message.orEmpty()}" }
            finally { machineNameSaving=machineNameSaving-key }
        }
    }
    var machineColorSaving by mutableStateOf<Set<MachineKey>>(emptySet()); private set
    fun machineColor(key:MachineKey):String = MachineColors.color(messageState,key)
    fun machineColorOverride(key:MachineKey):String? = MachineColors.overrideHex(messageState,key)
    fun setMachineColor(key:MachineKey,hex:String?,onSaved:()->Unit={}) {
        if(!storageReady || key in machineColorSaving || demo) return
        if(machines.none { MachineKey(it.connectionId,it.id)==key } || connections.none { it.id==key.connectionId }) { error="This machine is no longer connected to this workspace.";return }
        val checked=try { hex?.let(MachineColors::checked) } catch(e:IllegalArgumentException) { error=e.message.orEmpty();return }
        machineColorSaving=machineColorSaving+key
        viewModelScope.launch {
            try {
                durable({ state -> check(connections.any { it.id==key.connectionId }) { "Workspace disconnected before saving the color." };MachineColors.set(state,key,checked) },
                    { current,_ -> MachineColors.set(current,key,checked) })
                onSaved()
            } catch(e:Exception) { error="Could not save the machine color. ${e.message.orEmpty()}" }
            finally { machineColorSaving=machineColorSaving-key }
        }
    }
    fun flushDrafts() { persistence?.flushAsync();readWriter?.flushAsync();accountsWriter?.flushAsync();teamWriter?.flushAsync() }
    var updateInstallPreparing by mutableStateOf(false); private set
    fun updateInstallReason(): String = if (speech.busy || speechConfiguration.busy) "Finish the voice operation before installing." else UpdateInstallGuard.reason(messageState,storageReady,
        attachmentImporting || attachmentPreparing,preparingTargets.isNotEmpty(),
        deliveryFlights.isNotEmpty() || contextFlights.isNotEmpty() || actionFlights.isNotEmpty(),compactContinuationActive)
    suspend fun prepareUpdateInstall() {
        check(!updateInstallPreparing && updateInstallReason().isBlank()) { updateInstallReason().ifBlank { "Update preparation is already running." } }
        updateInstallPreparing = true
        try {
            persistence!!.flush();readWriter?.flush()
            check(updateInstallReason().isBlank()) { updateInstallReason() }
        } catch(e:Throwable) { updateInstallPreparing=false;throw e }
    }
    fun finishUpdateInstallPreparation() { updateInstallPreparing=false }
    private fun rememberFrame(frame: Frame) {
        frames[frame.target.key] = frame
        while (frames.size > 20 || frames.values.sumOf { it.bytes.toLong() } > 20L * 1024 * 1024) frames.remove(frames.keys.first())
    }
    private fun frameInstall(source: Frame): FrameInstall {
        val retained=listOfNotNull(activeFrame?.takeIf { SessionNotificationPreferences.sameConversation(it.target,source.target) }?.raw,
            selected?.takeIf { SessionNotificationPreferences.sameConversation(it.target,source.target) }?.raw,
            catalogSessions.firstOrNull { SessionNotificationPreferences.sameConversation(it.target,source.target) }?.raw)
        val raw=SessionNotificationPreferences.mergeDetail(source.raw,source.target,retained,messageState.sessionActions)
        val incoming=if(raw===source.raw) source else source.copy(raw=raw)
        val window=historyWindows[incoming.target.key]
        val priorInspection=activeFrame?.takeIf { previous -> previous.target==incoming.target }?.inspectedEvents.orEmpty()
        val changed=window!=null && incoming.window!=window && HistoryInspection.changed(window,incoming.inspectedEvents,priorInspection)
        val guarded=window?.let { HistoryInspection.retain(it,incoming.window,incoming.inspectedEvents,priorInspection) }
        val frame=if(guarded!=null && (incoming.window!=guarded || incoming.events!=guarded.displayed)) incoming.copy(events=guarded.displayed,presentation=null,truncated=incoming.truncated || guarded.hasBefore || guarded.hasAfter,window=guarded,indexComplete=historyComplete && !changed,indexing=historyIndexing) else incoming
        return FrameInstall(source, frame, guarded, changed)
    }
    private fun installFrame(source: Frame, verified: Boolean, staged: FrameInstall? = null) {
        val install = staged ?: frameInstall(source)
        check(install.source === source)
        val frame = install.frame
        val guarded = install.guarded
        if(install.changed) currentHistoryHeads-=ConversationReadPolicies.key(frame.target)
        if(guarded!=null) historyWindows[frame.target.key]=guarded
        activeFrame = frame; activeFrameVerified = verified
        if (frame.presentation == null) { activityVerified = false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-install-wait-presentation */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.INSTALL_WAIT_PRESENTATION) /* END QUESTION_AVAILABILITY_PROBE writer-install-wait-presentation */; historyCached = activity != null }
        frame.presentation?.let { prepared ->
            androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                conversationEvents = prepared.events; conversationOutgoing = prepared.messages; conversationQuestions = prepared.questions
                activity = frame.raw; activityVerified = verified /* BEGIN QUESTION_AVAILABILITY_PROBE writer-install-presentation */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.INSTALL_PRESENTATION) /* END QUESTION_AVAILABILITY_PROBE writer-install-presentation */; historyCached = !verified; conversationHistoryTruncated = frame.truncated
                conversationObservedAt = frame.observedAt.takeIf { it > 0 }
                if (verified) { questionCheckedAt = android.os.SystemClock.elapsedRealtime(); questionProjectionComplete = frame.raw.optBoolean("pending_questions_complete", true) }
                publishPagingMetadata(frame)
                captureUnreadBoundary(frame.target,prepared.events)
            }
        }
        preparePresentation()
    }
    private fun publishPagingMetadata(frame:Frame) {
        val window=frame.window
        if(window!=null) { hasOlder=window.hasBefore;hasNewer=window.hasAfter;historyEpoch=window.epoch;historyComplete=frame.indexComplete;historyIndexing=frame.indexing }
        else {
            // Native inspection is a bounded recent projection, including on
            // older CLIs without an explicit truncation flag. History remains
            // an available explicit choice even when this projection is empty.
            hasOlder=historyNetworkReason(frame.target).isBlank()
            hasNewer=false;historyEpoch="";historyComplete=false;historyIndexing=frame.indexing
        }
        if(pendingHistoryPublication==frame.target.key) {
            // A page may retarget an existing opening divider through an alias.
            // Repreparing a retained frame must never restore a retired divider.
            if(firstUnreadEventId.isNotBlank()) frame.unreadBoundary?.let { firstUnreadEventId=it }
            pendingHistoryPublication="";olderLoading=false;readPositionReady=true
        }
    }
    private fun preparePresentation() {
        val frame = activeFrame ?: return
        val target = selected?.target ?: return
        if (frame.target != target) return
        val messages = outgoing.filter { it.belongsTo(target) }
        val questionState = messageState
        val version = ++presentationRevision
        presentationJob?.cancel()
        presentationJob = viewModelScope.launch {
            val (merged, questions) = withContext(Dispatchers.Default) {
                (if (target.archiveId.isBlank()) ConversationMerge.merge(target, frame.events, messages)
                else ConversationMerge.Result(frame.events, emptySet())) to QuestionTracking.restore(questionState, target, frame.questions)
            }
            if (version != presentationRevision || selected?.target != target || activeFrame !== frame) return@launch
            frame.presentation = Presentation(messages, merged.events, questions)
            androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                if (conversationQuestions != questions) conversationQuestions = questions
                if (conversationEvents != merged.events) conversationEvents = merged.events
                if (conversationOutgoing != messages) conversationOutgoing = messages
                activity = frame.raw; activityVerified = activeFrameVerified /* BEGIN QUESTION_AVAILABILITY_PROBE writer-prepare-presentation */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.PREPARE_PRESENTATION) /* END QUESTION_AVAILABILITY_PROBE writer-prepare-presentation */; historyCached = !activeFrameVerified; conversationHistoryTruncated = frame.truncated
                conversationObservedAt = frame.observedAt.takeIf { it > 0 }
                if (activeFrameVerified) { questionCheckedAt = android.os.SystemClock.elapsedRealtime(); questionProjectionComplete = frame.raw.optBoolean("pending_questions_complete", true) }
                publishPagingMetadata(frame)
                if(pendingHistoryPosition==frame.target.key) { pendingHistoryPosition="";readPositionReady=true }
                captureUnreadBoundary(frame.target,merged.events)
            }
            if (activityVerified) {
                if(frame.raw.optJSONArray("pending_questions")!=null) editState { state -> QuestionTracking.reconcile(state,frame.target,frame.questions) }
                val tracked = QuestionTracking.remember(messageState, frame.target, frame.questions)
                if (tracked != messageState) editState { state -> QuestionTracking.remember(state, frame.target, frame.questions) }
                observeContext(frame)
                observeScheduledSettings(frame)
                restoreHistory(frame.target)
                val window=historyWindows[frame.target.key]
                if(window?.atHead==true && !olderLoading && frame.observedAt>0 && frame.observedAt!=lastHistoryObservation) {
                    lastHistoryObservation=frame.observedAt
                    // A complete page received during this inspection already
                    // supplies its opening history. New canonical evidence still
                    // invalidates completeness and requires the ordinary repair.
                    if(!historyComplete || frame.observedAt>=lastHistoryReadCompletedAt)
                        HistoryRequests.refreshAtHead(window)?.let { (direction,cursor) -> loadHistory(frame.target,direction,cursor) }
                }
            }
            if (activityVerified && messages.any { it.requestId in merged.recordedRequestIds && (it.status != "recorded" || merged.exactRepresentations[it.requestId]?.let { proof -> proof != it.coverage } == true) }) {
                editState { state -> OutboxRetention.recorded(state, merged.recordedRequestIds, merged.exactRepresentations) }
            }
            if (activityVerified) messages.lastOrNull { it.status == "uncertain" && it.requestId !in merged.recordedRequestIds }?.let { message ->
                if (outgoingReceiptProbe.claim(target, navigationId, message,
                    message.requestId in deliveryFlights || "outgoing:${message.requestId}" in receiptFlights)) checkOutgoing(message, automatic = true)
            }
            // Last: opening a child replaces this parent presentation.
            if (activityVerified) openPendingAgent()
        }
    }
    private suspend fun inspectOnce(connection: Connection, target: Target, fresh: Boolean = false,
        current: () -> Boolean = { true }): Deferred<Frame> {
        InspectionFlights.existing(inspections,target.key,fresh,current)?.let { return it }
        val observedAt = System.currentTimeMillis()
        val task = viewModelScope.async {
            val settingsInspectionStartNanos=android.os.SystemClock.elapsedRealtimeNanos()
            val raw = api.inspect(connection, target)
            val resolved = if (target.archiveId.isNotBlank()) target else target.copy(run = raw.string("run_id"), conversation = raw.string("conversation_id"))
            val previous = frames[resolved.key]
            val currentWindow=historyWindows[resolved.key]
            val (frame,aliases,nextWindow)=withContext(Dispatchers.Default) {
                val incoming=NativeParser.events(raw)
                val retained=RollingConversationHistory.merge(resolved,previous?.target,previous?.events.orEmpty(),incoming)
                val window=currentWindow?.takeIf { it.atHead }?.inspection(incoming,previous?.inspectedEvents.orEmpty())
                val events=window?.displayed ?: retained.events
                val nativeUnknown=currentWindow?.let { HistoryInspection.changed(it,incoming,previous?.inspectedEvents.orEmpty()) }==true
                Triple(Frame(resolved,raw,events,NativeParser.questions(raw),observedAt,
                    raw.toString().toByteArray(Charsets.UTF_8).size+EventHistoryCodec.encode(events).toString().toByteArray(Charsets.UTF_8).size,
                    retained.truncated || previous?.truncated==true || raw.optBoolean("mobile_history_truncated") || raw.optBoolean("history_truncated") ||
                        raw.optBoolean("message_events_truncated") || raw.optBoolean("provider_messages_truncated"),window=window,indexComplete=historyComplete && !nativeUnknown,indexing=historyIndexing,inspectedEvents=incoming,settingsInspectionStartNanos=settingsInspectionStartNanos,supersededIds=retained.supersededIds),retained.supersededIds,window)
            }
            // A page can publish during Default parsing. Rebase before mutating the
            // window map, so the older inspection snapshot cannot erase that page.
            val rebased=HistoryInspection.rebase(currentWindow,frame to nextWindow,{ historyWindows[resolved.key] }) { latest ->
                val complete=historyComplete;val indexing=historyIndexing
                val guard=withContext(Dispatchers.Default) {
                    latest?.let { HistoryInspection.retain(it,null,frame.inspectedEvents,previous?.inspectedEvents.orEmpty()) }
                }
                val changed=latest?.let { HistoryInspection.changed(it,frame.inspectedEvents,previous?.inspectedEvents.orEmpty()) }==true
                frame.copy(events=guard?.displayed ?: frame.events,window=guard,presentation=null,indexComplete=complete && !changed,indexing=indexing) to guard
            }
            val expectedWindow=rebased.base
            val (finalFrame,finalWindow)=rebased.value
            if (connections.none { it.id == connection.id }) throw CancellationException("Workspace disconnected.")
            if(finalWindow==null) currentHistoryHeads-=ConversationReadPolicies.key(resolved)
            if(finalWindow!=null) {
                historyWindows[resolved.key]=finalWindow
                if(expectedWindow?.let { HistoryInspection.changed(it,frame.inspectedEvents,previous?.inspectedEvents.orEmpty()) }==true) currentHistoryHeads-=ConversationReadPolicies.key(resolved)
            }
            if(aliases.isNotEmpty()) readWriter?.let { writer -> readState=writer.edit { state -> state.supersede(resolved,aliases) } }
            rememberFrame(finalFrame)
            viewModelScope.launch { runCatching {
                val cached = withContext(Dispatchers.Default) { ConversationDisplayCache.encode(resolved, raw, finalFrame.events, finalFrame.truncated) }
                historyCache.write(resolved, cached, observedAt)
            } }
            finalFrame
        }
        inspections[target.key] = target to task
        task.invokeOnCompletion { viewModelScope.launch { InspectionFlights.retire(inspections,target.key,task) } }
        return task
    }
    private fun authorizedSettingsReason(target:Target,args:JSONObject,authorized:SessionAction,frame:Frame?,retry:ScheduledSettingsRetry.Grant?=null):String {
        if(updateInstallPreparing) return "Saving drafts for an app update."
        val raw=frame?.raw ?: return "No fresh settings inspection."
        val age=android.os.SystemClock.elapsedRealtimeNanos()-frame.settingsInspectionStartNanos
        if(frame.settingsInspectionStartNanos<=0 || age !in 0..10_000_000_000L) return "No fresh settings inspection."
        if(retry!=null && (retry.original!=authorized || retry.target!=target || args.opt("model")!=retry.model || args.opt("effort")!=retry.effort || args.opt("expected_pending_id")!=retry.pendingId)) return "The authorized settings identity changed."
        if(frame.target != target || target.agentId.isNotBlank() || target.archiveId.isNotBlank() || authorized.operation != "settings" || authorized.status != "scheduled" || (authorized.applyAttemptId.isNotBlank() && retry==null) ||
            authorized.target.connectionId != target.connectionId || authorized.target.computerId != target.computerId || authorized.target.session != target.session || authorized.target.conversation != target.conversation ||
            raw.string("name") != target.session || raw.string("run_id") != target.run || raw.string("conversation_id") != target.conversation ||
            raw.string("pending_settings_id") != authorized.pendingId || args.string("expected_pending_id") != authorized.pendingId || raw.string("settings_apply_when") != "now" || raw.string("phase") != "idle" || raw.string("activity") != "idle") return "The authorized settings identity changed."
        if("settings" !in relayOperations[target.connectionId].orEmpty() || "settings" !in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty() || contextBlocked(target) || target.key in preparingTargets || outgoing.any { it.belongsTo(target) && it.blocksSending }) return "Resolve pending delivery first."
        return SessionActionPolicies.inspectReason(target,"settings",args,raw)
    }
    private suspend fun watchScheduledSettings() {
        val authorized=messageState.sessionActions.filter { it.operation == "settings" && it.status == "scheduled" && (it.applyAttemptId.isBlank() || scheduledSettingsRetry.tracks(it)) && it.pendingId.isNotBlank() &&
            runCatching { JSONObject(it.arguments).string("expected_pending_id").isBlank() }.getOrDefault(false) }
        if(authorized.isEmpty()) return
        val candidates=mutableListOf<Triple<Connection,SessionAction,Target>>()
        for(connection in connections.filter { c -> authorized.any { it.target.connectionId==c.id } }) {
            val computers=runCatching { api.computers(connection).optJSONArray("computers")?.objects().orEmpty() }.getOrNull() ?: continue
            for(intent in authorized.filter { it.target.connectionId==connection.id }) {
                val computer=computers.find { it.string("id")==intent.target.computerId && it.optBoolean("online") } ?: continue
                val row=computer.optJSONObject("snapshot")?.optJSONArray("sessions")?.objects().orEmpty().find { it.string("name")==intent.target.session && it.string("conversation_id")==intent.target.conversation && it.string("archive_id").isBlank() } ?: continue
                if(intent.applyAttemptId.isBlank() && (row.string("activity")!="idle" || row.string("phase")!="idle")) continue
                if(intent.applyAttemptId.isNotBlank() && row.string("run_id")!=intent.target.run) continue
                candidates+=Triple(connection,intent,intent.target.copy(run=row.string("run_id")))
            }
        }
        val chosen=scheduledReadScheduler.choose(candidates.map { it.third },android.os.SystemClock.elapsedRealtime())
        for(target in chosen) {
            val (connection,intent,_)=candidates.first { it.third==target }
            val frame=runCatching { inspectOnce(connection,target).await() }
            scheduledReadScheduler.result(target,android.os.SystemClock.elapsedRealtime(),frame.isSuccess)
            val inspected=frame.getOrNull() ?: continue
            if(inspected.raw.string("pending_settings_id")!=intent.pendingId) {
                editState { state -> state.sessionActions.find { it.requestId==intent.requestId && it.applyAttemptId.isBlank() }?.let { state.action(it.copy(status="cancelled",error="Native pending settings changed; the old intent was not applied.")) } ?: state }
            }
            observeScheduledSettings(inspected)
        }
    }
    private fun observeScheduledSettings(frame: Frame) {
        val raw=frame.raw;val pending=raw.string("pending_settings_id")
        if(pending.isBlank() || frame.settingsInspectionStartNanos<=0) return
        val online=connections.any { it.id==frame.target.connectionId } && machines.any { it.connectionId==frame.target.connectionId && it.id==frame.target.computerId && it.online }
        val authorized=messageState.sessionActions.lastOrNull { action -> action.operation == "settings" && action.status == "scheduled" && action.pendingId == pending &&
            (action.applyAttemptId.isBlank() || scheduledSettingsRetry.tracks(action)) &&
            runCatching { JSONObject(action.arguments).string("expected_pending_id").isBlank() }.getOrDefault(false) &&
            action.target.connectionId == frame.target.connectionId && action.target.computerId == frame.target.computerId && action.target.session == frame.target.session && action.target.conversation == frame.target.conversation } ?: return
        val retry=if(authorized.applyAttemptId.isBlank()) null else {
            val child=messageState.sessionActions.find { it.requestId==authorized.applyAttemptId } ?: return
            scheduledSettingsRetry.inspect(authorized,child,frame.target,raw,frame.settingsInspectionStartNanos,android.os.SystemClock.elapsedRealtimeNanos(),true,online) ?: return
        }
        if(raw.string("settings_apply_when") != "now" || raw.string("activity") != "idle" || raw.string("phase") != "idle") return
        val key=frame.target.key + ":" + pending
        if(retry==null && (key in pendingSettingsAttempts || messageState.sessionActions.any { action -> action.operation == "settings" && action.target.connectionId == frame.target.connectionId &&
            action.target.computerId == frame.target.computerId && action.target.session == frame.target.session && action.target.conversation == frame.target.conversation &&
            runCatching { JSONObject(action.arguments).string("expected_pending_id") == pending }.getOrDefault(false) })) return
        val args=if(retry==null) JSONObject(authorized.arguments) else JSONObject().put("model",retry.model).put("effort",retry.effort)
        if(listOf("model","effort").any { args.has(it) && raw.string("pending_" + it) != args.string(it) }) return
        args.put("expected_pending_id",pending)
        if(authorizedSettingsReason(frame.target,args,authorized,frame,retry).isNotBlank()) {
            retry?.let(scheduledSettingsRetry::discard)
            return
        }
        val consumed=retry?.let { scheduledSettingsRetry.consume(it) ?: return }
        if(retry==null) pendingSettingsAttempts += key
        submitSessionAction(frame.target,"settings",args,authorized,frame,retry=consumed)
    }
    var launchMachine by mutableStateOf<MachineKey?>(null);private set
    var launchVisible by mutableStateOf(false);private set
    var launchCatalogState by mutableStateOf<JSONObject?>(null);private set
    var launchDirectoryState by mutableStateOf<JSONObject?>(null);private set
    var launchWorktreeState by mutableStateOf<JSONObject?>(null);private set
    var launchWorktreeLoading by mutableStateOf(false);private set
    var worktreeSubmitting by mutableStateOf(false);private set
    var directoryCreateSubmitting by mutableStateOf(false);private set
    var launchCreatedDirectory by mutableStateOf("");private set
    private var launchWorktreeJob:Job?=null
    var launchCatalogLoading by mutableStateOf(false);private set
    var launchDirectoryLoading by mutableStateOf(false);private set
    var launchError by mutableStateOf("");private set
    private var mobileDeliveryMessage by mutableStateOf("")
    private var mobileDeliveryRequest by mutableStateOf("")
    val mobileDeliveryError get() = MobileDelivery.error(mobileDeliveryMessage,mobileDeliveryRequest,sessionActions)
    private fun showMobileDeliveryError(message:String,requestId:String="") { mobileDeliveryMessage=message;mobileDeliveryRequest=requestId }
    var launchSubmitting by mutableStateOf(false);private set
    private var launchGeneration=0L
    private var launchCatalogJob:Job?=null
    private var launchDirectoryJob:Job?=null
    private var launchCatalogEvidence=""
    /** Identifies one opened New session form; its saved UI state is keyed by it. */
    var launchFormId by mutableLongStateOf(0L);private set
    /** Generated once per form: the default session name and Auto's new branch. */
    var launchGeneratedName by mutableStateOf("");private set
    var launchRepositoryPath by mutableStateOf("");private set
    var launchRepositoryCheckId by mutableStateOf("");private set
    /** Launch and worktree receipt statuses when the form opened; later changes belong to this form. */
    var launchBaseline by mutableStateOf<Map<String,String>>(emptyMap());private set
    /** Auto worktree launch progress: creating, checking or launching. */
    var launchStage by mutableStateOf("");private set
    var launchCreatedWorktree by mutableStateOf("");private set
    var launchCreatedEvidence by mutableStateOf<JSONObject?>(null);private set
    var launchAutomaticFailed by mutableStateOf(false);private set
    val launchBusy get() = launchSubmitting || worktreeSubmitting || directoryCreateSubmitting || launchStage.isNotEmpty()
    fun launchCatalogSignature()=launchCatalogEvidence
    fun launchReason(machine:MachineKey):String {
        if(updateInstallPreparing) return "Saving drafts for an app update."
        if(!storageReady || demo || connections.none { it.id == machine.connectionId }) return "Connect to this workspace before creating a session."
        if(machines.none { it.connectionId == machine.connectionId && it.id == machine.computerId && it.online }) return "Refresh the computer list; this computer is offline or last known."
        if(listOf("catalog","dirs","launch").any { it !in relayOperations[machine.connectionId].orEmpty() || it !in machineOperations[machine].orEmpty() }) return "This computer does not support safe mobile session creation."
        if(sessionActions.any { it.operation == "launch" && it.target.connectionId == machine.connectionId && it.target.computerId == machine.computerId && it.blocksSending }) return "Check the previous launch receipt before creating another session."
        return ""
    }
    fun openLaunch() {
        val candidates=machines.sortedBy { if(MachineKey(it.connectionId,it.id) in selectedMachines) 0 else 1 }
        val chosen=candidates.firstOrNull { launchReason(MachineKey(it.connectionId,it.id)).isBlank() } ?: candidates.firstOrNull()
        if(chosen!=null) prepareLaunch(MachineKey(chosen.connectionId,chosen.id))
        else { launchVisible=true;launchError="Refresh your connected computers first." }
    }
    fun repositoryReason(machine:MachineKey,path:String?=null):String {
        if((RepositoryAcquisition.operations+"worktrees").any { it !in relayOperations[machine.connectionId].orEmpty() || it !in machineOperations[machine].orEmpty() })
            return "Update the gateway and Zerus on this computer to clone repositories."
        if(machines.find { it.connectionId==machine.connectionId && it.id==machine.computerId }?.machineId.isNullOrBlank()) return "Refresh this computer's identity."
        if(RepositoryAcquisition.pending(sessionActions,machine,path)!=null) return "This destination has an unconfirmed checkout. Check its result or choose another destination."
        return launchReason(machine)
    }
    fun checkRepositoryResult(action:SessionAction) {
        val machine=MachineKey(action.target.connectionId,action.target.computerId)
        val stored=sessionActions.find { it.requestId==action.requestId && it.operation==RepositoryAcquisition.clone } ?: return
        if(launchBusy || !launchVisible || launchMachine!=machine || stored.requestId in actionFlights) return
        val connection=connections.find { it.id==machine.connectionId } ?: return
        actionFlights+=stored.requestId;launchStage="checking"
        viewModelScope.launch {
            try {
                val id=UUID.randomUUID().toString()
                val payload=RepositoryAcquisition.statusPayload(stored,id)
                val receipt=api.await(connection,api.submit(connection,stored.target,"repository_status",payload,id, SendAttempt()))
                val settled=RepositoryAcquisition.result(stored,receipt,id)
                durable({ state -> state.action(settled) })
                launchError=settled.error
            } catch(e:Exception) { if(e is CancellationException) throw e;launchError=RepositoryAcquisition.uncertain }
            finally { launchStage="";actionFlights-=stored.requestId }
        }
    }
    fun cloneAndLaunch(machine:MachineKey,url:String,path:String,agent:String,tag:String,accountId:String,expectedCatalog:String,projectId:String,model:String,effort:String,implicitInbox:Boolean) {
        if(launchBusy || !launchVisible || launchMachine!=machine || launchCatalogEvidence!=expectedCatalog) return
        val problem=repositoryReason(machine,path);if(problem.isNotBlank()) { launchError=problem;return }
        val identity=machines.find { it.connectionId==machine.connectionId && it.id==machine.computerId }?.machineId.orEmpty()
        val args=try { RepositoryAcquisition.arguments(identity,url,path) } catch(e:Exception) { launchError=e.message.orEmpty();return }
        val connection=connections.find { it.id==machine.connectionId } ?: return
        val generation=launchGeneration;val target=Target(machine.computerId,"","","",machine.connectionId)
        val catalog=launchCatalogState ?: return
        launchStage="checking";launchError=""
        viewModelScope.launch {
            var action:SessionAction?=null;var attempted=false;var rejected=false
            try {
                val checkId=UUID.randomUUID().toString()
                val checked=api.await(connection,api.submit(connection,target,"repository_check",JSONObject(args.toString()).put("request_id",checkId),checkId, SendAttempt()))
                val destination=RepositoryAcquisition.checkedDestination(args,checked,checkId)
                check(launchVisible && launchMachine==machine && launchGeneration==generation && launchCatalogEvidence==expectedCatalog) { "The selected computer or form changed." }
                if(destination.needsConfirmation) {
                    launchRepositoryPath=destination.path;launchRepositoryCheckId=checkId
                    launchError="Destination resolved. Review Clone to, then choose Clone and start."
                    return@launch
                }
                // Validate every launch choice before reserving or creating any checkout.
                val launchArgs=LaunchPresentation.arguments(catalog,agent,path,tag,accountId,projectId,null,model,effort,launchEffortSupported(machine),implicitInbox)
                check(launchVisible && launchMachine==machine && launchGeneration==generation && launchCatalogEvidence==expectedCatalog && repositoryReason(machine,path).isBlank()) { "The selected computer or form changed." }
                val creation=SessionAction(UUID.randomUUID().toString(),target,RepositoryAcquisition.clone,args.toString());action=creation;actionFlights+=creation.requestId
                durable({ state -> check(RepositoryAcquisition.pending(state.sessionActions,machine,path)==null);state.action(creation) },
                    { current,written -> current.action(written.sessionActions.first { it.requestId==creation.requestId }) })
                check(launchVisible && launchGeneration==generation && launchMachine==machine && launchCatalogEvidence==expectedCatalog && connections.any { it==connection } &&
                    machines.any { it.connectionId==machine.connectionId && it.id==machine.computerId && it.machineId==identity && it.online && !it.lastKnown }) { "The selected computer changed before checkout." }
                launchStage="cloning";attempted=true
                val initial=try { api.submit(connection,target,creation.operation,JSONObject(args.toString()).put("request_id",creation.requestId),creation.requestId, SendAttempt()) }
                    catch(e:RelayException) { rejected=e.status in listOf(400,401,403,404,409,413,429);throw e }
                settleSessionAction(creation,api.await(connection,initial,maxPolls=100))
                val settled=messageState.sessionActions.first { it.requestId==creation.requestId }
                check(settled.status=="completed" && settled.resultPath==path) { settled.error.ifEmpty { RepositoryAcquisition.uncertain } }
                // Only this fresh, definite clone result continues. Receipt recovery never reaches launch.
                check(launchVisible && launchGeneration==generation && launchMachine==machine && launchCatalogEvidence==expectedCatalog && launchReason(machine).isBlank()) { "Checkout created. Review the current session choices." }
                launchStage="checking"
                launchDirectoryJob?.cancel();launchWorktreeJob?.cancel();launchDirectoryLoading=false;launchWorktreeLoading=false
                val browser=readLaunchDirectory(connection,machine,path)
                check(LaunchPresentation.selectedFolder(browser,path)) { "Checkout created; check its folder before starting." }
                val checkout=readLaunchWorktrees(connection,machine,path,path)
                check(RepositoryAcquisition.verifiedCheckout(checkout,path)) { "Checkout created; its Git folder could not be confirmed. Use checkout to review it." }
                launchDirectoryState=browser;launchWorktreeState=checkout
                val launch=SessionAction(UUID.randomUUID().toString(),target,"launch",launchArgs.toString())
                launchStage="launching";launchSubmitting=true;actionFlights+=launch.requestId
                submitLaunch(machine,connection,launch,agent,path,expectedCatalog,generation)
            } catch(e:Exception) {
                action?.let { original ->
                    try { durable({ state -> state.sessionActions.find { it.requestId==original.requestId }?.let { current ->
                        if(current.blocksSending) state.action(current.copy(status=if(!attempted || rejected) "failed" else "uncertain",
                            error=if(!attempted || rejected) e.message.orEmpty() else RepositoryAcquisition.uncertain)) else state
                    } ?: state }) } catch(_:Exception) { launchError="Could not save checkout outcome. Check the original result." }
                }
                if(e is CancellationException) throw e
                launchError=e.message.orEmpty()
            } finally { launchStage="";action?.let { actionFlights-=it.requestId } }
        }
    }
    fun prepareLaunch(machine:MachineKey) {
        if(launchBusy) return
        val reason=launchReason(machine)
        val opening=!launchVisible
        launchRepositoryPath="";launchRepositoryCheckId=""
        launchGeneration++;launchMachine=machine;launchVisible=true;launchCatalogState=null;launchDirectoryState=null;launchWorktreeState=null;launchError="";launchCatalogEvidence=""
        if(opening) { launchFormId=launchGeneration;launchGeneratedName=LaunchPresentation.generatedName() };launchStage="";launchCreatedWorktree="";launchCreatedEvidence=null;launchAutomaticFailed=false
        launchCreatedDirectory=""
        launchBaseline=sessionActions.filter { it.operation in setOf("launch","worktree_create","directory_create") }.associate { it.requestId to it.status }
        launchCatalogJob?.cancel();launchDirectoryJob?.cancel();launchWorktreeJob?.cancel();launchWorktreeLoading=false;launchDirectoryLoading=false
        if(reason.isNotBlank()) { launchError=reason;launchCatalogLoading=false;return }
        val connection=connections.find { it.id == machine.connectionId } ?: return
        val generation=launchGeneration;launchCatalogLoading=true
        launchCatalogJob=viewModelScope.launch {
            try {
                val target=Target(machine.computerId,"","","",machine.connectionId);val id=UUID.randomUUID().toString()
                val receipt=api.await(connection,api.submit(connection,target,"catalog",JSONObject().put("request_id",id),id, SendAttempt()))
                check(receipt.string("state") == "completed") { receipt.string("error").ifBlank { "The native account catalog is unavailable." } }
                val raw=receipt.getJSONObject("result")
                val sanitized=withContext(Dispatchers.Default) { LaunchPresentation.sanitizeCatalog(raw) }
                if(launchGeneration == generation && launchMachine == machine) { launchCatalogState=sanitized;launchCatalogEvidence=sanitized.toString() }
            } catch(e:Exception) { if(e is CancellationException) throw e;if(launchGeneration == generation) launchError=e.message.orEmpty() }
            finally { if(launchGeneration == generation) launchCatalogLoading=false }
        }
        browseLaunchDirectory(machine,"~")
    }
    /** Closing never interrupts worktree creation or a launch; their receipts must settle first. */
    fun closeLaunch() { if(launchBusy) return;finishLaunch() }
    private fun finishLaunch() { launchWorktreeJob?.cancel();launchWorktreeLoading=false;launchVisible=false;launchGeneration++;launchCatalogJob?.cancel();launchDirectoryJob?.cancel();launchCatalogLoading=false;launchDirectoryLoading=false }
    private suspend fun readLaunchDirectory(connection:Connection,machine:MachineKey,path:String):JSONObject {
        val target=Target(machine.computerId,"","","",machine.connectionId);val id=UUID.randomUUID().toString()
        val receipt=api.await(connection,api.submit(connection,target,"dirs",JSONObject().put("request_id",id).put("path",path),id, SendAttempt()))
        check(receipt.string("state") == "completed") { receipt.string("error").ifBlank { "This folder is unavailable." } }
        val raw=receipt.getJSONObject("result")
        check(raw.string("path").startsWith('/')) { "Computer returned an invalid folder." }
        return raw.put("requested_path",path)
    }
    private suspend fun readLaunchWorktrees(connection:Connection,machine:MachineKey,path:String,requested:String):JSONObject {
        val id=UUID.randomUUID().toString();val target=Target(machine.computerId,"","","",machine.connectionId)
        val receipt=api.await(connection,api.submit(connection,target,"worktrees",JSONObject().put("request_id",id).put("path",path),id, SendAttempt()))
        check(receipt.string("state")=="completed") { receipt.string("error").ifBlank { "Worktree catalog unavailable." } }
        return receipt.getJSONObject("result").put("requested_path",requested)
    }
    fun browseLaunchDirectory(machine:MachineKey,path:String) {
        if(launchMachine != machine || launchDirectoryLoading || !launchVisible || launchStage.isNotEmpty() || directoryCreateSubmitting) return
        if(path != "~" && (!path.startsWith('/') || path.length > 4096 || path.any(Char::isISOControl))) { launchError="Choose an absolute native folder.";return }
        val connection=connections.find { it.id == machine.connectionId } ?: return
        launchWorktreeJob?.cancel();launchWorktreeState=null;launchWorktreeLoading=false;launchDirectoryState=null
        val generation=launchGeneration;launchDirectoryLoading=true;launchError=""
        launchDirectoryJob=viewModelScope.launch {
            try {
                val raw=readLaunchDirectory(connection,machine,path)
                if(launchGeneration == generation && launchMachine == machine) {
                    launchDirectoryState=raw
                    loadLaunchWorktrees(machine,raw.getString("path"),path)
                }
            } catch(e:Exception) { if(e is CancellationException) throw e;if(launchGeneration == generation) launchError=e.message.orEmpty() }
            finally { if(launchGeneration == generation) launchDirectoryLoading=false }
        }
    }
    fun directoryCreateReason(machine:MachineKey):String {
        val reason=launchReason(machine);if(reason.isNotBlank()) return reason
        if("directory_create" !in relayOperations[machine.connectionId].orEmpty() || "directory_create" !in machineOperations[machine].orEmpty())
            return "Update the gateway and Zerus on this computer to create folders."
        if(DirectoryCreation.pending(sessionActions,machine)!=null) return "Check the previous folder creation receipt before creating another."
        return ""
    }
    private fun publishCreatedDirectory(machine:MachineKey,generation:Long,action:SessionAction) {
        if(launchGeneration!=generation || launchMachine!=machine || !launchVisible || !DirectoryCreation.computerTarget(action,machine) ||
            action.status!="completed" || action.resultPath.isBlank()) return
        launchCreatedDirectory=action.resultPath
        // The result proves creation, but browsing still verifies the selectable native folder.
        browseLaunchDirectory(machine,action.resultPath)
    }
    fun createLaunchDirectory(machine:MachineKey,path:String) {
        if(launchBusy || launchDirectoryLoading || !launchVisible || launchMachine!=machine) return
        val problem=directoryCreateReason(machine).ifBlank { DirectoryCreation.pathError(path) }
        if(problem.isNotBlank()) { launchError=problem;return }
        val connection=connections.find { it.id==machine.connectionId } ?: return
        val generation=launchGeneration
        val action=SessionAction(UUID.randomUUID().toString(),Target(machine.computerId,"","","",machine.connectionId),DirectoryCreation.operation,
            SessionActionPolicies.arguments(DirectoryCreation.operation,JSONObject().put("path",path)).toString())
        directoryCreateSubmitting=true;launchError="";actionFlights+=action.requestId
        viewModelScope.launch {
            var attempted=false;var rejected=false
            try {
                durable({ state -> check(DirectoryCreation.pending(state.sessionActions,machine)==null);state.action(action) },
                    { current,written -> current.action(written.sessionActions.first { it.requestId==action.requestId }) })
                check(!updateInstallPreparing && storageReady && !demo && launchVisible && launchGeneration==generation && launchMachine==machine &&
                    connections.any { it==connection } && machines.any { it.connectionId==machine.connectionId && it.id==machine.computerId && it.online && !it.lastKnown } &&
                    "directory_create" in relayOperations[machine.connectionId].orEmpty() && "directory_create" in machineOperations[machine].orEmpty()) {
                    "The selected computer or form changed. Folder creation was not sent."
                }
                attempted=true
                val initial=try { api.submit(connection,action.target,action.operation,JSONObject(action.arguments).put("request_id",action.requestId),action.requestId, SendAttempt()) }
                    catch(error:RelayException) { rejected=error.status in listOf(400,401,403,404,409,413,429);throw error }
                settleSessionAction(action,api.await(connection,initial))
                val settled=messageState.sessionActions.first { it.requestId==action.requestId }
                if(launchGeneration==generation) launchError=settled.error
            } catch(error:Exception) {
                try { durable({ state -> state.sessionActions.find { it.requestId==action.requestId }?.let { current ->
                    if(current.blocksSending) state.action(current.copy(status=if(!attempted || rejected) "failed" else "uncertain",
                        error=if(!attempted || rejected) error.message.orEmpty() else DirectoryCreation.uncertain)) else state
                } ?: state }) } catch(_:Exception) { if(launchGeneration==generation) launchError="Could not save the folder outcome. Check the original request." }
                if(error is CancellationException) throw error
                if(launchGeneration==generation) launchError=if(!attempted || rejected) error.message.orEmpty() else DirectoryCreation.uncertain
            } finally {
                directoryCreateSubmitting=false;actionFlights-=action.requestId
                messageState.sessionActions.find { it.requestId==action.requestId }?.let { publishCreatedDirectory(machine,generation,it) }
            }
        }
    }
    /** Only reads the original durable receipt. An unknown result never submits again. */
    fun checkLaunchDirectoryCreation(machine:MachineKey,action:SessionAction) {
        val stored=sessionActions.find { it.requestId==action.requestId && DirectoryCreation.computerTarget(it,machine) } ?: return
        if(launchBusy || !launchVisible || launchMachine!=machine || stored.requestId in actionFlights) return
        val connection=connections.find { it.id==machine.connectionId } ?: return
        val generation=launchGeneration
        directoryCreateSubmitting=true;actionFlights+=stored.requestId
        viewModelScope.launch {
            try { settleSessionAction(stored,api.await(connection,api.receipt(connection,stored.requestId))) }
            catch(error:Exception) { if(error is CancellationException) throw error;if(launchGeneration==generation) launchError="The original folder receipt could not be checked. Nothing has been retried." }
            finally {
                directoryCreateSubmitting=false;actionFlights-=stored.requestId
                messageState.sessionActions.find { it.requestId==stored.requestId }?.let { publishCreatedDirectory(machine,generation,it) }
            }
        }
    }
    /** The relay and this computer's connector accept an OpenCode model variant as `effort`; older ones never receive one. */
    fun launchEffortSupported(machine:MachineKey)=LaunchPresentation.effortSupported(relayFeatures[machine.connectionId].orEmpty(),machineFeatures[machine].orEmpty())
    /** OpenCode variant names for this exact inspected session; every other provider keeps the fixed effort names. */
    fun nativeEffortsSupported(target:Target,inspected:JSONObject?)=SessionActionPolicies.nativeEfforts(target,inspected,launchEffortSupported(MachineKey(target.connectionId,target.computerId)))
    fun worktreeOperationsSupported(machine:MachineKey)=listOf("worktrees","worktree_create").all { it in relayOperations[machine.connectionId].orEmpty() && it in machineOperations[machine].orEmpty() }
    fun worktreeReason(machine:MachineKey):String {
        if(launchReason(machine).isNotBlank()) return launchReason(machine)
        if(!worktreeOperationsSupported(machine)) return "Update Zerus on this computer to use worktree actions."
        if(sessionActions.any { it.operation=="worktree_create" && it.target.connectionId==machine.connectionId && it.target.computerId==machine.computerId && it.blocksSending })
            return "Check the previous worktree creation receipt before creating another."
        return ""
    }
    private fun loadLaunchWorktrees(machine:MachineKey,path:String,requested:String) {
        launchWorktreeJob?.cancel();launchWorktreeState=null;launchWorktreeLoading=false
        if("worktrees" !in relayOperations[machine.connectionId].orEmpty() || "worktrees" !in machineOperations[machine].orEmpty()) return
        val connection=connections.find { it.id==machine.connectionId } ?: return
        val generation=launchGeneration;launchWorktreeLoading=true
        launchWorktreeJob=viewModelScope.launch {
            try {
                val raw=readLaunchWorktrees(connection,machine,path,requested)
                if(launchGeneration==generation && launchMachine==machine) launchWorktreeState=raw
            } catch(e:Exception) { if(e is CancellationException) throw e }
            finally { if(launchGeneration==generation && launchMachine==machine) launchWorktreeLoading=false }
        }
    }
    private fun worktreeAction(machine:MachineKey,evidence:JSONObject,branch:String,base:String,destination:String):Pair<SessionAction,JSONObject> {
        val arguments=JSONObject().put("path",evidence.getString("path")).put("common_dir",evidence.getString("common_dir"))
            .put("branch",branch).put("base",base).put("destination",destination)
        SessionActionPolicies.arguments("worktree_create",arguments)
        return SessionAction(UUID.randomUUID().toString(),Target(machine.computerId,"","","",machine.connectionId),"worktree_create",arguments.toString()) to arguments
    }
    /** Sends one durable creation request and returns its confirmed path, or "" after a recorded failure or unknown result. */
    private suspend fun submitWorktree(machine:MachineKey,connection:Connection,action:SessionAction,arguments:JSONObject,evidence:JSONObject,generation:Long):String {
        var attempted=false;var rejected=false
        try {
            durable({ state -> check(state.sessionActions.none { it.operation=="worktree_create" && it.target==action.target && it.blocksSending });state.action(action) },
                { current,written -> current.action(written.sessionActions.first { it.requestId==action.requestId }) })
            check(launchVisible && launchGeneration==generation && launchMachine==machine && launchWorktreeState?.string("common_dir")==evidence.string("common_dir")) { "The selected repository changed. Worktree creation was not sent." }
            attempted=true
            val initial=try { api.submit(connection,action.target,"worktree_create",JSONObject(arguments.toString()).put("request_id",action.requestId),action.requestId, SendAttempt()) }
                catch(e:RelayException) { rejected=e.status in listOf(400,401,403,404,409,413,429);throw e }
            settleSessionAction(action,api.await(connection,initial))
            val completed=messageState.sessionActions.find { it.requestId==action.requestId }
            if(completed?.status=="completed" && completed.resultPath.isNotBlank()) return completed.resultPath
            launchError=completed?.error.orEmpty().ifBlank { "Worktree creation is unconfirmed. Check its original receipt; nothing has been retried." }
            return ""
        } catch(e:Exception) {
            try { durable({ state -> state.sessionActions.find { it.requestId==action.requestId }?.let { existing ->
                state.action(existing.copy(status=if(!attempted || rejected) "failed" else "uncertain",error=if(!attempted || rejected) e.message.orEmpty() else "Worktree creation is unconfirmed. Check the original receipt and folder; nothing has been retried.")) } ?: state }) }
            catch(_:Exception) { launchError="Could not save the creation outcome. The original request remains recoverable." }
            if(e is CancellationException) throw e
            launchError=if(!attempted || rejected) e.message.orEmpty() else "Worktree creation is unconfirmed. Check its original receipt; nothing has been retried."
            return ""
        }
    }
    /** Keeps a confirmed checkout in this form, selected for a direct-folder launch. */
    private fun retainCreatedWorktree(path:String,evidence:JSONObject) { launchCreatedEvidence=WorktreePresentation.withCreated(evidence,path);launchCreatedWorktree=path }
    fun createLaunchWorktree(machine:MachineKey,evidence:JSONObject,branch:String,base:String,destination:String) {
        val problem=worktreeReason(machine)
        if(problem.isNotBlank() || launchBusy || !launchVisible || launchMachine!=machine || !WorktreePresentation.healthy(evidence) ||
            launchWorktreeState?.string("common_dir")!=evidence.string("common_dir")) { launchError=problem.ifBlank { "Refresh the current worktrees before creation." };return }
        val (action,arguments)=try { worktreeAction(machine,evidence,branch,base,destination) } catch(e:Exception) { launchError=e.message.orEmpty();return }
        val connection=connections.find { it.id==machine.connectionId } ?: return
        val generation=launchGeneration;worktreeSubmitting=true;actionFlights+=action.requestId;launchError=""
        viewModelScope.launch {
            try {
                val created=submitWorktree(machine,connection,action,arguments,evidence,generation)
                if(created.isNotBlank() && launchVisible && launchGeneration==generation && launchMachine==machine) {
                    retainCreatedWorktree(created,evidence);browseLaunchDirectory(machine,created)
                }
            } finally { actionFlights-=action.requestId;worktreeSubmitting=false }
        }
    }
    fun launchSession(machine:MachineKey,agent:String,directory:String,tag:String,accountId:String?=null,expectedCatalog:String=launchCatalogSignature(),projectId:String="",worktrees:JSONObject?=null,model:String="",effort:String="",implicitInbox:Boolean=false) {
        val reason=launchReason(machine);if(reason.isNotBlank()) { launchError=reason;return }
        if(launchBusy || !launchVisible || launchMachine != machine || launchCatalogLoading || launchDirectoryLoading || launchWorktreeLoading || expectedCatalog != launchCatalogEvidence) { launchError="The creation details changed. Review the current choices.";return }
        val catalog=launchCatalogState ?: return
        if(!LaunchPresentation.selectedFolder(launchDirectoryState,directory)) { launchError="Select a folder from the current native browser.";return }
        val args=try { LaunchPresentation.arguments(catalog,agent,directory,tag,accountId,projectId,worktrees,model,effort,launchEffortSupported(machine),implicitInbox) } catch(e:Exception) { launchError=e.message.orEmpty();return }
        val connection=connections.find { it.id == machine.connectionId } ?: return
        val action=SessionAction(UUID.randomUUID().toString(),Target(machine.computerId,"","","",machine.connectionId),"launch",args.toString())
        val generation=launchGeneration;launchSubmitting=true;actionFlights+=action.requestId;launchError=""
        viewModelScope.launch { submitLaunch(machine,connection,action,agent,directory,expectedCatalog,generation) }
    }
    /** The caller marks the launch in flight; this records, sends and settles it exactly once. */
    private suspend fun submitLaunch(machine:MachineKey,connection:Connection,action:SessionAction,agent:String,directory:String,expectedCatalog:String,generation:Long) {
        val target=action.target
        var attempted=false;var rejected=false
        try {
            durable({ state -> check(state.sessionActions.none { it.operation == "launch" && it.target.computerId == target.computerId && it.target.connectionId == target.connectionId && it.blocksSending });state.action(action) }, { current,written -> current.action(written.sessionActions.first { it.requestId == action.requestId }) })
            check(launchVisible && launchGeneration == generation && launchMachine == machine && launchCatalogEvidence == expectedCatalog && connections.any { it.id == machine.connectionId }) { "The launch form changed before creating the session." }
            attempted=true
            val initial=try { api.submit(connection,target,"launch",JSONObject(action.arguments).put("request_id",action.requestId),action.requestId, SendAttempt()) } catch(e:RelayException) { rejected=e.status in listOf(400,401,403,404,409,413,429);throw e }
            val receipt=api.await(connection,initial);settleSessionAction(action,receipt)
            val completed=messageState.sessionActions.find { it.requestId == action.requestId }
            if(completed?.status == "completed" && completed.resultTarget != null && launchVisible && launchGeneration == generation && launchMachine == machine) {
                if(completed.error.isBlank()) {
                    val created=completed.resultTarget;finishLaunch()
                    open(Session(created,created.session,agent,"starting",directory,"",0,JSONObject().put("name",created.session).put("run_id",created.run).put("conversation_id",created.conversation)))
                    refresh()
                } else { launchError=completed.error;refresh() }
            } else if(completed?.status != "completed") launchError=completed?.error.orEmpty()
        } catch(e:Exception) {
            val status=if(!attempted || rejected) "failed" else "uncertain"
            try { durable({ state -> state.sessionActions.find { it.requestId == action.requestId }?.let { if(it.status in setOf("sending","uncertain")) state.action(it.copy(status=status,error=if(status=="failed") e.message.orEmpty() else "Launch is unconfirmed. Check its original receipt; it has not been retried.")) else state } ?: state }) } catch(_:Exception) { launchError="Could not save the launch result. Its original request remains recoverable." }
            if(e is CancellationException) throw e
            launchError=if(status=="failed") e.message.orEmpty() else "Launch is unconfirmed. Check its original receipt before creating another session."
        } finally { actionFlights-=action.requestId;launchSubmitting=false }
    }
    /**
     * Desktop Auto: create the generated sibling worktree, verify its folder and catalog, then launch there.
     * Every step is pinned to this form; a failed or unknown creation is never replayed and never launches.
     */
    fun launchWithWorktree(machine:MachineKey,agent:String,folder:String,tag:String,accountId:String?,expectedCatalog:String,projectId:String,model:String="",effort:String="",implicitInbox:Boolean=false) {
        val reason=worktreeReason(machine);if(reason.isNotBlank()) { launchError=reason;return }
        if(launchBusy || !launchVisible || launchMachine != machine || launchCatalogLoading || launchDirectoryLoading || launchWorktreeLoading || expectedCatalog != launchCatalogEvidence) { launchError="The creation details changed. Review the current choices.";return }
        // One generated branch per form: after any creation or failed attempt, only direct launches remain.
        if(launchAutomaticFailed || launchCreatedWorktree.isNotBlank()) { launchError="This form already attempted worktree creation. Choose None or another worktree.";return }
        val catalog=launchCatalogState ?: return
        if(!LaunchPresentation.selectedFolder(launchDirectoryState,folder)) { launchError="Select a folder from the current native browser.";return }
        val evidence=WorktreePresentation.forFolder(launchWorktreeState,folder)?.takeIf { WorktreePresentation.automaticAvailable(it,folder) }
            ?: run { launchError="The repository could not be confirmed. Choose a working folder again.";return }
        val branch=launchGeneratedName
        val destination=WorktreePresentation.automaticPath(evidence,branch)
        // Everything the session needs is checked before anything is created.
        try { LaunchPresentation.arguments(catalog,agent,destination,tag,accountId,projectId,WorktreePresentation.withCreated(evidence,destination),model,effort,launchEffortSupported(machine),implicitInbox) } catch(e:Exception) { launchError=e.message.orEmpty();return }
        val (creation,arguments)=try { worktreeAction(machine,evidence,branch,"HEAD",destination) } catch(e:Exception) { launchError=e.message.orEmpty();return }
        val connection=connections.find { it.id == machine.connectionId } ?: return
        val generation=launchGeneration
        launchStage="creating";worktreeSubmitting=true;actionFlights+=creation.requestId;launchError=""
        fun current()=launchVisible && launchGeneration == generation && launchMachine == machine
        viewModelScope.launch {
            try {
                val created=try { submitWorktree(machine,connection,creation,arguments,evidence,generation) } finally { actionFlights-=creation.requestId;worktreeSubmitting=false }
                if(created.isBlank()) { launchAutomaticFailed=true;return@launch }
                if(!current()) return@launch
                launchDirectoryJob?.cancel();launchWorktreeJob?.cancel()
                launchStage="checking";launchDirectoryState=null;launchWorktreeState=null;retainCreatedWorktree(created,evidence)
                launchDirectoryLoading=true
                val directory=try { readLaunchDirectory(connection,machine,created) }
                    catch(e:Exception) { if(e is CancellationException) throw e
                        if(current()) launchError="Worktree created at $created. Checking its folder failed: ${e.message.orEmpty().trimEnd('.').ifBlank { "no response" }}. Check it again before starting there."
                        return@launch }
                    finally { if(launchGeneration == generation) launchDirectoryLoading=false }
                if(!current()) return@launch
                launchDirectoryState=directory
                launchWorktreeLoading=true
                val fresh=try { if("worktrees" in relayOperations[machine.connectionId].orEmpty() && "worktrees" in machineOperations[machine].orEmpty()) readLaunchWorktrees(connection,machine,directory.getString("path"),created) else null }
                    catch(e:Exception) { if(e is CancellationException) throw e;null }
                    finally { if(launchGeneration == generation) launchWorktreeLoading=false }
                if(!current()) return@launch
                launchWorktreeState=fresh
                val blocked=launchReason(machine)
                if(blocked.isNotBlank() || expectedCatalog != launchCatalogEvidence) { launchError="Worktree created at $created. ${blocked.ifBlank { "The creation details changed." }} Start the session there when ready.";return@launch }
                val project=LaunchPresentation.projects(catalog).find { it.id == projectId }
                val placement=WorktreePresentation.placement(fresh,launchCreatedEvidence,project,created)
                val args=try { LaunchPresentation.arguments(catalog,agent,created,tag,accountId,projectId,placement,model,effort,launchEffortSupported(machine),implicitInbox) }
                    catch(e:Exception) { launchError="Worktree created at $created. ${e.message.orEmpty()}";return@launch }
                val launch=SessionAction(UUID.randomUUID().toString(),Target(machine.computerId,"","","",machine.connectionId),"launch",args.toString())
                launchStage="launching";launchSubmitting=true;actionFlights+=launch.requestId
                submitLaunch(machine,connection,launch,agent,created,expectedCatalog,generation)
            } finally { launchStage="" }
        }
    }
    fun mobileDeliveryGateway(connectionId:String)=MobileDelivery.gateway(machines.filter { it.connectionId==connectionId }) {
        MobileDelivery.OPERATION in machineOperations[MachineKey(it.connectionId,it.id)].orEmpty()
    }
    fun mobileDeliveryAction(connectionId:String)=sessionActions.lastOrNull { it.operation==MobileDelivery.OPERATION && it.target.connectionId==connectionId && it.status!="reviewed" }
    fun mobileDeliveryReason(connectionId:String):String {
        if(demo || !storageReady) return "Connect a workspace to change this setting."
        if(updateInstallPreparing) return "Saving drafts for an app update."
        val gateway=mobileDeliveryGateway(connectionId)
        return MobileDelivery.reason(gateway,
            gateway!=null && MobileDelivery.OPERATION in machineOperations[MachineKey(connectionId,gateway.id)].orEmpty(),
            MobileDelivery.OPERATION in relayOperations[connectionId].orEmpty(),
            sessionActions.any { it.operation==MobileDelivery.OPERATION && it.target.connectionId==connectionId && it.blocksSending })
    }
    fun dismissMobileDeliveryError() { showMobileDeliveryError("") }
    /** A durable request with the usual receipt states. An unconfirmed change is checked, never repeated. */
    fun setMobileDelivery(connectionId:String,value:String) {
        val reason=mobileDeliveryReason(connectionId);if(reason.isNotBlank()) { showMobileDeliveryError(reason);return }
        val gateway=mobileDeliveryGateway(connectionId) ?: return
        if(MobileDelivery.shown(gateway.mobileDelivery,mobileDeliveryAction(connectionId),System.currentTimeMillis())==value) { showMobileDeliveryError("");return }
        val args=try { MobileDelivery.arguments(value) } catch(e:Exception) { showMobileDeliveryError(e.message.orEmpty());return }
        val connection=connections.find { it.id==connectionId } ?: return
        val target=Target(gateway.id,"","","",connectionId)
        val action=SessionAction(UUID.randomUUID().toString(),target,MobileDelivery.OPERATION,args.toString())
        showMobileDeliveryError("");actionFlights+=action.requestId
        viewModelScope.launch {
            var attempted=false;var rejected=false
            try {
                durable({ state -> check(state.sessionActions.none { it.operation==MobileDelivery.OPERATION && it.target.connectionId==connectionId && it.blocksSending });state.action(action) },
                    { current,written -> current.action(written.sessionActions.first { it.requestId==action.requestId }) })
                attempted=true
                val initial=try { api.submit(connection,target,MobileDelivery.OPERATION,JSONObject(args.toString()),action.requestId, SendAttempt()) }
                    catch(e:RelayException) { rejected=e.status in listOf(400,401,403,404,409,413,429);throw e }
                settleSessionAction(action,api.await(connection,initial))
                val settled=messageState.sessionActions.find { it.requestId==action.requestId }
                if(settled?.status=="completed") refresh() else showMobileDeliveryError(settled?.error.orEmpty(),action.requestId)
            } catch(e:Exception) {
                val status=if(!attempted || rejected) "failed" else "uncertain"
                try { durable({ state -> state.sessionActions.find { it.requestId==action.requestId }?.let { if(it.status in setOf("sending","uncertain")) state.action(it.copy(status=status,error=if(status=="failed") e.message.orEmpty() else MobileDelivery.UNCONFIRMED)) else state } ?: state }) }
                catch(_:Exception) { showMobileDeliveryError("Could not save the change result. Its original request remains recoverable.",action.requestId) }
                if(e is CancellationException) throw e
                showMobileDeliveryError(if(status=="failed") e.message.orEmpty() else MobileDelivery.UNCONFIRMED,action.requestId)
            } finally { actionFlights-=action.requestId }
        }
    }
    var terminalState by mutableStateOf<JSONObject?>(null);private set
    var terminalLoading by mutableStateOf(false);private set
    var terminalError by mutableStateOf("");private set
    var terminalVisible by mutableStateOf(false);private set
    private var terminalVerified=false
    private var terminalReceivedAt=0L
    private var terminalJob:Job?=null
    private var terminalAvailabilityJob:Job?=null
    private var terminalAvailabilitySequence=0L
    private var terminalAvailabilityTarget by mutableStateOf<Target?>(null)
    private val emptyTerminalBuffers=mutableMapOf<String,TerminalBuffer>()
    fun terminalAvailabilityChecking(target:Target)=terminalAvailabilityTarget==target
    /** Explicit opening checks discovery again, even when a previous catalog cached a refusal. */
    fun prepareTerminal(target:Target) {
        if(selected?.target!=target || target.agentId.isNotBlank() || target.archiveId.isNotBlank() || demo || updateInstallPreparing) return
        cancelTerminalAvailability()
        val sequence=++terminalAvailabilitySequence
        val navigation=navigationId
        terminalAvailabilityTarget=target
        terminalAvailabilityJob=viewModelScope.launch {
            fun current()=terminalAvailabilitySequence==sequence && terminalAvailabilityTarget==target && navigationId==navigation && selected?.target==target
            try {
                TerminalAvailabilityRefresh.run(current=::current,
                    catalogIdle={ snapshotFlow { catalogBusy }.first { !it } },refreshCatalog={ refresh(explicit=true) },
                    detailIdle={ snapshotFlow { detailBusy }.first { !it } },
                    refreshDetail={ refreshActivity(explicit=true,freshInspection=true,ownerCurrent=::current) })
            } finally {
                if(terminalAvailabilitySequence==sequence) terminalAvailabilityTarget=null
            }
        }
    }
    fun cancelTerminalAvailability() {
        terminalAvailabilitySequence++
        terminalAvailabilityJob?.cancel();terminalAvailabilityJob=null;terminalAvailabilityTarget=null
    }
    fun terminalOpenReason(target:Target):String {
        if(selected?.target != target || target.agentId.isNotBlank() || target.archiveId.isNotBlank()) return "Terminal is available only for the live parent session."
        if(demo) return "Unavailable in preview."
        if(terminalAvailabilityChecking(target)) return TerminalAvailability.checking
        val row=activity?.takeIf { activityVerified && it.string("run_id")==target.run && it.string("conversation_id")==target.conversation &&
            it.string("archive_id").isBlank() && it.string("agent_id").isBlank() && it.string("state")!="archived" &&
            (it.string("name").isBlank() || it.string("name")==target.session) } ?: sessions.find { it.target==target }?.raw
        val machine=machines.find { it.connectionId==target.connectionId && it.id==target.computerId }
        return TerminalAvailability.reason(target,row,machine?.let { it.online && !it.lastKnown }==true,
            relayOperations[target.connectionId],machineOperations[MachineKey(target.connectionId,target.computerId)])
    }
    private fun terminalCurrent(target:Target,binding:String):Boolean {
        val raw=terminalState ?: return false
        return selected?.target == target && terminalVisible && terminalVerified && android.os.SystemClock.elapsedRealtime()-terminalReceivedAt in 0..5000 &&
            raw.string("name") == target.session && raw.string("run_id") == target.run && raw.string("conversation_id") == target.conversation && raw.string("terminal_binding_id") == binding && binding.isNotBlank()
    }
    fun terminalFresh(target:Target)=terminalCurrent(target,terminalState?.string("terminal_binding_id").orEmpty())
    fun terminalReason(target:Target):String {
        if(updateInstallPreparing) return "Saving drafts for an app update."
        val unavailable=terminalOpenReason(target);if(unavailable.isNotBlank()) return unavailable
        if(!storageReady || "terminal_input" !in relayOperations[target.connectionId].orEmpty() || "terminal_input" !in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty()) return "This computer does not support verified Terminal input."
        if(!terminalCurrent(target,terminalState?.string("terminal_binding_id").orEmpty())) return "Refresh Terminal before sending input."
        if(contextBlocked(target) || target.key in preparingTargets || outgoing.any { it.belongsTo(target) && it.blocksSending } || attachmentImporting || attachmentPreparing) return "Resolve the pending delivery before typing into Terminal."
        return ""
    }
    fun setTerminalVisible(target:Target,visible:Boolean) {
        if(selected?.target != target) return
        terminalVisible=visible
        if(visible) terminalSnapshot(target) else { terminalJob?.cancel();terminalLoading=false;terminalVerified=false;activityVerified=false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-terminal-closed */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.TERMINAL_CLOSED) /* END QUESTION_AVAILABILITY_PROBE writer-terminal-closed */;activeFrameVerified=false;refreshActivity() }
    }
    fun terminalSnapshot(target:Target) {
        val reason=terminalOpenReason(target);if(reason.isNotBlank()) { terminalError=reason;return }
        if(terminalLoading) return
        val connection=connections.find { it.id == target.connectionId } ?: return
        val navigation=navigationId
        terminalLoading=true;terminalError=""
        terminalJob=viewModelScope.launch {
            try {
                val id=UUID.randomUUID().toString()
                val payload=target.json().put("request_id",id)
                val receipt=api.await(connection,api.submit(connection,target,"terminal_snapshot",payload,id, SendAttempt()),50,100)
                check(receipt.string("state") == "completed") { receipt.string("error").ifBlank { "Terminal is not available yet." } }
                val result=receipt.getJSONObject("result")
                check(result.string("request_id") == id && result.string("name") == target.session && result.string("run_id") == target.run && result.string("conversation_id") == target.conversation &&
                    result.string("terminal_binding_id").isNotBlank() && result.string("screen").toByteArray(Charsets.UTF_8).size <= 512*1024) { "Computer returned a different Terminal binding." }
                if(navigationId == navigation && selected?.target == target && terminalVisible) { terminalState=result;terminalVerified=true;terminalReceivedAt=android.os.SystemClock.elapsedRealtime() }
            } catch(e:Exception) { if(e is CancellationException) throw e; if(navigationId == navigation && selected?.target == target) { terminalError=e.message.orEmpty();terminalVerified=false } }
            finally { if(navigationId == navigation) terminalLoading=false }
        }
    }
    fun terminalBuffer(target:Target):TerminalBuffer? {
        val raw=terminalState ?: return null
        if(raw.string("name") != target.session || raw.string("run_id") != target.run || raw.string("conversation_id") != target.conversation) return null
        val binding=raw.string("terminal_binding_id");if(binding.isBlank()) return null
        val key=target.key + ":terminal:" + binding
        return messageState.terminalBuffers.find { it.key == key } ?: emptyTerminalBuffers.getOrPut(key) { TerminalBuffer(target,binding) }
    }
    fun editTerminal(target:Target,text:String) {
        val buffer=terminalBuffer(target) ?: return
        if(!storageReady || text.toByteArray(Charsets.UTF_8).size > 65536 || text == buffer.text) return
        val editor=buffer.copy(text=text,revision=UUID.randomUUID().toString())
        editState { state -> TerminalBuffers.edit(state,editor) }
    }
    fun restoreTerminalInput(action:SessionAction) {
        if(action.operation != "terminal_input" || action.status !in setOf("failed","reviewed")) return
        val current=terminalBuffer(action.target) ?: return
        val args=runCatching { JSONObject(action.arguments) }.getOrNull() ?: return
        if(args.string("terminal_binding_id") != current.binding || !args.has("text") || current.text.isNotEmpty()) { terminalError="The original Terminal input remains saved. Restore it only into its empty matching Terminal buffer.";return }
        editTerminal(action.target,args.getString("text"));flushDrafts()
    }
    fun terminalInput(target:Target,key:String?=null,enter:Boolean=false) {
        val reason=terminalReason(target);if(reason.isNotBlank()) { terminalError=reason;return }
        val buffer=terminalBuffer(target) ?: return
        val args=JSONObject().put("terminal_binding_id",buffer.binding)
        if(key != null) args.put("key",key) else args.put("text",buffer.text).put("enter",enter)
        submitSessionAction(target,"terminal_input",args,terminalBuffer=if(key == null) buffer else null)
    }
    var processOutputResult by mutableStateOf<JSONObject?>(null); private set
    var processOutputId by mutableStateOf(""); private set
    var processOutputLoading by mutableStateOf(false); private set
    var processOutputError by mutableStateOf(""); private set
    private var processOutputJob:Job?=null
    fun processOutputReason(target:Target,processId:String):String {
        if(selected?.target != target || !activityVerified || target.agentId.isNotBlank()) return "Refresh the parent session before reading process output."
        if("process_output" !in relayOperations[target.connectionId].orEmpty() || "process_output" !in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty()) return "This computer does not support remote process output."
        val process=activity?.optJSONObject("processes")?.optJSONArray("items")?.objects().orEmpty().find { it.string("id") == processId } ?: return "This process is no longer present."
        return if(process.optJSONObject("capabilities")?.optBoolean("output") == true) "" else "This process does not provide output."
    }
    fun processOutput(target: Target, processId: String, generation: String? = null) {
        val reason=processOutputReason(target,processId);if(reason.isNotBlank()) { processOutputError=reason;return }
        if("process_output" !in relayOperations[target.connectionId].orEmpty() || "process_output" !in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty()) { processOutputError="This computer does not support remote process output.";return }
        val process=activity?.optJSONObject("processes")?.optJSONArray("items")?.objects().orEmpty().find { it.string("id") == processId } ?: return
        if(process.optJSONObject("capabilities")?.optBoolean("output") != true) return
        if(processOutputLoading && processOutputId == processId) return
        val connection=connections.find { it.id == target.connectionId } ?: return
        val navigation=navigationId
        processOutputJob?.cancel();processOutputId=processId;processOutputResult=null;processOutputError="";processOutputLoading=true
        processOutputJob=viewModelScope.launch {
            try {
                val id=UUID.randomUUID().toString()
                val payload=JSONObject().put("request_id",id).put("expected_run_id",target.run).put("expected_conversation_id",target.conversation).put("process_id",processId)
                if(target.archiveId.isNotBlank()) payload.put("archive_id",target.archiveId)
                if(generation != null) payload.put("generation",generation)
                val receipt=api.await(connection,api.submit(connection,target,"process_output",payload,id, SendAttempt()))
                check(receipt.string("state") == "completed") { receipt.string("error").ifBlank { "Process output is not available yet." } }
                val result=receipt.getJSONObject("result")
                check(result.string("request_id") == id && result.string("name") == target.session && result.string("id") == processId && result.string("run_id") == target.run && result.string("conversation_id") == target.conversation && result.string("archive_id") == target.archiveId) { "Computer returned output for another process." }
                if(navigationId == navigation && selected?.target == target && processOutputId == processId) processOutputResult=result
            } catch(e:Exception) { if(e is CancellationException) throw e; if(navigationId == navigation && selected?.target == target) processOutputError=e.message.orEmpty() }
            finally { if(navigationId == navigation && processOutputId == processId) processOutputLoading=false }
        }
    }
    fun actionReason(target: Target, operation: String): String {
        if(updateInstallPreparing) return "Saving drafts for an app update."
        if(operation == "terminal_input") return terminalReason(target)
        if(terminalVisible) return "Return to Activity before using session controls."
        if(target.agentId.isNotBlank()) return "Session controls belong to the parent conversation."
        if (!storageReady || demo || selected?.target != target || !activityVerified) return "Refresh this exact session before taking an action."
        val raw = activity ?: return "Refresh this session."
        if (raw.string("name") != target.session || raw.string("run_id") != target.run || raw.string("conversation_id") != target.conversation || raw.string("archive_id") != target.archiveId) return "The session identity changed. Refresh before taking an action."
        if (operation !in SessionActionPolicies.operations || operation !in relayOperations[target.connectionId].orEmpty() || operation !in machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty()) return "This gateway or computer does not support this action."
        if (target.archiveId.isNotBlank() && operation !in setOf("restore", "forget", "rename", "fork")) return "This archived conversation is read-only."
        if (target.archiveId.isBlank() && operation == "restore") return "Select an archived version first."
        if(operation == SessionNotificationPreferences.operation) SessionNotificationPreferences.reason(target,raw).takeIf { it.isNotBlank() }?.let { return it }
        if(operation in SessionActionPolicies.lifecycle && raw.optJSONArray("allowed_actions")?.let { value -> (0 until value.length()).any { value.optString(it) == operation } } != true) return raw.optJSONObject("action_reasons")?.string(operation).orEmpty().ifBlank { "Native session controls are unavailable for this action." }
        if (contextBlocked(target) || outgoing.any { it.belongsTo(target) && it.blocksSending } || target.key in preparingTargets || compactContinuationActive) return "Resolve the pending delivery or action first."
        if (attachmentImporting || attachmentPreparing || pickerDraft?.target == target || drafts.any { it.target == target && it.status in listOf("submitting", "uncertain") }) return "Wait for the current answer or file selection."
        return ""
    }
    fun sendNow(target: Target, queueId: String) = sessionAction(target,"send_now",JSONObject().put("queue_id",queueId))
    fun updateSettings(target: Target, model: String? = null, effort: String? = null, expectedPendingId: String = "") = sessionAction(target,"settings",JSONObject().also {
        if(model != null) it.put("model",model); if(effort != null) it.put("effort",effort)
        if(expectedPendingId.isNotBlank()) it.put("expected_pending_id",expectedPendingId)
    })
    fun processStop(target: Target, processId: String, generation: String? = null) = sessionAction(target,"process_stop",JSONObject().put("process_id",processId).also { if(generation != null) it.put("generation",generation) })
    fun sessionAction(target: Target, operation: String, args: JSONObject = JSONObject())=submitSessionAction(target,operation,args)
    /** Logical grouping uses its own exact scoped metadata ABI, independent of launch folders. */
    internal fun projectAssignmentReason(target:Target):String {
        val key=MachineKey(target.connectionId,target.computerId)
        val machine=machines.singleOrNull { it.connectionId==key.connectionId && it.id==key.computerId }
        return when {
            demo || !storageReady -> "Connect a computer and wait for private storage."
            updateInstallPreparing -> "Saving drafts for an app update."
            target.archiveId.isNotBlank() || target.agentId.isNotBlank() -> "Project grouping belongs to the parent session."
            target.run.isBlank() -> "Wait for the session identity to finish loading."
            connections.count { it.id==target.connectionId }!=1 -> "The original workspace is disconnected."
            machine==null || !machine.online || machine.lastKnown -> "The selected computer is offline."
            !AttachmentPolicy.validId(machine.machineId) || machine.route==MachineRoute.Unknown -> "Refresh to verify this computer's identity and route."
            !relayOperations[target.connectionId].orEmpty().containsAll(SessionProjectProtocol.operations) || SessionProjectProtocol.feature !in relayFeatures[target.connectionId].orEmpty() -> "Update this gateway to move session project grouping."
            !machineOperations[key].orEmpty().containsAll(SessionProjectProtocol.operations) || SessionProjectProtocol.feature !in machineFeatures[key].orEmpty() -> "Update Zerus on this computer to move session project grouping."
            else -> ""
        }
    }
    internal val sessionProjects by lazy {
        val owner=this
        SessionProjectController(object:SessionProjectAccess {
        override fun capture(target:Target):SessionProjectHost? {
            if(projectAssignmentReason(target).isNotBlank()) return null
            val connection=connections.singleOrNull { it.id==target.connectionId } ?: return null
            val machine=machines.singleOrNull { it.connectionId==target.connectionId && it.id==target.computerId } ?: return null
            return SessionProjectHost(target,connection,machine,navigationId,connections)
        }
        override fun current(captured:SessionProjectHost):Boolean = captured.connectionGeneration===connections &&
            captured.navigation==navigationId && selected?.target==captured.target && projectAssignmentReason(captured.target).isBlank() &&
            capture(captured.target)?.let { it.connection==captured.connection && it.route==captured.route }==true
        override fun reason(target:Target)=projectAssignmentReason(target)
        override fun mutationReason(target:Target,ownRequest:String):String = projectAssignmentReason(target).ifBlank {
            if(clearExpectation?.let { ContextPolicies.sameSessionRun(it.target,target) }==true ||
                contextOperations.any { ContextPolicies.sameSessionRun(it.target,target) && it.blocksSending } ||
                sessionActions.any { it.target==target && it.blocksSending && it.requestId!=ownRequest } ||
                outgoing.any { it.belongsTo(target) && it.blocksSending } || target.key in preparingTargets)
                "Resolve the pending delivery or action first." else ""
        }
        override fun actions()=sessionActions
        override fun flight(id:String,active:Boolean) { actionFlights=if(active) actionFlights+id else actionFlights-id }
        override suspend fun persist(action:SessionAction) {
            durable({ state ->
                val old=state.sessionActions.find { it.requestId==action.requestId }
                check(old==null || old.target==action.target && old.operation==action.operation && old.arguments==action.arguments) { "The saved project request changed." }
                if(old==null || old.blocksSending) state.action(action) else state
            })
        }
        override suspend fun settle(action:SessionAction,receipt:JSONObject) { settleSessionAction(action,receipt) }
        override fun refresh() { owner.refresh(explicit=true) }
    },viewModelScope,api) }
    /** Presentation for one Sessions row's long-press actions; the opened conversation re-verifies before any native request. */
    fun sessionMenu(session: Session): List<List<SessionMenuItem>> {
        val target = session.target
        val online = SessionFilters.online(session,machines)
        val blocked = when {
            demo -> "Unavailable in preview."
            updateInstallPreparing -> "Saving drafts for an app update."
            !storageReady -> "Private storage is still loading."
            else -> ""
        }
        val busy = contextBlocked(target) || outgoing.any { it.belongsTo(target) && it.blocksSending } || target.key in preparingTargets
        val read = SessionMenuRead(
            unavailable = if(demo) "Unavailable in preview." else if(!readTrackingReady || readWriter == null) "Read state is still loading on this phone." else "",
            canMark = canReadAll(target), partial = readAllPartial(target),
            replies = ReadAllPolicies.unreadReplies(indexedReadRecord(target),loadedRows(target).orEmpty()) ||
                session.raw.optBoolean("unread_reply") || session.raw.optBoolean("mobile_unread_reply"),
            reminder = reviewLater(target), checking = online && historyNetworkReason(target).isBlank())
        val reference = SessionReference.resolve(target,machines)
        return SessionActionsMenu.items(SessionMenuFacts(target,session.raw,SessionFilters.state(session),SessionFilters.archived(session),online,
            relayOperations[target.connectionId].orEmpty() intersect machineOperations[MachineKey(target.connectionId,target.computerId)].orEmpty(),
            blocked,if(busy) "Resolve the pending delivery or action first." else "",SessionFilters.needsAttention(session,online),read,
            reference.uri,session.raw.string("cwd"),reference.reason,
            TerminalAvailability.reason(target,session.raw,online,relayOperations[target.connectionId],machineOperations[MachineKey(target.connectionId,target.computerId)]),projectAssignmentReason(target)))
    }
    var sessionMenuRequest by mutableStateOf<SessionMenuRequest?>(null); private set
    /** A Sessions action opens its exact session; the conversation header then confirms it against fresh native details. */
    fun openWithSessionAction(session: Session, operation: String) {
        if(operation != "terminal" && operation != SessionNotificationPreferences.operation && operation != SessionProjectProtocol.assign && operation !in SessionActionPolicies.lifecycle) return
        open(session,preserveProject = true)
        sessionMenuRequest = SessionMenuRequest(session.target,operation,navigationId)
    }
    /** Hands the request over once, only to the session and navigation that it opened. */
    fun takeSessionMenuRequest(target: Target): String? {
        val request = sessionMenuRequest ?: return null
        if(request.navigation == navigationId && request.target != target) return null
        sessionMenuRequest = null
        return request.operation.takeIf { request.navigation == navigationId && request.target == target }
    }
    private fun submitSessionAction(target:Target,operation:String,args:JSONObject,authorized:SessionAction?=null,frame:Frame?=null,terminalBuffer:TerminalBuffer?=null,retry:ScheduledSettingsRetry.ConsumedGrant?=null) {
        val inspected=frame?.raw ?: activity ?: JSONObject()
        val reason = if(authorized == null) actionReason(target,operation) else authorizedSettingsReason(target,args,authorized,frame,retry?.grant); if(reason.isNotBlank()) { error=reason; return }
        val nativeEfforts = nativeEffortsSupported(target,inspected)
        SessionActionPolicies.effortReason(operation,args,nativeEfforts).takeIf { it.isNotBlank() }?.let { error=it; return }
        val arguments = try { SessionActionPolicies.arguments(operation,args,nativeEfforts) } catch(e:Exception) { error=e.message.orEmpty(); return }
        val eligibility = SessionActionPolicies.inspectReason(target,operation,arguments,inspected)
        if(eligibility.isNotBlank()) { error=eligibility; return }
        val connection = connections.find { it.id == target.connectionId } ?: return
        val action = SessionAction(UUID.randomUUID().toString(),target,operation,arguments.toString())
        val evidence=SessionActionPolicies.evidence(operation,inspected,arguments)
        val navigation = navigationId
        preparingTargets += target.key; actionFlights += action.requestId
        viewModelScope.launch {
            var attempted=false; var rejected=false
            try {
                durable({ state ->
                    if(terminalBuffer != null) TerminalBuffers.enqueue(state,terminalBuffer,action) else if(authorized == null) state.action(action) else {
                        ScheduledSettingsAttempt.begin(state,authorized,action,retry)
                    }
                }, { current,written ->
                    var next=if(terminalBuffer == null) current.action(written.sessionActions.first { it.requestId == action.requestId }) else TerminalBuffers.acknowledge(current,written,terminalBuffer,action)
                    if(authorized != null) written.sessionActions.find { it.requestId == authorized.requestId }?.let { next=next.action(it) }
                    next
                })
                preparingTargets -= target.key
                val live=if(authorized == null) activity ?: JSONObject() else inspected
                if(authorized == null) check(selected?.target == target && navigationId == navigation && (operation == "terminal_input" || activityVerified)) { "The session changed before the action was sent." }
                else check(frame != null && frame.target == target && System.currentTimeMillis()-frame.observedAt in 0..10_000 && connections.any { it.id == target.connectionId } &&
                    messageState.sessionActions.any { it.requestId == authorized.requestId && it.applyAttemptId == action.requestId }) { "The authorized native settings changed before delivery." }
                if(operation == "terminal_input") check(terminalCurrent(target,arguments.string("terminal_binding_id"))) { "Terminal binding expired before delivery. Input was not sent." }
                if(operation != "terminal_input") check(SessionActionPolicies.evidence(operation,live,arguments) == evidence) { "The displayed native action details changed before delivery." }
                check(SessionActionPolicies.inspectReason(target,operation,arguments,live).isBlank()) { "The native action target or catalog changed before delivery." }
                val payload = if(operation == SessionNotificationPreferences.operation) SessionNotificationPreferences.payload(target,arguments)
                    else JSONObject(arguments.toString()).put("request_id",action.requestId).put("expected_run_id",target.run).put("expected_conversation_id",target.conversation)
                if(target.archiveId.isNotBlank()) payload.put("archive_id",target.archiveId)
                attempted=true
                val initial=try { api.submit(connection,target,operation,payload,action.requestId, SendAttempt()) }
                    catch(e:RelayException) { rejected=e.status in listOf(400,401,403,404,409,413,429); throw e }
                val receipt = if(operation == "terminal_input") api.await(connection,initial,50,100) else api.await(connection,initial)
                settleSessionAction(action,receipt,authorized)
                val completed=messageState.sessionActions.find { it.requestId == action.requestId }
                if(operation == "terminal_input") { if(selected?.target == target && terminalVisible) terminalSnapshot(target) }
                else if(completed != null) when(SessionActionFollowUp.kind(completed,selected?.target,navigationId==navigation)) {
                    SessionActionFollowUp.Kind.Sessions -> { back();returnToSessionsSequence++;refresh() }
                    SessionActionFollowUp.Kind.OpenResult -> {
                        activityVerified=false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-action-result */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.ACTION_RESULT) /* END QUESTION_AVAILABILITY_PROBE writer-action-result */;activeFrameVerified=false;historyCached=activity != null
                        resolveActionResult(completed,navigation,target)
                    }
                    SessionActionFollowUp.Kind.Refresh -> {
                        refreshActivity()
                    }
                    SessionActionFollowUp.Kind.None -> Unit
                }
            } catch(e:Exception) {
                val status = if(!attempted || rejected) "failed" else "uncertain"
                try { durable({ state -> state.sessionActions.find { it.requestId == action.requestId }?.let { current ->
                    if(current.status in listOf("sending","uncertain")) state.action(current.copy(status=status,error=if(status=="failed") e.message.orEmpty() else "Action delivery is unconfirmed. Check its original receipt; it has not been retried.")) else state
                } ?: state }) } catch(_:Exception) { error="Could not save the action result. Its original request remains recoverable." }
                if(e is CancellationException) throw e
            } finally { preparingTargets -= target.key; actionFlights -= action.requestId }
        }
    }
    private var actionResultJob:Job?=null
    private val resolvingActionResults=mutableSetOf<String>()
    var actionResultNotice by mutableStateOf("");private set
    var returnToSessionsSequence by mutableStateOf(0L);private set
    fun pendingActionResult(target:Target)=messageState.sessionActions.lastOrNull { action ->
        action.target==target && SessionActionFollowUp.openable(action)
    }
    fun openSessionActionResult(action:SessionAction) {
        val stored=messageState.sessionActions.find { it.requestId==action.requestId && it.status=="completed" && it.resultTarget!=null } ?: return
        if(actionResultJob?.isActive==true) return
        val original=selected?.target
        val navigation=navigationId
        actionResultJob=viewModelScope.launch { resolveActionResult(stored,navigation,original) }
    }
    fun confirmedDraftAction(draft:Draft)=messageState.sessionActions.lastOrNull { action ->
        action.resultTarget?.let { ActionDraftPolicies.proof(action,draft.target,it) }==true && draft.status=="editing" && draft.questionId.isBlank() && draft.detachedId.isBlank()
    }
    fun restoreDraftToConfirmedResult(draft:Draft,action:SessionAction) {
        val stored=confirmedDraftAction(draft)?.takeIf { it.requestId==action.requestId } ?: return
        val source=messageState.drafts.find { it.key==draft.key && it.generation==draft.generation && it.revision==draft.revision && it.attachments==draft.attachments } ?: return
        val destination=stored.resultTarget ?: return
        if(!ActionDraftPolicies.destinationReady(messageState,destination)) { actionResultNotice="The confirmed session already has a composition. Your original draft remains in Saved drafts.";return }
        if(actionResultJob?.isActive==true) return
        val navigation=navigationId;val original=selected?.target
        actionResultJob=viewModelScope.launch { resolveActionResult(stored,navigation,original,source) }
    }
    private suspend fun resolveActionResult(action:SessionAction,navigation:Long,original:Target?,restore:Draft?=null) {
        val destination=action.resultTarget ?: return
        val connection=connections.find { it.id==destination.connectionId } ?: return
        if(!resolvingActionResults.add(action.requestId)) return
        try {
            val frame=ConfirmedTargetResolution.read(active={ navigationId==navigation && selected?.target==original && connections.any { it.id==destination.connectionId } },
                pause={ delay(750);runCatching { api.computers(connection) } },inspect={ inspectOnce(connection,destination).await().also {
                    check(it.target==destination || destination.resolvesTo(it.target)) { "The confirmed destination has changed. The original draft remains saved." }
                } })
            if(navigationId!=navigation || selected?.target!=original) return
            val messages=outgoing.filter { it.belongsTo(frame.target) };val state=messageState
            val prepared=withContext(Dispatchers.Default) { Presentation(messages,ConversationMerge.merge(frame.target,frame.events,messages).events,QuestionTracking.restore(state,frame.target,frame.questions)) }
            if(navigationId!=navigation || selected?.target!=original) return
            androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                if((original==action.target || restore!=null) && ActionDraftPolicies.proof(action,action.target,frame.target)) {
                    editState { if(restore==null) ActionDraftPolicies.promote(it,action) else ActionDraftPolicies.restore(it,action,restore) }
                    readWriter?.let { readState=it.edit { state -> state.promote(action) } }
                }
                selected=Session(frame.target,frame.raw.string("title","label","name"),frame.raw.string("agent"),frame.raw.string("activity","state"),frame.raw.string("project","cwd"),"",0,frame.raw)
                frame.presentation=prepared;installFrame(frame,true)
                actionResultNotice=if(restore!=null && drafts.any { it.key==restore.key }) "Your original draft remains in Saved drafts because the composition changed or the destination is occupied." else ""
            }
            if(action.operation=="launch") closeLaunch()
        } catch(error:CancellationException) { throw error }
        catch(_:Exception) { if(navigationId==navigation && selected?.target==original) actionResultNotice="The computer confirmed the action. Its destination is not reachable yet. Open the confirmed result when connected; your original draft remains saved." }
        finally { resolvingActionResults-=action.requestId }
    }
    private suspend fun settleSessionAction(action: SessionAction,receipt: JSONObject,scheduledAuthorization:SessionAction?=null) {
        val result = SessionActionPolicies.result(action,receipt)
        if(result.status=="applied" && action.operation in setOf("archive","terminate","restore","forget","rename"))archive.refresh()
        durable({ state -> state.sessionActions.find { it.requestId == action.requestId }?.let { current ->
            if(current.status in listOf("sending","uncertain")) {
                var next=state.action(result)
                if(result.operation == "settings" && result.status == "applied") state.sessionActions.find { it.applyAttemptId == action.requestId }?.let { next=next.action(it.copy(status="applied")) }
                next
            } else state
        } ?: state }, { current,written ->
            var next=written.sessionActions.find { it.requestId == action.requestId }?.let(current::action) ?: current
            written.sessionActions.find { it.applyAttemptId == action.requestId }?.let { next=next.action(it) }
            next
        })
        if(action.operation == SessionNotificationPreferences.operation) {
            if(result.status=="completed") confirmNotificationPreference(result)
            refresh(explicit=true)
            SessionNotifications.enqueue(getApplication(),action.target.connectionId,forceCatalog=true)
        }
        // Only this process's authorized submit callback can establish another cycle.
        // Receipt checking/restoration deliberately uses the default null authorization.
        if(scheduledAuthorization!=null && result.operation=="settings" && result.status=="scheduled") {
            val original=messageState.sessionActions.find { it.requestId==scheduledAuthorization.requestId && it.applyAttemptId==action.requestId } ?: return
            val child=messageState.sessionActions.find { it.requestId==action.requestId && it==result } ?: return
            receipt.optJSONObject("result")?.let { scheduledSettingsRetry.register(original,child,android.os.SystemClock.elapsedRealtimeNanos(),it) }
        }
    }
    private var notificationPreferenceRevision=0L
    private suspend fun confirmNotificationPreference(result:SessionAction) {
        notificationPreferenceRevision++
        while(true) {
            val rows=catalogSessions;val actions=messageState.sessionActions + result
            val version=notificationPreferenceRevision
            val merged=withContext(Dispatchers.Default) { SessionNotificationPreferences.mergeRows(rows,rows,actions) }
            if(version!=notificationPreferenceRevision || rows!==catalogSessions) continue
            catalogSessions=merged
            selected?.let { row ->
                val raw=SessionNotificationPreferences.mergeDetail(row.raw,row.target,emptyList(),actions)
                if(raw!==row.raw) selected=row.copy(raw=raw)
            }
            activeFrame?.let { frame -> installFrame(frame,activeFrameVerified) }
            return
        }
    }
    fun recoveryActionReason(target:Target,jobId:String,choice:String):String {
        val reason=actionReason(target,"recovery_action");if(reason.isNotBlank()) return reason
        val args=runCatching { SessionActionPolicies.arguments("recovery_action",JSONObject().put("job_id",jobId).put("action",choice)) }.getOrNull() ?: return "Invalid native recovery action."
        return SessionActionPolicies.inspectReason(target,"recovery_action",args,activity ?: JSONObject())
    }
    fun recoveryAction(target:Target,jobId:String,choice:String) {
        val reason=recoveryActionReason(target,jobId,choice);if(reason.isNotBlank()) { error=reason;return }
        sessionAction(target,"recovery_action",JSONObject().put("job_id",jobId).put("action",choice))
    }
    fun checkSessionAction(action: SessionAction) {
        if(action.operation==SessionProjectProtocol.assign) { sessionProjects.check(action);return }
        if(action.requestId in actionFlights) return
        val connection=connections.find { it.id == action.target.connectionId } ?: return
        actionFlights += action.requestId
        viewModelScope.launch { try { settleSessionAction(action,api.await(connection,api.receipt(connection,action.requestId))) }
            catch(e:Exception) { if(e is CancellationException) throw e; error="The original action receipt could not be checked. Nothing has been retried." }
            finally { actionFlights -= action.requestId } }
    }
    fun reviewSessionAction(action: SessionAction) {
        if(action.requestId in actionFlights) return
        editState { state -> state.sessionActions.find { it.requestId == action.requestId && (it.blocksSending || it.needsProjectReview) }?.let { state.action(it.copy(status="reviewed")) } ?: state }
        flushDrafts()
    }
    fun cancelCompactContinuation(reason: String = "") {
        if (continuation != null && reason.isNotBlank()) contextNotice = reason
        continuation = null; continuationEnqueuing = ""
    }
    fun contextActionReason(target: Target, operation: String, dismissPrompt: Boolean = false): String =
        contextActionReason(target, operation, dismissPrompt, "")
    private fun contextActionReason(target: Target, operation: String, dismissPrompt: Boolean, reservedRequest: String): String {
        if (target.agentId.isNotBlank() || target.archiveId.isNotBlank() || !fresh(target)) return "Refresh the live conversation before changing its context."
        if (target.run.isBlank() || target.conversation.isBlank()) return "Wait for the conversation to finish starting."
        if (operation !in relayOperations[target.connectionId].orEmpty() || operation !in machineOperations[MachineKey(target.connectionId, target.computerId)].orEmpty()) return "This gateway or computer does not support native context commands."
        val pendingContext = if (reservedRequest.isBlank()) contextBlocked(target) else
            sessionActions.any { it.target == target && it.blocksSending } ||
                clearExpectation?.let { it.requestId != reservedRequest && ContextPolicies.sameSessionRun(it.target, target) } == true ||
                contextOperations.any { it.requestId != reservedRequest && ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending }
        if (compactContinuationActive || pendingContext || outgoing.any { it.belongsTo(target) && it.blocksSending } ||
            reservedRequest.isBlank() && target.key in preparingTargets) return "Resolve the pending delivery or context command first."
        if (pickerDraft?.target == target || attachmentImporting || attachmentPreparing) return "Wait for the selected files to finish importing."
        if (drafts.any { it.target == target && it.status in listOf("submitting", "uncertain") }) return "Wait for the pending answer or interrupt."
        if (dismissPrompt) {
            if (operation != "clear_context") return "Prompt dismissal is available only when clearing context."
            if (activeFrame?.target != target) return "Refresh the selected conversation before dismissing its prompt."
            if (speech.recording) return "Finish recording before changing context."
            return ContextPolicies.dismissReason(target, activity, relayFeatures[target.connectionId].orEmpty(),
                machineFeatures[MachineKey(target.connectionId, target.computerId)].orEmpty())
        }
        if (activity?.optBoolean(operation + "_supported") != true || activity?.string("activity") != "idle" || activity?.string("phase") != "idle") return "Native context changes are unavailable while the agent is busy or waiting for input."
        return ""
    }
    fun compactContext(target: Target, continueDraft: Boolean = false, expectedDraft: Draft? = null) = startContext(target, "compact_context", continueDraft, expectedDraft)
    fun clearContext(target: Target, dismissPrompt: Boolean = false) = startContext(target, "clear_context", false, null, dismissPrompt)
    private fun startContext(target: Target, operation: String, continueDraft: Boolean, expectedDraft: Draft?, dismissPrompt: Boolean = false) = viewModelScope.launch {
        val reason = contextActionReason(target, operation, dismissPrompt)
        if (demo || !storageReady || reason.isNotBlank()) { if (reason.isNotBlank()) contextNotice = reason; return@launch }
        val original = draft(target)
        if (expectedDraft != null && !ContextPolicies.sameDraft(expectedDraft, original)) { contextNotice = "Your composition changed. Review it before choosing a context action."; return@launch }
        if (continueDraft && original.text.isBlank() && original.attachments.isEmpty()) return@launch
        val connection = connections.find { it.id == target.connectionId } ?: return@launch
        val op = ContextOperation(UUID.randomUUID().toString(), target, operation, dismissPrompt = true.takeIf { dismissPrompt })
        val navigation = navigationId
        preparingTargets += target.key; contextFlights += op.requestId
        if (continueDraft) continuation = CompactContinuation(op.requestId, original.copy(attachments = original.attachments.toList()), navigation, android.os.SystemClock.elapsedRealtime())
        contextNoticeOwner=op.requestId;contextNoticeValue=""
        if (operation == "clear_context") clearExpectation = ClearExpectation(op.requestId, target, navigation)
        var attempted = false
        var rejection: RelayException? = null
        try {
            durable({ state ->
                check(state.contextOperations.none { ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending })
                state.context(op)
            }, { current, written -> current.context(written.contextOperations.first { it.requestId == op.requestId }) })
            check(fresh(target) && navigationId == navigation && (!continueDraft || continuation?.requestId == op.requestId) && (operation != "clear_context" || clearExpectation?.requestId == op.requestId)) { "The selected conversation changed before transport." }
            if (dismissPrompt) check(!demo && storageReady && contextActionReason(target, operation, true, op.requestId).isBlank()) {
                "Prompt dismissal is no longer available. Nothing was sent."
            }
            val payload = ContextPolicies.payload(op)
            attempted = true
            val initial = try { api.submit(connection, target, operation, payload, op.requestId, SendAttempt()) }
            catch (error: RelayException) { if (error.status in listOf(400, 401, 403, 404, 409, 413, 429)) rejection = error; throw error }
            val receipt = api.await(connection, initial)
            val result = ContextPolicies.result(op, receipt)
            durable({ state -> updateContext(state, op.requestId) { current ->
                if (current.status in listOf("completed", "failed", "cancelled", "unchanged", "reviewed")) current else result
            } })
            if (result.status == "submitted") acknowledgedContexts += op.requestId
            if (result.status !in listOf("submitted", "completed")) { contextFlights -= op.requestId; cancelCompactContinuation("Context delivery was not confirmed. Your message was not sent.") }
            if (operation == "clear_context" && clearExpectation?.requestId == op.requestId) {
                if (result.status == "completed") clearExpectation = clearExpectation?.copy(confirmed = true)
                else if (result.status != "submitted") clearExpectation = null
            }
            if (result.status == "completed") contextFlights -= op.requestId
            setContextNotice(op.requestId,if (result.status == "submitted") "Native command submitted. Waiting for the agent to confirm the result." else result.error)
            if (selected?.target?.let { ContextPolicies.sameSessionRun(it, target) } == true) refreshActivity()
            activeFrame?.takeIf { it.target == target && activityVerified }?.let(::observeContext)
        } catch (failure: Exception) {
            val status = if (attempted && rejection == null) "uncertain" else "failed"
            try { durable({ state -> updateContext(state, op.requestId) { current -> if (current.status in listOf("completed", "reviewed", "cancelled", "unchanged")) current else current.copy(status = status,
                error = if (status == "uncertain") "Delivery is unconfirmed. Check the original context receipt before trying again." else "The context command was not sent or was rejected. Your draft is preserved.") } }) }
            catch (_: Exception) { contextNotice = "Could not save the command outcome. Its original identity remains recoverable." }
            cancelCompactContinuation("Compaction was not confirmed. Your message was not sent."); clearExpectation = null; contextFlights -= op.requestId
            if (failure is CancellationException) throw failure
        } finally { preparingTargets -= target.key }
    }
    private fun updateContext(state: MessageState, requestId: String, transform: (ContextOperation) -> ContextOperation): MessageState {
        val current = state.contextOperations.find { it.requestId == requestId } ?: return state
        return state.context(transform(current))
    }
    private fun observeContext(frame: Frame) {
        val intent = continuation
        if (intent != null && !intent.allowed(selected?.target, navigationId, android.os.SystemClock.elapsedRealtime())) cancelCompactContinuation("Continuation was canceled. Your draft has not been sent.")
        messageState.contextOperations.filter { it.target == frame.target && it.operation == "compact_context" && it.status != "reviewed" }.forEach { op ->
            val status = ContextPolicies.compactStatus(op, frame.raw) ?: return@forEach
            if (status != "completed" && continuation?.requestId == op.requestId) cancelCompactContinuation("Compaction did not complete. Your draft was kept without sending.")
            if (op.requestId in contextObservations) return@forEach
            contextObservations += op.requestId
            viewModelScope.launch {
                try {
                    if (op.status != status) durable({ state -> updateContext(state, op.requestId) { current -> if (current.status == "reviewed") current else current.copy(status = status, error = if (status == "completed") "" else "Native compaction is $status. Your draft was not sent.") } })
                    contextFlights -= op.requestId
                    setContextNotice(op.requestId,if(status == "completed") "" else "Native compaction is $status. Your draft was not sent.")
                    val current = continuation?.takeIf { it.requestId == op.requestId } ?: return@launch
                    if (status == "completed" && op.requestId in acknowledgedContexts && continuationEnqueuing.isBlank() && fresh(current.draft.target) &&
                        current.allowed(selected?.target, navigationId, android.os.SystemClock.elapsedRealtime()) && current.unchanged(draft(current.draft.target)) && !sendingBlocked(current.draft.target)) {
                        continuationEnqueuing = current.requestId
                        sendMessageInternal(current.draft.target, current.draft, current.requestId, true, current.requestId)
                    }
                } catch (_: Exception) { cancelCompactContinuation("Could not save compaction confirmation. Your message was not sent.") }
                finally { contextObservations -= op.requestId }
            }
        }
    }
    fun checkContextOperation(operation: ContextOperation) = viewModelScope.launch {
        val connection = connections.find { it.id == operation.target.connectionId } ?: return@launch
        val flight = "context:${operation.requestId}"
        if (flight in receiptFlights) return@launch
        receiptFlights += flight
        try {
            val result = ContextPolicies.result(operation, api.await(connection, api.receipt(connection, operation.requestId)))
            durable({ state -> updateContext(state, operation.requestId) { current -> if (current.status in listOf("completed", "reviewed")) current else result } })
            val frame = inspectOnce(connection, operation.target).await()
            ContextPolicies.compactStatus(operation, frame.raw)?.let { status -> durable({ state -> updateContext(state, operation.requestId) { current -> if (current.status == "reviewed") current else current.copy(status = status, error = "") } }) }
            contextNotice = "Checked the original command. No command or message was retried."
            if (selected?.target?.let { ContextPolicies.sameSessionRun(it, operation.target) } == true) refreshActivity()
        } catch (failure: CancellationException) { throw failure }
        catch (_: Exception) { contextNotice = "The original receipt is unavailable. Its saved identity has been preserved." }
        finally { receiptFlights -= flight; retainOutbox() }
    }
    fun reviewContextOperation(operation: ContextOperation) {
        if ("context:${operation.requestId}" in receiptFlights) return
        if (continuation?.requestId == operation.requestId) cancelCompactContinuation()
        if (clearExpectation?.requestId == operation.requestId) clearExpectation = null
        try { editState { state -> updateContext(state, operation.requestId) { if (it.status == "completed") it else it.copy(status = "reviewed") } }; flushDrafts() }
        catch (_: Exception) { contextNotice = "Could not save the review. No command was retried." }
    }
    fun contextDrafts(target: Target) = drafts.filter { it.target != target && ContextPolicies.sameSession(it.target, target) && it.questionId.isBlank() && it.detachedId.isBlank() && it.status == "editing" && (it.text.isNotBlank() || it.attachments.isNotEmpty()) }
    fun canRestoreContextDraft(draft: Draft, target: Target) = fresh(target) && ContextPolicies.sameSession(draft.target, target) && draft.target != target && draft.questionId.isBlank() && draft.detachedId.isBlank() && draft.status == "editing"
    fun restoreContextDraft(draft: Draft, target: Target) {
        if (!canRestoreContextDraft(draft, target) || target.key in preparingTargets) return
        cancelCompactContinuation()
        val restored = draft.copy(target = target, generation = UUID.randomUUID().toString(), revision = UUID.randomUUID().toString())
        val parkedId = UUID.randomUUID().toString()
        try { editState { state ->
            val original = state.drafts.find { it.key == draft.key && it.revision == draft.revision && it.generation == draft.generation } ?: return@editState state
            val editor = state.drafts.find { it.target == target && it.questionId.isBlank() && it.detachedId.isBlank() }
            if (editor != null && editor.status != "editing") return@editState state
            val parked = editor?.takeIf { it.text.isNotBlank() || it.attachments.isNotEmpty() }?.copy(detachedId = parkedId)
            state.copy(drafts = state.drafts.filterNot { it.key == original.key || it.key == restored.key } + listOfNotNull(parked) + restored)
        }; flushDrafts() } catch (_: Exception) { contextNotice = "The original draft remains saved." }
    }
    fun clearError() { error = "" }
    fun preview() {
        demo = true
        val previewMachines = linkedMapOf("demo" to Demo.computer.label,"demo-laptop" to "Preview laptop","demo-build" to "Preview build machine with a long display name")
        val project = ProjectSummary(ProjectKey("demo", "demo", "zerus"), "Zerus", "#67E8CB", listOf(
            ProjectFolder("demo", "Zerus", "/workspace/zerus", "demo", previewMachines.getValue("demo")),
            ProjectFolder("demo-design", "Design sandbox", "/workspace/zerus/design", "demo", previewMachines.getValue("demo")),
            ProjectFolder("demo-long", "mobile", "/workspace/research/experiments/a-long-folder-name-for-previewing-wrapped-paths/mobile", "demo", previewMachines.getValue("demo")),
            ProjectFolder("demo-laptop-main", "zerus", "/workspace/zerus", "demo-laptop", previewMachines.getValue("demo-laptop")),
            ProjectFolder("demo-laptop-mobile", "mobile", "/workspace/zerus/mobile", "demo-laptop", previewMachines.getValue("demo-laptop")),
            ProjectFolder("demo-laptop-test", "mobile", "/workspace/testing/mobile", "demo-laptop", previewMachines.getValue("demo-laptop")),
            ProjectFolder("demo-build-main", "zerus", "/workspace/build/zerus", "demo-build", previewMachines.getValue("demo-build"))
        ), previewMachines)
        projects = listOf(project,ProjectSummary(ProjectKey("demo","demo","empty-preview"),"Empty preview project","#8DA9E8",emptyList(),emptyMap()))
        catalogSessions = Demo.sessions.map { it.copy(projectKey = project.key) }
        machines = previewMachines.map { (id,name) -> Machine("demo",id,name,true) }
    }
    fun stopPreview() { demo = false; back(); selectedProject = null; catalogSessions = emptyList(); archive.configure(emptySet(),"",false); projects = emptyList(); machines = emptyList(); refresh() }
    fun pair(url: String, code: String, onDone: () -> Unit) = viewModelScope.launch {
        if (!storageReady || busy) return@launch
        busy = true
        try {
            val encrypted=WorkspaceProtocol.encryptedInvitationCode(code.trim())
            val connection = if(encrypted) api.prepareEncryptedPair(url,code) else api.pair(url, code)
            val next = connections + connection
            withContext(Dispatchers.IO) { store.saveConnections(next) }; connections = next; demo = false
            if(encrypted) pendingEncryptedPairings+=connection.id
            onDone()
            if(encrypted) api.enrollPrepared(connection)
            refresh(); AppTelemetry.registerPush(getApplication())
            if (LiveConnectionService.running) getApplication<Application>().startService(android.content.Intent(getApplication(), LiveConnectionService::class.java))
        } catch (e: Exception) { error = e.message ?: "Pairing failed." }
        finally { busy = false }
    }
    fun resumeEncryptedPair(connection:Connection)=viewModelScope.launch {
        if(busy || connection.protocolVersion!=2 || connection !in connections) return@launch
        busy=true
        try { api.enrollPrepared(connection);refresh() }
        catch(e:Exception) { error=e.message ?: "Could not resume encrypted phone enrollment." }
        finally { busy=false }
    }
    fun disconnect(connection: Connection) = viewModelScope.launch {
        if (busy) return@launch
        busy = true
        try {
            try { if(connection.protocolVersion==1) api.revoke(connection) }
            catch (e: RelayException) { if (e.status != 401) throw e }
            val current=connections
            val next=withContext(Dispatchers.IO) {
                ConnectionRemoval.forget(connection,current,{ store.workspaceJournal.forget(it) },store::saveConnections)
            }
            connections = next
            api.closeResultStreams(connection.id)
            pendingEncryptedPairings-=connection.id
            sharedReadRetries.forget(connection.id); scheduleSharedReadRetries()
            if (catalogErrorOwner == connection.id || detailReadErrorOwner?.first?.connectionId == connection.id) error = ""
            accountsWriter?.edit { rows -> rows.filterNot { it.key.connectionId==connection.id } }; accountsWriter?.flushAsync()
            inspections.values.filter { it.first.connectionId == connection.id }.forEach { it.second.cancel() }
            inspections.entries.removeAll { it.value.first.connectionId == connection.id }
            frames.entries.removeAll { it.value.target.connectionId == connection.id }
            editState { state -> state.copy(conversationIndex=state.conversationIndex.filterNot { it.target.connectionId == connection.id }) }
            val removedReadKeys=readState.records.filter { it.target.connectionId==connection.id }.map { ConversationReadPolicies.key(it.target) }.toSet()
            readWriter?.let { readState=it.edit { state -> state.copy(records=state.records.filterNot { it.target.connectionId == connection.id }) } };flushDrafts()
            projectionJobs.values.filter { it.first.connectionId==connection.id }.forEach { it.second.cancel() }
            projectionJobs.entries.removeAll { it.value.first.connectionId==connection.id }
            projectionHeads.keys.removeAll { key -> historyWindows[key]?.target?.connectionId==connection.id }
            historyWindows.entries.removeAll { it.value.target.connectionId==connection.id }
            historySnapshotHints.keys.removeAll { it in removedReadKeys };currentHistoryHeads-=removedReadKeys
            historyFlights.values.filter { it.first.connectionId==connection.id }.forEach { it.second.cancel() }
            historyFlights.entries.removeAll { it.value.first.connectionId==connection.id }
            runCatching { historyCache.purgeWorkspace(connection.id);pageCache.purgeWorkspace(connection.id) }
            taskDigests = taskDigests.filterKeys { it.connectionId != connection.id }
            // A merge is idempotent: the running exchange stops here, and the replicas go.
            taskSyncJob?.cancelAndJoin()
            runCatching { taskSync.purge(connection.id) }
            attachments.clear()
            computerFiles.clear()
            catalogSessions = catalogSessions.filterNot { it.target.connectionId == connection.id }
            projects = projects.filterNot { it.key.connectionId == connection.id }
            if (selectedProject?.key?.connectionId == connection.id) selectedProject = null
            if (sessionProjectScope?.key?.connectionId == connection.id) clearSessionScope()
            machines = machines.filterNot { it.connectionId == connection.id }
            selectedMachines = selectedMachines.filterNot { it.connectionId == connection.id }.toSet()
            relayOperations = relayOperations - connection.id; machineOperations = machineOperations.filterKeys { it.connectionId != connection.id }
            if (selected?.target?.connectionId == connection.id) { navigationId++; cancelDetail(); selected = null; activity = null; activeFrame = null; activityVerified = false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-connection-forgotten */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.CONNECTION_FORGOTTEN) /* END QUESTION_AVAILABILITY_PROBE writer-connection-forgotten */; conversationEvents = emptyList(); conversationQuestions = emptyList(); conversationOutgoing = emptyList(); cancelCompactContinuation(); clearExpectation = null }
        } catch (e: Exception) { error = if(connection.protocolVersion==2) "Could not finish forgetting this encrypted connection. Try Forget again. ${e.message}"
            else "Could not revoke this phone on the gateway. The connection is preserved. ${e.message}" }
        finally { busy = false }
    }
    fun refresh(explicit: Boolean = false) {
        if (demo) return
        val start = catalogGate.begin("catalog", explicit)
        catalogProgress = catalogGate.visible()
        if (!start.newRequest) return
        val operation = start.operation
        val originalConnections = connections
        catalogBusy = true
        catalogRefresh=catalogRefresh.begin(operation.id,originalConnections.map { it.id to it.displayName })
        catalogTimer?.cancel()
        catalogTimer = viewModelScope.launch {
            delay(catalogGate.remaining(operation.id))
            if (catalogGate.owns(operation.id)) catalogProgress = catalogGate.visible()
        }
        catalogJob = viewModelScope.launch {
        val newSessions = mutableListOf<Session>(); val newMachines = mutableListOf<Machine>(); val newAccounts=mutableListOf<AccountCatalog>()
        val successfulConnections=mutableSetOf<String>()
        var published=false
        val newProjects = mutableListOf<ProjectSummary>(); val newOperations = mutableMapOf<MachineKey, Set<String>>(); val newFeatures = mutableMapOf<MachineKey, Set<String>>(); val warnings = mutableListOf<String>()
        val newDigests = mutableMapOf<ProjectKey, Map<String, String>>()
        try {
            pushStatuses = withContext(Dispatchers.IO) { originalConnections.associate { it.id to store.pushStatus(it.id) } }
            for (connection in originalConnections) {
                try {
                    if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
                    if (explicit || connection.id !in relayOperations) {
                        catalogRefresh=catalogRefresh.stage(operation.id,connection.id,CatalogRefreshStage.Capabilities)
                        try {
                            val capabilities = api.capabilities(connection)
                            val operations = OperationCapabilities.parse(capabilities,requireVersion=false)
                            if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
                            relayFeatures=relayFeatures + (connection.id to capabilities.optJSONArray("features")?.let { value -> (0 until value.length()).map { value.optString(it) }.toSet() }.orEmpty())
                            relayOperations = OperationCapabilities.replace(relayOperations,connection.id,operations)
                        } catch (e: CancellationException) { throw e }
                        catch (_: Exception) {
                            // Session discovery remains available; failed discovery is unknown, not an adapter refusal.
                            if (catalogGate.owns(operation.id) && connections == originalConnections)
                                relayOperations = relayOperations - connection.id
                        }
                    }
                    if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
                    catalogRefresh=catalogRefresh.stage(operation.id,connection.id,CatalogRefreshStage.Reports)
                    val computers = api.computers(connection)
                    if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
                    if(connection.protocolVersion==2) {
                        pendingEncryptedPairings-=connection.id
                        AppTelemetry.encryptedWorkspaceReady(getApplication(),connection)
                    }
                    val retained=catalogSessions;val actions=messageState.sessionActions
                    val previousMachines=machines;val previousProjects=projects;val previousAccounts=accountCatalogs
                    val prepared = withContext(Dispatchers.Default) {
                        WorkspaceCatalogCache.preserve(connection,computers,
                            SessionCatalogParser.parse(connection, computers.optJSONArray("computers")?.objects().orEmpty(),retained,actions),
                            retained,previousMachines,previousProjects,previousAccounts)
                    }
                    if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
                    newAccounts += prepared.accounts; newMachines += prepared.machines
                    newOperations += prepared.operations; newFeatures += prepared.features
                    val catalog = prepared.projects
                    newSessions += catalog.sessions; newProjects += catalog.projects; warnings += catalog.unavailableComputers
                    newDigests += catalog.taskDigests
                    successfulConnections += connection.id
                    catalogRefresh=catalogRefresh.stage(operation.id,connection.id,CatalogRefreshStage.Applying)
                    if (catalogGate.owns(operation.id) && connections == originalConnections && catalogErrorOwner == connection.id) error = ""
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) { if (catalogGate.owns(operation.id) && connections == originalConnections) {
                    catalogRefresh=catalogRefresh.stage(operation.id,connection.id,CatalogRefreshStage.Failed)
                    if(selected==null) { error = "${connection.displayName}: ${ReadFailure.message(e, "sessions")}"; catalogErrorOwner = connection.id }
                } }
            }
            if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
            val unavailable=originalConnections.map { it.id }.toSet() - successfulConnections
            if(unavailable.isNotEmpty()) {
                val retained=catalogSessions
                newSessions += withContext(Dispatchers.Default) {
                    retained.filter { it.target.connectionId in unavailable }.map { old -> old.copy(raw=JSONObject(old.raw.toString()).put("last_known",true)) }
                }
                if (!catalogGate.owns(operation.id) || connections != originalConnections) return@launch
            }
            newMachines += machines.filter { it.connectionId in unavailable }.map { it.copy(online=false,lastKnown=true) }
            newProjects += projects.filter { it.key.connectionId in unavailable }
            // A receipt or newer catalog may arrive while background parsing is suspended.
            var preparedSessions:List<Session>
            while(true) {
                val retained=catalogSessions;val actions=messageState.sessionActions;val version=notificationPreferenceRevision
                val reads=readState;val ready=readTrackingReady;val heads=currentHistoryHeads;val loaded=loadedReadRows()
                val merged=withContext(Dispatchers.Default) { SessionCatalogUpdate.prepare(newSessions,retained,actions,reads,ready,heads,loaded) }
                if(!catalogGate.owns(operation.id) || connections!=originalConnections) return@launch
                if(version!=notificationPreferenceRevision || retained!==catalogSessions || reads!==readState || ready!=readTrackingReady || heads!==currentHistoryHeads) continue
                preparedSessions=merged;break
            }
            taskDigests = newDigests + taskDigests.filterKeys { it.connectionId in unavailable }
            taskDigestsAt = android.os.SystemClock.elapsedRealtime()
            accountsWriter?.edit { old -> AccountSnapshots.merge(old,newAccounts,successfulConnections,originalConnections.map { it.id }.toSet()) }
            androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                sessions = preparedSessions; machines = newMachines.map { MachineNames.apply(it,messageState.machineAliases) }; machineOperations = newOperations; machineFeatures = newFeatures
                projects = newProjects; projectWarnings = warnings
            }
            refreshReadAttention();adoptSharedReads()
            val index=withContext(Dispatchers.Default) { ConversationIndex.capture(preparedSessions,newMachines) }
            if(connections == originalConnections) editState { state -> state.copy(conversationIndex=index) }
            watchHistoryHeads()
            selectedProject?.let { chosen -> selectedProject = projects.find { it.key == chosen.key } ?: chosen }
            sessionProjectScope?.let { chosen -> sessionProjectScope = projects.find { it.key == chosen.key } ?: chosen }
            pendingNotification?.let { (connection, computer, name) ->
                newSessions.find { it.target.connectionId == connection && it.target.computerId == computer && it.target.session == name && it.target.archiveId.isBlank() }?.let {
                    pendingNotification = null; open(it)
                }
            }
            published=connections==originalConnections && catalogGate.owns(operation.id)
        } finally {
            if (catalogGate.finish(operation.id)) {
                catalogRefresh=catalogRefresh.finish(operation.id,if(published) successfulConnections else emptySet(),System.currentTimeMillis()/1000.0)
                catalogTimer?.cancel(); catalogProgress = false; catalogBusy = false
                if (connections != originalConnections) refresh()
            }
        }
        }
    }
    /**
     * Exchanges every catalogued project's tasks with each computer whose digest differs
     * (TaskSync); the activity calls it every 15 s while started. With no catalog read
     * for 20 s, as while a conversation is open, it reads the computers itself.
     */
    fun syncTasks() {
        if (demo || taskSyncRunning) return
        taskSyncRunning = true
        val original = connections
        taskSyncJob = viewModelScope.launch {
            try {
                var digests = taskDigests; var operations = machineOperations; var known = machines
                if (android.os.SystemClock.elapsedRealtime() - taskDigestsAt > 20_000) {
                    val fresh = mutableMapOf<ProjectKey, Map<String, String>>(); val freshOperations = mutableMapOf<MachineKey, Set<String>>()
                    val seen = mutableListOf<Machine>()
                    for (connection in original) {
                        try {
                            val computers = api.computers(connection)
                            val prepared = withContext(Dispatchers.Default) {
                                SessionCatalogParser.parse(connection, computers.optJSONArray("computers")?.objects().orEmpty())
                            }
                            fresh += prepared.projects.taskDigests; freshOperations += prepared.operations; seen += prepared.machines
                        } catch (e: CancellationException) { throw e }
                        catch (_: Exception) { /* That connection syncs once it answers again. */ }
                    }
                    digests = fresh; operations = freshOperations; known = seen
                }
                val names = projects.associate { it.key to it.name }
                for ((key, peers) in TaskSyncPlan.peers(digests, relayOperations, operations, known)) {
                    if (connections != original) return@launch
                    val connection = original.find { it.id == key.connectionId } ?: continue
                    taskSync.sync(connection, key, names[key] ?: "Project", peers)
                    refreshReviewCount(key)
                    if (tasksKey == key) refreshTasks()
                }
                // A Send now that may have happened waits for an exchange with its computer.
                for (connection in original) {
                    if (connections != original) return@launch
                    taskSync.settleSends(connection) { key -> names[key] ?: "Project" }
                }
                heldSends = taskSync.heldTasks()
                refreshTasks()
            } catch (e: CancellationException) { throw e }
            // Failures wait in TaskSync's backoff; tasks have no screen to report them yet.
            catch (_: Exception) { }
            finally { taskSyncRunning = false }
        }
    }
    /** Nothing syncs in the background: the activity stops the running exchange when it stops; merges are idempotent. */
    fun stopTaskSync() { taskSyncJob?.cancel(); projectSyncs.toList().forEach { it.cancel() } }
    /** The exchanges that edits and opening Tasks start; they stop with the app too. */
    private val projectSyncs = java.util.concurrent.ConcurrentHashMap.newKeySet<Job>()
    private fun launchProjectSync(key: ProjectKey, then: suspend () -> Unit = {}) {
        val job = viewModelScope.launch { syncProject(key); then() }
        projectSyncs += job
        job.invokeOnCompletion { projectSyncs -= job }
    }

    // The Tasks screen (TasksUi.kt) over the phone's replica of a project.
    var tasksKey by mutableStateOf<ProjectKey?>(null); private set
    var tasksChosen by mutableStateOf<ChosenSession?>(null); private set
    var taskListing by mutableStateOf<List<TaskInfo>>(emptyList()); private set
    var taskTab by mutableStateOf(TaskTab.TODO)
    var tasksOnlyThisSession by mutableStateOf(false); private set
    /** The snackbar's next message on the Tasks screen. */
    var taskNotice by mutableStateOf<String?>(null)
    var taskBusy by mutableStateOf(false); private set
    var doneRows by mutableStateOf<List<DoneRow>>(emptyList()); private set
    var doneTotal by mutableStateOf(0); private set
    var doneRecent by mutableStateOf(0); private set
    var doneNext by mutableStateOf<DoneCursor?>(null); private set
    var doneExpanded by mutableStateOf(false); private set
    var doneLoading by mutableStateOf(false); private set
    var doneError by mutableStateOf(""); private set
    /** The tasks In review of each project, for the badge on the Tasks button. */
    var reviewCounts by mutableStateOf<Map<ProjectKey, Int>>(emptyMap()); private set
    /** TaskSync's held Send nows, as state, so a card follows their release. */
    private var heldSends by mutableStateOf<Set<String>>(emptySet())

    fun taskProjectName(key: ProjectKey) = projects.find { it.key == key }?.name ?: "Project"
    /** Only catalogued projects have tasks. */
    fun taskProject(session: Session) = session.projectKey?.takeIf { key -> projects.any { it.key == key && it.catalogued } }
    fun projectSessions(key: ProjectKey) = sessions.filter { it.projectKey == key && it.target.archiveId.isBlank() && it.target.agentId.isBlank() }
    fun taskChips(key: ProjectKey) = TasksPresentation.chips(projectSessions(key), machines)
    /** An operation counts when both the relay and the computer offer it. */
    private fun taskOperations(machine: Machine) =
        machineOperations[MachineKey(machine.connectionId, machine.id)].orEmpty() intersect relayOperations[machine.connectionId].orEmpty()
    /** The computers that list the project; every computer of its connection before the catalog names them. */
    private fun projectMachines(key: ProjectKey): List<Machine> {
        val listed = taskDigests[key]?.keys.orEmpty()
        return machines.filter { it.connectionId == key.connectionId && (listed.isEmpty() || it.id in listed) }
    }
    fun sendRoute(task: TaskInfo): RouteState {
        val key = tasksKey ?: return RouteState.Blocked("No project")
        if (taskBusy) return RouteState.Blocked("Another task action is running")
        if (task.uuid in heldSends) return RouteState.Blocked("It may have been sent already; its stage arrives with the next sync")
        return TasksPresentation.route(task, tasksChosen, machines, key.connectionId, ::taskOperations, projectSessions(key))
    }
    fun cancelRoute(task: TaskInfo): RouteState {
        val key = tasksKey ?: return RouteState.Blocked("No project")
        if (taskBusy) return RouteState.Blocked("Another task action is running")
        return TasksPresentation.cancelRoute(task, projectMachines(key), ::taskOperations)
    }

    fun openTasks(session: Session) {
        val key = taskProject(session) ?: return
        val machine = machines.firstOrNull { it.connectionId == session.target.connectionId && it.id == session.target.computerId }
        showTasks(key, ChosenSession(session.target.session, session.target.computerId, machine?.machineId.orEmpty()))
    }
    /** Another session for Tasks only, keeping the tab; one of another project shows that project's tasks. */
    fun chooseTaskSession(key: ProjectKey, chosen: ChosenSession) {
        if (key != tasksKey) showTasks(key, chosen, selectDefault = false)
        else { tasksChosen = chosen; if (tasksOnlyThisSession) loadDone(more = false) }
    }
    /** Only this session: Done then lists and counts only the tasks sent to it. */
    fun setOnlyThisSession(on: Boolean) {
        tasksOnlyThisSession = on
        loadDone(more = false)
    }
    private fun showTasks(key: ProjectKey, chosen: ChosenSession, selectDefault: Boolean = true) {
        tasksKey = key; tasksChosen = chosen; taskListing = emptyList(); doneRows = emptyList(); doneNext = null
        doneTotal = 0; doneRecent = 0; doneExpanded = false; doneError = ""; doneLoaded = false; taskNotice = null
        refreshTasks(selectDefault)
        loadDone(more = false)
        launchProjectSync(key)
    }
    fun closeTasks() { tasksKey = null; taskNotice = null }
    /** Done lists from a computer each time it is opened, so tasks accepted since show. */
    fun selectTaskTab(tab: TaskTab) {
        taskTab = tab
        if (tab == TaskTab.DONE && !doneLoading) loadDone(more = false)
    }
    /** A page of Done arrived, so its count is known. */
    var doneLoaded by mutableStateOf(false); private set

    fun refreshTasks(selectDefault: Boolean = false) {
        val key = tasksKey ?: return
        viewModelScope.launch {
            // An import may hold the project for a moment; the listing waits once, then keeps what it shows.
            var failure = ""
            var read: app.zerus.mobile.replica.TaskListing? = null
            for (attempt in 0..1) {
                try { read = withContext(Dispatchers.IO) { taskReplicas.store(key, taskProjectName(key)).use { it.list() } }; break }
                catch (e: CancellationException) { throw e }
                catch (e: Exception) { failure = e.message.orEmpty(); if (attempt == 0) delay(500) }
            }
            val listing = read ?: run {
                if (tasksKey == key && (selectDefault || taskListing.isEmpty())) taskNotice = failure.ifBlank { "The tasks could not be read." }
                return@launch
            }
            if (tasksKey != key) return@launch
            taskListing = listing.tasks
            reviewCounts = reviewCounts + (key to listing.tasks.count { it.stage == TaskStage.REVIEW })
            if (selectDefault) taskTab = TasksPresentation.defaultTab(TasksPresentation.counts(listing.tasks, tasksChosen, tasksOnlyThisSession))
        }
    }
    fun refreshReviewCount(key: ProjectKey) {
        viewModelScope.launch {
            val count = try {
                withContext(Dispatchers.IO) {
                    taskReplicas.store(key, taskProjectName(key)).use { store -> store.list().tasks.count { it.stage == TaskStage.REVIEW } }
                }
            } catch (e: CancellationException) { throw e }
            catch (_: Exception) { return@launch }
            reviewCounts = reviewCounts + (key to count)
        }
    }

    /** A local edit, then the project's exchange with its computers, so the edit reaches them. */
    private fun editTasks(change: (TaskStore) -> Unit) {
        val key = tasksKey ?: return
        viewModelScope.launch {
            try { withContext(Dispatchers.IO) { taskReplicas.store(key, taskProjectName(key)).use(change) } }
            catch (e: CancellationException) { throw e }
            catch (e: Exception) { taskNotice = e.message.orEmpty().ifBlank { "The task could not be changed." }; return@launch }
            refreshTasks()
            launchProjectSync(key) { if (tasksKey == key && taskTab == TaskTab.DONE) loadDone(more = false, keepExpanded = true) }
        }
    }
    /** One sync of the project with its computers now; failures wait in TaskSync's backoff. */
    private suspend fun syncProject(key: ProjectKey) {
        if (demo) return
        val connection = connections.find { it.id == key.connectionId } ?: return
        val peers = TaskSyncPlan.peers(mapOf(key to taskDigests[key].orEmpty()), relayOperations, machineOperations, machines)[key].orEmpty()
        if (peers.isEmpty()) return
        try { taskSync.sync(connection, key, taskProjectName(key), peers) }
        catch (e: CancellationException) { throw e }
        catch (_: Exception) { }
        if (tasksKey == key) refreshTasks()
    }

    /** New tasks go to To do, or to Backlog while Backlog is open, pinned to the chosen session. */
    fun addTask(text: String) {
        val chosen = tasksChosen
        TasksPresentation.addReason(chosen).takeIf { it.isNotEmpty() }?.let { taskNotice = it; return }
        val stage = if (taskTab == TaskTab.BACKLOG) TaskStage.BACKLOG else TaskStage.TODO
        val target = TaskTarget.Session(SessionRef(chosen!!.machineId, chosen.name))
        editTasks { it.add(text, stage, target, TaskPosition.End) }
    }
    private fun computerName(key: ProjectKey, computerId: String) =
        machines.firstOrNull { it.connectionId == key.connectionId && it.id == computerId }?.name ?: computerId
    /** The task settings' Save: the prompt and the session, whichever changed. */
    fun saveTask(uuid: String, text: String?, target: TaskTarget?) = editTasks { store ->
        if (text != null) store.editText(uuid, text)
        if (target != null) store.setTarget(uuid, target)
    }
    fun commentTask(uuid: String, text: String) = editTasks { it.comment(uuid, text) }
    /** Only the fields changed here, onto the replica's current settings, so a change that arrived meanwhile stays. */
    fun saveTaskSettings(original: TaskSettings, edited: TaskSettings) = editTasks { store ->
        val current = store.settings()
        fun <T> pick(field: (TaskSettings) -> T) = if (field(edited) != field(original)) field(edited) else field(current)
        store.setSettings(TaskSettings(pick { it.taskTemplate }, pick { it.cancelTemplate }, pick { it.agentsOther }, pick { it.agentLimit }))
    }
    suspend fun taskSettings(): TaskSettings? {
        val key = tasksKey ?: return null
        return try { withContext(Dispatchers.IO) { taskReplicas.store(key, taskProjectName(key)).use { it.settings() } } }
        catch (e: CancellationException) { throw e }
        catch (e: Exception) { taskNotice = e.message; null }
    }

    /** The task settings' action: their unsaved prompt and session first, then the action. */
    fun saveThen(task: TaskInfo, text: String?, target: TaskTarget?, action: TaskAction) {
        if (text == null && target == null) return taskAction(task, action)
        val key = tasksKey ?: return
        viewModelScope.launch {
            val saved = try {
                withContext(Dispatchers.IO) {
                    taskReplicas.store(key, taskProjectName(key)).use { store ->
                        if (text != null) store.editText(task.uuid, text)
                        if (target != null) store.setTarget(task.uuid, target)
                        store.get(task.uuid)
                    }
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { taskNotice = e.message.orEmpty().ifBlank { "The task could not be changed." }; return@launch }
            refreshTasks()
            taskAction(saved, action)
        }
    }
    /** A stage action of a card or the task settings; Cancel goes through its dialog. */
    fun taskAction(task: TaskInfo, action: TaskAction) {
        when (action) {
            TaskAction.TO_BACKLOG -> editTasks { it.setStage(task.uuid, TaskStage.BACKLOG) }
            TaskAction.TO_TODO, TaskAction.BACK_TO_TODO -> editTasks { it.setStage(task.uuid, TaskStage.TODO) }
            TaskAction.ACCEPT -> editTasks { it.setStage(task.uuid, TaskStage.DONE) }
            TaskAction.TO_TOP -> editTasks { it.moveTask(task.uuid, TaskPosition.Top) }
            TaskAction.SEND -> sendTask(task)
            TaskAction.EDIT, TaskAction.CANCEL -> {}
        }
    }
    private fun sendTask(task: TaskInfo) {
        val key = tasksKey ?: return
        val route = when (val state = sendRoute(task)) {
            is RouteState.Blocked -> { taskNotice = state.reason; return }
            is RouteState.Ready -> state.route
        }
        val connection = connections.find { it.id == key.connectionId } ?: return
        taskBusy = true
        viewModelScope.launch {
            try {
                taskNotice = when (val outcome = taskSync.send(connection, key, taskProjectName(key), route.computerId, task.uuid, route.session,
                    computerName(key, route.computerId))) {
                    TaskOutcome.Done -> "Sent to ${route.session}"
                    is TaskOutcome.Refused -> outcome.reason
                    is TaskOutcome.Uncertain -> "It may have been sent: ${outcome.reason}"
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { taskNotice = e.message.orEmpty().ifBlank { "The task could not be sent." } }
            finally { taskBusy = false; heldSends = taskSync.heldTasks() }
            refreshTasks()
        }
    }
    fun cancelTask(task: TaskInfo, reason: String) {
        val key = tasksKey ?: return
        val route = when (val state = cancelRoute(task)) {
            is RouteState.Blocked -> { taskNotice = state.reason; return }
            is RouteState.Ready -> state.route
        }
        val connection = connections.find { it.id == key.connectionId } ?: return
        taskBusy = true
        viewModelScope.launch {
            try {
                taskNotice = when (val outcome = taskSync.cancel(connection, key, taskProjectName(key), route.computerId, task.uuid, reason,
                    computerName(key, route.computerId))) {
                    TaskOutcome.Done -> "Cancelled; it is In review"
                    is TaskOutcome.Refused -> outcome.reason
                    is TaskOutcome.Uncertain -> "It may have been cancelled: ${outcome.reason}"
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { taskNotice = e.message.orEmpty().ifBlank { "The task could not be cancelled." } }
            finally { taskBusy = false }
            refreshTasks()
        }
    }

    /** The computer that lists Done: the chosen session's, or any of the project's that offers it. */
    private fun doneComputer(key: ProjectKey): Machine? {
        val able = projectMachines(key).filter { it.online && !it.lastKnown && "task_done" in taskOperations(it) }
        return able.firstOrNull { it.id == tasksChosen?.computerId } ?: able.firstOrNull()
    }
    /** The latest first-page read; an older one's answer is dropped. */
    private var doneRequest = 0
    /** The first page, or with `more` the next one; "Show older" also shows the older rows already loaded.
     * A reload after an edit keeps the older rows shown. */
    fun loadDone(more: Boolean, keepExpanded: Boolean = false) {
        val key = tasksKey ?: return
        if (more && doneLoading) return
        // The first page may already hold older tasks; showing them needs no read.
        if (more && (!doneExpanded && doneRows.size > doneRecent || doneNext == null)) { doneExpanded = true; return }
        doneExpanded = more || keepExpanded && doneExpanded
        val chosen = tasksChosen
        val session = if (!tasksOnlyThisSession) null else {
            TasksPresentation.addReason(chosen).takeIf { it.isNotEmpty() }?.let { doneError = it; return }
            SessionRef(chosen!!.machineId, chosen.name)
        }
        val machine = doneComputer(key) ?: run { doneError = "No computer of this project lists Done tasks now"; return }
        val connection = connections.find { it.id == key.connectionId } ?: return
        val request = ++doneRequest
        doneLoading = true
        viewModelScope.launch {
            try {
                val page = taskSync.donePage(connection, key, machine.id, if (more) doneNext else null, session = session)
                if (tasksKey != key || request != doneRequest) return@launch
                doneRows = if (more) doneRows + page.rows else page.rows
                doneNext = page.next; doneTotal = page.total; doneRecent = page.recent; doneError = ""; doneLoaded = true
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (request == doneRequest) doneError = e.message.orEmpty().ifBlank { "The Done list could not be read." } }
            finally { if (request == doneRequest) doneLoading = false }
        }
    }
    /** Fetches a Done task from that computer into the replica, then hands it over. */
    fun openDoneTask(row: DoneRow, opened: (TaskInfo) -> Unit) {
        val key = tasksKey ?: return
        val machine = doneComputer(key) ?: run { taskNotice = "No computer of this project is online"; return }
        val connection = connections.find { it.id == key.connectionId } ?: return
        taskBusy = true
        viewModelScope.launch {
            try {
                taskSync.exchangeNow(connection, key, taskProjectName(key), machine.id, fetch = listOf(row.uuid))
                val task = withContext(Dispatchers.IO) { taskReplicas.store(key, taskProjectName(key)).use { it.get(row.uuid) } }
                refreshTasks()
                opened(task)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { taskNotice = e.message.orEmpty().ifBlank { "The task could not be opened." } }
            finally { taskBusy = false }
        }
    }
    fun openProject(project: ProjectSummary) { selectedProject = project }
    fun clearProject() { selectedProject = null }
    fun open(session: Session, preserveProject: Boolean = false) {
        RelayReadTiming.point(RelayReadTiming.Stage.OpenConversation)
        contextNotice="";actionResultNotice="";firstUnreadEventId="";unreadBoundaryCaptured=false;restoreHistoryAttempt="";pendingHistoryPosition="";pendingHistoryPublication="";lastHistoryObservation=0L;lastHistoryReadCompletedAt=0L
        hasOlder=false;hasNewer=false;historyEpoch="";historyIndexing=false;historyComplete=false;olderError="";lastHistoryWasLatest=false
        if(session.target.agentId.isBlank()) parentSelection=null
        cancelCompactContinuation(); clearExpectation = null
        flushDrafts()
        if (!preserveProject) selectedProject = null
        navigationId++; cancelDetail()
        currentHistoryHeads-=ConversationReadPolicies.key(session.target)
        selected = session; activityVerified = false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-open-session */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.OPEN_SESSION) /* END QUESTION_AVAILABILITY_PROBE writer-open-session */; historyCached = false
        activeFrame = null; activity = null; conversationEvents = emptyList(); conversationQuestions = emptyList(); conversationOutgoing = emptyList(); conversationHistoryTruncated = false
        val following=readState.record(session.target)?.viewport?.following==true
        if(following) historyWindows.remove(session.target.key)
        val warm = frames[session.target.key]?.takeIf { if (it.observedAt == 0L) historyCache.getMemory(it.target) != null else System.currentTimeMillis() - it.observedAt in 0..7L * 24 * 60 * 60 * 1000 }
            ?.let { if(following) it.copy(events=it.inspectedEvents.ifEmpty { it.events },window=null,indexComplete=false,indexing=false,presentation=null) else it }
        if (warm == null) frames.remove(session.target.key)
        if (warm != null) installFrame(warm, false)
        else if (!demo) {
            val navigation = navigationId
            viewModelScope.launch {
                val raw = runCatching { historyCache.read(session.target) }.getOrNull() ?: return@launch
                val frame = withContext(Dispatchers.Default) {
                    val decoded = ConversationDisplayCache.decode(raw)
                    val inspected=NativeParser.events(decoded.raw)
                    Frame(session.target, decoded.raw, if(following) inspected.ifEmpty { decoded.events } else decoded.events, NativeParser.questions(decoded.raw), 0,
                        raw.toString().toByteArray(Charsets.UTF_8).size, decoded.truncated,inspectedEvents=inspected)
                }
                if (navigationId == navigation && selected?.target == session.target && !activityVerified && activeFrame == null) {
                    rememberFrame(frame); installFrame(frame, false)
                }
            }
        }
        if(!demo && readTrackingReady) {
            restoreSavedWindow(session.target,navigationId)
            // Only a saved older reading position requests canonical history.
            // A normal opening uses the bounded inspection tail immediately.
            restoreHistory(session.target)
        }
        if (demo) { conversationEvents = Demo.events; conversationQuestions = emptyList() }
        else refreshActivity()
    }
    fun childSendReason(target:Target):String {
        if(target.agentId.isBlank()) return ""
        if(!fresh(target)) return "Refresh this child conversation before sending."
        if("send_agent" !in relayFeatures[target.connectionId].orEmpty() || "send_agent" !in machineFeatures[MachineKey(target.connectionId,target.computerId)].orEmpty()) return "This native adapter supports child history only."
        return if(activity?.optBoolean("can_send") == true) "" else activity?.string("send_reason","can_send_reason").orEmpty().ifBlank { "This child conversation is read-only." }
    }
    fun agentInspectReason(parent:Target,agentId:String):String {
        if(parent.agentId.isNotBlank() || !fresh(parent)) return "Refresh the live parent conversation first."
        if("inspect_agent" !in relayFeatures[parent.connectionId].orEmpty() || "inspect_agent" !in machineFeatures[MachineKey(parent.connectionId,parent.computerId)].orEmpty()) return "This computer does not support child inspection."
        if(agentId.isBlank() || agentId == "main" || agentId.length > 256 || agentId.any { it.isISOControl() || it == '/' || it == '\\' }) return "Invalid child identity."
        if(activity?.optJSONObject("subagents")?.has(agentId) != true && activity?.optJSONArray("subagents")?.objects().orEmpty().none { it.string("id","agent_id") == agentId }) return "This child agent is no longer present."
        return ""
    }
    fun inspectAgent(parent: Target, agentId: String) {
        val reason=agentInspectReason(parent,agentId);if(reason.isNotBlank()) { error=reason;return }
        if(parent.agentId.isNotBlank() || !fresh(parent) || agentId.isBlank() || agentId == "main" || agentId.length > 256 || agentId.any { it.isISOControl() || it == '/' || it == '\\' }) return
        if("inspect_agent" !in relayFeatures[parent.connectionId].orEmpty() || "inspect_agent" !in machineFeatures[MachineKey(parent.connectionId,parent.computerId)].orEmpty()) { error="This computer does not support child inspection.";return }
        val roster=activity?.optJSONObject("subagents")
        if(roster?.has(agentId) != true && activity?.optJSONArray("subagents")?.objects().orEmpty().none { it.string("id","agent_id") == agentId }) { error="The child agent is no longer present.";return }
        val current=selected ?: return
        parentSelection=current
        open(current.copy(target=parent.copy(agentId=agentId,parentConversation=parent.conversation,conversation=parent.conversation + "/" + agentId),title=agentId),true)
    }
    private data class PendingAgentOpen(val navigation: Long, val parent: Target, val agentId: String, val deadline: Long)
    private var pendingAgentOpen: PendingAgentOpen? = null
    /** Checks a session card can make before its parent is open; [agentInspectReason] repeats them on the verified parent. */
    fun agentListReason(parent: Target, agentId: String, online: Boolean): String {
        if (demo) return "Preview sessions have no subagent activity."
        if (parent.agentId.isNotBlank() || parent.archiveId.isNotBlank()) return "Archived subagent activity is not available on this phone."
        if (!online) return "This computer is unavailable."
        if("inspect_agent" !in relayFeatures[parent.connectionId].orEmpty() || "inspect_agent" !in machineFeatures[MachineKey(parent.connectionId,parent.computerId)].orEmpty()) return "This computer does not support child inspection."
        return if (SessionCardPresentation.openableAgent(agentId)) "" else "Invalid child identity."
    }
    /** As in the desktop session tree, a child row opens its parent first, then the child once that exact conversation is verified. */
    fun openAgent(parent: Session, agentId: String, online: Boolean) {
        val reason = agentListReason(parent.target, agentId, online); if (reason.isNotBlank()) { error = reason; return }
        open(parent, preserveProject = true)
        pendingAgentOpen = PendingAgentOpen(navigationId, parent.target, agentId, android.os.SystemClock.elapsedRealtime() + 30_000)
    }
    private fun openPendingAgent() {
        val pending = pendingAgentOpen ?: return
        val target = selected?.target
        if (pending.navigation != navigationId || target == null) { pendingAgentOpen = null; return }
        if (!fresh(target)) return
        pendingAgentOpen = null
        when {
            android.os.SystemClock.elapsedRealtime() > pending.deadline -> error = "The session took too long to load. Open the subagent again."
            target.run != pending.parent.run || target.conversation != pending.parent.conversation -> error = "This session has a new conversation. Its earlier subagent was not opened."
            else -> inspectAgent(target, pending.agentId)
        }
    }
    fun openFromNotification(connection: String, computer: String, session: String) {
        pendingNotification = Triple(connection, computer, session); refresh()
    }
    private fun cancelDetail() {
        cancelTerminalAvailability()
        if(detailReadErrorOwner!=null) error=""
        detailGate.cancel(); detailJob?.cancel(); detailTimer?.cancel()
        detailBusy = false; detailProgress = false;latestNavigationIntent=null
        presentationRevision++; presentationJob?.cancel()
        olderJob?.cancel();olderLoading=false;readPositionReady=readTrackingReady
        processOutputJob?.cancel();processOutputLoading=false;processOutputResult=null;processOutputError=""
        terminalJob?.cancel();terminalVisible=false;terminalLoading=false;terminalVerified=false;terminalState=null
    }
    fun back() {
        if(selected?.target?.agentId?.isNotBlank() == true && parentSelection != null) { val parent=parentSelection!!;parentSelection=null;open(parent,true);return }
        cancelCompactContinuation(); clearExpectation = null
        flushDrafts()
        if (selected != null) { navigationId++; cancelDetail(); selected = null; activity = null; activeFrame = null; activityVerified = false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-back */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.BACK) /* END QUESTION_AVAILABILITY_PROBE writer-back */ }
        else selectedProject = null
        if(!demo) watchHistoryHeads()
    }
    fun refreshActivity(explicit: Boolean = false) {
        refreshActivity(explicit,freshInspection=false)
    }
    /** Cancelling a visible timer only stops its wait, never the retained read. */
    internal suspend fun awaitActivityRead() { detailJob?.join() }

    private fun refreshActivity(explicit: Boolean, freshInspection: Boolean, ownerCurrent: () -> Boolean = { true }) {
        if (demo || terminalVisible || latestNavigationIntent!=null) return
        val original = selected ?: return
        // Background refresh must not start another inspection
        // while the opening inspection is already verified and its first
        // history page is still arriving. Explicit refresh stays available.
        if(!explicit && !freshInspection && activityVerified && olderLoading && lastHistoryObservation==0L) return
        pendingActionResult(original.target)?.let { action -> openSessionActionResult(action);return }
        val connection = connections.find { it.id == original.target.connectionId } ?: return
        val navigation = navigationId
        val historyIntent=historyViewIntent
        val start = detailGate.begin("$navigation:${original.target.key}", explicit)
        detailProgress = detailGate.visible()
        if (!start.newRequest) return
        val operation = start.operation
        detailBusy = true
        detailJob?.cancel(); detailTimer?.cancel()
        detailTimer = viewModelScope.launch {
            delay(detailGate.remaining(operation.id))
            if (detailGate.owns(operation.id) && navigationId == navigation) detailProgress = detailGate.visible()
        }
        detailJob = viewModelScope.launch {
        var preparingIncoming = false
        var preparationOwner: Frame? = null
        var publicationFrame: Frame? = null
        fun ownsDetail() = detailGate.owns(operation.id) && navigationId == navigation &&
            historyViewIntent==historyIntent && selected?.target == original.target && (!freshInspection || ownerCurrent()) && connections.any { it == connection }
        try {
            val frame = inspectOnce(connection, original.target,fresh=freshInspection,current={
                !freshInspection || ownerCurrent() && detailGate.owns(operation.id) && navigationId==navigation && selected?.target==original.target
            }).await()
            if (detailGate.owns(operation.id) && navigationId == navigation && historyViewIntent==historyIntent && selected?.target == original.target && (!freshInspection || ownerCurrent())) {
                if(detailReadErrorOwner==(original.target to navigation)) error=""
                val clear = clearExpectation?.takeIf { it.navigation == navigation && ContextPolicies.sameSessionRun(it.target, frame.target) }
                if (clear != null && frame.target.conversation != clear.target.conversation && !clear.confirmed) {
                    activityVerified = false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-clear-unconfirmed */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.CLEAR_UNCONFIRMED) /* END QUESTION_AVAILABILITY_PROBE writer-clear-unconfirmed */; activeFrameVerified = false; historyCached = activity != null
                    return@launch
                }
                val clearOp = clear?.takeIf { it.confirmed && frame.target.conversation.isNotBlank() && frame.target.conversation != it.target.conversation }
                    ?.let { expectation -> messageState.contextOperations.find { it.requestId == expectation.requestId && it.status == "completed" } }
                if (clearOp != null) {
                    val messages = outgoing.filter { it.belongsTo(frame.target) }
                    val questionState = messageState
                    val (merged, questions) = withContext(Dispatchers.Default) {
                        ConversationMerge.merge(frame.target, frame.events, messages) to QuestionTracking.restore(questionState, frame.target, frame.questions)
                    }
                    if (!detailGate.owns(operation.id) || navigationId != navigation || selected?.target != original.target || clearExpectation?.requestId != clearOp.requestId) return@launch
                    frame.presentation = Presentation(messages, merged.events, questions)
                    androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                        editState { state -> ContextPolicies.promoteClearDraft(state, clearOp, frame.target) }
                        selected = original.copy(target = frame.target, status = frame.raw.string("activity", "phase", "state"), raw = frame.raw)
                        installFrame(frame, true)
                    }
                    clearExpectation = null
                    contextNotice = if (drafts.any { it.target == clearOp.target && it.questionId.isBlank() && it.detachedId.isBlank() })
                        "Context cleared. The previous composition remains in Saved drafts; restore it explicitly when ready." else "Context cleared. Your composition was kept."
                } else {
                    preparingIncoming = true
                    preparationOwner = activeFrame
                    val clearOwner = clearExpectation
                    val result = InspectionPublication.prepare(
                        owns = { ownsDetail() && activeFrame === preparationOwner && clearExpectation == clearOwner },
                        capture = {
                            FramePreparation(frameInstall(frame), messageState, outgoing.filter { it.belongsTo(frame.target) },
                                historyWindows[frame.target.key], presentationRevision, historyComplete, historyIndexing,
                                checkNotNull(selected), catalogSessions)
                        },
                        build = { captured -> withContext(Dispatchers.Default) {
                            val incoming = captured.install.frame
                            val state = captured.state.resolveConversation(original.target, incoming.target)
                            val merged = if (incoming.target.archiveId.isBlank()) ConversationMerge.merge(incoming.target, incoming.events, captured.messages)
                                else ConversationMerge.Result(incoming.events, emptySet())
                            Presentation(captured.messages, merged.events, QuestionTracking.restore(state, incoming.target, incoming.questions))
                        } },
                        unchanged = { captured ->
                            messageState === captured.state && presentationRevision == captured.revision &&
                                historyWindows[frame.target.key] === captured.window && historyComplete == captured.complete && historyIndexing == captured.indexing &&
                                outgoing.filter { it.belongsTo(frame.target) } == captured.messages &&
                                selected === captured.selection && catalogSessions === captured.catalog
                        },
                        publish = { captured, prepared ->
                            publicationFrame = captured.install.frame
                            androidx.compose.runtime.snapshots.Snapshot.withMutableSnapshot {
                                if (clearExpectation?.let { !ContextPolicies.sameSessionRun(it.target, frame.target) } == true) clearExpectation = null
                                selected = captured.selection.copy(target = frame.target, status = frame.raw.string("activity", "phase", "state"), raw = frame.raw)
                                editState { state -> state.resolveConversation(original.target, frame.target) }
                                captured.install.frame.presentation = prepared
                                installFrame(frame, true, captured.install)
                            }
                        })
                    if (result == InspectionPublication.Result.SUPERSEDED) return@launch
                    check(result == InspectionPublication.Result.PUBLISHED) { "The conversation changed. Refresh to verify it." }
                }
            }
        } catch (e: CancellationException) { throw e }
        catch (e: Exception) {
            val publishedHere = publicationFrame?.let {
                activeFrame === it && (selected?.target == original.target || selected?.target == it.target)
            } == true
            val originalHere = selected?.target == original.target && (!preparingIncoming || activeFrame === preparationOwner)
            val invalidate = if (!preparingIncoming) detailGate.owns(operation.id) && navigationId == navigation else
                detailGate.owns(operation.id) && navigationId == navigation && (!freshInspection || ownerCurrent()) &&
                    connections.any { it == connection } && (originalHere || publishedHere)
            if (invalidate) {
                activeFrameVerified = false; activityVerified = false /* BEGIN QUESTION_AVAILABILITY_PROBE writer-inspect-failed */; questionVerificationProbe(QuestionUiProbe.VerificationWriter.INSPECT_FAILED) /* END QUESTION_AVAILABILITY_PROBE writer-inspect-failed */; historyCached = activity != null; error = ReadFailure.message(e, "conversation");detailReadErrorOwner=original.target to navigation;detailReadOffline=e is java.io.IOException;if(!olderLoading) { readPositionReady=true;captureUnreadBoundary(original.target,conversationEvents) }
            }
        }
        finally {
            if (detailGate.finish(operation.id)) {
                detailTimer?.cancel(); detailBusy = false; detailProgress = false
            }
        }
        }
    }
    fun draft(target: Target, question: Question? = null): Draft {
        val questionId = question?.id.orEmpty(); val hash = question?.hash.orEmpty()
        return QuestionPolicies.savedDraft(drafts, target, question)
            ?: emptyDrafts.getOrPut(Triple(target, questionId, hash)) { Draft(target, "", questionId = questionId, questionHash = hash) }
    }
    private fun save(draft: Draft) { editState { state -> MessageChanges.draft(state, draft) } }
    private fun fresh(target: Target): Boolean {
        if(updateInstallPreparing) return false
        val raw = activity ?: return false
        return !terminalVisible && activityVerified && selected?.target == target && target.archiveId.isBlank() &&
            raw.string("run_id") == target.run && raw.string("conversation_id") == target.conversation &&
            raw.string("archive_id").isBlank() && raw.string("state") != "archived" && raw.string("agent_id") == target.agentId &&
            (target.agentId.isBlank() || raw.string("parent_conversation_id") == target.parentConversation) &&
            (raw.string("name").isBlank() || raw.string("name") == target.session)
    }
    fun edit(target: Target, text: String, question: Question? = null, answers: String = "") {
        val current = draft(target, question)
        if (target.archiveId.isNotBlank() || current.status != "editing" || !storageReady) return
        if (continuation?.draft?.target == target && (current.text != text || current.answers != answers)) cancelCompactContinuation("Draft changed. Your message will not be sent automatically.")
        val editor = current.copy(text = text, answers = answers, updatedAt = System.currentTimeMillis(), revision = UUID.randomUUID().toString())
        try { editState { state -> MessageChanges.text(state, editor) } } catch (e: Exception) { error = e.message.orEmpty() }
    }
    fun discard(draft: Draft) {
        if (draft.key in receiptFlights || drafts.find { it.key==draft.key }?.let(QuestionTracking::locked)==true || answerInFlight(draft)) return
        if (continuation?.draft?.target == draft.target) cancelCompactContinuation()
        emptyDrafts.remove(Triple(draft.target, draft.questionId, draft.questionHash))
        try { editState { state -> MessageChanges.discard(state, draft) }; flushDrafts() }
        catch (e: Exception) { error = e.message.orEmpty() }
    }
    fun review(draft: Draft) {
        if (draft.key in receiptFlights || drafts.find { it.key==draft.key }?.let(QuestionTracking::locked)==true ||
            answerInFlight(draft) || draft.status == "submitting" && draft.questionId.isBlank()) return
        try { editState { state -> state.copy(drafts = state.drafts.map { if (it.key == draft.key && it.requestId == draft.requestId) it.copy(status = "editing", requestId = "") else it }) }; flushDrafts() }
        catch (e: Exception) { error = e.message.orEmpty() }
    }
    private val answerFlights=mutableStateMapOf<String,String>()
    fun answerInFlight(draft:Draft)=answerFlights[draft.key]==draft.requestId && draft.requestId.isNotBlank()
    fun answerStatus(draft:Draft)=if(draft.status=="submitting" && !answerInFlight(draft)) "uncertain" else draft.status
    private val questionSendGate = QuestionSendGate()
    private var questionSendRefusal by mutableStateOf<QuestionSendRefusal?>(null)
    internal fun questionSendNotice(target: Target, question: Question): String =
        questionSendRefusal?.textFor(target, question).orEmpty()
    private fun refuseQuestionSend(intent: QuestionSendIntent, text: String) {
        questionSendRefusal = QuestionSendRefusal(intent.target, intent.question.id, intent.question.hash, text)
        contextNotice = text
        if (selected?.target == intent.target && conversationQuestions.none { it.id == intent.question.id && it.hash == intent.question.hash })
            error = text
    }
    internal var pendingQuestionSend by mutableStateOf<QuestionSendIntent?>(null); private set
    internal fun questionSendValid(intent: QuestionSendIntent): Boolean =
        fresh(intent.target) && !demo && storageReady && !contextBlocked(intent.target) &&
            intent.target.key !in preparingTargets && connections.any { it.id == intent.target.connectionId } &&
            intent.matches(selected?.target, navigationId,
                conversationQuestions.find { it.id == intent.question.id && it.hash == intent.question.hash },
                draft(intent.target, intent.question)) && intent.draft.key !in answerFlights
    internal fun cancelQuestionSend(intent: QuestionSendIntent) {
        questionSendGate.cancel(intent); pendingQuestionSend = questionSendGate.pending
    }
    internal fun confirmQuestionSend(intent: QuestionSendIntent) {
        if (!questionSendGate.active(intent)) return
        val confirmed = questionSendGate.confirm(intent, questionSendValid(intent))
        pendingQuestionSend = questionSendGate.pending
        if (confirmed == null) { refuseQuestionSend(intent, "The question or draft changed. Nothing was sent. Review it before submitting."); return }
        sendQuestion(intent.target, intent.question, intent.answers, confirmed)
    }
    fun send(target: Target, question: Question? = null, answers: JSONArray? = null) =
        if (question == null) sendMessage(target) else sendQuestion(target, question, answers?.toString())
    private fun sendQuestion(target: Target, question: Question, answerText: String?, consent: QuestionSendIntent? = null) = viewModelScope.launch {
        if(target.agentId.isNotBlank() || !fresh(target) || demo || !storageReady || contextBlocked(target) || target.key in preparingTargets) return@launch
        val native=conversationQuestions.find { it.id==question.id && it.hash==question.hash } ?: return@launch
        if(QuestionSendPolicies.schema(question) != QuestionSendPolicies.schema(native)) return@launch
        val connection=connections.find { it.id==target.connectionId } ?: return@launch
        val original=draft(target,question)
        if(original.status!="editing" || target.run.isBlank() || original.key in answerFlights) return@launch
        val frozenAnswers = answerText ?: original.answers
        if (!QuestionSendPolicies.available(native, target, frozenAnswers)) return@launch
        val intent = consent ?: QuestionSendPolicies.capture(target, navigationId, native, original, frozenAnswers)
        if (!questionSendValid(intent) || intent.answers != frozenAnswers) return@launch
        questionSendRefusal = null
        if (consent == null) {
            val authorized = questionSendGate.request(intent, ContextPresentation.isCold(activity))
            pendingQuestionSend = questionSendGate.pending
            if (authorized == null) return@launch
        }
        val sending=original.begin().copy(answers=frozenAnswers)
        var attempted=false;var refused=false
        answerFlights[sending.key]=sending.requestId;preparingTargets+=target.key
        try {
            save(sending);persistence!!.flush()
            if (consent == null && QuestionSendPolicies.requiresCacheConsent(native, ContextPresentation.isCold(activity))) {
                durable({ state -> intent.restoreAfterExpiry(state, sending.requestId) })
                refuseQuestionSend(intent, "The prompt cache expired before sending. Review your answer and confirm full context; nothing was sent.")
                return@launch
            }
            val currentDraft = draft(target, question)
            val currentQuestion = conversationQuestions.find { it.id == question.id && it.hash == question.hash }
            check(fresh(target) && !contextBlocked(target) && !demo && storageReady && connections.any { it == connection } &&
                intent.permitsPrepared(selected?.target, navigationId, currentQuestion, currentDraft,
                    sending.requestId, ContextPresentation.isCold(activity), consent != null)) {
                "The selected question changed before sending."
            }
            preparingTargets-=target.key;lastOwnSend=target to sending.requestId
            val payload=target.json().put("request_id",sending.requestId).put("question_id",question.id).put("expected_question_hash",question.hash).put("answers",JSONArray(frozenAnswers))
            val receipt=withTimeout(45_000) {
                attempted=true
                val initial=try { api.submit(connection,target,"answer",payload,sending.requestId, SendAttempt()) }
                    catch(e:RelayException) { refused=e.status in listOf(400,401,403,404,409,413,429);throw e }
                api.await(connection,initial)
            }
            settle(sending,receipt)
        } catch(e:Exception) {
            val status=if(!attempted || refused) "failed" else "uncertain"
            withContext(NonCancellable) {
                try { durable({ state -> state.copy(drafts=state.drafts.map { existing ->
                    if(existing.key==sending.key && existing.requestId==sending.requestId && existing.generation==sending.generation && !QuestionTracking.locked(existing)) existing.copy(status=status) else existing }) }) }
                catch(_:Exception) { error="Could not save the answer outcome. Its original request remains recoverable." }
            }
            error=if(status=="failed") "Answer not sent. ${e.message.orEmpty()}" else "Answer delivery is unknown. Check the original receipt; nothing has been retried."
            if(e is CancellationException && e !is kotlinx.coroutines.TimeoutCancellationException) throw e
        } finally {
            if(answerFlights[sending.key]==sending.requestId) answerFlights.remove(sending.key)
            preparingTargets-=target.key
        }
    }
    fun sendingBlocked(target: Target) = speech.recording || contextBlocked(target) || target.key in preparingTargets || outgoing.any { it.belongsTo(target) && it.blocksSending } || pickerDraft?.target == target
    fun sendMessage(target: Target, expectedDraft: Draft? = null, expectedCompactionId: String = "", allowColdCache: Boolean = false) =
        sendMessageInternal(target, expectedDraft, expectedCompactionId, allowColdCache, "")
    private fun sendMessageInternal(target: Target, expectedDraft: Draft?, expectedCompactionId: String, allowColdCache: Boolean, continuationId: String) = viewModelScope.launch {
        if (childSendReason(target).isNotBlank() || !fresh(target) || demo || !storageReady || NativeParser.messageBlockReason(activity).isNotBlank() || sendingBlocked(target)) { if (continuationId.isNotBlank()) cancelCompactContinuation("The conversation is not ready. Your draft was kept without sending."); return@launch }
        val connection = connections.find { it.id == target.connectionId } ?: return@launch
        val original = draft(target)
        if(original.attachments.isNotEmpty() && attachmentReason(target).isNotBlank()) { contextNotice=attachmentReason(target);return@launch }
        if (expectedDraft != null && !ContextPolicies.sameDraft(expectedDraft, original)) { contextNotice = "Your composition changed. Review it before sending."; return@launch }
        if (expectedCompactionId.isNotBlank() && (continuationId != expectedCompactionId || continuation?.requestId != continuationId ||
            ContextPolicies.compactStatus(messageState.contextOperations.find { it.requestId == continuationId } ?: return@launch, activity ?: return@launch) != "completed")) return@launch
        if (expectedCompactionId.isBlank() && !allowColdCache && ContextPresentation.isCold(activity)) { contextNotice = "Choose how to continue with the cold cache before sending."; return@launch }
        val intent = continuation?.takeIf { it.requestId == continuationId }
        if (intent != null) continuationEnqueuing = intent.requestId
        val id = UUID.randomUUID().toString(); val at = System.currentTimeMillis() / 1000.0
        preparingTargets += target.key; deliveryFlights += id
        val frozen=if(original.attachments.isEmpty()) captureSendIntent(target,id,original.text,expectedCompactionId.takeIf { it.isNotBlank() }) else null
        val written = try { durable({ state -> state.enqueue(original, id, at, frozen) }, { state, committed -> MessageChanges.enqueueAcknowledged(state, original, committed, id) }) }
        catch (e: Exception) { error = "Could not save the outgoing message. It was not sent. Your composition is preserved."; deliveryFlights -= id; syncOutgoing(); preparingTargets -= target.key; continuationEnqueuing = ""; cancelCompactContinuation(); return@launch }
        preparingTargets -= target.key
        val message = written.outgoing.first { it.requestId == id }
        lastOwnSend = target to id
        val send = OutgoingSend(SendAttempt(),
            submit = { evidence ->
                val files = attachmentStore.payload(message.attachments)
                check(fresh(target) && connections.any { it.id == connection.id }) { "The session changed before transport. Review the saved message." }
                if (intent != null) {
                    val cleared = written.drafts.first { it.key == original.key }
                    check(continuation?.requestId == intent.requestId && intent.permitsTransport(selected?.target, navigationId, android.os.SystemClock.elapsedRealtime(), cleared, draft(target)) &&
                        ContextPolicies.compactStatus(messageState.contextOperations.first { it.requestId == intent.requestId }, activity!!) == "completed") { "Compact continuation was canceled. The captured message was not sent and remains recoverable." }
                }
                val payload = message.sendIntent?.let { saved -> saved.value()?.getJSONObject("payload") ?: throw IllegalStateException("The saved message details are unavailable. Review this message.") }
                    ?: target.json().put("request_id", id).put("text", message.text).put("attachments", files).also {
                        if (expectedCompactionId.isNotBlank()) it.put("expected_compaction_id", expectedCompactionId)
                    }
                submitOutgoing(api, connection, target, payload, id, evidence)
            },
            await = { initial -> api.await(connection, initial) },
            settle = { receipt -> settleOutgoing(message, receipt) },
            failed = { status, failure ->
                try { durable({ state -> MessageChanges.result(state, id) { it.copy(status = status, error = failure) } }) }
                catch (_: Exception) { error = "Could not save the delivery result. The original outgoing message remains recoverable. Check its receipt." }
            },
            release = { api.releaseReservedOutgoing(connection, id) })
        try { send.run() } finally { if (intent != null) { continuationEnqueuing = ""; continuation = null }; deliveryFlights -= id; syncOutgoing() }
    }
    private fun captureSendIntent(target:Target,id:String,text:String,compactionId:String?=null):SendIntent? {
        val connection=connections.singleOrNull { it.id==target.connectionId } ?: return null
        val machine=SendReceiptRecovery.machine(machines,target) ?: return null
        val source=sessions.singleOrNull { it.target.connectionId==target.connectionId && it.target.computerId==target.computerId && it.target.run==target.run && it.target.agentId.isEmpty() && it.target.archiveId.isEmpty() } ?: return null
        val encoding=SendReceiptRecovery.encoding(connection.protocolVersion,machine.route) ?: return null
        return SendReceiptRecovery.freeze(target,id,text,source.agent,machine.machineId,source.raw.string("session_id"),encoding,compactionId)
    }
    private suspend fun settleOutgoing(message: OutgoingMessage, receipt: JSONObject, deadlineMs:Long?=null) {
        val result = receipt.optJSONObject("result"); val state = receipt.string("state")
        val proof=if(result!=null) message.originalSendIntent()?.let { SendReceiptRecovery.acknowledgement(it,result) } else null
        val strict=message.hasOriginalSendIdentity && SendReceiptRecovery.supported(relayOperations[message.target.connectionId],machineOperations[MachineKey(message.target.connectionId,message.target.computerId)])
        val confirmed = receipt.string("request_id") == message.requestId && state == "completed" && result?.string("status") in listOf("submitted", "queued") && (!strict || proof!=null) && (!message.hasOriginalSendIdentity || message.originalSendIntent()!=null)
        val rejected = state == "failed" && receipt.string("request_id") == message.requestId
        try { durable({ current -> if(current.outgoing.count { it.requestId==message.requestId }!=1) current else MessageChanges.result(current, message.requestId) { existing ->
            if(existing.target!=message.target || existing.text!=message.text || existing.attachments!=message.attachments || existing.createdAt!=message.createdAt || existing.originalSendIntent()!=message.originalSendIntent() ||
                deadlineMs!=null && SendReceiptRecovery.nowMs()>=deadlineMs || existing.sendReceiptProof!=null && existing.status in setOf("submitted","recorded")) existing
            else if (confirmed) existing.copy(status = "submitted", submittedAt = result!!.optDouble("submitted_at", existing.createdAt), submittedText = result.string("submitted_text").ifBlank { existing.text.trim() }, error = "",sendReceiptProof=proof)
            else existing.copy(status = if (rejected) "failed" else "uncertain", error = if (rejected) "The computer rejected this message. Review it before trying again." else "Delivery is unconfirmed. Check its receipt before sending again.")
        } }) } catch (_: Exception) { error = "Could not save the delivery result. Check its receipt; the outgoing message remains recoverable."; return }
        if (selected?.target?.let(message::belongsTo) == true) refreshActivity()
    }
    fun checkOutgoing(message: OutgoingMessage, automatic: Boolean = false) = viewModelScope.launch {
        val connection = connections.find { it.id == message.target.connectionId } ?: return@launch
        val flight = "outgoing:${message.requestId}"
        if (flight in receiptFlights || message.status == "sending") return@launch
        receiptFlights += flight
        try {
            val supported=SendReceiptRecovery.supported(relayOperations[message.target.connectionId],machineOperations[MachineKey(message.target.connectionId,message.target.computerId)])
            val intent=if(!supported) null else if(message.hasOriginalSendIdentity) message.originalSendIntent() else captureSendIntent(message.target,message.requestId,message.text)
            when(val checked=api.checkDelivery(connection,message,intent)) {
                is SendReceiptCheck.Original -> settleOutgoing(message,checked.receipt,checked.deadlineMs)
                is SendReceiptCheck.Native -> {
                    durable({ current -> SendReceiptRecovery.settle(current,message,checked.intent,checked.proof,checked.deadlineMs,SendReceiptRecovery.nowMs()) })
                    if(selected?.target?.let(message::belongsTo)==true) refreshActivity()
                }
                SendReceiptCheck.Unconfirmed -> if(!automatic) error="Delivery is still unconfirmed. The original message remains saved."
            }
        }
        catch (_: kotlinx.coroutines.TimeoutCancellationException) { if (!automatic) error = "Receipt check timed out. You can dismiss the warning and continue; the original message remains saved." }
        catch (e: CancellationException) { throw e }
        catch (_: Exception) { if (!automatic) error = "Receipt unavailable. The outgoing message remains saved." }
        finally { receiptFlights -= flight; retainOutbox() }
    }
    fun dismissOutgoing(message: OutgoingMessage) = viewModelScope.launch {
        val flight = "dismiss:${message.requestId}"
        if (!storageReady || flight in receiptFlights || message.requestId in deliveryFlights) return@launch
        receiptFlights += flight
        try { durable({ state -> MessageChanges.dismissOutgoing(state, message, recoverInterruptedSending = true) }) }
        catch (_: Exception) { error = "Could not save the dismissal. Your message and current composition are preserved." }
        finally { receiptFlights -= flight; retainOutbox() }
    }
    fun prepareOutgoing(message: OutgoingMessage) {
        if (message.status != "failed" || "outgoing:${message.requestId}" in receiptFlights) return
        val restored = Draft(message.target, message.text, attachments = message.attachments)
        val parkedId = UUID.randomUUID().toString()
        try { editState { state ->
            val current = state.outgoing.find { it.requestId == message.requestId }
            if (current != message || current.status != "failed") state else {
                val editor = state.drafts.find { it.target == message.target && it.questionId.isBlank() && it.detachedId.isBlank() }
                val parked = editor?.takeIf { it.text.isNotBlank() || it.attachments.isNotEmpty() }?.copy(detachedId = parkedId)
                state.copy(drafts = state.drafts.filterNot { it.key == restored.key } + listOfNotNull(parked) + restored,
                    outgoing = state.outgoing.map { if (it.requestId == message.requestId) it.copy(status = "reviewed", coverage = OutgoingCoverage.draft(it, restored)) else it })
            }
        }; flushDrafts() } catch (e: Exception) { error = e.message.orEmpty() }
    }
    fun attachmentReason(target:Target):String = NativeAttachments.reason(target,
        activity?.takeIf { it.string("run_id")==target.run && it.string("conversation_id")==target.conversation } ?: sessions.find { it.target==target }?.raw)
    fun beginAttachmentPick(target: Target, onReady: (String) -> Unit) {
        if(attachmentReason(target).isNotBlank()) return
        if (speech.recording || updateInstallPreparing || target.archiveId.isNotBlank() || childSendReason(target).isNotBlank() || demo || !storageReady || target.key in preparingTargets || attachmentPreparing || pickerDraft != null || importJob?.isActive == true || attachmentImporting) return
        if (continuation?.draft?.target == target) cancelCompactContinuation("Selecting files canceled the automatic continuation. Your composition is preserved.")
        val origin = draft(target); if (origin.status != "editing") return
        val id = UUID.randomUUID().toString()
        save(origin); attachmentPreparing = true; preparingTargets += target.key
        viewModelScope.launch {
            try {
                durable({ state -> state.copy(pickerDraft = origin, pickerId = id) })
                onReady(id)
            } catch (_: Exception) { error = "Could not save the file selection. Your draft is preserved." }
            finally { attachmentPreparing = false; preparingTargets -= target.key }
        }
    }
    fun finishAttachmentPick(uris: List<Uri>, selectionId: String) {
        if (selectionId != pickerId) return
        val origin = pickerDraft ?: return
        if (importJob?.isActive == true) return
        importJob = viewModelScope.launch {
            attachmentImporting = true
            val added = mutableListOf<Attachment>()
            val revision = UUID.randomUUID().toString(); val detachedId = UUID.randomUUID().toString()
            try {
                require(origin.attachments.size + uris.size <= AttachmentPolicy.MAX_FILES) { "Attach at most 8 files." }
                for (uri in uris) added += attachmentStore.importDocument(uri, origin.attachments + added)
                val newIds = added.map { it.id }.toSet()
                val result = withContext(NonCancellable) { durable({ state ->
                    check(state.pickerId == selectionId) { "File selection was canceled." }
                    state.selectionResult(origin, added).let { next -> next.copy(drafts = next.drafts.map { draft ->
                        if (draft.attachments.any { it.id in newIds }) draft.copy(revision = revision, detachedId = if (draft.detachedId.isBlank()) "" else detachedId) else draft
                    }) }
                }, { current, written -> MessageChanges.selectionAcknowledged(current, origin, written, newIds, selectionId) }) }
                if (result.drafts.any { it.detachedId == detachedId }) error = "The original composition changed. Its selected files are preserved in Saved drafts."
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { error = (e as? AttachmentException)?.message ?: e.message ?: "Could not import the selected files. The original draft is preserved." }
            finally {
                withContext(NonCancellable) {
                    runCatching { if (pickerId == selectionId) editState { state -> if (state.pickerId == selectionId) state.copy(pickerDraft = null, pickerId = "") else state } }
                    flushDrafts()
                    val protected = protectedAttachmentIds()
                    added.forEach { runCatching { attachmentStore.remove(it.id, protected) } }
                }
                attachmentImporting = false
            }
        }
    }
    fun recoverAttachmentPick(selectionId: String, launchedId: String) {
        if (!storageReady) return
        val busy = attachmentPreparing || attachmentImporting || importJob?.isActive == true
        if (AttachmentSelection.recover(messageState, selectionId, launchedId, busy) == messageState) return
        try {
            editState { AttachmentSelection.recover(it, selectionId, launchedId, busy) }
            flushDrafts()
            error = "File selection was interrupted. Your draft is preserved; choose files again."
        } catch (e: Exception) { error = e.message.orEmpty() }
    }
    fun cancelAttachmentPick(selectionId: String = pickerId, pendingOnly: Boolean = false) {
        if (selectionId != pickerId || (pendingOnly && (attachmentPreparing || attachmentImporting || importJob?.isActive == true))) return
        importJob?.cancel()
        try { editState { AttachmentSelection.cancel(it, selectionId) }; flushDrafts() } catch (e: Exception) { error = e.message.orEmpty() }
    }
    private fun protectedAttachmentIds(): Set<String> {
        fun refs(state: MessageState) = state.drafts.flatMap { it.attachments } + state.outgoing.flatMap { it.attachments } + state.pickerDraft?.attachments.orEmpty()
        return (refs(messageState) + persistence?.durableState?.let(::refs).orEmpty()).map { it.id }.toSet() + speech.protectedAudioIds()
    }
    fun removeAttachment(target: Target, id: String) = viewModelScope.launch {
        if (speech.recording || target.archiveId.isNotBlank() || pickerDraft?.target == target) return@launch
        if (continuation?.draft?.target == target) cancelCompactContinuation()
        val current = draft(target); if (current.status != "editing") return@launch
        try {
            editState { state -> state.copy(drafts = state.drafts.map { if (it.key == current.key && it.generation == current.generation && it.status == "editing") it.copy(attachments = it.attachments.filterNot { file -> file.id == id }) else it }) }
            persistence!!.flush()
            attachmentStore.remove(id, protectedAttachmentIds())
        } catch (e: Exception) { error = e.message.orEmpty() }
    }
    fun restoreDetached(draft: Draft) {
        if(QuestionTracking.locked(draft)) return
        if (draft.detachedId.isBlank()) return
        val restored = draft.copy(detachedId = "", generation = UUID.randomUUID().toString(), revision = UUID.randomUUID().toString())
        val parkedId = UUID.randomUUID().toString()
        try { editState { state ->
            val editor = state.drafts.find { it.key == restored.key }
            val parked = editor?.takeIf { it.text.isNotBlank() || it.attachments.isNotEmpty() }?.copy(detachedId = parkedId)
            state.copy(drafts = state.drafts.filterNot { it.key == draft.key || it.key == restored.key } + listOfNotNull(parked) + restored)
        }; flushDrafts() } catch (e: Exception) { error = e.message.orEmpty() }
    }
    private suspend fun settle(draft: Draft, receipt: JSONObject) {
        durable({ state -> QuestionTracking.settle(state,draft,receipt) })
        val latest=drafts.find { it.key==draft.key && it.requestId==draft.requestId }
        if(latest?.status=="submitted" || draft.questionId=="__interrupt" && latest==null) refreshActivity()
        else error=receipt.string("error").ifBlank { "Delivery is unknown. Check the original receipt and saved answer." }
    }
    fun checkReceipt(draft: Draft) = viewModelScope.launch {
        val connection=connections.find { it.id==draft.target.connectionId } ?: return@launch
        if(draft.requestId.isBlank() || draft.key in receiptFlights || answerInFlight(draft)) return@launch
        receiptFlights+=draft.key
        try { settle(draft,withTimeout(45_000) { api.await(connection,api.receipt(connection,draft.requestId)) }) }
        catch(e:Exception) { if(e is CancellationException && e !is kotlinx.coroutines.TimeoutCancellationException) throw e;error="Receipt unavailable. The answer remains saved." }
        finally { receiptFlights-=draft.key }
    }
    fun interrupt(target: Target, turn: Double) = viewModelScope.launch {
        if(target.agentId.isNotBlank()) return@launch
        val raw = activity ?: return@launch
        if (!fresh(target) || contextBlocked(target) || !raw.optBoolean("interrupt_supported") || raw.optDouble("turn_started") != turn || target.key in preparingTargets) return@launch
        val connection = connections.find { it.id == target.connectionId } ?: return@launch
        val draft = Draft(target, "Stop current turn", questionId = "__interrupt", questionHash = turn.toString()).begin()
        if (drafts.any { it.target == target && it.questionId == "__interrupt" && it.status != "editing" }) return@launch
        preparingTargets += target.key; save(draft)
        try { persistence!!.flush() }
        catch (_: Exception) { error = "Could not save the interruption request. It was not sent."; preparingTargets -= target.key; return@launch }
        preparingTargets -= target.key
        try {
            check(fresh(target) && activity?.optDouble("turn_started") == turn)
            val payload = target.json().put("request_id", draft.requestId).put("expected_turn_started", turn)
            settle(draft, api.await(connection, api.submit(connection, target, "interrupt", payload, draft.requestId, SendAttempt())))
        } catch (_: Exception) {
            editState { state -> state.copy(drafts = state.drafts.map { if (it.key == draft.key && it.requestId == draft.requestId) it.copy(status = "uncertain") else it }) }
            flushDrafts(); error = "Interruption is unconfirmed. Check the conversation before trying again."
        }
    }
    fun updateNotifications(value: Boolean) = viewModelScope.launch {
        try { withContext(Dispatchers.IO) { store.setNotificationEnabled(value) }; notifications = value }
        catch (_: Exception) { notifications = store.notificationEnabled(); error = "Notification choices could not be saved. Try again." }
    }
}
