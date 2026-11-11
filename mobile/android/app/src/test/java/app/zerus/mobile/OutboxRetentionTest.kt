package app.zerus.mobile

import org.json.JSONObject
import kotlinx.coroutines.*
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.*
import org.junit.Test
import java.util.UUID

class OutboxRetentionTest {
    private val machine="33333333-3333-4333-8333-333333333333"
    private val session="44444444-4444-4444-8444-444444444444"
    private val target=Target(machine,"opencode/example/outbox","run","conversation","gateway")
    private fun row(text:String="original", provider:String="opencode", status:String="submitted", compaction:String?=null):OutgoingMessage {
        val id=UUID.randomUUID().toString()
        val intent=SendReceiptRecovery.freeze(target,id,text,provider,machine,session,"python_default",compaction)!!
        return OutgoingMessage(id,target,text,emptyList(),1.0,status,2.0,text,sendIntent=intent,
            sendReceiptProof=SendReceiptProof(id,intent.intentSha256,SendReceiptRecovery.nativeDigest(intent.value()!!),2.0,false,
                if(provider=="opencode") "msg_"+id.replace("-","") else ""))
    }
    private fun native(row:OutgoingMessage)=Event("native:"+row.requestId,"You",row.text,"",2.0,
        source="opencode_native",nativeMessageId=row.sendReceiptProof!!.nativeMessageId)
    private fun represented(text:String="original"):OutgoingMessage {
        val row=row(text)
        return row.copy(status="recorded",coverage=OutgoingCoverage.native(row,native(row))!!)
    }
    @Test fun sharedCorrelationGoldensMatchBothNativeEncodings() {
        val bytes=javaClass.getResource("/send-correlation.json")!!.readBytes()
        assertArrayEquals(java.io.File("../../../services/mobile/vectors/send_receipt/compaction.json").readBytes(),bytes)
        val fixture=JSONObject(bytes.toString(Charsets.UTF_8))
        assertEquals(OutboxRetention.MAX_ROWS,fixture.getInt("max_settled_rows"));assertEquals(OutboxRetention.MAX_BYTES,fixture.getInt("max_settled_bytes"))
        val vectors=fixture.getJSONArray("vectors")
        for(n in 0 until vectors.length()) {
            val v=vectors.getJSONObject(n);val i=v.getJSONObject("intent");val p=i.getJSONObject("payload");val expected=v.getJSONObject("correlation")
            val original=OutgoingMessage(p.getString("request_id"),MessageCodec.target(v.getJSONObject("target")),p.getString("text"),emptyList(),1.0,"submitted",
                sendIntent=SendIntent(v.getString("canonical"),expected.getString("intent_sha256")))
            val compact=OutboxRetention.compact(original)
            assertEquals(v.string("id"),SendReceiptRecovery.encode(expected),SendReceiptRecovery.encode(compact.sendCorrelation!!.json()))
            assertEquals(original.sendIntent,compact.originalSendIntent())
        }
    }
    @Test fun compactionKeepsExactIdentityProviderAndReadBytes() {
        for(provider in listOf("claude","codex","kimi","dsh","opencode")) for(option in listOf(null,"","22222222-2222-4222-8222-222222222222")) {
            val original=row("  e\u0301 日本語 😀\nexact trailing space  ",provider,compaction=option)
            val compact=OutboxRetention.compact(original)
            assertNull(compact.sendIntent);assertNotNull(compact.sendCorrelation)
            assertEquals(original.sendIntent,compact.originalSendIntent());assertTrue(compact.hasOriginalSendIdentity)
            assertEquals(SendReceiptRecovery.exactNativeId(original),SendReceiptRecovery.exactNativeId(compact))
            assertEquals(original.sendReceiptProof,compact.sendReceiptProof)
            val restored=MessageCodec.outgoing(MessageCodec.outgoingJson(compact))
            assertEquals(compact,restored);assertEquals(original.sendIntent,restored.originalSendIntent())
            assertFalse(compact.sendCorrelation!!.encoded.contains("trailing space"))
        }
    }
    @Test fun everyCompactBindingFailsClosedAndUnknownVersionRoundTrips() {
        val original=row();val compact=OutboxRetention.compact(original);val metadata=compact.sendCorrelation!!.json()
        for(key in listOf("schema","header","intent_sha256","native_digest","row_sha256")) {
            val changed=JSONObject(metadata.toString()).put(key,"foreign")
            val bad=compact.copy(sendCorrelation=SendCorrelation(changed.toString()))
            assertNull(key,bad.originalSendIntent());assertEquals("",SendReceiptRecovery.exactNativeId(bad));assertTrue(bad.hasOriginalSendIdentity)
        }
        for(bad in listOf(compact.copy(text="changed"),compact.copy(requestId=UUID.randomUUID().toString()),
            compact.copy(target=target.copy(connectionId="other")),compact.copy(target=target.copy(run="other")))) {
            assertNull(bad.originalSendIntent());assertEquals("",SendReceiptRecovery.exactNativeId(bad))
        }
        val unknown=compact.copy(sendCorrelation=SendCorrelation(JSONObject(metadata.toString()).put("schema",2).toString()))
        assertEquals(unknown,MessageCodec.outgoing(MessageCodec.outgoingJson(unknown)))
        assertFalse(OutboxRetention.evictable(unknown.copy(status="recorded",coverage=OutgoingCoverage("native",OutboxRetention.binding(unknown),"event"))))
        val both=unknown.copy(sendIntent=original.sendIntent)
        assertEquals(both,OutboxRetention.compact(both))
        assertFalse(OutboxRetention.evictable(both.copy(status="recorded",coverage=OutgoingCoverage("native",OutboxRetention.binding(both),"event"))))
        assertNull(OutboxRetention.compact(original.copy(sendReceiptProof=original.sendReceiptProof!!.copy(nativeDigest="0".repeat(64)))).sendCorrelation)
        assertTrue(runCatching { MessageCodec.outgoing(MessageCodec.outgoingJson(compact).put("send_correlation",42)) }.isFailure)
    }
    @Test fun unconfirmedAndFailedCompositionsStayRecoverable() {
        val sending=row(status="sending").copy(sendReceiptProof=null)
        val uncertain=sending.copy(status="uncertain",deliveryDismissed=true)
        val failed=row(status="failed").copy(sendReceiptProof=null)
        val protected=listOf(sending,uncertain,failed,row(),represented().copy(coverage=null),failed.copy(status="reviewed"))
        val normalized=OutboxRetention.normalize(MessageState(outgoing=protected+List(300){represented()}))
        assertEquals(100+protected.size,normalized.outgoing.size)
        assertEquals(sending.sendIntent,normalized.outgoing[0].sendIntent);assertEquals(uncertain.sendIntent,normalized.outgoing[1].sendIntent)
        assertEquals(failed.sendIntent,normalized.outgoing[2].originalSendIntent())
        assertEquals(protected.map { it.text },normalized.outgoing.take(protected.size).map { it.text });assertTrue(normalized.outgoing[1].deliveryDismissed)
    }
    @Test fun exactRepresentationDoesNotComeFromTextTimeOrTruncation() {
        val row=row();val echo=native(row);assertNotNull(OutgoingCoverage.native(row,echo))
        for(event in listOf(echo.copy(nativeMessageId="msg_other"),echo.copy(source="codex_native"),echo.copy(inheritedFrom="parent"),
            echo.copy(text="provider transformed"),echo.copy(detailTruncated=true))) assertNull(OutgoingCoverage.native(row,event))
        val merged=ConversationMerge.merge(target,listOf(echo.copy(source="codex_native",nativeMessageId="")),listOf(row))
        assertEquals(setOf(row.requestId),merged.recordedRequestIds);assertTrue(merged.exactRepresentations.isEmpty())
        val exact=ConversationMerge.merge(target,listOf(echo),listOf(OutboxRetention.compact(row)))
        assertEquals(setOf(row.requestId),exact.exactRepresentations.keys)
        val transformed=ConversationMerge.merge(target,listOf(echo.copy(text="provider transformed",at=1000.0)),listOf(OutboxRetention.compact(row)))
        assertEquals(setOf(row.requestId),transformed.recordedRequestIds);assertTrue(transformed.exactRepresentations.isEmpty())
    }
    @Test fun globalNewestRowAndByteLimitsAndPins() {
        val rows=List(10001){represented()};val pins=setOf(rows.first().requestId)
        val reduced=OutboxRetention.normalize(MessageState(outgoing=rows),pins)
        assertEquals(listOf(rows.first().requestId)+rows.takeLast(100).map { it.requestId },reduced.outgoing.map { it.requestId })
        assertEquals(100,OutboxRetention.normalize(reduced).outgoing.size)
        val large=List(35){represented("Я".repeat(32_768))};val bounded=OutboxRetention.normalize(MessageState(outgoing=large))
        assertTrue(bounded.outgoing.isNotEmpty());assertTrue(bounded.outgoing.size<35)
        assertTrue(bounded.outgoing.sumOf { MessageCodec.outgoingJson(it).toString().toByteArray(Charsets.UTF_8).size }<=OutboxRetention.MAX_BYTES)
        assertEquals(large.last().requestId,bounded.outgoing.last().requestId)
    }
    @Test fun attachmentAndDraftTransferCoverageProtectsLastCopy() {
        val file=Attachment(UUID.randomUUID().toString(),"file.txt","text/plain",4,"a".repeat(64))
        val failed=OutgoingMessage(UUID.randomUUID().toString(),target,"original",listOf(file),1.0,"failed")
        val dismissed=MessageChanges.dismissOutgoing(MessageState(outgoing=listOf(failed)),failed)
        assertEquals(listOf(file),OutboxRetention.normalize(dismissed).outgoing.single().attachments)
        val draft=Draft(target,failed.text,attachments=failed.attachments)
        val transferred=failed.copy(status="reviewed",coverage=OutgoingCoverage.draft(failed,draft))
        val bounded=OutboxRetention.normalize(MessageState(drafts=listOf(draft),outgoing=listOf(transferred)+List(101){represented()}))
        assertEquals(listOf(draft),bounded.drafts);assertFalse(bounded.outgoing.any { it.requestId==failed.requestId })
        assertNull(OutgoingCoverage.draft(failed,draft.copy(attachments=emptyList())))
        val event=Event("receipt","You",failed.text,"",requestId=failed.requestId,source="hgs_delivery",attachments=listOf(file.displayed()))
        assertNull(OutgoingCoverage.native(failed,event))
        val complete=event.copy(attachments=listOf(file.displayed().copy(messageId=failed.requestId,index=0)))
        assertNotNull(OutgoingCoverage.native(failed,complete))
        assertNull(OutgoingCoverage.native(failed,complete.copy(attachments=listOf(complete.attachments[0].copy(sha256="b".repeat(64))))))
    }
    @Test fun legacyMigrationIsIdempotentAndLateResponseCannotRecreateAnEvictedRow() {
        val rows=List(110){represented()};val decoded=MessageCodec.state(MessageCodec.stateJson(MessageState(outgoing=rows)))
        val compact=OutboxRetention.normalize(decoded)
        assertEquals(compact,OutboxRetention.normalize(compact));assertEquals(100,compact.outgoing.size)
        assertEquals(compact,MessageChanges.result(compact,rows.first().requestId){it.copy(status="submitted")})
        assertEquals(compact,SendReceiptRecovery.settle(compact,rows.first().copy(status="uncertain"),rows.first().sendIntent!!,rows.first().sendReceiptProof!!,100,99))
    }
    @Test fun failedMigrationAndDraftTransferKeepAuthoritativeDataUntilSuccessfulCommit() = runBlocking {
        val scope=CoroutineScope(SupervisorJob()+Dispatchers.Default)
        try {
            val failed=row(status="failed").copy(sendReceiptProof=null)
            val draft=Draft(target,failed.text,attachments=failed.attachments)
            val before=MessageState(outgoing=listOf(failed)+List(110){represented()})
            val saved=MessageCodec.stateJson(before).toString();val disk=AtomicReference(saved)
            val failing=java.util.concurrent.atomic.AtomicBoolean(true)
            val actor=OrderedStatePersistence(before,scope,{ state ->
                val encoded=MessageCodec.stateJson(state).toString()
                if(failing.get()) throw java.io.IOException("synthetic storage failure")
                disk.set(encoded)
            },Dispatchers.Unconfined,normalize={OutboxRetention.normalize(it)})
            val transfer:(MessageState)->MessageState={state -> state.copy(drafts=state.drafts+draft,outgoing=state.outgoing.map {
                if(it.requestId!=failed.requestId) it else it.copy(status="reviewed",coverage=OutgoingCoverage.draft(it,draft))
            })}
            assertTrue(runCatching { actor.durable(transfer) }.isFailure)
            assertEquals(before,actor.state.value);assertEquals(before,actor.durableState);assertEquals(saved,disk.get())
            failing.set(false)
            val committed=actor.durable(transfer)
            assertEquals(100,committed.outgoing.size);assertEquals(listOf(draft),committed.drafts)
            assertEquals(committed,actor.state.value);assertEquals(committed,MessageCodec.state(JSONObject(disk.get())))
            actor.edit { MessageChanges.result(it,failed.requestId){row -> row.copy(status="submitted")} }
            assertEquals(committed,actor.flush());assertEquals(committed,actor.state.value)
        } finally { scope.cancel() }
    }
    @Test fun debouncedWriteReleasesPinsAndPublishesTheSameBoundedState() = runBlocking {
        val scope=CoroutineScope(SupervisorJob()+Dispatchers.Default)
        try {
            val rows=List(105){represented()};val pins=AtomicReference(rows.map { it.requestId }.toSet())
            val initial=MessageState(outgoing=rows);val saved=AtomicReference(initial)
            val actor=OrderedStatePersistence(initial,scope,{saved.set(it)},Dispatchers.Unconfined,debounceMillis=200,
                normalize={OutboxRetention.normalize(it,pins.get())})
            actor.edit { it };assertEquals(105,actor.state.value.outgoing.size)
            // Wait until the queued edit has reached the writer, then release live references.
            delay(50);pins.set(emptySet())
            withTimeout(5000) { while(saved.get().outgoing.size!=100 || actor.state.value.outgoing.size!=100) delay(10) }
            assertEquals(saved.get(),actor.state.value);assertEquals(saved.get(),actor.durableState)
        } finally { scope.cancel() }
    }

