package app.zerus.mobile

import org.json.JSONArray
import java.text.DateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/** Machine and project scopes include the workspace, even when native names coincide. */
data class MachineKey(val connectionId: String, val computerId: String) {
    val key: String get() = JSONArray(listOf(connectionId, computerId)).toString()
}
enum class SessionFilter(val id: String, val caption: String) {
    All("all", "All sessions"), Attention("attention", "Needs attention"), Active("active", "Active"),
    Saved("paused", "Saved sessions"), Archive("archived", "Archive")
}
/** `cards` nests child sessions under their parents (ChildSessions.nest); a group built without nesting lists each session. */
data class SessionGroup(val key: ProjectKey, val name: String, val color: String, val sessions: List<Session>,
    val cards: List<SessionCardEntry> = sessions.map { SessionCardEntry(it) })

object SessionFilters {
    fun machineKey(target: Target) = MachineKey(target.connectionId, target.computerId)
    fun online(session: Session, machines: List<Machine>) = machines.any {
        it.connectionId == session.target.connectionId && it.id == session.target.computerId && it.online
    }
    fun archived(session: Session) = session.target.archiveId.isNotBlank() || session.raw.string("state") == "archived"
    fun state(session: Session) = session.raw.string("state").ifBlank { if (archived(session)) "archived" else "running" }
    fun currentActivity(session: Session, online: Boolean) = online && state(session) == "running" &&
        session.raw.string("process_state") != "exited" && session.raw.string("activity") != "unknown" &&
        !(session.raw.string("conversation_state") == "ended" && session.raw.string("activity") != "idle")
    fun needsAction(session: Session): Boolean {
        val recovery = session.raw.optJSONObject("recovery")?.string("state").orEmpty()
        val providerError = session.raw.optJSONObject("provider_error")?.string("error_kind").orEmpty()
        val phase = session.raw.string("phase")
        return !archived(session) && state(session) == "running" && session.raw.string("process_state") != "exited" &&
            (phase in listOf("approval", "input") || phase == "error" &&
                (recovery !in listOf("waiting", "dispatching", "retrying") || providerError in listOf("quota", "provider_policy")) ||
                recovery in listOf("uncertain", "blocked", "exhausted"))
    }
    /** A keep-warm ping is running: the agent works, but not on the person's turn (desktop core `keep_warm_turn`). */
    fun keepWarmTurn(session: Session) = KeepWarmPresentation.turnActive(session.raw)
    fun working(session: Session, online: Boolean) = !archived(session) && currentActivity(session, online) &&
        (session.raw.string("activity") == "busy" || backgroundIdle(session)) && !needsAction(session) && !keepWarmTurn(session)
    private fun positiveCount(value: Any?) = (value as? Number)?.toDouble()?.let { it.isFinite() && it > 0 && it % 1.0 == 0.0 } == true
    fun hasCoordinationWait(session: Session): Boolean {
        val wait = session.raw.optJSONObject("coordination_wait") ?: return false
        return wait.optInt("schema") == 1 && wait.string("status") == "ok" && session.target.run.isNotBlank() &&
            session.target.conversation.isNotBlank() && wait.string("run_id") == session.target.run &&
            wait.string("conversation_id") == session.target.conversation && positiveCount(wait.opt("count")) && wait.optDouble("count") <= 1024
    }
    fun waitingIdle(session: Session) = session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle" &&
        state(session) == "running" && session.raw.string("process_state") != "exited" && !backgroundIdle(session) &&
        !positiveCount(session.raw.opt("subagent_active_count") as? Number ?: session.raw.opt("subagent_count")) && hasCoordinationWait(session)
    fun waitingLabel(session: Session, includeProducer: Boolean = true, compact: Boolean = false): String {
        val wait = session.raw.optJSONObject("coordination_wait") ?: return "Waiting for agent"
        val item = wait.optJSONArray("items")?.optJSONObject(0)
        val producer = item?.optJSONObject("producer")
        val label = if(compact) "Waiting" else if(item?.string("state") == "awaiting_result") "Waiting for result" else "Waiting for acceptance"
        val name = producer?.string("name").orEmpty().split('/').drop(2).joinToString("/")
        val host = producer?.string("host").orEmpty()
        val elapsed = item?.optDouble("created_at", 0.0)?.takeIf { it.isFinite() && it > 0 }?.let {
            val seconds = ((System.currentTimeMillis()/1000.0)-it).toLong().coerceAtLeast(0)
            if(seconds < 60) "${seconds}s" else if(seconds < 3600) "${seconds/60}m" else "${seconds/3600}h ${seconds/60%60}m"
        }.orEmpty()
        return label + (if(includeProducer && name.isNotBlank()) " from $name" else "") + (if(includeProducer && host.isNotBlank()) " on $host" else "") +
            (if(wait.optInt("count") > 1) " (+${wait.optInt("count")-1})" else "") + (if(elapsed.isNotBlank()) " ($elapsed)" else "")
    }
    fun replyInProgress(session: Session, online: Boolean) = currentActivity(session, online) && !keepWarmTurn(session) &&
        (session.raw.string("activity") == "busy" || positiveCount(session.raw.optJSONObject("process_work")?.opt("count")) ||
            positiveCount(session.raw.opt("subagent_active_count") as? Number ?: session.raw.opt("subagent_count")) ||
            session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle" && hasCoordinationWait(session))
    /** Keep the read identity while postponing reply badges and reply-only attention. */
    fun visibleUnreadReply(session: Session, online: Boolean) = !archived(session) && !replyInProgress(session, online) && SharedReadPolicies.canShowReplyBadge(session) &&
        (session.raw.optBoolean("unread_reply") || session.raw.optBoolean("mobile_unread_reply"))
    fun backgroundIdle(session: Session): Boolean {
        val count = (session.raw.optJSONObject("process_work")?.opt("count") as? Number)?.toDouble() ?: return false
        return count.isFinite() && count > 0 && count % 1.0 == 0.0 && session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle"
    }
    /** Jobs the agent scheduled for itself start its next turns: neither Ready nor Working (SessionPresentation::scheduledIdle). */
    fun scheduledIdle(session: Session): Boolean {
        val jobs = session.raw.optJSONObject("schedules")?.opt("count") as? Number
        return session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle" && (jobs?.toInt() ?: 0) > 0
    }
    fun scheduled(session: Session, online: Boolean) = !archived(session) && currentActivity(session, online) &&
        !needsAction(session) && !backgroundIdle(session) && !waitingIdle(session) && scheduledIdle(session)
    /** The next run as a time of day, "due" once it has passed, or empty if unknown. */
    fun nextScheduledRun(session: Session, now: Long = System.currentTimeMillis(), locale: Locale = Locale.getDefault(),
        zone: TimeZone = TimeZone.getDefault()): String {
        val seconds = (session.raw.optJSONObject("schedules")?.opt("next_at") as? Number)?.toDouble() ?: return ""
        if (!seconds.isFinite() || seconds <= 0) return ""
        val next = (seconds * 1000).toLong()
        if (next <= now) return "due"
        val day = java.text.SimpleDateFormat("yyyyMMdd", locale).apply { timeZone = zone }
        val sameDay = day.format(Date(next)) == day.format(Date(now))
        return java.text.SimpleDateFormat(if (sameDay) "HH:mm" else "d MMM HH:mm", locale).apply { timeZone = zone }.format(Date(next))
    }
    fun needsAttention(session: Session, online: Boolean): Boolean {
        if (archived(session)) return false
        if (session.raw.optBoolean("review_later") || session.raw.optBoolean("mobile_review_later")) return true
        if (!online) return false
        val acknowledged = session.raw.optBoolean("attention_acknowledged")
        val children = session.raw.optJSONObject("subagents")?.let { roster -> roster.keys().asSequence().mapNotNull { roster.optJSONObject(it) }.toList() }.orEmpty() +
            session.raw.optJSONArray("subagent_previews")?.objects().orEmpty()
        val childAttention = currentActivity(session, online) && session.raw.string("subagent_source") != "hook_profiles" && children.any { it.string("display_state", "state") in listOf("approval", "input", "attention") }
        return !acknowledged && (needsAction(session) || childAttention) || visibleUnreadReply(session, online) ||
            currentActivity(session, online) && session.pendingCount > 0
    }
    /** Composer drafts remain local and count even when their machine is offline. */
    fun draftedSessions(sessions: List<Session>, drafts: List<Draft>): Set<String> {
        val unsent = drafts.filter { it.questionId.isBlank() && it.detachedId.isBlank() && it.target.archiveId.isBlank() &&
            it.status !in listOf("submitting", "submitted", "recorded") && (it.text.isNotBlank() || it.attachments.isNotEmpty()) }
        if (unsent.isEmpty()) return emptySet()
        val parents = unsent.filter { it.target.agentId.isBlank() }
            .mapTo(hashSetOf()) { Triple(it.target.connectionId, it.target.computerId, it.target.session) }
        val children = unsent.filter { it.target.agentId.isNotBlank() }.groupBy {
            it.target.copy(conversation = it.target.parentConversation, agentId = "", parentConversation = "")
        }
        return sessions.filterNot(::archived).filter { session ->
            session.target.agentId.isBlank() && Triple(session.target.connectionId, session.target.computerId, session.target.session) in parents ||
                session.raw.string("subagent_source") != "hook_profiles" && children[session.target].orEmpty().any { draft ->
                    session.raw.optJSONObject("subagents")?.optJSONObject(draft.target.agentId)?.optBoolean("group") == false
                }
        }.mapTo(mutableSetOf()) { it.target.key }
    }
    /**
     * Needs attention as the status filters list and count it (desktop filterAttention, zerus-sa9qn.18): a quiet
     * supervised child's unread reply, retry or provider limit waits for its parent, so only the person's own Review
     * later mark lists it. Every other row keeps needsAttention: a child with a request only the person can answer is
     * never quiet, and a muted session stays listed.
     */
    fun filterAttention(session: Session, online: Boolean): Boolean =
        if (ChildSessions.quiet(session.raw)) !archived(session) &&
            (session.raw.optBoolean("review_later") || session.raw.optBoolean("mobile_review_later"))
        else needsAttention(session, online)
    fun active(session: Session, online: Boolean, hasDraft: Boolean = false) =
        !archived(session) && (hasDraft || filterAttention(session, online) || working(session, online) || scheduled(session, online) ||
            replyInProgress(session, online) && session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle")
    fun matches(session: Session, filter: SessionFilter, online: Boolean, hasDraft: Boolean = false): Boolean = when (filter) {
        SessionFilter.All -> !archived(session)
        SessionFilter.Attention -> filterAttention(session, online)
        SessionFilter.Active -> active(session, online, hasDraft)
        SessionFilter.Saved -> !archived(session) && state(session) != "running"
        SessionFilter.Archive -> archived(session)
    }
    fun label(session: Session): String = session.raw.string("tag").ifBlank {
        session.target.session.split('/').drop(2).joinToString("/").ifBlank {
            session.raw.string("project").ifBlank { session.target.session }
        }
    }
    fun archiveDate(session: Session, locale: Locale = Locale.getDefault(), zone: TimeZone = TimeZone.getDefault()): String {
        if (!archived(session)) return ""
        val seconds = session.raw.optDouble("archived_at", Double.NaN)
        if (!seconds.isFinite() || seconds <= 0 || seconds > 253_402_300_799.0) return ""
        return runCatching {
            DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.MEDIUM, locale).apply { timeZone = zone }
                .format(Date((seconds * 1000).toLong()))
        }.getOrDefault("")
    }
    fun status(session: Session, online: Boolean): String {
        val raw = session.raw
        return when {
            !online -> "Offline"
            archived(session) -> "Archived"
            state(session) == "paused" -> "Paused"
            state(session) == "stopped" -> "Stopped"
            raw.string("process_state") == "exited" -> "Agent exited"
            !raw.optBoolean("tracked") && raw.string("activity").isBlank() -> "Not tracked"
            raw.string("phase") == "approval" -> "Needs approval"
            raw.string("phase") == "input" -> "Needs input"
            raw.optJSONObject("recovery")?.string("state") == "waiting" -> "Waiting to retry"
            raw.optJSONObject("recovery")?.string("state") in listOf("dispatching", "retrying") &&
                !RecoveryPresentation.resumed(raw.optJSONObject("recovery")) -> "Retrying"
            raw.string("phase") == "error" -> "Error"
            raw.string("phase") == "interrupted" -> "Interrupted"
            keepWarmTurn(session) -> "Warming cache"
            raw.string("phase") == "compacting" -> "Compacting"
            raw.string("phase") == "starting" -> "Starting"
            raw.string("phase") == "tool" -> "Using a tool"
            raw.string("activity") == "busy" -> "Working"
            backgroundIdle(session) -> "Background"
            waitingIdle(session) -> waitingLabel(session)
            scheduledIdle(session) -> nextScheduledRun(session).let { if (it.isEmpty()) "Scheduled" else "Scheduled · $it" }
            raw.string("activity") == "idle" -> "Ready"
            raw.string("activity") in listOf("ended", "unknown") -> "Status unknown"
            else -> "Connecting"
        }
    }
    fun scoped(sessions: List<Session>, machines: List<Machine>, selectedMachines: Set<MachineKey>, query: String,
        project: ProjectKey? = null): List<Session> = sessions.filter { session ->
        (project == null || session.projectKey == project) && (selectedMachines.isEmpty() || machineKey(session.target) in selectedMachines) &&
            (query.isBlank() || (listOf(session.target.session, session.target.conversation, session.target.run, session.target.archiveId,
                session.target.agentId, label(session), session.title, session.preview, session.agent, session.project) + childIdentifiers(session) +
                listOf("prompt", "current_tool", "git_branch", "git_worktree_name", "activity_summary", "activity_detail", "cwd").map { SessionPreview.render(session.raw.string(it)) } +
                machines.filter { machineKey(session.target) == MachineKey(it.connectionId, it.id) }.map { it.name }).joinToString(" ").contains(query.trim(), true))
    }
    private fun childIdentifiers(session: Session): List<String> {
        if (session.raw.string("subagent_source") == "hook_profiles") return emptyList()
        val roster = session.raw.optJSONObject("subagents")
        val previews = session.raw.optJSONArray("subagent_previews")?.objects().orEmpty()
        return roster?.keys()?.asSequence()?.flatMap { id -> sequenceOf(id, roster.optJSONObject(id)?.string("conversation_id").orEmpty()) }?.toList().orEmpty() +
            previews.flatMap { listOf(it.string("id", "agent_id"), it.string("conversation_id")) }
    }
    /**
     * The one rule a status tab's list, its count and the machine chooser share (zerus-sa9qn.19): the filter, and for
     * Archive the loaded date window [from, until) (desktop archiveListed).
     */
    fun listed(session: Session, filter: SessionFilter, online: Boolean, hasDraft: Boolean = false, archiveRange: ArchiveRange = ArchiveRange()) =
        matches(session, filter, online, hasDraft) &&
            (filter != SessionFilter.Archive || session.raw.optDouble("archived_at", 0.0).let { at -> at >= archiveRange.from && at < archiveRange.until })
    fun counts(sessions: List<Session>, machines: List<Machine>, drafts: Set<String> = emptySet(),
        archiveRange: ArchiveRange = ArchiveRange()): Map<SessionFilter, Int> {
        val online = machines.filter { it.online }.mapTo(hashSetOf()) { MachineKey(it.connectionId, it.id) }
        val counts = SessionFilter.entries.associateWith { 0 }.toMutableMap()
        sessions.forEach { row ->
            val isOnline = machineKey(row.target) in online
            val hasDraft = row.target.key in drafts
            SessionFilter.entries.forEach { filter -> if (listed(row, filter, isOnline, hasDraft, archiveRange)) counts[filter] = counts.getValue(filter) + 1 }
        }
        return counts
    }
    /** Chooser counts precede machine selection and text search, retaining project/state scope. */
    fun machineCounts(sessions: List<Session>, machines: List<Machine>, filter: SessionFilter,
        drafts: Set<String> = emptySet(), project: ProjectKey? = null, archiveRange: ArchiveRange = ArchiveRange()): Map<MachineKey, Int> {
        val result = machines.associate { MachineKey(it.connectionId, it.id) to 0 }.toMutableMap()
        val online = machines.filter { it.online }.mapTo(hashSetOf()) { MachineKey(it.connectionId, it.id) }
        for(session in sessions) if((project == null || session.projectKey == project) &&
            listed(session, filter, machineKey(session.target) in online, session.target.key in drafts, archiveRange)) {
            val key = machineKey(session.target)
            result[key] = result.getOrDefault(key, 0) + 1
        }
        return result
    }
    fun grouped(sessions: List<Session>, projects: List<ProjectSummary>): List<SessionGroup> {
        val grouped = sessions.groupBy { it.projectKey ?: ProjectKey(it.target.connectionId, "unassigned:${it.target.computerId}", "unassigned") }
        val catalog = projects.associateBy { it.key }
        val order = projects.map { it.key } + grouped.keys.filter { key -> key !in catalog }
        return order.mapNotNull { key -> grouped[key]?.takeIf { it.isNotEmpty() }?.let { members ->
            val project = catalog[key]
            SessionGroup(key, project?.name ?: members.first().project.ifBlank { "Unassigned sessions" }, project?.color.orEmpty(),
                if (members.all(::archived)) members.sortedByDescending { it.raw.optDouble("archived_at", 0.0) } else members)
        } }
    }
}
