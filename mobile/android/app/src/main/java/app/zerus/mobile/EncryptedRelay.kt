package app.zerus.mobile

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

private object WorkspaceFlights { val locks=ConcurrentHashMap<String,Mutex>() }
private fun RelayApi.journal()=checkNotNull(workspace) { "Encrypted workspace storage is unavailable. Plaintext fallback is disabled." }
private fun nowSeconds()=System.currentTimeMillis()/1000
private data class WorkspaceCatalogCandidate(val computer:String,val row:JSONObject,val route:WorkspaceRoute,val expiresAt:Long)

/** Pure preparation plus durable reservation; caller saves this connection before enrollPrepared. */
internal suspend fun RelayApi.prepareEncryptedPair(url:String,code:String):Connection=withContext(Dispatchers.IO) {
    val normalized=EndpointPolicy.normalize(url);val invitation=WorkspaceInvitation.parse(code.trim())
    val identity=WorkspaceIdentity.generate(UUID.randomUUID().toString())
    val candidate=invitation.candidate(identity,nowSeconds())
    val token=ByteArray(32).also { SecureRandom().nextBytes(it) }
    val connection=Connection(UUID.randomUUID().toString(),"Workspace",normalized,WorkspaceProtocol.encode(token),2)
    journal().create(connection,WorkspaceState(identity,candidate,invitation.issuer,invitation.membershipRevision,invitation.membershipHead,null))
    connection
}

/** Called only once, after the prepared connection and its keys have become durable. */
internal suspend fun RelayApi.enrollPrepared(connection:Connection) {
    val state=withContext(Dispatchers.IO) { journal().load(connection) }
    require(state.membership==null && state.enrollment==null)
    call(connection.url,"","/v2/enrollments",JSONObject().put("candidate",state.candidate.json()).put("service_token",connection.token))
}

/** Receipt-only recovery, including after process death. Never issues an enrollment POST. */
internal suspend fun RelayApi.synchronizeWorkspace(connection:Connection):WorkspaceState = RelayReadTiming.measure(RelayReadTiming.Stage.MembershipSync) {
    val waiting=RelayReadTiming.begin()
    WorkspaceFlights.locks.getOrPut(connection.id) { Mutex() }.withLock {
        RelayReadTiming.finish(RelayReadTiming.Stage.MembershipLock,waiting)
        withContext(Dispatchers.IO) {
            val journal=journal();var state=journal.load(connection)
            if(state.enrollment==null) {
                val status=call(connection.url,connection.token,"/v2/enrollments/${state.identity.device.deviceId}")
                require(status.string("workspace_id")==state.candidate.workspaceId)
                if(status.string("state")!="active") return@withContext state
                val approval=WorkspaceApproval.parse(status.getJSONObject("enrollment"));approval.verify(state.candidate,state.issuer)
                var next=state.withEnrollment(approval)
                if(state.membership==null) {
                    val bootstrap=WorkspaceBootstrap.parse(status.getJSONObject("bootstrap"))
                    next=next.withMembership(bootstrap.verify(state.candidate.workspaceId,state.pinnedRevision,state.pinnedHead,state.issuer))
                }
                journal.update(connection,state,next);state=next
            }
            var pages=0
            while(true) {
                check(++pages<=83) { "Workspace history exceeded its bounded membership limit." }
                val membership=requireNotNull(state.membership)
                val page=call(connection.url,connection.token,"/v2/membership?after=${membership.revision}&limit=100")
                val raw=page.getJSONArray("changes");require(raw.length()<=100)
                val changes=raw.objects().map(WorkspaceMembershipChange::parse);require(changes.size==raw.length())
                var nextMembership=membership
                changes.forEach { nextMembership=nextMembership.apply(it) }
                val latest=WorkspaceProtocol.number(page,"membership_revision");val latestHead=WorkspaceProtocol.string(page,"membership_head")
                require(latest>=nextMembership.revision && (latest!=nextMembership.revision || latestHead==nextMembership.head)) { "Workspace membership was rolled back or forked." }
                if(changes.isNotEmpty()) {
                    val next=state.withMembership(nextMembership);journal.update(connection,state,next,changes);state=next
                }
                if(page.isNull("next_after")) { require(nextMembership.revision==latest);break }
                require(changes.isNotEmpty() && WorkspaceProtocol.number(page,"next_after")==nextMembership.revision)
            }
            val membership=requireNotNull(state.membership)
            state.requireEnrollmentHistory(journal,connection)
            check(state.isActiveMember(membership)) { "This phone has been removed from the workspace." }
            state
        }
    }
}

