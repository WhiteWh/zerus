package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Locale

class ChildSessionsTest {
    private val machine = "11111111-1111-4111-8111-111111111111"
    private val other = "99999999-9999-4999-8999-999999999999"
    private val upper = "AAAAAAAA-1111-4111-8111-111111111111"
    private fun id(n: Int) = "%08d-0000-4000-8000-000000000000".format(n)
    private fun link(parent: Int, state: String = "live", conversation: String = "", sessionId: String = id(parent)) = JSONObject()
        .put("schema", 1).put("machine", machine).put("session_id", sessionId).put("name", "claude/project/p$parent")
        .put("conversation_id", conversation).put("run_id", "r").put("source", "launch").put("state", state)
    private fun session(n: Int, raw: JSONObject = JSONObject(), parent: JSONObject? = null) = Session(
        Target("computer", "claude/project/s$n", "run-$n", "conversation-$n", "workspace"), "s$n", "claude", "", "project", "", 0,
        raw.put("session_id", id(n)).put("conversation_id", "conversation-$n").put("state", "running").apply { parent?.let { put("parent", it) } })
    private fun uuid(row: Session) = machine
    private fun nest(rows: List<Session>, collapsed: Set<Int> = emptySet()) =
        ChildSessions.nest(rows, ChildSessions.parentIndex(rows, ::uuid), { true }) { row -> row.target.session.substringAfterLast('s').toInt() in collapsed }
    private fun shape(entries: List<SessionCardEntry>) = entries.map { "${"  ".repeat(it.depth)}${it.session.title}" }

    /** The supervised-attention cases the desktop core checks (client/core/tests/supervised_attention.rs). */
    @Test fun everySharedFixtureRowHasTheDesktopQuietAttention() {
        val file = generateSequence(java.io.File("").absoluteFile) { it.parentFile }
            .map { java.io.File(it, "client/core/tests/fixtures/supervised-attention.json") }.firstOrNull { it.isFile }
        assertNotNull("The shared fixture is part of this repository", file)
        val fixture = JSONObject(file!!.readText())
        assertEquals(1, fixture.getInt("schema"))
        val cases = fixture.getJSONArray("cases")
        assertTrue(cases.length() >= 10)
        for (i in 0 until cases.length()) {
            val case = cases.getJSONObject(i)
            assertEquals(case.getString("name"), case.getBoolean("quiet"), ChildSessions.quiet(case.getJSONObject("row")))
        }
    }

    @Test fun onlyWellFormedParentLinksCount() {
        assertNotNull(ChildSessions.parent(JSONObject().put("parent", link(1))))
        // An older parent record without a session ID matches by conversation.
        assertNotNull(ChildSessions.parent(JSONObject().put("parent", link(1, conversation = "c", sessionId = ""))))
        for (broken in listOf(link(1).put("machine", upper), link(1).put("name", ""), link(1, sessionId = "not-a-uuid"),
            link(1, sessionId = ""), link(1).put("machine", 7), link(1).put("name", "a\u0007b"))) {
            assertNull(broken.toString(), ChildSessions.parent(JSONObject().put("parent", broken)))
        }
        assertTrue(ChildSessions.supervised(JSONObject().put("parent", link(1))))
        assertFalse(ChildSessions.supervised(JSONObject().put("parent", link(1, "saved"))))
        assertEquals("p1", ChildSessions.parent(JSONObject().put("parent", link(1)))!!.shortName)
    }

    @Test fun parentsMatchByMachineAndSessionIdThenByConversationOnlyForAnUnidentifiedParent() {
        val parent = session(1)
        val child = session(2, parent = link(1))
        val elsewhere = session(3, parent = link(1).put("machine", other))
        val index = ChildSessions.parentIndex(listOf(parent, child, elsewhere), ::uuid)
        assertEquals(parent.target.key, index(child))
        assertNull(index(elsewhere)); assertNull(index(parent))
        // Fallback by conversation: only to a listed parent without a session ID.
        val legacy = Session(parent.target.copy(session = "claude/project/legacy"), "legacy", "claude", "", "project", "", 0,
            JSONObject().put("conversation_id", "c-old"))
        val old = session(4, parent = link(9, conversation = "c-old", sessionId = ""))
        val identified = session(5, parent = link(9, conversation = "c-old"))
        val fallback = ChildSessions.parentIndex(listOf(legacy, old, identified), ::uuid)
        assertEquals(legacy.target.key, fallback(old))
        assertEquals(legacy.target.key, fallback(identified))
        val renamed = session(6).also { it.raw.put("conversation_id", "c-old") }
        assertNull(ChildSessions.parentIndex(listOf(renamed, identified), ::uuid)(identified))
        // A computer without a machine UUID lists nothing to match.
        assertNull(ChildSessions.parentIndex(listOf(parent, child)) { null }(child))
    }

