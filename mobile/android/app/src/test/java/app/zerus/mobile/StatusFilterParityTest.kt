package app.zerus.mobile

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * zerus-sa9qn.19: every status tab lists exactly what its count counts, through the production list projection,
 * over the whole shared contract fleet at once (desktop shared-session-contract.test.ts and ZerusKit
 * StatusFilterParityTests load the same fleet).
 */
class StatusFilterParityTest {
    private val contract = JSONObject(checkNotNull(javaClass.getResourceAsStream("/session-filter-contract.json")).bufferedReader().use { it.readText() })
    private val cases = contract.getJSONArray("cases").let { array -> (0 until array.length()).map(array::getJSONObject) }
    private val up = Machine("fleet", "up", "Online", true)
    private val down = Machine("fleet", "down", "Offline", false)

    private fun session(name: String, raw: JSONObject, computer: String = up.id, archive: String = ""): Session =
        Session(Target(computer, "codex/contract/$name", raw.string("run_id").ifBlank { "run-$name" },
            raw.string("conversation_id").ifBlank { "conversation-$name" }, "fleet", archive),
            name, "codex", "", "Project", "", QuestionPolicies.blockingRequests(raw), raw)

    private fun names(presentation: SessionListPresentation) =
        presentation.groups.flatMap { group -> group.sessions.map { it.target.session.removePrefix("codex/contract/") } }.sorted()

    @Test fun eachTabListsExactlyWhatItsCountCountsOverTheContractFleet() {
        val live = cases.filter { !it.getJSONObject("raw").optString("state").equals("archived") }
        val rows = live.map { example ->
            session(example.getString("name"), example.getJSONObject("raw"), if (example.getBoolean("online")) up.id else down.id)
        }
        val drafts = live.zip(rows).filter { (example, _) -> example.getBoolean("draft") }.map { (_, row) -> Draft(row.target, "draft") }
        for (filter in listOf(SessionFilter.All, SessionFilter.Attention, SessionFilter.Active, SessionFilter.Saved)) {
            val presentation = SessionListPresentation.prepare(rows, listOf(up, down), emptyList(), emptySet(), "", null, filter, drafts)
            val expected = live.filter { example ->
                example.getJSONArray("matches").let { array -> (0 until array.length()).any { array.getString(it) == filter.id } }
            }.map { it.getString("name") }.sorted()
            assertEquals(filter.id, expected, names(presentation))
            assertEquals(filter.id + " count", presentation.visibleCount, presentation.counts.getValue(filter))
            assertEquals(filter.id + " machine chooser", presentation.visibleCount, presentation.machineCounts.values.sum())
        }
    }

    @Test fun archiveListsAndCountsInsideTheSharedDateWindow() {
        val window = contract.getJSONObject("archive_window")
        val rows = window.getJSONArray("rows").let { array -> (0 until array.length()).map(array::getJSONObject) }.map { row ->
            session(row.getString("name"), JSONObject().put("state", "archived").put("archived_at", row.getLong("archived_at")), archive = "archive-" + row.getString("name"))
        }
        val range = ArchiveRange(window.getLong("from"), window.getLong("until"))
        val presentation = SessionListPresentation.prepare(rows, listOf(up), emptyList(), emptySet(), "", null, SessionFilter.Archive, emptyList(), range)
        val listed = window.getJSONArray("listed").let { array -> (0 until array.length()).map(array::getString) }.sorted()
        assertEquals(listed, names(presentation))
        assertEquals(listed.size, presentation.counts.getValue(SessionFilter.Archive))
        assertEquals(listed.size, presentation.machineCounts.values.sum())
    }
}
