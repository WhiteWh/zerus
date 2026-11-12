import Foundation

/// One workspace's computer inventory, prepared off the main actor before publication.
public struct SessionCatalog: Sendable {
    public var machines: [Machine]
    public var accounts: [AccountCatalog]
    public var projects: ProjectCatalog
    public var operations: [MachineKey: Set<String>]
    public var features: [MachineKey: Set<String>]
}

public enum SessionCatalogParser {
    public static func parse(_ connection: Connection, _ computers: [JSONObject], retained: [Session] = [], actions: [SessionAction] = []) -> SessionCatalog {
        var operations: [MachineKey: Set<String>] = [:]
        var features: [MachineKey: Set<String>] = [:]
        var machines: [Machine] = []
        var valid: [JSONObject] = []
        for raw in computers {
            guard let machine = try? MachineCatalog.parse(connection.id, raw) else { continue }
            let capabilities = raw.object("snapshot")?.object("mobile_capabilities")
            operations[machine.key] = capabilities?.int("protocol_version") == 1 ? Set(capabilities?.strings("operations") ?? []) : []
            features[machine.key] = Set(capabilities?.strings("features") ?? [])
            machines.append(machine)
            valid.append(raw)
        }
        let known = SessionNotificationPreferences.known(retained,actions:actions)
        let sessions = valid.flatMap { NativeSessions.sessions(connection, $0) }.map { row in
            var copy = row;copy.raw = SessionNotificationPreferences.merge(row.raw,row.target,known);return copy
        }
        return SessionCatalog(machines: machines, accounts: valid.map { AccountSnapshots.parse(connection.id, $0) },
                              projects: ProjectParser.parse(connection, valid, sessions), operations: operations, features: features)
    }
}

/// Live catalog rows plus bounded archive pages; archives of computers that page
/// their archive come only from the pages. Each target appears once.
public enum SessionCatalogProjection {
    public static func prepare(catalog: [Session], archives: [Session], native: [MachineKey: Set<String>],
                               relay: [String: Set<String>]) -> [Session] {
        var seen = Set<String>()
        var result: [Session] = []
        result.reserveCapacity(catalog.count + archives.count)
        for row in catalog {
            if SessionFilters.archived(row), native[row.target.machineKey]?.contains("archive_list") == true,
               relay[row.target.connectionId]?.contains("archive_list") == true { continue }
            if seen.insert(row.target.key).inserted { result.append(row) }
        }
        for row in archives where seen.insert(row.target.key).inserted { result.append(row) }
        return result
    }
}

/// Immutable list projection. Built off the main actor; scrolling only renders visible cards.
public struct SessionListPresentation: Sendable {
    public var groups: [SessionGroup] = []
    public var counts: [SessionFilter: Int] = Dictionary(uniqueKeysWithValues: SessionFilter.allCases.map { ($0, 0) })
    public var machineCounts: [MachineKey: Int] = [:]
    public var readAllTargets: [Target] = []
    public var visibleCount = 0

    public init() {}

    public static func prepare(sessions: [Session], machines: [Machine], projects: [ProjectSummary], selectedMachines: Set<MachineKey>,
                               query: String, project: ProjectKey?, filter: SessionFilter, drafts: [Draft],
                               archiveRange: ArchiveRange = ArchiveRange(),
                               readAllCandidate: (Session, Bool) -> Bool = { _, _ in false },
                               teams: TeamCollapse = TeamCollapse()) -> SessionListPresentation {
        let scoped = SessionFilters.scoped(sessions, machines, selectedMachines, query, project: project)
        let drafted = SessionFilters.draftedSessions(sessions, drafts)
        let online = Set(machines.filter(\.online).map(\.key))
        // The list, the tab counts and the machine chooser read one rule (SessionFilters.listed, zerus-sa9qn.19).
        let visible = scoped.filter { row in
            SessionFilters.listed(row, filter, online.contains(row.target.machineKey), hasDraft: drafted.contains(row.target.key), archiveRange: archiveRange)
        }
        var result = SessionListPresentation()
        // Child sessions nest under a parent listed in the same project group (zerus-sa9qn.5).
        let uuids = Dictionary(machines.map { ($0.key, $0.machineId) }) { first, _ in first }
        let uuid = { (row: Session) in uuids[row.target.machineKey] }
        let parentKey = ChildSessions.parentIndex(sessions, machine: uuid)
        result.groups = SessionFilters.grouped(visible, projects).map { group in
            SessionGroup(key: group.key, name: group.name, color: group.color, sessions: group.sessions,
                         cards: ChildSessions.nest(group.sessions, parentKey: parentKey, online: { online.contains($0.target.machineKey) },
                                                   collapsed: { teams.collapsed($0, machine: uuid($0)) }))
        }
        result.counts = SessionFilters.counts(scoped, machines, drafts: drafted, archiveRange: archiveRange)
        result.machineCounts = SessionFilters.machineCounts(sessions, machines, filter, drafts: drafted, project: project, archiveRange: archiveRange)
        result.readAllTargets = filter == .archive ? [] : scoped.filter { readAllCandidate($0, online.contains($0.target.machineKey)) }.map(\.target)
        result.visibleCount = visible.count
        return result
    }
}