    @Test fun childrenNestUnderTheirParentInListOrderAndCollapseHidesOnlyDescendants() {
        val rows = listOf(session(3, parent = link(1)), session(1), session(2, parent = link(1)), session(4, parent = link(3)), session(5))
        assertEquals(listOf("s1", "  s3", "    s4", "  s2", "s5"), shape(nest(rows)))
        val entries = nest(rows).associateBy { it.session.title }
        assertEquals(rows[1].target.key, entries.getValue("s3").parentKey)
        assertFalse(entries.getValue("s3").lastChild); assertTrue(entries.getValue("s2").lastChild)
        assertEquals(3, entries.getValue("s1").team!!.total); assertEquals(1, entries.getValue("s3").team!!.total)
        assertNull(entries.getValue("s5").team); assertNull(entries.getValue("s4").team)
        assertEquals(listOf("s1", "s5"), shape(nest(rows, collapsed = setOf(1))))
        assertEquals(listOf("s1", "  s3", "  s2", "s5"), shape(nest(rows, collapsed = setOf(3))))
        // Collapsing a session without children changes nothing.
        assertEquals(shape(nest(rows)), shape(nest(rows, collapsed = setOf(5))))
    }

    @Test fun nestingNeverHidesARowWithoutAShownParent() {
        // Parent filtered out: the child stays at top level and names it.
        val orphan = nest(listOf(session(2, parent = link(1)))).single()
        assertEquals(0, orphan.depth); assertEquals("p1", orphan.fromParent!!.shortName); assertNull(orphan.fromParentKey)
        // A loop starts at its first card in list order.
        val loop = listOf(session(1, parent = link(2)), session(2, parent = link(1)))
        assertEquals(listOf("s1", "  s2"), shape(nest(loop)))
        // Beyond the depth bound a card starts at top level again.
        val chain = (1..ChildSessions.MAX_DEPTH + 2).map { n -> if (n == 1) session(1) else session(n, parent = link(n - 1)) }
        val depths = nest(chain).map { it.depth }
        assertEquals(chain.size, depths.size)
        assertEquals(ChildSessions.MAX_DEPTH, depths.max())
        assertEquals(0, depths[ChildSessions.MAX_DEPTH + 1])
    }

    @Test fun theParentCountsWorkingQuietRequestsAndRequestsOnlyThePersonCanAnswer() {
        fun quiet(raw: JSONObject) = raw.put("human_request", false)
        val rows = listOf(session(1),
            session(2, JSONObject().put("activity", "busy").put("phase", "tool").put("tracked", true), link(1)),
            session(3, quiet(JSONObject().put("activity", "idle").put("phase", "input").put("tracked", true)), link(1)),
            session(4, quiet(JSONObject().put("activity", "idle").put("phase", "approval").put("tracked", true)), link(1)),
            session(5, JSONObject().put("activity", "idle").put("phase", "input").put("tracked", true), link(1)),
            session(6, quiet(JSONObject().put("activity", "idle").put("phase", "error").put("tracked", true)), link(1)))
        val team = nest(rows).first().team!!
        assertEquals(SessionTeam(total = 5, working = 1, asking = 1, needsYou = 2, errors = 1), team)
        assertEquals(listOf("1 of 5 working", "Needs you: 2", "Asking: 1", "Errors: 1"), team.summary())
        // A quiet child's badge is gray; its approval stays amber (spec 2.1, 2.2).
        assertEquals(DesktopBadgeKind.Neutral, DesktopSessionPalette.badge(rows[2], true).kind)
        assertEquals(DesktopBadgeKind.Attention, DesktopSessionPalette.badge(rows[3], true).kind)
        assertEquals(DesktopBadgeKind.Attention, DesktopSessionPalette.nativeKind(rows[2], true))
        // A quiet child's error is gray too; an explicitly muted error stays red, quiet or not (zerus-nlck0).
        assertEquals(DesktopBadgeKind.Error, DesktopSessionPalette.nativeKind(rows[5], true))
        assertEquals(DesktopBadgeKind.Neutral, DesktopSessionPalette.badge(rows[5], true).kind)
        assertEquals(DesktopBadgeKind.Error, DesktopSessionPalette.badge(session(7, JSONObject().put("activity", "idle").put("phase", "error").put("tracked", true), link(1)), true).kind)
        val mutedError = session(8, quiet(JSONObject().put("activity", "idle").put("phase", "error").put("tracked", true)), link(1))
        mutedError.raw.put("notification_preferences", JSONObject().put("conversation_id", "conversation-8").put("muted", true).put("revision", 2)
            .put("suppressed", JSONObject().put("reply_id", JSONObject.NULL).put("attention_signature", JSONObject.NULL)
                .put("optional_question_ids", JSONArray()).put("main_attention", JSONObject.NULL).put("child_attention", JSONObject())))
        assertTrue(SessionNotificationPreferences.muted(mutedError))
        assertEquals(DesktopBadgeKind.Error, DesktopSessionPalette.badge(mutedError, true).kind)
        // Its request waits for the parent, so it leaves Needs attention and Active; an approval only the person can
        // answer stays in both (spec 2.1, zerus-sa9qn.18/.19).
        assertTrue(SessionFilters.needsAttention(rows[2], true))
        assertFalse(SessionFilters.matches(rows[2], SessionFilter.Attention, true))
        assertFalse(SessionFilters.matches(rows[2], SessionFilter.Active, true))
        assertTrue(SessionFilters.matches(rows[3], SessionFilter.Attention, true))
        assertTrue(SessionFilters.matches(rows[3], SessionFilter.Active, true))
        // Shown subagents join the one counter.
        val subagents = listOf(SessionCardChild("a", "a", "Working", "", "claude", DesktopBadgeKind.Working, false),
            SessionCardChild("b", "b", "Ready", "", "claude", DesktopBadgeKind.Neutral, false))
        assertEquals(team.copy(total = 7, working = 2), team.withSubagents(subagents))
    }