private fun WorkspaceState.requireEnrollmentHistory(journal:WorkspaceJournal,connection:Connection) {
    val approval=requireNotNull(enrollment)
    require(journal.head(connection,approval.membership.revision)==approval.membership.head()) { "Enrollment is not on this workspace's verified history." }
}
private fun WorkspaceState.isActiveMember(current:WorkspaceMembership)=
    enrollment!=null && current.members[identity.device.deviceId]==identity.device

private fun WorkspaceState.requireActive():WorkspaceMembership {
    val current=checkNotNull(membership) { "Waiting for the inviting computer to approve this phone. Refresh to check again." }
    check(isActiveMember(current)) { "This encrypted phone enrollment is not active." }
    return current
}

internal enum class WorkspacePushReadiness { Pending, Active, Removed }

/** Local verified authority only. A pending credential must never POST /push.
 * The sync lock excludes the intermediate approval+bootstrap save before its
 * signed Add reaches the history index. No enrollment POST or recovery occurs. */
internal suspend fun RelayApi.workspacePushReadiness(connection:Connection):WorkspacePushReadiness =
    WorkspaceFlights.locks.getOrPut(connection.id) { Mutex() }.withLock {
        withContext(Dispatchers.IO) {
            val journal=journal();val state=journal.load(connection)
            val approval=state.enrollment;val membership=state.membership
            if(approval==null || membership==null || membership.revision<approval.membership.revision)
                return@withContext WorkspacePushReadiness.Pending
            state.requireEnrollmentHistory(journal,connection)
            if(state.isActiveMember(membership)) WorkspacePushReadiness.Active else WorkspacePushReadiness.Removed
        }
    }

