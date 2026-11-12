package app.zerus.mobile

/** Immutable list projection. Build on Default; scrolling only composes visible cards. */
data class SessionListPresentation(
    val groups: List<SessionGroup> = emptyList(),
    val counts: Map<SessionFilter, Int> = SessionFilter.entries.associateWith { 0 },
    val machineCounts: Map<MachineKey, Int> = emptyMap(),
    val readAllTargets: List<Target> = emptyList(),
    val visibleCount: Int = 0,
) {
    companion object {
        fun prepare(sessions: List<Session>, machines: List<Machine>, projects: List<ProjectSummary>,
            selectedMachines: Set<MachineKey>, query: String, project: ProjectKey?, filter: SessionFilter,
            drafts: List<Draft>, archiveRange: ArchiveRange = ArchiveRange(), teams: TeamCollapse = TeamCollapse()): SessionListPresentation {
            val scoped = SessionFilters.scoped(sessions, machines, selectedMachines, query, project)
            val drafted = SessionFilters.draftedSessions(sessions, drafts)
            val online = machines.filter { it.online }.mapTo(hashSetOf()) { MachineKey(it.connectionId, it.id) }
            fun online(row: Session) = SessionFilters.machineKey(row.target) in online
            // The list, the tab counts and the machine chooser read one rule (SessionFilters.listed, zerus-sa9qn.19).
            val visible = scoped.filter { SessionFilters.listed(it, filter, online(it), it.target.key in drafted, archiveRange) }
            // Child sessions nest under a parent listed in the same project group (zerus-sa9qn.4).
            val uuids = machines.associate { MachineKey(it.connectionId, it.id) to it.machineId }
            fun uuid(row: Session) = uuids[SessionFilters.machineKey(row.target)]
            val parentKey = ChildSessions.parentIndex(sessions, ::uuid)
            val groups = SessionFilters.grouped(visible, projects).map { group ->
                group.copy(cards = ChildSessions.nest(group.sessions, parentKey, ::online) { teams.collapsed(it, uuid(it)) })
            }
            return SessionListPresentation(groups,
                SessionFilters.counts(scoped, machines, drafted, archiveRange),
                SessionFilters.machineCounts(sessions, machines, filter, drafted, project, archiveRange),
                if (filter == SessionFilter.Archive) emptyList() else scoped.filter { ReadAllPolicies.listCandidate(it, online(it)) }.map { it.target }, visible.size)
        }
    }
}