    @Test fun collapsedParentsAreKeptByMachineAndSessionIdWithNewestLast() {
        val parent = session(1)
        val durable = "$machine\n${id(1)}"
        assertEquals(durable, ChildSessions.teamId(machine, parent))
        assertNull(ChildSessions.teamId(null, parent)); assertNull(ChildSessions.teamId(upper, parent))
        assertNull(ChildSessions.teamId(machine, session(2).also { it.raw.put("session_id", "") }))
        assertTrue(TeamCollapse(listOf(durable)).collapsed(parent, machine))
        assertFalse(TeamCollapse().collapsed(parent, machine))
        // This app's choice wins by row key, also for a row without durable IDs.
        assertFalse(TeamCollapse(listOf(durable), mapOf(parent.target.key to false)).collapsed(parent, machine))
        assertTrue(TeamCollapse(overrides = mapOf(parent.target.key to true)).collapsed(parent, null))
        val stored = JSONArray(listOf(durable, "garbage", "$machine\nnot-a-uuid", durable, "$other\n${id(2)}"))
        assertEquals(listOf(durable, "$other\n${id(2)}"), ChildSessions.parseTeams(stored))
        assertEquals(listOf("$other\n${id(2)}", durable), ChildSessions.withTeam(listOf(durable, "$other\n${id(2)}"), durable, true))
        assertEquals(listOf("$other\n${id(2)}"), ChildSessions.withTeam(listOf(durable, "$other\n${id(2)}"), durable, false))
        // A saved choice needs no override; a row without durable IDs keeps its own, bounded (zerus-mepi6).
        val chosen = TeamCollapse(overrides = mapOf(parent.target.key to true))
        assertEquals(TeamCollapse(listOf(durable)), chosen.choose(parent.target.key, true, listOf(durable)))
        var transient = TeamCollapse()
        repeat(TeamCollapse.MAX_OVERRIDES + 5) { transient = transient.choose("row-$it", it % 2 == 0, null) }
        assertEquals(TeamCollapse.MAX_OVERRIDES, transient.overrides.size)
        assertEquals("row-5", transient.overrides.keys.first())
        transient = transient.choose("row-5", false, null)
        assertEquals("row-5", transient.overrides.keys.last()); assertEquals(false, transient.overrides["row-5"])
        assertEquals(TeamCollapse.MAX_OVERRIDES, transient.overrides.size)
        val full = (0 until ChildSessions.MAX_COLLAPSED_TEAMS).map { "$machine\n${id(it)}" }
        assertEquals(ChildSessions.MAX_COLLAPSED_TEAMS, ChildSessions.withTeam(full, "$other\n${id(1)}", true).size)
        assertEquals("$other\n${id(1)}", ChildSessions.withTeam(full, "$other\n${id(1)}", true).last())
    }