internal suspend fun RelayApi.encryptedComputers(connection:Connection):JSONObject=withContext(Dispatchers.Default) {
    val state=synchronizeWorkspace(connection);val membership=state.requireActive();val journal=journal()
    val candidates=mutableListOf<WorkspaceCatalogCandidate>();val unavailable=mutableSetOf<String>();val seen=mutableSetOf<String>();var after="";var pages=0;var privateBytes=0L
    while(true) {
        check(++pages<=41) { "The workspace computer catalog exceeds its safety limit." }
        val page=call(connection.url,connection.token,"/v2/computers?limit=100"+(if(after.isEmpty()) "" else "&after="+java.net.URLEncoder.encode(after,"UTF-8")))
        val raw=page.getJSONArray("computers");require(raw.length()<=100)
        for(index in 0 until raw.length()) {
            currentCoroutineContext().ensureActive()
            val row=raw.getJSONObject(index)
            if(row.isNull("envelope")) continue
            val envelope=WorkspaceEnvelope.parse(row.getJSONObject("envelope"));val h=envelope.header
            val computer=WorkspaceProtocol.string(row,"computer_id");val gateway=WorkspaceProtocol.string(row,"gateway_id")
            require(h.kind=="snapshot" && h.operationClass=="read" && h.workspaceId==membership.workspaceId && h.gatewayId==gateway && h.senderDeviceId==gateway && h.routeId==row.string("route_id") &&
                h.recipientDeviceId==state.identity.device.deviceId) { "Computer snapshot has a different route or identity." }
            val bytes=withContext(Dispatchers.IO) {
                val sender=membership.senderAtKnownHead(envelope,journal.head(connection,h.membershipRevision));require(sender.role=="computer")
                journal.open(connection,envelope,h)
            };require(bytes.size<=1024*1024)
            privateBytes+=bytes.size;check(privateBytes<=16L*1024*1024) { "The encrypted computer catalog exceeds this phone's safety limit." }
            val privateRow=JSONObject(String(bytes,Charsets.UTF_8));val binding=privateRow.getJSONObject("encrypted_transport")
            val nativeId=WorkspaceProtocol.string(privateRow,"machine_id")
            require(privateRow.string("id")==computer && nativeId==computer && binding.string("gateway_id")==gateway && binding.string("route_id")==h.routeId && binding.string("machine_id")==nativeId)
            val route=WorkspaceRoute.parse(JSONObject().put("computer_id",computer).put("gateway_id",gateway).put("route_id",h.routeId).put("machine_id",nativeId))
            seen+=nativeId;check(seen.size<=4096) { "The workspace computer catalog exceeds its safety limit." }
            if(h.expiresAt<=catalogNowSeconds()) {
                // Only the authenticated native binding identifies a stale cache row.
                // Its expired private content never replaces the caller's prior cache.
                unavailable+=nativeId
                check(unavailable.size<=4096) { "The workspace computer catalog exceeds its safety limit." }
                continue
            }
            privateRow.put("online",row.optBoolean("online",false))
            candidates+=WorkspaceCatalogCandidate(computer,privateRow,route,h.expiresAt)
            check(candidates.size<=4100) { "The workspace computer catalog exceeds its safety limit." }
        }
        if(page.isNull("next_after")) break
        val next=WorkspaceProtocol.string(page,"next_after");require(next.length in 1..2048 && next!=after);after=next
    }
    currentCoroutineContext().ensureActive()
    WorkspaceFlights.locks.getOrPut(connection.id) { Mutex() }.withLock { withContext(Dispatchers.IO) {
        val latest=journal.load(connection);require(latest.membership==membership) { "Workspace membership changed while loading computers. Refresh again." }
        currentCoroutineContext().ensureActive()
        // Admit once, after page/lock/journal waits. Keep alternatives until this instant.
        val admissionTime=catalogNowSeconds()
        val rows=linkedMapOf<String,JSONObject>();val routes=linkedMapOf<String,WorkspaceRoute>()
        for(candidate in candidates) {
            if(candidate.expiresAt<=admissionTime) { unavailable+=candidate.computer;continue }
            val computer=candidate.computer;val online=candidate.row.optBoolean("online")
            val selected=routes[computer];val selectedOnline=rows[computer]?.optBoolean("online")==true
            if(selected==null || online && !selectedOnline || online==selectedOnline && candidate.route==state.routes[computer]) {
                routes[computer]=candidate.route;rows[computer]=candidate.row
            }
        }
        journal.update(connection,latest,latest.withRoutes(routes))
        JSONObject().put("computers",JSONArray(rows.values.toList())).put("unavailable_computers",JSONArray((unavailable-rows.keys).sorted()))
    } }
}

/** Reuse prior display data only; expired reports never restore action capabilities. */
internal object WorkspaceCatalogCache {
    fun preserve(connection:Connection,response:JSONObject,fresh:SessionCatalog,sessions:List<Session>,machines:List<Machine>,projects:List<ProjectSummary>,accounts:List<AccountCatalog>):SessionCatalog {
        if(connection.protocolVersion!=2) return fresh
        val raw=response.getJSONArray("unavailable_computers");require(raw.length()<=4096)
        val unavailable=(0 until raw.length()).map { raw.getString(it).also(WorkspaceProtocol::identifier) }.toSet()-fresh.machines.map { it.id }.toSet()
        fun stale(id:String)=id in unavailable
        val oldSessions=sessions.filter { it.target.connectionId==connection.id && stale(it.target.computerId) }
            .map { it.copy(raw=JSONObject(it.raw.toString()).put("last_known",true)) }
        val oldMachines=machines.filter { it.connectionId==connection.id && stale(it.id) }.map { it.copy(online=false,lastKnown=true) }
        val oldAccounts=accounts.filter { it.key.connectionId==connection.id && stale(it.key.computerId) }.map { it.copy(available=false,online=false,stale=true) }
        val merged=fresh.projects.projects.associateBy { it.key }.toMutableMap()
        for(project in projects.filter { it.key.connectionId==connection.id }) {
            val folders=project.folders.filter { stale(it.computerId) };val computers=project.computers.filterKeys(::stale)
            if(folders.isEmpty() && computers.isEmpty()) continue
            val current=merged[project.key]
            merged[project.key]=if(current==null) project.copy(folders=folders,computers=computers,stale=true)
                else current.copy(folders=(current.folders+folders).distinctBy { it.computerId to it.id.ifBlank { it.path } },computers=computers+current.computers,stale=true)
        }
        return fresh.copy(machines=fresh.machines+oldMachines,accounts=fresh.accounts+oldAccounts,
            projects=fresh.projects.copy(sessions=fresh.projects.sessions+oldSessions,projects=merged.values.toList()))
    }
}

