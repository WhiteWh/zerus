package app.zerus.mobile

import com.google.crypto.tink.subtle.Ed25519Sign
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import java.io.File

/** The person's latch statement (zerus-sa9qn.50) against the shared crypto vector, and its session action. */
class QuestionRoutePersonTest {
    private val vector = JSONObject(javaClass.classLoader!!.getResource("question-route-person-v1.json")!!.readText())
    private fun hex(bytes: ByteArray) = bytes.joinToString("") { "%02x".format(it) }

    @Test fun theStatementFramesSignsAndVerifiesAsTheSharedVector() {
        val root = generateSequence(File("").absoluteFile) { it.parentFile }.first { File(it,"services/mobile/crypto/tests/vectors/question-route-person-v1.json").isFile }
        assertEquals(File(root,"services/mobile/crypto/tests/vectors/question-route-person-v1.json").readText(),
            javaClass.classLoader!!.getResource("question-route-person-v1.json")!!.readText())
        val statement = RoutePersonStatement.parse(vector.getJSONObject("statement"))
        assertEquals(vector.getString("domain"),RoutePersonStatement.DOMAIN)
        assertEquals(vector.getString("canonical_hex"),hex(statement.frame()))
        assertEquals(vector.getJSONObject("statement").keys().asSequence().toSet(),statement.json().keys().asSequence().toSet())
        assertEquals(statement,RoutePersonStatement.parse(statement.json()))
        // Ed25519 is deterministic: the phone's key signs exactly the vector's signature.
        val seed = WorkspaceProtocol.decode(vector.getString("phone_signing_seed"),32)
        assertEquals(vector.getString("signature"),WorkspaceProtocol.encode(Ed25519Sign(seed).sign(statement.frame())))
        WorkspaceCrypto.verify(WorkspaceProtocol.decode(vector.getString("phone_signing_public_key"),32),
            WorkspaceProtocol.decode(vector.getString("signature"),64),statement.frame())
        // Every field is signed.
        val changed = statement.copy(personOff = !statement.personOff)
        assertThrows(Exception::class.java) {
            WorkspaceCrypto.verify(WorkspaceProtocol.decode(vector.getString("phone_signing_public_key"),32),
                WorkspaceProtocol.decode(vector.getString("signature"),64),changed.frame())
        }
    }

    @Test fun onlyAValidStatementWithinItsLifetimeIsMade() {
        val good = RoutePersonStatement.parse(vector.getJSONObject("statement"))
        for(bad in listOf<() -> Any>({ good.copy(expiresAt = good.issuedAt) }, { good.copy(expiresAt = good.issuedAt + 301) },
                { good.copy(runId = "") }, { good.copy(conversationId = "a\nb") }, { good.copy(sessionId = "a b") },
                { RoutePersonStatement.parse(JSONObject(vector.getJSONObject("statement").toString()).put("extra",1)) }))
            assertThrows(Exception::class.java) { bad() }
    }

    @Test fun thePhoneUsesItOnlyWhereAnEncryptedConnectionItsRelayAndTheComputerOfferIt() {
        val encrypted = Connection("c","label","https://relay.example.invalid","token",2)
        val legacy = encrypted.copy(protocolVersion = 1)
        val both = setOf(SessionQuestionRouting.personOperation)
        assertTrue(SessionQuestionRouting.person(encrypted,both,both))
        assertFalse(SessionQuestionRouting.person(legacy,both,both))
        assertFalse(SessionQuestionRouting.person(encrypted,emptySet(),both))
        assertFalse(SessionQuestionRouting.person(encrypted,both,emptySet()))
        assertFalse(SessionQuestionRouting.person(null,both,both))
    }

    private val sessionId = "00000000-0000-4000-8000-0000000000c1"
    private fun args(send: Boolean = false) = JSONObject().put("send_to_parent",send).put("expected_revision",2).put("session_id",sessionId)
    private val target = Target("00000000-0000-4000-8000-000000000001","claude/project/child","run-1","conversation-1","workspace")
    private fun action() = SessionAction("00000000-0000-4000-8000-0000000000aa",target,SessionQuestionRouting.personOperation,args().toString())
    private fun receipt(result: JSONObject) = JSONObject().put("request_id","00000000-0000-4000-8000-0000000000aa").put("state","completed").put("result",result)
    private fun confirmed() = JSONObject().put("request_id","00000000-0000-4000-8000-0000000000aa").put("op","question_route_child")
        .put("accepted",true).put("durable",true).put("session_id",sessionId).put("person_off",true).put("revision",3)
        .put("applied",true).put("code","")

    @Test fun onlyTheServicesExactReceiptSettlesIt() {
        assertTrue(SessionQuestionRouting.personOperation in SessionActionPolicies.operations)
        SessionActionPolicies.arguments(SessionQuestionRouting.personOperation,args())
        for(bad in listOf(JSONObject(args().toString()).put("extra",1), JSONObject(args().toString()).put("session_id","a b"),
                JSONObject(args().toString()).apply { remove("session_id") }))
            assertThrows(IllegalArgumentException::class.java) { SessionActionPolicies.arguments(SessionQuestionRouting.personOperation,bad) }
        assertEquals("completed",SessionActionPolicies.result(action(),receipt(confirmed())).status)
        for((key,value) in listOf("person_off" to false,"session_id" to "00000000-0000-4000-8000-0000000000c2","request_id" to "x",
                "op" to "question_routes","durable" to false,"applied" to JSONObject.NULL,"code" to "x","extra" to 1)) {
            assertEquals(key,"uncertain",SessionActionPolicies.result(action(),receipt(confirmed().put(key,value))).status)
        }
        for(code in SessionQuestionRouting.personRefusals.keys) {
            val refusal = JSONObject().put("request_id","00000000-0000-4000-8000-0000000000aa").put("op","question_route_child")
                .put("accepted",false).put("durable",false).put("code",code)
            val settled = SessionActionPolicies.result(action(),receipt(refusal))
            assertEquals(code,"failed",settled.status)
            assertEquals(SessionQuestionRouting.personRefusals[code],settled.error)
        }
        val store = JSONObject().put("request_id","00000000-0000-4000-8000-0000000000aa").put("op","question_route_child")
            .put("accepted",false).put("durable",false).put("code","question_route_store_refused")
        assertEquals("uncertain",SessionActionPolicies.result(action(),receipt(store)).status)
    }

    @Test fun theLatchActionChecksTheChildsDurableIdentityBeforeSending() {
        val raw = JSONObject().put("name","claude/project/child").put("cmd","claude").put("state","running").put("session_id",sessionId)
            .put("parent",JSONObject().put("name","codex/project/lead")).put("question_routing",JSONObject().put("send_to_parent",true).put("revision",2))
        assertEquals("",SessionActionPolicies.inspectReason(target,SessionQuestionRouting.personOperation,args(),raw))
        assertEquals("The conversation changed. Refresh its exact identity before applying.",
            SessionActionPolicies.inspectReason(target,SessionQuestionRouting.personOperation,args(),JSONObject(raw.toString()).put("session_id","00000000-0000-4000-8000-0000000000c2")))
    }
}
