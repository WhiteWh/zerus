package app.zerus.mobile

import kotlinx.coroutines.*
import okhttp3.*
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class SendReceiptRecoveryTest {
    private val original="22222222-2222-4222-8222-222222222222"
    private val read="11111111-1111-4111-8111-111111111111"
    private val machine="33333333-3333-4333-8333-333333333333"
    private val session="44444444-4444-4444-8444-444444444444"
    private val target=Target(machine,"codex/example/send","run","conversation","gateway")
    private fun intent(text:String="exact text",provider:String="codex",compaction:String?=null)=SendReceiptRecovery.freeze(target,original,text,provider,machine,session,"python_default",compaction)!!
    private fun message(i:SendIntent=intent())=OutgoingMessage(original,target,i.value()!!.getJSONObject("payload").getString("text"),emptyList(),1.0,"uncertain",sendIntent=i)
    private fun answer(id:String=read,i:SendIntent=intent()):JSONObject {
        val v=i.value()!!;val p=v.getJSONObject("payload")
        return JSONObject().put("schema",1).put("request_id",id).put("message_request_id",original)
            .put("computer_id",machine).put("machine_id",machine).put("session_id",session).put("session",target.session)
            .put("run_id",target.run).put("conversation_id",target.conversation).put("agent_id","").put("provider",v.get("provider"))
            .put("native_digest",SendReceiptRecovery.nativeDigest(v)).put("intent_sha256",i.intentSha256)
            .put("status","submitted").put("proof","original_receipt").put("submitted_at",2.0).put("queued",false)
            .also { if(v.string("provider")=="opencode") it.put("native_message_id","msg_exact") }
    }
    @Test fun sharedGoldensPreserveExactBytesAndBothNativeEncodings() {
        val bytes=javaClass.getResource("/send-receipt-intents.json")!!.readBytes()
        assertArrayEquals(java.io.File("../../../services/mobile/vectors/send_receipt/intents.json").readBytes(),bytes)
        val vectors=JSONObject(bytes.toString(Charsets.UTF_8)).getJSONArray("vectors")
        for(n in 0 until vectors.length()) {
            val v=vectors.getJSONObject(n);val i=v.getJSONObject("intent")
            assertEquals(v.string("canonical"),SendReceiptRecovery.encode(i))
            assertEquals(v.string("intent_sha256"),SendReceiptRecovery.hash(SendReceiptRecovery.encode(i)))
            assertEquals(v.string("terminal_python_default_sha256"),SendReceiptRecovery.nativeDigest(i))
            assertEquals(v.string("terminal_compact_sha256"),SendReceiptRecovery.nativeDigest(JSONObject(i.toString()).put("native_encoding","compact")))
            for(provider in listOf("dsh","opencode")) assertEquals(v.string("native_agent_sha256"),SendReceiptRecovery.nativeDigest(JSONObject(i.toString()).put("provider",provider)))
            assertEquals(v.getBoolean("admitted"),runCatching { SendReceiptRecovery.validateIntent(i) }.isSuccess)
        }
        assertNotEquals(intent("\u00e9").intentSha256,intent("e\u0301").intentSha256)
    }
    @Test fun enqueueCodecRestartKeepsExactIntentAndContinuationOptions() {
        val i=intent("  e\u0301\nexact trailing space  ",compaction=read)
        val draft=Draft(target,message(i).text)
        val written=MessageState(drafts=listOf(draft)).enqueue(draft,original,1.0,i)
        val restored=MessageCodec.state(MessageCodec.stateJson(written))
        assertEquals("",restored.drafts.single().text);assertEquals(i,restored.outgoing.single().sendIntent)
        assertEquals("uncertain",restored.outgoing.single().status)
        assertEquals(read,i.value()!!.getJSONObject("payload").string("expected_compaction_id"))
        assertEquals("SendIntent(retained)",i.toString())
        assertNotEquals(SendReceiptRecovery.nativeDigest(i.value()!!),SendReceiptRecovery.nativeDigest(intent(message(i).text).value()!!))
    }
    @Test fun everyBindingDigestStatusAndTypeMustMatch() {
        val i=intent();assertNotNull(SendReceiptRecovery.proof(read,i,answer()))
        for(key in listOf("request_id","message_request_id","computer_id","machine_id","session_id","session","run_id","conversation_id","provider","agent_id","intent_sha256","native_digest","status","proof","submitted_at","queued","schema")) {
            val bad=answer().put(key,"foreign")
            assertNull(key,SendReceiptRecovery.proof(read,i,bad))
            val missing=answer();missing.remove(key);assertNull(key,SendReceiptRecovery.proof(read,i,missing))
        }
        for(status in listOf("pending","failed","unknown","uncertain","queued")) assertNull(SendReceiptRecovery.proof(read,i,answer().put("status",status)))
        assertNull(SendReceiptRecovery.proof(read,i,answer().put("submitted_at",-1)))
    }
    @Test fun invalidNativeIdsNeverBecomeEchoAuthority() {
        val i=intent(provider="opencode")
        for(id in listOf("", "msg_", "/private/fixture/session.json", "wrong", "msg_bad/name", "msg_bad\nname", "msg_"+"a".repeat(125)))
            assertNull(SendReceiptRecovery.proof(read,i,answer(i=i).put("native_message_id",id)))
        val missing=answer(i=i);missing.remove("native_message_id");assertNull(SendReceiptRecovery.proof(read,i,missing))
        assertNull(SendReceiptRecovery.proof(read,intent(),answer().put("native_message_id","msg_foreign")))
    }
    @Test fun exactLaneSettlementPreservesDismissalEditorAndOtherLanes() {
        val i=intent();val captured=message(i);val proof=SendReceiptRecovery.proof(read,i,answer())!!
        val other=captured.copy(requestId=read,target=target.copy(run="other"),sendIntent=null)
        val draft=Draft(target,"new composition")
        val before=MessageState(drafts=listOf(draft),outgoing=listOf(captured.copy(deliveryDismissed=true),other))
        val settled=SendReceiptRecovery.settle(before,captured,i,proof,100,99)
        assertEquals("submitted",settled.outgoing[0].status);assertTrue(settled.outgoing[0].deliveryDismissed)
        assertEquals(other,settled.outgoing[1]);assertEquals(listOf(draft),settled.drafts)
        assertEquals(settled,SendReceiptRecovery.settle(settled,captured,i,proof,200,101))
        assertEquals(before,SendReceiptRecovery.settle(before,captured,i,proof,100,100))
        assertEquals(before,SendReceiptRecovery.settle(before,captured,i,proof.copy(nativeDigest="0".repeat(64)),100,99))
        assertEquals(before,SendReceiptRecovery.settle(before,captured.copy(text="changed"),i,proof,100,99))
    }
    @Test fun confirmedEchoDeduplicatesButUncertaintyNeedsReceiptSettlement() {
        val i=intent();val captured=message(i).copy(submittedAt=2.0);val proof=SendReceiptRecovery.proof(read,i,answer())!!
        val native=Event("native","You",captured.text,"",2.0,source="codex_native")
        val ordinary=captured.copy(status="submitted")
        val normal=ConversationMerge.merge(target,listOf(native),listOf(ordinary))
        assertEquals(1,normal.events.size);assertEquals(setOf(original),normal.recordedRequestIds)
        for(row in listOf(native,native.copy(requestId=original),native.copy(source="opencode_native",nativeMessageId="msg_unproven"))) {
            val unknown=ConversationMerge.merge(target,listOf(row),listOf(captured))
            assertEquals(2,unknown.events.size);assertTrue(unknown.recordedRequestIds.isEmpty())
            assertEquals("uncertain",captured.status)
        }
        val otherConfirmed=ordinary.copy(requestId=read,createdAt=5.0,sendIntent=null)
        val concurrent=ConversationMerge.merge(target,listOf(native),listOf(captured,otherConfirmed))
        assertEquals(setOf(read),concurrent.recordedRequestIds)
        assertTrue(concurrent.events.any { it.outgoingId==original && it.source=="mobile_outbox" && it.delivery=="uncertain" })
        val settled=SendReceiptRecovery.settle(MessageState(outgoing=listOf(captured)),captured,i,proof,100,99).outgoing.single()
        val recovered=ConversationMerge.merge(target,listOf(native),listOf(settled))
        assertEquals(1,recovered.events.size);assertEquals(setOf(original),recovered.recordedRequestIds)
        val oc=intent(provider="opencode");val msg=message(oc);val p=SendReceiptRecovery.proof(read,oc,answer(i=oc))!!
        val sent=SendReceiptRecovery.settle(MessageState(outgoing=listOf(msg)),msg,oc,p,100,99).outgoing.single()
        val exact=native.copy(text="provider transformed text",at=1000.0,source="opencode_native",nativeMessageId="msg_exact")
        assertEquals(setOf(original),ConversationMerge.merge(target,listOf(exact),listOf(sent)).recordedRequestIds)
        for(row in listOf(exact.copy(nativeMessageId="msg_other"),exact.copy(source="codex_native"),exact.copy(inheritedFrom="foreign")))
            assertTrue(ConversationMerge.merge(target,listOf(row),listOf(sent)).recordedRequestIds.isEmpty())
    }
    @Test fun boundsOptionsAndCapabilityGateFailClosed() {
        assertNull(SendReceiptRecovery.freeze(target,original,"x".repeat(65537),"codex",machine,session,"compact"))
        assertNull(SendReceiptRecovery.freeze(target,original,"exact\r\ntext","codex",machine,session,"compact"))
        assertFalse(SendReceiptRecovery.supported(setOf("send_receipt"),setOf("send")))
        assertEquals("read",WorkspaceProtocol.operationClass("send_receipt"))
        assertEquals("mutation",WorkspaceProtocol.operationClass("send"))
        assertNull(SendReceiptRecovery.encoding(1,MachineRoute.Unknown))
        assertEquals("compact",SendReceiptRecovery.encoding(1,MachineRoute.Direct))
        assertEquals("python_default",SendReceiptRecovery.encoding(2,MachineRoute.Direct))
        assertEquals("compact",SendReceiptRecovery.encoding(2,MachineRoute.Via("gateway","Gateway")))
        assertNull(intent().copy(canonical=intent().canonical+" ").value())
    }
    @Test fun lostOriginalQueriesOnlyFreshReadIdentityAndReadFailureStaysUnknown()=runBlocking {
        val seen=mutableListOf<Pair<String,JSONObject?>>()
        var failRead=false
        val client=OkHttpClient.Builder().addInterceptor { chain ->
            val request=chain.request();val body=request.body?.let { val b=Buffer();it.writeTo(b);JSONObject(b.readUtf8()) }
            seen+=request.method to body
            val response=if(body==null) JSONObject().put("request_id",original).put("state","uncertain")
                else JSONObject().put("request_id",body.get("request_id")).put("state",if(failRead) "failed" else "completed").put("result",answer(body.string("request_id")))
            Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("OK").body(response.toString().toResponseBody()).build()
        }.build()
        try {
            val api=RelayApi(client);val c=Connection("gateway","Fixture","https://gateway.example.test","synthetic")
            val result=api.checkDelivery(c,message(),intent(),1000)
            assertTrue(result is SendReceiptCheck.Native)
            val readBody=seen[1].second!!;assertEquals("send_receipt",readBody.string("operation"));assertNotEquals(original,readBody.string("request_id"))
            assertEquals(original,readBody.getJSONObject("payload").getJSONObject("intent").getJSONObject("payload").string("request_id"))
            failRead=true;assertEquals(SendReceiptCheck.Unconfirmed,api.checkDelivery(c,message(),intent(),1000))
            assertTrue(seen.mapNotNull { it.second }.all { it.string("operation")=="send_receipt" })
            val count=seen.size;assertEquals(SendReceiptCheck.Unconfirmed,api.checkDelivery(c,message(),null,1000));assertEquals(count+1,seen.size)
        } finally { client.connectionPool.evictAll();client.dispatcher.executorService.shutdown() }
    }
    @Test fun legacyPlainReconstructionNeverGuessesLostOptionsAndQueueStaysExplicit() {
        val plain=intent();val old=message().copy(sendIntent=null)
        val proof=SendReceiptRecovery.proof(read,plain,answer().put("queued",true))!!
        val settled=SendReceiptRecovery.settle(MessageState(outgoing=listOf(old)),old,plain,proof,100,99)
        assertEquals("submitted",settled.outgoing.single().status)
        assertEquals(plain,settled.outgoing.single().sendIntent)
        assertEquals("queued",ConversationMerge.merge(target,emptyList(),settled.outgoing).events.single().delivery)
        val continuation=intent(compaction=read)
        val lost=answer(i=continuation).put("intent_sha256",plain.intentSha256)
        assertNull(SendReceiptRecovery.proof(read,plain,lost))
    }
    @Test fun ordinaryAcknowledgementUsesTheSameExactOriginalBindings() {
        val i=intent();val positive=answer(original).put("name",target.session).put("digest",SendReceiptRecovery.nativeDigest(i.value()!!))
            .put("delivery_binding",JSONObject().put("schema",1).put("machine_id",machine).put("session_id",session).put("provider","codex"))
        assertNotNull(SendReceiptRecovery.acknowledgement(i,positive))
        for(key in listOf("machine_id","session_id","provider")) {
            val bad=JSONObject(positive.toString());bad.getJSONObject("delivery_binding").put(key,"foreign")
            assertNull(SendReceiptRecovery.acknowledgement(i,bad))
        }
        assertNull(SendReceiptRecovery.acknowledgement(i,JSONObject(positive.toString()).put("digest","0".repeat(64))))
    }
    @Test fun campaignCancelsEitherHungReadWithoutStartingAnotherRead()=runBlocking {
        for(originalHangs in listOf(true,false)) {
            val server=okhttp3.mockwebserver.MockWebServer();server.start()
            val client=OkHttpClient.Builder().retryOnConnectionFailure(false).build()
            if(!originalHangs) server.enqueue(okhttp3.mockwebserver.MockResponse().setBody(JSONObject().put("request_id",original).put("state","uncertain").toString()))
            server.enqueue(okhttp3.mockwebserver.MockResponse().setSocketPolicy(okhttp3.mockwebserver.SocketPolicy.NO_RESPONSE))
            try {
                val c=Connection("gateway","Fixture",server.url("/").toString(),"synthetic")
                try { RelayApi(client).checkDelivery(c,message(),intent(),300);fail("Hung read escaped the one deadline") }
                catch(_:TimeoutCancellationException) {}
                withTimeout(2000) { while(client.dispatcher.runningCallsCount()!=0) delay(10) }
                delay(100)
                assertEquals(if(originalHangs) 1 else 2,server.requestCount)
            } finally { client.connectionPool.evictAll();client.dispatcher.executorService.shutdown();server.shutdown() }
        }
    }

    @Test fun nativeEchoIdentitySurvivesTheBoundedEncryptedHistoryCodec() {
        val raw=JSONObject().put("type","UserPromptSubmit").put("source","opencode_native").put("detail","exact text").put("message_id","msg_exact")
        val row=NativeParser.events(JSONObject().put("provider_messages",org.json.JSONArray().put(raw))).single()
        assertEquals("msg_exact",row.nativeMessageId)
        raw.remove("message_id");raw.put("id","msg_invented")
        assertEquals("",NativeParser.events(JSONObject().put("provider_messages",org.json.JSONArray().put(raw))).single().nativeMessageId)
        val restored=EventHistoryCodec.decode(EventHistoryCodec.encode(listOf(row))).single()
        assertEquals("msg_exact",restored.nativeMessageId)
        assertFalse(EventHistoryCodec.encode(listOf(row.copy(nativeMessageId=""))).toString().contains("native_message_id"))
    }

    @Test fun sharedParityRejectsAmbiguousMachinesAndNonBooleanQueued() {
        val vectors=JSONObject(javaClass.getResource("/send-receipt-phone-parity.json")!!.readText())
        val machines=vectors.getJSONArray("machines")
        for(n in 0 until machines.length()) {
            val row=machines.getJSONObject(n)
            val catalog=List(row.getInt("count")) { Machine("gateway",machine,"Fixture",true,machineId=machine) }
            assertEquals(row.getBoolean("selected"),SendReceiptRecovery.machine(catalog,target)!=null)
        }
        val i=intent();val positive=answer(original).put("name",target.session).put("digest",SendReceiptRecovery.nativeDigest(i.value()!!))
            .put("delivery_binding",JSONObject().put("schema",1).put("machine_id",machine).put("session_id",session).put("provider","codex"))
        val queued=vectors.getJSONArray("queued")
        for(n in 0 until queued.length()) {
            val row=queued.getJSONObject(n);val value=JSONObject(positive.toString());value.remove("queued")
            if(row.getBoolean("present")) value.put("queued",row.get("value"))
            assertEquals(row.toString(),row.getBoolean("accepted"),SendReceiptRecovery.acknowledgement(i,value)!=null)
        }
    }

    @Test fun compactIdentityNeverDowngradesToLegacyReceiptHandling()=runBlocking {
        val i=intent(compaction=read)
        val positive=answer(original,i).put("name",target.session).put("digest",SendReceiptRecovery.nativeDigest(i.value()!!))
            .put("delivery_binding",JSONObject().put("schema",1).put("machine_id",machine).put("session_id",session).put("provider","codex"))
        val seen=mutableListOf<String>()
        val client=OkHttpClient.Builder().addInterceptor { chain ->
            val request=chain.request();seen+=request.method
            val response=JSONObject().put("request_id",original).put("state","completed").put("result",positive)
            Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(200).message("OK").body(response.toString().toResponseBody()).build()
        }.build()
        try {
            val api=RelayApi(client);val c=Connection("gateway","Fixture","https://gateway.example.test","synthetic")
            val compact=OutboxRetention.compact(message(i).copy(status="submitted"))
            val confirmed=api.checkDelivery(c,compact,compact.originalSendIntent(),1000)
            assertTrue(confirmed is SendReceiptCheck.Native);assertEquals(i,(confirmed as SendReceiptCheck.Native).intent)
            val bad=compact.copy(sendCorrelation=SendCorrelation(compact.sendCorrelation!!.json().put("schema",2).toString()))
            assertEquals(SendReceiptCheck.Unconfirmed,api.checkDelivery(c,bad,null,1000))
            assertEquals(SendReceiptCheck.Unconfirmed,api.checkDelivery(c,bad,i,1000))
            assertTrue(api.checkDelivery(c,compact.copy(sendCorrelation=null),null,1000) is SendReceiptCheck.Original)
            assertEquals(listOf("GET","GET","GET","GET"),seen)
            val broken=bad.copy(status="uncertain");val state=MessageState(outgoing=listOf(broken))
            assertEquals(state,SendReceiptRecovery.settle(state,broken,i,SendReceiptRecovery.proof(read,i,answer(read,i))!!,100,99))
        } finally { client.connectionPool.evictAll();client.dispatcher.executorService.shutdown() }
    }

}