internal fun WorkspaceMembership.senderAtKnownHead(envelope:WorkspaceEnvelope,knownHead:String?):WorkspaceDevice {
    val h=envelope.header
    require(h.workspaceId==workspaceId && h.membershipRevision<=revision && h.membershipHead==knownHead && (h.membershipRevision!=revision || h.membershipHead==head)) { "Workspace payload belongs to an unknown membership history." }
    val sender=members[h.senderDeviceId] ?: error("Unrecognized encrypted sender.")
    val recipient=members[h.recipientDeviceId] ?: error("Unrecognized encrypted recipient.")
    require(sender.keyGeneration==h.senderKeyGeneration && recipient.keyGeneration==h.recipientKeyGeneration)
    return sender
}

/** Service scope IDs are refresh hints; only decrypted snapshots map them to native sessions. */
internal suspend fun RelayApi.encryptedEvents(connection:Connection,after:Long,wait:Int):JSONObject {
    require(after>=0 && wait in 0..25)
    val page=call(connection.url,connection.token,"/v2/events?after=$after&wait=$wait")
    val raw=page.getJSONArray("events");require(raw.length()<=100)
    val records=if(raw.length()>0) encryptedComputers(connection).getJSONArray("computers").objects() else emptyList()
    return WorkspaceNotifications.events(connection,records,page,after,System.currentTimeMillis()/1000.0)
}

internal suspend fun RelayApi.encryptedPost(connection:Connection,body:JSONObject,attempt:SendAttempt):JSONObject=withContext(Dispatchers.IO) {
    val state=synchronizeWorkspace(connection);val membership=state.requireActive();val journal=journal()
    val id=WorkspaceProtocol.string(body,"request_id");WorkspaceProtocol.identifier(id)
    val computer=WorkspaceProtocol.string(body,"computer_id");val previous=journal.recordedRequest(connection,id)
    val route=if(previous!=null) WorkspaceRoute(computer,previous.gatewayId,previous.routeId,computer)
        else state.routes[computer] ?: error("Refresh this computer's encrypted route before sending.")
    val recipient=membership.members[route.gatewayId] ?: error("The executing gateway is no longer enrolled.")
    require(recipient.role=="computer")
    val boundBody=JSONObject(body.toString()).put("encrypted_transport",JSONObject().put("gateway_id",route.gatewayId).put("route_id",route.routeId).put("machine_id",route.machineId))
    val plaintext=boundBody.toString().toByteArray();require(plaintext.size<=WorkspaceProtocol.MAX_PAYLOAD)
    // Retain every identity at the endpoint; read hints never authorize native replay.
    val header=WorkspaceHeader(membership.workspaceId,state.identity.device.deviceId,recipient.deviceId,state.identity.device.keyGeneration,
        recipient.keyGeneration,route.gatewayId,route.routeId,id,"request",WorkspaceProtocol.operationClass(body.string("operation")),nowSeconds()+300,membership.revision,membership.head)
    val envelope=RelayReadTiming.measure(RelayReadTiming.Stage.Seal) { sealEnvelope(state.identity,recipient,header,plaintext) }
    val hash=WorkspaceProtocol.encode(MessageDigest.getInstance("SHA-256").digest(plaintext))
    val retained=RelayReadTiming.measure(RelayReadTiming.Stage.Reserve) { journal.reserve(connection,id,hash,envelope) }
    val result=call(connection.url,connection.token,"/v2/requests",JSONObject().put("envelope",retained.json()),attempt=attempt)
    decodeEncryptedReceipt(connection,state,result,id,retained.header)
}