    @Test fun accountMarksMatchTheDesktopLetterSlotAndInk() {
        // Slots computed by the desktop's labelSlot (client/src/store/accountMarks.ts).
        mapOf("Work" to 1, "Personal" to 0, "ops@example.com" to 7, "Ünïcode Team" to 3, "a".repeat(40) to 0, "😀 Fun" to 2)
            .forEach { (label, slot) -> assertEquals(label, slot, ChildSessions.labelSlot(label)) }
        assertEquals("W", ChildSessions.markLetter("  work", "claude", Locale.ROOT))
        assertEquals("F", ChildSessions.markLetter("😀 fun", "claude", Locale.ROOT))
        assertEquals("2", ChildSessions.markLetter("-- 2nd", "claude", Locale.ROOT))
        assertEquals("C", ChildSessions.markLetter("", "claude", Locale.ROOT))
        assertEquals("?", ChildSessions.markLetter("--", "", Locale.ROOT))
        // Any Unicode number, as the desktop's [\p{L}\p{N}] (zerus-1g5vt).
        mapOf("① Work" to "①", "½ cup" to "½", "Ⅻ team" to "Ⅻ", "٣ arabic" to "٣", "ⅻ small" to "Ⅻ")
            .forEach { (label, letter) -> assertEquals(label, letter, ChildSessions.markLetter(label, "claude", Locale.ROOT)) }
        assertEquals(0xFF10151B, ChildSessions.inkFor(0xFFFFCF33)); assertEquals(0xFFFFFFFF, ChildSessions.inkFor(0xFF1F2A44))
        assertEquals(0xFFFFFFFF, ChildSessions.inkFor(0xFF5A2CA0)); assertEquals(0xFF10151B, ChildSessions.inkFor(0xFF2B97FF))
    }

    @Test fun aRowShowsTheMarkOfItsRecordedProfileOnItsComputerOnly() {
        fun account(profile: String, provider: String, label: String, email: String) = ReportedAccount(profile, provider, label, true, true, false,
            "ok", "signed_in", AccountIdentity(email = email), emptyList(), null, null, emptyList(), 10.0, false, false, false, false)
        val catalogs = listOf(AccountCatalog(MachineKey("workspace", "computer"), "Arch", true, true, false, true, 10.0,
            listOf(account("work", "claude", "Work", "w@example.com"), account("plain", "claude", "", "p@example.com"),
                account("ds", "dsh", "Deep", "d@example.com"))),
            AccountCatalog(MachineKey("workspace", "laptop"), "Laptop", true, true, false, true, 10.0, listOf(account("away", "claude", "Away", "a@example.com"))))
        val groups = AccountGrouping.group(catalogs) { it.lowercase() }
        fun row(raw: JSONObject, computer: String = "computer", agent: String = "claude") =
            Session(Target(computer, "s", "r", "c", "workspace"), "s", agent, "", "", "", 0, raw)
        val work = ChildSessions.accountMark(row(JSONObject().put("account_id", "work").put("cmd", "claude")), groups)!!
        assertEquals("Work", work.label); assertEquals("W", work.letter); assertEquals("claude", work.provider)
        assertEquals(0xFF8EA9F4, work.color)
        // The color the computer keeps for the account wins (zerus-sa9qn.6); an invalid one is ignored.
        val kept = ChildSessions.accountMark(row(JSONObject().put("account_id", "work").put("cmd", "claude")
            .put("account_mark", JSONObject().put("color", "#ffcf33").put("emoji", "🦊"))), groups)!!
        assertEquals(0xFFFFCF33, kept.color); assertEquals(0xFF10151B, kept.ink)
        for (bad in listOf("#FFCF33", "red", "#ffcf3")) assertEquals(0xFF8EA9F4,
            ChildSessions.accountMark(row(JSONObject().put("account_id", "work").put("cmd", "claude")
                .put("account_mark", JSONObject().put("color", bad))), groups)!!.color)
        // An unnamed account is labeled by its email.
        assertEquals("p@example.com", ChildSessions.accountMark(row(JSONObject().put("account_id", "plain").put("cmd", "claude")), groups)!!.label)
        // DeepSeek sessions record dsh profiles.
        assertEquals("Deep", ChildSessions.accountMark(row(JSONObject().put("account_id", "ds").put("cmd", "deepseek"), agent = "deepseek"), groups)!!.label)
        // No guess: no binding, another computer's profile, or a profile the catalog does not list.
        assertNull(ChildSessions.accountMark(row(JSONObject().put("cmd", "claude")), groups))
        assertNull(ChildSessions.accountMark(row(JSONObject().put("account_id", "away").put("cmd", "claude")), groups))
        assertNull(ChildSessions.accountMark(row(JSONObject().put("account_id", "missing").put("cmd", "claude")), groups))
        assertNull(ChildSessions.accountMark(row(JSONObject().put("account_id", "work").put("cmd", "codex"), agent = "codex"), groups))
    }
}
