package app.zerus.mobile

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class SessionFiltersTest {
    @Test fun explicitWaitKeepsNativePriorityAndRejectsReusedIdentity() {
        val wait = JSONObject("""{"schema":1,"status":"ok","run_id":"run","conversation_id":"conversation","count":1,
            "items":[{"state":"awaiting_ack","created_at":1700000000,"producer":{"name":"claude/example/worker","host":"peer"}}]}""")
        val row = session("""{"activity":"idle","phase":"idle","subagent_active_count":0,"unread_reply":true}""")
        row.raw.put("coordination_wait", wait)
        assertTrue(SessionFilters.active(row,true)); assertFalse(SessionFilters.visibleUnreadReply(row,true))
        assertTrue(SessionFilters.status(row,true).startsWith("Waiting for acceptance from worker on peer"))
        assertEquals(DesktopBadgeKind.Waiting,DesktopSessionPalette.badge(row,true).kind)
        row.raw.put("phase","approval"); assertEquals("Needs approval",SessionFilters.status(row,true))
        row.raw.put("phase","tool").put("activity","busy"); assertEquals("Using a tool",SessionFilters.status(row,true))
        row.raw.put("phase","idle").put("activity","idle").put("process_work",JSONObject().put("count",1))
        assertEquals("Background",SessionFilters.status(row,true))
        row.raw.put("process_work",JSONObject().put("count",0)); wait.put("count",0)
        assertEquals("Ready",SessionFilters.status(row,true)); assertTrue(SessionFilters.visibleUnreadReply(row,true))
        wait.put("count",1).put("run_id","other-run"); assertFalse(SessionFilters.hasCoordinationWait(row))
    }
    @Test fun intermediateRepliesKeepTheirIdentityAndReappearWhenAllWorkFinishes() {
        for (readFlag in listOf("unread_reply", "mobile_unread_reply")) {
            for (work in listOf("""{"activity":"busy","phase":"working"}""",
                """{"activity":"idle","phase":"idle","process_work":{"count":1}}""",
                """{"activity":"idle","phase":"idle","subagent_active_count":1}""",
                """{"activity":"idle","phase":"idle","subagent_count":1}""")) {
                val raw = JSONObject(work).put(readFlag, true).put("reply_id", "progress")
                val item = session(raw.toString())
                assertFalse(SessionFilters.visibleUnreadReply(item, true))
                assertFalse(SessionFilters.needsAttention(item, true))
                assertTrue(SessionFilters.active(item, true))
                assertTrue(item.raw.optBoolean(readFlag)); assertEquals("progress", item.raw.string("reply_id"))
                val finished = item.copy(raw = JSONObject(item.raw.toString()).put("activity", "idle").put("phase", "idle")
                    .put("process_work", JSONObject().put("count", 0)).put("subagent_active_count", 0).put("subagent_count", 0))
                assertTrue(SessionFilters.visibleUnreadReply(finished, true))
                assertTrue(SessionFilters.needsAttention(finished, true))
                assertEquals("progress", finished.raw.string("reply_id"))
                for (phase in listOf("input", "approval", "error")) {
                    assertTrue(SessionFilters.needsAttention(item.copy(raw = JSONObject(item.raw.toString()).put("phase", phase)), true))
                }
                assertTrue(SessionFilters.needsAttention(item.copy(raw = JSONObject(item.raw.toString()).put("mobile_review_later", true)), true))
            }
        }
    }

    @Test fun liveShellWorkIsActiveAndOutranksScheduleWhileNativeInputStaysIdle() {
        val item = session("""{"activity":"idle","phase":"idle","process_work":{"count":1},"schedules":{"count":1}}""")
        assertEquals("Background",SessionFilters.status(item,true))
        assertTrue(SessionFilters.working(item,true))
        assertTrue(SessionFilters.active(item,true))
        assertFalse(SessionFilters.scheduled(item,true))
        assertFalse(SessionFilters.needsAction(item))
        assertFalse(SessionFilters.working(item,false))
        val finished=item.copy(raw=JSONObject(item.raw.toString()).put("process_work",JSONObject().put("count",0)).put("schedules",JSONObject().put("count",0)))
        assertEquals("Ready",SessionFilters.status(finished,true))
        assertFalse(SessionFilters.active(finished,true))
    }
    private val machines = listOf(Machine("a", "same", "Computer A", true), Machine("b", "same", "Computer B", false))
    private fun session(raw: String = "{}", connection: String = "a", pending: Int? = null, archive: String = "", name: String = "codex/project/task"): Session {
        val data=JSONObject(raw)
        return Session(Target("same", name, data.string("run_id").ifBlank {"run"}, data.string("conversation_id").ifBlank {"conversation"}, connection, archive),
            "Long title", "codex", "MISLEADING UI STATUS", "Project", "Preview", pending ?: QuestionPolicies.blockingRequests(data), data)
    }
    @Test fun aKeepWarmPingIsReadyAndHoldsNoReplyLikeTheDesktop() {
        val ping = session("""{"activity":"busy","phase":"working","keep_warm":{"enabled":true,"turn_active":true}}""")
        assertEquals("Warming cache", SessionFilters.status(ping, true))
        assertFalse(SessionFilters.working(ping, true))
        assertFalse(SessionFilters.replyInProgress(ping, true))
        assertFalse(SessionFilters.active(ping, true))
        val person = session("""{"activity":"busy","phase":"working","keep_warm":{"enabled":true,"turn_active":false}}""")
        assertEquals("Working", SessionFilters.status(person, true))
        assertTrue(SessionFilters.working(person, true))
        val asking = session("""{"activity":"busy","phase":"approval","keep_warm":{"enabled":true,"turn_active":true}}""")
        assertEquals("Needs approval", SessionFilters.status(asking, true))
    }
    @Test fun filtersFollowSharedDesktopContract() {
        val contract = JSONObject(checkNotNull(javaClass.getResourceAsStream("/session-filter-contract.json")).bufferedReader().use { it.readText() })
        assertEquals(contract.getJSONArray("filters").let { array -> (0 until array.length()).map(array::getString) }, SessionFilter.entries.map { it.id })
        contract.getJSONArray("cases").objects().forEach { example ->
            val item = session(example.getJSONObject("raw").toString())
            val matches = SessionFilter.entries.filter { SessionFilters.matches(item, it, example.getBoolean("online"), example.getBoolean("draft")) }.map { it.id }
            val expected = example.getJSONArray("matches").let { array -> (0 until array.length()).map(array::getString) }
            assertEquals(example.getString("name"), expected, matches)
            // The quiet-child and mute rules read the same classification as the desktop core (zerus-sa9qn.19).
            if (example.has("quiet")) assertEquals(example.getString("name") + " quiet", example.getBoolean("quiet"), ChildSessions.quiet(item.raw))
            if (example.has("muted")) assertEquals(example.getString("name") + " muted", example.getBoolean("muted"), SessionNotificationPreferences.muted(item))
        }
    }
    @Test fun machineChooserCountsKeepWorkspaceIdentityStateProjectAndZeroMachines() {
        val a=MachineKey("a","same");val b=MachineKey("b","same");val zero=MachineKey("a","empty")
        val hosts=machines+Machine(zero.connectionId,zero.computerId,"Empty",true)
        val project=ProjectKey("a","swarm","project")
        val busy=session("""{"activity":"busy","phase":"tool"}""",name="codex/project/busy").copy(projectKey=project)
        val offline=session("""{"activity":"idle","phase":"idle"}""",connection="b").copy(projectKey=ProjectKey("b","swarm","project"))
        val saved=session("""{"state":"paused"}""",name="codex/project/saved").copy(projectKey=project)
        val archive=busy.copy(target=busy.target.copy(archiveId="archive"))
        val other=busy.copy(target=busy.target.copy(session="codex/other/busy"),projectKey=null)
        val sessions=listOf(busy,offline,saved,archive,other)
        val drafts=setOf(busy.target.key,offline.target.key,archive.target.key)
        assertEquals(mapOf(a to 3,b to 1,zero to 0),SessionFilters.machineCounts(sessions,hosts,SessionFilter.All,drafts))
        assertEquals(mapOf(a to 2,b to 1,zero to 0),SessionFilters.machineCounts(sessions,hosts,SessionFilter.Active,drafts))
        assertEquals(mapOf(a to 1,b to 0,zero to 0),SessionFilters.machineCounts(sessions,hosts,SessionFilter.Active,drafts,project))
        assertEquals(mapOf(a to 1,b to 0,zero to 0),SessionFilters.machineCounts(sessions,hosts,SessionFilter.Saved,drafts,project))
        assertEquals(mapOf(a to 1,b to 0,zero to 0),SessionFilters.machineCounts(sessions,hosts,SessionFilter.Archive,drafts,project))
        assertEquals(mapOf(a to 1,b to 0,zero to 0),SessionFilters.machineCounts(sessions,hosts,SessionFilter.Active,emptySet(),project))
    }
    @Test fun localDraftsJoinActiveOnceWithinMachineProjectAndSearchScope() {
        val project = ProjectKey("a", "swarm", "project")
        val idle = session("""{"activity":"idle"}""", name="codex/project/draft").copy(projectKey=project)
        val working = session("""{"activity":"busy","phase":"input"}""", name="codex/project/overlap").copy(projectKey=project)
        val offline = idle.copy(target=idle.target.copy(connectionId="b"))
        val archive = idle.copy(target=idle.target.copy(archiveId="archive"))
        val items = listOf(idle, working, offline, archive)
        val local = listOf(Draft(idle.target, "unsent"), Draft(working.target, "unsent"),
            Draft(offline.target, "", attachments=listOf(Attachment("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "notes.txt", "text/plain", 4, "a".repeat(64)))),
            Draft(archive.target, "unsent"))
        val drafts = SessionFilters.draftedSessions(items, local)
        assertEquals(3, SessionFilters.counts(items, machines, drafts).getValue(SessionFilter.Active))
        assertEquals(1, SessionFilters.counts(items, machines, drafts).getValue(SessionFilter.Attention))
        assertTrue(SessionFilters.matches(offline, SessionFilter.Active, false, offline.target.key in drafts))
        assertFalse(SessionFilters.matches(archive, SessionFilter.Active, true, true))
        val scoped = SessionFilters.scoped(items, machines, setOf(MachineKey("a", "same")), "draft", project)
        assertEquals(listOf(idle, archive), scoped)
        assertEquals(1, SessionFilters.counts(scoped, machines, drafts).getValue(SessionFilter.Active))
        assertEquals(1, SessionFilters.counts(items, machines).getValue(SessionFilter.Active))
    }
    @Test fun draftPresenceExcludesBlankAndDispatchedPayloadsAndKeepsRecoverableSessionDrafts() {
        val item = session("""{"activity":"idle","subagents":{"child":{"state":"idle"}}}""")
        val editing = Draft(item.target, "unsent")
        listOf(editing.copy(text=" \n\t"), editing.copy(status="submitting"), editing.copy(status="submitted"),
            editing.copy(status="recorded"), editing.copy(detachedId="detached"), editing.copy(questionId="question"),
            editing.copy(target=item.target.copy(connectionId="b"))).forEach { excluded ->
            assertTrue(SessionFilters.draftedSessions(listOf(item), listOf(excluded)).isEmpty())
        }
        assertEquals(setOf(item.target.key), SessionFilters.draftedSessions(listOf(item), listOf(editing.copy(target=item.target.copy(run="old-run", conversation="old-conversation")))))
        val child = editing.copy(target=item.target.copy(conversation="child-conversation", agentId="child", parentConversation=item.target.conversation))
        assertEquals(setOf(item.target.key), SessionFilters.draftedSessions(listOf(item), listOf(child)))
        assertTrue(SessionFilters.draftedSessions(listOf(item), listOf(child.copy(target=child.target.copy(parentConversation="older-parent")))).isEmpty())
        val profile = item.copy(raw=JSONObject(item.raw.toString()).put("subagent_source", "hook_profiles"))
        assertTrue(SessionFilters.draftedSessions(listOf(profile), listOf(child)).isEmpty())
    }
    @Test fun identitySearchFindsSavedArchiveAndChildIdsWithinTheMachineScope() {
        val item = session("""{"state":"paused","subagents":{"agent-one":{"conversation_id":"child-conversation"}}}""", archive="archive-one")
        listOf("CONVERSATION", "versat", "run", "archive-one", "agent-one", "child-conversation").forEach { query ->
            assertEquals(listOf(item), SessionFilters.scoped(listOf(item), machines, emptySet(), query))
            assertTrue(SessionFilters.scoped(listOf(item), machines, setOf(MachineKey("b", "same")), query).isEmpty())
        }
        val profiles = item.copy(raw=JSONObject("""{"subagent_source":"hook_profiles","subagents":{"profile-only":{"conversation_id":"profile-conversation"}}}"""))
        assertTrue(SessionFilters.scoped(listOf(profiles), machines, emptySet(), "profile-only").isEmpty())
        val preview = item.copy(raw=JSONObject("""{"subagent_previews":[{"agent_id":"preview-child","conversation_id":"preview-conversation"}]}"""))
        assertEquals(listOf(preview), SessionFilters.scoped(listOf(preview), machines, emptySet(), "preview-child"))
        val mention = item.copy(preview="Working on task zerus-ab12")
        assertEquals(listOf(mention), SessionFilters.scoped(listOf(mention), machines, emptySet(), "zerus-ab12"))
    }
    @Test fun omittedLifecycleDefaultsRunningAsDesktopDoes() {
        val item = session("""{"activity":"busy","phase":"tool","process_state":"running"}""")
        assertEquals("running", SessionFilters.state(item))
        assertTrue(SessionFilters.matches(item, SessionFilter.Active, true))
        assertFalse(SessionFilters.matches(item, SessionFilter.Saved, true))
    }
    @Test fun activeWorkRequiresReachableCurrentProcessAndNeverDisplayString() {
        val item = session("""{"activity":"busy","phase":"working","state":"running"}""")
        assertTrue(SessionFilters.matches(item, SessionFilter.Active, true))
        assertFalse(SessionFilters.matches(item, SessionFilter.Active, false))
        listOf("""{"activity":"busy","process_state":"exited"}""", """{"activity":"busy","state":"paused"}""",
            """{"activity":"busy","conversation_state":"ended"}""", """{"activity":"unknown"}""", """{"activity":"idle"}""").forEach {
            assertFalse(SessionFilters.matches(session(it), SessionFilter.Active, true))
        }
    }
    @Test fun activeIncludesAttentionForInputApprovalAndNonRecoveringError() {
        listOf("input", "approval", "error").forEach { phase ->
            val item = session(JSONObject().put("activity", "busy").put("phase", phase).toString())
            assertTrue(SessionFilters.matches(item, SessionFilter.Attention, true))
            assertTrue(SessionFilters.matches(item, SessionFilter.Active, true))
            assertFalse(SessionFilters.matches(item, SessionFilter.Attention, false))
            assertFalse(SessionFilters.matches(item, SessionFilter.Active, false))
        }
    }
    @Test fun activeCountsUnionOnceAndPreservesExplicitAttentionOffline() {
        val working = session("""{"activity":"busy","phase":"tool"}""", name = "codex/project/working")
        val attention = session("""{"activity":"idle","phase":"input"}""", name = "codex/project/input")
        val overlap = session("""{"activity":"busy"}""", pending = 1, name = "codex/project/pending-request")
        val child = session("""{"activity":"idle","subagents":{"child":{"display_state":"input"}}}""", name = "codex/project/child")
        val reminder = session("""{"state":"paused","review_later":true}""", connection = "b", name = "codex/project/reminder")
        val archived = session("""{"activity":"busy","phase":"input","review_later":true}""", archive = "archive-one")
        val saved = session("""{"state":"paused","activity":"busy"}""")
        val items = listOf(working, attention, overlap, child, reminder, archived, saved)
        val counts = SessionFilters.counts(items, machines)
        assertEquals(5, counts.getValue(SessionFilter.Active))
        assertEquals(4, counts.getValue(SessionFilter.Attention))
        assertEquals(listOf(working, attention, overlap, child, reminder), items.filter {
            SessionFilters.matches(it, SessionFilter.Active, SessionFilters.online(it, machines))
        })
        assertTrue(SessionFilters.matches(reminder, SessionFilter.Active, false))
        assertFalse(SessionFilters.matches(attention, SessionFilter.Active, false))
        assertFalse(SessionFilters.matches(session("""{"phase":"approval","attention_acknowledged":true}"""), SessionFilter.Active, true))
    }
    @Test fun scheduledRecoveryErrorDoesNotRequestAttentionButBlockedRecoveryDoes() {
        listOf("waiting", "dispatching", "retrying").forEach { recovery ->
            assertFalse(SessionFilters.needsAction(session(JSONObject().put("phase", "error").put("recovery", JSONObject().put("state", recovery)).toString())))
        }
        listOf("uncertain", "blocked", "exhausted").forEach { recovery ->
            assertTrue(SessionFilters.needsAction(session(JSONObject().put("phase", "idle").put("recovery", JSONObject().put("state", recovery)).toString())))
        }
        assertFalse(SessionFilters.needsAction(session("""{"state":"paused","recovery":{"state":"blocked"}}""")))
    }
    @Test fun optionalOnlySessionsAreNotAttentionButRequiredAndExplicitChildRequestsAreAndRoutineFailuresAreNot() {
        val optional = JSONObject("""{"question_id":"codex-async:c:0","question_hash":"h0","optional":true}""")
        // Older connectors omit `optional`; every codex-async card is optional.
        val olderOptional = JSONObject("""{"question_id":"codex-async:c:1","question_hash":"h1"}""")
        val required = JSONObject("""{"question_id":"request","question_hash":"h2"}""")
        val summary = JSONObject("""{"count":2,"ids":["codex-async:c:0","codex-async:c:1"],"latest_at":1760000000}""")
        for (base in listOf("""{"activity":"busy","phase":"tool"}""", """{"activity":"idle","phase":"idle"}""")) {
            val listed = JSONObject(base).put("mobile_attention", JSONArray(listOf(optional, olderOptional))).put("optional_questions", summary)
            val olderComputer = JSONObject(base).put("mobile_attention", JSONArray(listOf(olderOptional)))
            for (raw in listOf(listed, olderComputer)) {
                // The agent keeps working without them, as on the desktop: no status, filter or counter.
                val item = session(raw.toString())
                assertEquals(0, item.pendingCount)
                assertFalse(SessionFilters.needsAttention(item, true))
                assertEquals(SessionFilters.working(item, true), SessionFilters.active(item, true))
                assertEquals(0, SessionFilters.counts(listOf(item), machines).getValue(SessionFilter.Attention))
                assertNotEquals(DesktopBadgeKind.Attention, DesktopSessionPalette.badge(item, true).kind)
                // A required request, approval, error or explicit child request beside them still needs attention.
                val withRequired = session(JSONObject(raw.toString()).apply { getJSONArray("mobile_attention").put(required) }.toString())
                assertEquals(1, withRequired.pendingCount)
                assertTrue(SessionFilters.needsAttention(withRequired, true))
                assertEquals(1, SessionFilters.counts(listOf(withRequired), machines).getValue(SessionFilter.Attention))
                listOf("input", "approval", "error").forEach { phase ->
                    assertTrue(SessionFilters.needsAttention(session(JSONObject(raw.toString()).put("phase", phase).toString()), true))
                }
                assertTrue(SessionFilters.needsAttention(session(JSONObject(raw.toString())
                    .put("subagents", JSONObject().put("child", JSONObject().put("display_state", "input"))).toString()), true))
                // Offline rows keep only device reminders, as before.
                assertFalse(SessionFilters.needsAttention(withRequired, false))
                assertFalse(SessionFilters.needsAttention(item, false))
                assertTrue(SessionFilters.needsAttention(session(JSONObject(raw.toString()).put("mobile_review_later", true).toString()), false))
            }
        }
        // Without connector items, the older pending count stays a blocking request.
        assertTrue(SessionFilters.needsAttention(session("""{"activity":"busy","pending_question_count":1}"""), true))
        assertFalse(SessionFilters.needsAttention(session("""{"activity":"busy","pending_question_count":1}"""), false))
        listOf("input", "approval", "attention").forEach { child ->
            assertTrue(SessionFilters.needsAttention(session(JSONObject().put("activity", "busy").put("subagents", JSONObject().put("child", JSONObject().put("display_state", child))).toString()), true))
        }
        assertFalse(SessionFilters.needsAttention(session("""{"activity":"busy","subagents":{"child":{"state":"error"}}}"""), true))
        assertFalse(SessionFilters.needsAttention(session("""{"activity":"busy","subagent_source":"hook_profiles","subagents":{"child":{"display_state":"input"}}}"""), true))
        assertTrue(SessionFilters.needsAttention(session("""{"activity":"busy","subagent_source":"hooks","subagent_previews":[{"state":"input"}]}"""), true))
    }
    @Test fun exposedReviewFlagCanRemainOfflineButUnreadAndStaleApprovalCannot() {
        assertTrue(SessionFilters.needsAttention(session("""{"review_later":true}"""), false))
        assertFalse(SessionFilters.needsAttention(session("""{"unread_reply":true,"phase":"approval"}"""), false))
        assertFalse(SessionFilters.needsAttention(session("""{"phase":"approval","attention_acknowledged":true}"""), true))
    }
    @Test fun archivesAreExclusiveAndSavedMeansNonRunning() {
        val archived = session("""{"state":"archived","activity":"busy","phase":"input"}""", archive = "archive-one")
        SessionFilter.entries.filterNot { it == SessionFilter.Archive }.forEach { assertFalse(SessionFilters.matches(archived, it, true)) }
        assertTrue(SessionFilters.matches(archived, SessionFilter.Archive, false))
        listOf("paused", "stopped").forEach { state -> assertTrue(SessionFilters.matches(session(JSONObject().put("state", state).toString()), SessionFilter.Saved, true)) }
        assertFalse(SessionFilters.matches(session(), SessionFilter.Archive, true))
    }
    @Test fun machineScopeNeverConflatesSameIdAcrossWorkspaces() {
        val a = session("""{"activity":"busy"}""")
        val b = session("""{"activity":"busy"}""", connection = "b")
        val scoped = SessionFilters.scoped(listOf(a, b), machines, setOf(MachineKey("a", "same")), "")
        assertEquals(listOf(a), scoped)
        assertNotEquals(MachineKey("a", "same").key, MachineKey("b", "same").key)
        assertEquals(1, SessionFilters.counts(scoped, machines).getValue(SessionFilter.Active))
    }
    @Test fun searchAndProjectScopeApplyBeforeEveryCounterAndCards() {
        val key = ProjectKey("a", "swarm", "project")
        val a = session("""{"activity":"busy","git_branch":"feature/synthetic"}""").copy(projectKey = key)
        val b = session("""{"activity":"busy"}""").copy(projectKey = ProjectKey("a", "swarm", "other"))
        val scoped = SessionFilters.scoped(listOf(a, b), machines, emptySet(), "synthetic", key)
        assertEquals(listOf(a), scoped)
        assertEquals(1, SessionFilters.counts(scoped, machines).getValue(SessionFilter.All))
        assertEquals(1, SessionFilters.counts(scoped, machines).getValue(SessionFilter.Active))
        assertTrue(SessionFilters.scoped(listOf(a), machines, emptySet(), "computer a").isNotEmpty())
    }
    @Test fun groupOrderUsesCatalogAndNeverShowsEmptyGroupsOrMergesNames() {
        val a = ProjectKey("a", "swarm", "a"); val b = ProjectKey("b", "swarm", "b"); val empty = ProjectKey("a", "swarm", "empty")
        val projects = listOf(ProjectSummary(b, "Same", "", emptyList(), emptyMap()), ProjectSummary(empty, "Empty", "", emptyList(), emptyMap()), ProjectSummary(a, "Same", "", emptyList(), emptyMap()))
        val groups = SessionFilters.grouped(listOf(session().copy(projectKey = a), session(connection = "b").copy(projectKey = b)), projects)
        assertEquals(listOf(b, a), groups.map { it.key })
        assertEquals(2, groups.size)
    }
    @Test fun lifecycleStatusAndShortLabelMirrorDesktop() {
        assertEquals("task", SessionFilters.label(session()))
        assertEquals("nested/task", SessionFilters.label(session(name = "codex/project/nested/task")))
        assertEquals("custom", SessionFilters.label(session("""{"tag":"custom"}""")))
        assertEquals("Archived", SessionFilters.status(session("""{"activity":"idle"}""", archive = "archive-one"), true))
        assertEquals("Paused", SessionFilters.status(session("""{"state":"paused","activity":"idle"}"""), true))
        assertEquals("Stopped", SessionFilters.status(session("""{"state":"stopped","activity":"idle"}"""), true))
        assertEquals("Offline", SessionFilters.status(session("""{"activity":"busy"}"""), false))
    }
    @Test fun archiveDatesUseLocalTimeWithSecondsToDistinguishSameMinuteVersions() {
        val zone = java.util.TimeZone.getTimeZone("UTC")
        val a = session("{\"archived_at\":3}", archive = "first")
        val b = session("{\"archived_at\":4}", archive = "second")
        val first = SessionFilters.archiveDate(a, java.util.Locale.US, zone)
        assertTrue(first.contains("12:00:03"))
        assertNotEquals(first, SessionFilters.archiveDate(b, java.util.Locale.US, zone))
        assertNotEquals(first, SessionFilters.archiveDate(a, java.util.Locale.US, java.util.TimeZone.getTimeZone("GMT+03:00")))
    }
    @Test fun missingInvalidOrLiveArchiveDatesAreOmitted() {
        assertEquals("", SessionFilters.archiveDate(session("{\"archived_at\":3}")))
        listOf("{}", "{\"archived_at\":null}", "{\"archived_at\":\"invalid\"}", "{\"archived_at\":0}", "{\"archived_at\":-1}", "{\"archived_at\":1e100}").forEach {
            assertEquals("", SessionFilters.archiveDate(session(it, archive = "first")))
        }
    }

}