/** A phone's signed latch statement for the gateway that routes [computer] (zerus-sa9qn.50): built and signed once, when the
 * person's action is sent; that gateway's own service verifies it. The payload of session_question_route_person. */
internal suspend fun RelayApi.signRoutePerson(connection:Connection,computer:String,sessionId:String,run:String,conversation:String,
                                              personOff:Boolean,requestId:String):JSONObject=withContext(Dispatchers.IO) {
    val state=synchronizeWorkspace(connection);val membership=state.requireActive()
    val route=state.routes[computer] ?: error("Refresh this computer's encrypted route before sending.")
    require(membership.members[route.gatewayId]?.role=="computer") { "The executing gateway is no longer enrolled." }
    val now=nowSeconds()
    val statement=RoutePersonStatement(membership.workspaceId,state.identity.device.deviceId,route.gatewayId,sessionId,run,conversation,
        personOff,requestId,now,now+RoutePersonStatement.LIFETIME)
    JSONObject().put("statement",statement.json()).put("signature",WorkspaceProtocol.encode(WorkspaceCrypto.sign(state.identity.keys,statement.frame())))
}

/** Establish local retained authority before registering any readiness watch.
 * This does not replace the NEW membership check in encryptedReceipt(). */
internal suspend fun RelayApi.prepareReceiptWatch(connection:Connection,id:String)=withContext(Dispatchers.IO) {
    WorkspaceProtocol.identifier(id)
    val journal=journal();val previous=journal.load(connection)
    previous.requireActive();previous.requireEnrollmentHistory(journal,connection)
    journal.request(connection,id)
    Unit
}

internal suspend fun RelayApi.encryptedReceipt(connection:Connection,id:String):JSONObject=withContext(Dispatchers.IO) {
    WorkspaceProtocol.identifier(id)
    val journal=journal();val previous=journal.load(connection)
    previous.requireActive();previous.requireEnrollmentHistory(journal,connection)
    val expected=journal.request(connection,id)
    coroutineScope {
        // Fetch only retained ciphertext in parallel with a NEW membership check.
        // No completion, decryption or settlement is admitted before that check.
        val outer=async { call(connection.url,connection.token,"/v2/requests/$id") }
        val state=synchronizeWorkspace(connection);state.requireActive()
        decodeEncryptedReceipt(connection,state,outer.await(),id,expected)
    }
}

private fun RelayApi.decodeEncryptedReceipt(connection:Connection,state:WorkspaceState,outer:JSONObject,id:String,request:WorkspaceHeader):JSONObject = RelayReadTiming.measure(RelayReadTiming.Stage.OpenResult) {
    require(outer.string("request_id")==id)
    if(outer.isNull("envelope")) {
        val status=outer.string("state")
        // Only a verified endpoint receipt can confirm completion or native failure.
        return@measure JSONObject().put("request_id",id).put("state",if(status in setOf("queued","claimed")) status else "uncertain")
    }
    val envelope=WorkspaceEnvelope.parse(outer.getJSONObject("envelope"));val h=envelope.header
    require(h.workspaceId==request.workspaceId && h.requestId==id && h.senderDeviceId==request.recipientDeviceId &&
        h.recipientDeviceId==request.senderDeviceId && h.gatewayId==request.gatewayId && h.routeId==request.routeId && h.kind=="result" &&
        h.operationClass==request.operationClass && h.expiresAt>nowSeconds()) { "Encrypted result belongs to a different request or route." }
    val sender=state.requireActive().senderAtKnownHead(envelope,journal().head(connection,h.membershipRevision));require(sender.role=="computer")
    val bytes=journal().open(connection,envelope,h);require(bytes.size<=1024*1024)
    val result=JSONObject(String(bytes,Charsets.UTF_8));require(result.string("request_id")==id && result.string("state") in setOf("completed","failed","uncertain"))
    journal().settle(connection,id)
    result
}