    @Test fun checkingACompactUncertainRowDoesNotReintroduceCanonicalPayloadCopies() {
        val original=row();val captured=OutboxRetention.compact(original).copy(status="uncertain")
        val settled=SendReceiptRecovery.settle(MessageState(outgoing=listOf(captured)),captured,original.sendIntent!!,original.sendReceiptProof!!,100,99)
        val normalized=OutboxRetention.normalize(settled).outgoing.single()
        assertEquals("submitted",normalized.status);assertNull(normalized.sendIntent)
        assertEquals(original.sendIntent,normalized.originalSendIntent())
    }

    @Test fun memoizationRechecksChangedRowsAndPinsAndDropsStaleReferences() {
        val engine=OutboxRetention.Normalizer()
        val initial=engine.normalize(MessageState(outgoing=List(100){represented()}))
        val evaluations=engine.rowEvaluations;val walks=engine.walks
        val typed=initial.copy(drafts=listOf(Draft(target,"typing")))
        assertEquals(typed,engine.normalize(typed));assertEquals(walks,engine.walks)
        // A new immutable list with unchanged rows and a new pin set do not redo row validation/encoding.
        val pinned=engine.normalize(initial.copy(outgoing=initial.outgoing.toList()),setOf(initial.outgoing.first().requestId))
        assertEquals(evaluations,engine.rowEvaluations);assertEquals(walks+1,engine.walks)
        val changed=pinned.outgoing.first().copy(text="changed")
        val checked=engine.normalize(pinned.copy(outgoing=listOf(changed)+pinned.outgoing.drop(1)))
        assertEquals(evaluations+1,engine.rowEvaluations);assertFalse(OutboxRetention.evictable(checked.outgoing.first()))
        val large=engine.normalize(MessageState(outgoing=List(700){row(status="submitted")}))
        assertEquals(700,large.outgoing.size);assertTrue(engine.memoizedRows<=512)
        engine.normalize(MessageState());assertEquals(0,engine.memoizedRows)
    }
    @Test fun typingSkipsOutboxWalksAndStaysBelowTheJvmFrameBudget() = runBlocking {
        fun measure(action:()->Unit):Pair<Double,Double> {
            repeat(8){action()}
            val samples=List(40){val start=System.nanoTime();action();(System.nanoTime()-start)/1_000_000.0}.sorted()
            return samples[20] to samples[36]
        }
        for((label,rows) in listOf(
            "100-recorded-200B" to List(100){represented("x".repeat(200))},
            "58-recorded-8KiB-Cyrillic" to List(58){represented("Я".repeat(4096))},
            "58-recorded-8KiB-plus-200-protected" to (List(58){represented("Я".repeat(4096))}+List(200){row(status="submitted")})
        )) {
            val scope=CoroutineScope(SupervisorJob()+Dispatchers.Default)
            try {
                val pins=OutboxPins();val delivery=emptySet<String>();val receipts=emptySet<String>()
                val events=rows.take(100).map(::native)
                pins.retain(delivery,receipts,events)
                val writer=OutboxRetention.Normalizer();val publication=OutboxRetention.Normalizer()
                val draft=Draft(target,"typing")
                val initial=publication.normalize(writer.normalize(MessageState(drafts=listOf(draft),outgoing=rows),pins.snapshot()),pins.snapshot())
                val baseline=measure { OutboxRetention.normalize(initial,pins.snapshot()) }
                val cached=measure { publication.normalize(initial,pins.snapshot()) }
                val actor=OrderedStatePersistence(initial,scope,{},Dispatchers.Unconfined,debounceMillis=100_000,maxDelayMillis=100_000,
                    normalize={writer.normalize(it,pins.snapshot())},normalizePublished={publication.normalize(it,pins.snapshot())})
                val walks=publication.walks;val evaluations=publication.rowEvaluations
                var edit=0
                val typing=measure {
                    pins.retain(delivery,receipts,events)
                    val editor=draft.copy(text="typed-"+(edit++),revision="revision-"+edit)
                    actor.edit { MessageChanges.text(it,editor) }
                }
                println("OUTBOX_TIMING case=$label rows="+initial.outgoing.size+" uncached_median_ms="+baseline.first+" uncached_p90_ms="+baseline.second+
                    " cached_median_ms="+cached.first+" cached_p90_ms="+cached.second+" edit_median_ms="+typing.first+" edit_p90_ms="+typing.second)
                assertEquals(label,walks,publication.walks);assertEquals(label,evaluations,publication.rowEvaluations)
                assertTrue("$label draft edit median "+typing.first+" ms exceeded 3 ms",typing.first<3.0)
                assertTrue("$label did not avoid the validation cost",typing.first*5<baseline.first)
                assertEquals(initial.outgoing,actor.state.value.outgoing)
            } finally { scope.cancel() }
        }
    }

}
