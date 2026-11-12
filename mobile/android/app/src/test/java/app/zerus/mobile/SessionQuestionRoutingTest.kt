package app.zerus.mobile

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

/** The person's per-child question route switch against the shared contract (question-routing-contract.json). */
class SessionQuestionRoutingTest {
    private val contract = javaClass.classLoader!!.getResourceAsStream("question-routing-contract.json")!!.bufferedReader().use { JSONObject(it.readText()) }
    private fun cases(key: String) = contract.getJSONArray(key).let { list -> (0 until list.length()).map { list.getJSONObject(it) } }
    private fun target(raw: JSONObject, run: String = "run-1", conversation: String = "conversation-1") =
        Target("00000000-0000-4000-8000-000000000001",raw.optString("name","claude/project/child"),run,conversation,"workspace")

    @Test fun theOperationAndMarkerAreTheContracts() {
        assertEquals(contract.getString("operation"),SessionQuestionRouting.operation)
        assertTrue(SessionQuestionRouting.operation in SessionActionPolicies.operations)
    }

    @Test fun onlyTheExactShapeIsKnownAndAnythingElseIsUnknown() {
        for(case in cases("parse")) {
            val raw = JSONObject().put("question_routing",case.opt("question_routing") ?: JSONObject.NULL)
            val expected = case.optJSONObject("routing")?.let { QuestionRouting(it.getBoolean("send_to_parent"),it.getLong("revision")) }
            assertEquals(case.getString("name"),expected,SessionQuestionRouting.parse(raw))
        }
        assertNull("absent",SessionQuestionRouting.parse(JSONObject()))
        assertNull("no row",SessionQuestionRouting.parse(null))
    }

    @Test fun theSessionActionsShowItForLiveClaudeChildrenOnly() {
        for(case in cases("availability")) {
            val raw = case.getJSONObject("row")
            val t = target(raw)
            val facts = SessionMenuFacts(t,raw,raw.optString("state"),raw.optString("state")=="archived",true,
                setOf(SessionQuestionRouting.operation))
            val item = SessionActionsMenu.items(facts).flatten().singleOrNull { it.id == SessionQuestionRouting.operation }
            val name = case.getString("name")
            assertEquals(name,case.getBoolean("shown"),item != null)
            if(item == null) continue
            assertEquals(name,case.getBoolean("enabled"),item.enabled)
            assertTrue(name,item.label.endsWith(case.getString("parent")))
            // Offline, or a computer or gateway without the operation, disables it with its reason.
            assertEquals("The machine is offline.",SessionActionsMenu.questionRoutingReason(facts.copy(online = false)))
            assertEquals(SessionQuestionRouting.unavailable,SessionActionsMenu.questionRoutingReason(facts.copy(supported = emptySet())))
        }
    }

    @Test fun theLabelSaysWhatChoosingItDoes() {
        val raw = JSONObject().put("parent",JSONObject().put("name","codex/project/lead"))
        assertEquals("Stop sending questions to lead",SessionQuestionRouting.label(JSONObject(raw.toString()).put("question_routing",JSONObject().put("send_to_parent",true).put("revision",0))))
        assertEquals("Send questions to lead",SessionQuestionRouting.label(JSONObject(raw.toString()).put("question_routing",JSONObject().put("send_to_parent",false).put("revision",1))))
        assertEquals("Questions to lead",SessionQuestionRouting.label(raw))
    }

    @Test fun theChangeTurnsTheCurrentValueTheOtherWayAtItsRevision() {
        val change = contract.getJSONObject("change")
        val session = change.getJSONObject("session")
        val t = target(JSONObject(),session.getString("run_id"),session.getString("conversation_id"))
        val current = change.getJSONObject("routing").let { QuestionRouting(it.getBoolean("send_to_parent"),it.getLong("revision")) }
        val args = SessionActionPolicies.arguments(SessionQuestionRouting.operation,SessionQuestionRouting.toggle(current))
        assertEquals(change.getJSONObject("payload").toString(),SessionQuestionRouting.payload(t,args).toString())
        // Only exactly the switch and its revision.
        for(bad in listOf(JSONObject().put("send_to_parent",true),JSONObject().put("send_to_parent",1).put("expected_revision",0),
            JSONObject().put("send_to_parent",true).put("expected_revision",-1),JSONObject().put("send_to_parent",true).put("expected_revision",0).put("muted",true)))
            assertThrows(IllegalArgumentException::class.java) { SessionActionPolicies.arguments(SessionQuestionRouting.operation,bad) }
    }

    @Test fun aStaleRevisionOrANonChildIsRefusedBeforeSending() {
        val raw = JSONObject().put("name","claude/project/child").put("cmd","claude").put("state","running")
            .put("parent",JSONObject().put("name","codex/project/lead")).put("question_routing",JSONObject().put("send_to_parent",true).put("revision",2))
        val t = target(raw)
        val args = SessionActionPolicies.arguments(SessionQuestionRouting.operation,JSONObject().put("send_to_parent",false).put("expected_revision",2))
        assertEquals("",SessionActionPolicies.inspectReason(t,SessionQuestionRouting.operation,args,raw))
        val stale = SessionActionPolicies.arguments(SessionQuestionRouting.operation,JSONObject().put("send_to_parent",false).put("expected_revision",1))
        assertEquals("Where this child's questions go changed. Refresh before applying.",SessionActionPolicies.inspectReason(t,SessionQuestionRouting.operation,stale,raw))
        val codex = JSONObject(raw.toString()).put("cmd","codex")
        assertEquals(SessionQuestionRouting.notChild,SessionActionPolicies.inspectReason(t,SessionQuestionRouting.operation,args,codex))
    }

    @Test fun onlyTheComputersExactReplySettlesTheChange() {
        val change = contract.getJSONObject("change")
        val session = change.getJSONObject("session")
        val t = target(JSONObject(),session.getString("run_id"),session.getString("conversation_id"))
        val args = JSONObject().put("send_to_parent",change.getJSONObject("payload").getBoolean("send_to_parent"))
            .put("expected_revision",change.getJSONObject("payload").getLong("expected_revision"))
        for(case in cases("results")) {
            val action = SessionAction("00000000-0000-4000-8000-0000000000aa",t,SessionQuestionRouting.operation,args.toString())
            val receipt = JSONObject().put("request_id",action.requestId).put("state","completed").put("result",case.getJSONObject("result"))
            val settled = SessionActionPolicies.result(action,receipt)
            val expected = when(case.getString("outcome")) { "confirmed" -> "completed";"refused" -> "failed";else -> "uncertain" }
            assertEquals(case.getString("name"),expected,settled.status)
            if(expected == "failed") assertEquals(SessionQuestionRouting.refusals[case.getString("error")],settled.error)
        }
        // A receipt of another request is never this change's.
        val action = SessionAction("00000000-0000-4000-8000-0000000000aa",t,SessionQuestionRouting.operation,args.toString())
        val other = JSONObject().put("request_id","00000000-0000-4000-8000-0000000000bb").put("state","completed").put("result",cases("results")[0].getJSONObject("result"))
        assertEquals("uncertain",SessionActionPolicies.result(action,other).status)
    }
}
