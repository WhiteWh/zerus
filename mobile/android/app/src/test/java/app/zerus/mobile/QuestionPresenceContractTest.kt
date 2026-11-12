package app.zerus.mobile

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The shared expanded-question presence contract (question-presence-contract.json); ZerusKit checks the same rows. */
class QuestionPresenceContractTest {
    private val contract = JSONObject(checkNotNull(javaClass.getResourceAsStream("/question-presence-contract.json")).bufferedReader().readText())
    private val target = Target("computer", "session", "run", "conversation", "workspace")
    private val prompt = Prompt("q", "Question", listOf(Choice("first", "First", "")), false, true, false)
    private val now = 1_000_000_000L

    private fun question(row: JSONObject) = Question(row.getString("id"), row.getString("hash"), listOf(prompt),
        row.optBoolean("can_answer"), true, row.optBoolean("optional"), "", createdAt = 1.0, source = "codex_async",
        unavailableReason = row.optString("unavailable_reason"), run = target.run, conversation = target.conversation)
    private fun checkedAt(row: JSONObject) = if (row.isNull("checked_ago_ms")) 0L else now - row.getLong("checked_ago_ms")

    @Test fun everySharedCaseResolvesExactly() {
        assertEquals(1, contract.getInt("schema"))
        val cases = contract.getJSONArray("cases")
        assertTrue(cases.length() >= 15)
        for (index in 0 until cases.length()) {
            val case = cases.getJSONObject(index)
            val name = case.getString("name")
            val rows = case.getJSONArray("questions")
            val questions = (0 until rows.length()).map { question(rows.getJSONObject(it)) }
            val display = if (case.isNull("expanded")) QuestionDisplayState(target.key) else case.getJSONObject("expanded").let { expanded ->
                QuestionDisplayState(target.key).open(question(JSONObject().put("id", expanded.getString("id")).put("hash", expanded.getString("hash"))))
            }
            val view = QuestionPresencePolicies.resolve(display, questions, case.getBoolean("verified"), case.getBoolean("available"),
                case.getBoolean("checking"), checkedAt(case), now, complete = case.optBoolean("complete", true))
            if (case.isNull("expect")) { assertNull(name, view); continue }
            val expect = case.getJSONObject("expect")
            checkNotNull(view) { name }
            assertEquals(name, expect.getString("presence"), view.presence.name)
            assertEquals(name, expect.getString("title"), view.title)
            assertEquals(name, expect.getString("detail"), view.detail)
            assertEquals(name, expect.getString("age"), view.age)
            assertEquals(name, expect.getBoolean("modal"), view.modal)
            assertEquals(name, expect.getBoolean("refresh"), view.refresh)
            assertEquals(name, if (expect.isNull("replacement_hash")) null else expect.getString("replacement_hash"), view.replacement?.hash)
        }
    }

    @Test fun everySharedAgeAndReasonMatches() {
        val ages = contract.getJSONArray("ages")
        for (index in 0 until ages.length()) {
            val row = ages.getJSONObject(index)
            assertEquals(row.toString(), row.getString("age"), QuestionPresencePolicies.age(checkedAt(row), now))
        }
        val reasons = contract.getJSONArray("reasons")
        for (index in 0 until reasons.length()) {
            val row = reasons.getJSONObject(index)
            assertEquals(row.toString(), row.getString("reason"), QuestionPresencePolicies.reason(row.getString("raw"), row.getBoolean("optional")))
        }
    }
}
