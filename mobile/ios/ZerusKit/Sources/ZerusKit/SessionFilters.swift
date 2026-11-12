import Foundation

/// Desktop session filters, with the shared IDs of session-filter-contract.json.
public enum SessionFilter: String, CaseIterable, Sendable, Codable {
    case all, attention, active, saved = "paused", archive = "archived"

    public var id: String { rawValue }
    public var caption: String {
        switch self {
        case .all: "All sessions"
        case .attention: "Needs attention"
        case .active: "Active"
        case .saved: "Saved sessions"
        case .archive: "Archive"
        }
    }
}

public struct SessionGroup: Hashable, Sendable, Identifiable {
    public let key: ProjectKey
    public let name: String
    public let color: String
    public let sessions: [Session]
    /// The group's cards, child sessions nested under their parents (ChildSessions.nest); a group
    /// built without nesting lists each session.
    public var cards: [SessionCardEntry]
    public var id: String { key.key }

    public init(key: ProjectKey, name: String, color: String, sessions: [Session], cards: [SessionCardEntry]? = nil) {
        self.key = key
        self.name = name
        self.color = color
        self.sessions = sessions
        self.cards = cards ?? sessions.map { SessionCardEntry(session: $0) }
    }
}

/// Native session semantics ported from Android `SessionFilters` (desktop SessionPresentation.h).
public enum SessionFilters {
    public static func online(_ session: Session, _ machines: [Machine]) -> Bool {
        machines.contains { $0.connectionId == session.target.connectionId && $0.id == session.target.computerId && $0.online }
    }

    public static func archived(_ session: Session) -> Bool { !session.target.archiveId.isEmpty || session.raw.string("state") == "archived" }

    public static func state(_ session: Session) -> String {
        let value = session.raw.string("state")
        return value.isEmpty ? (archived(session) ? "archived" : "running") : value
    }

    public static func currentActivity(_ session: Session, _ online: Bool) -> Bool {
        let raw = session.raw
        return online && state(session) == "running" && raw.string("process_state") != "exited" && raw.string("activity") != "unknown" &&
            !(raw.string("conversation_state") == "ended" && raw.string("activity") != "idle")
    }

    public static func needsAction(_ session: Session) -> Bool {
        let raw = session.raw
        let recovery = raw.object("recovery")?.string("state") ?? ""
        let providerError = raw.object("provider_error")?.string("error_kind") ?? ""
        let phase = raw.string("phase")
        return !archived(session) && state(session) == "running" && raw.string("process_state") != "exited" &&
            (["approval", "input"].contains(phase) ||
                phase == "error" && (!["waiting", "dispatching", "retrying"].contains(recovery) || ["quota", "provider_policy"].contains(providerError)) ||
                ["uncertain", "blocked", "exhausted"].contains(recovery))
    }

    /// A keep-warm ping is running: the agent works, but not on the person's turn (desktop core
    /// `keep_warm_turn`; Android `KeepWarmPresentation.turnActive`, #1716).
    public static func keepWarmTurn(_ session: Session) -> Bool { session.raw.object("keep_warm")?["turn_active"] == .bool(true) }

    public static func working(_ session: Session, _ online: Bool) -> Bool {
        !archived(session) && currentActivity(session, online) && (session.raw.string("activity") == "busy" || backgroundIdle(session)) &&
            !needsAction(session) && !keepWarmTurn(session)
    }

    static func positiveCount(_ value: JSON?) -> Bool {
        guard let number = value?.numberValue else { return false }
        return number.isFinite && number > 0 && number.truncatingRemainder(dividingBy: 1) == 0
    }

    /// `subagent_active_count` when it is a number, else `subagent_count`.
    static func activeChildren(_ raw: JSONObject) -> JSON? {
        if let value = raw["subagent_active_count"], value.numberValue != nil { return value }
        return raw["subagent_count"]
    }

    public static func hasCoordinationWait(_ session: Session) -> Bool {
        guard let wait = session.raw.object("coordination_wait") else { return false }
        return wait.int("schema") == 1 && wait.string("status") == "ok" && !session.target.run.isEmpty && !session.target.conversation.isEmpty &&
            wait.string("run_id") == session.target.run && wait.string("conversation_id") == session.target.conversation &&
            positiveCount(wait["count"]) && wait.double("count") <= 1024
    }

    public static func waitingIdle(_ session: Session) -> Bool {
        let raw = session.raw
        return raw.string("activity") == "idle" && raw.string("phase") == "idle" && state(session) == "running" &&
            raw.string("process_state") != "exited" && !backgroundIdle(session) && !positiveCount(activeChildren(raw)) && hasCoordinationWait(session)
    }

    public static func waitingLabel(_ session: Session, includeProducer: Bool = true, compact: Bool = false, now: Double = Clock.seconds()) -> String {
        guard let wait = session.raw.object("coordination_wait") else { return "Waiting for agent" }
        let item = wait.array("items")?.first?.object
        let producer = item?.object("producer")
        let label = compact ? "Waiting" : (item?.string("state") == "awaiting_result" ? "Waiting for result" : "Waiting for acceptance")
        let name = (producer?.string("name") ?? "").split(separator: "/", omittingEmptySubsequences: false).dropFirst(2).joined(separator: "/")
        let host = producer?.string("host") ?? ""
        var elapsed = ""
        if let created = item?.double("created_at", default: 0), created.isFinite, created > 0 {
            let seconds = max(0, Int64(now - created))
            elapsed = seconds < 60 ? "\(seconds)s" : seconds < 3600 ? "\(seconds / 60)m" : "\(seconds / 3600)h \(seconds / 60 % 60)m"
        }
        var text = label
        if includeProducer && !name.isEmpty { text += " from \(name)" }
        if includeProducer && !host.isEmpty { text += " on \(host)" }
        if wait.int("count") > 1 { text += " (+\(wait.int("count") - 1))" }
        if !elapsed.isEmpty { text += " (\(elapsed))" }
        return text
    }

    public static func replyInProgress(_ session: Session, _ online: Bool) -> Bool {
        let raw = session.raw
        return currentActivity(session, online) && !keepWarmTurn(session) && (raw.string("activity") == "busy" || positiveCount(raw.object("process_work")?["count"]) ||
            positiveCount(activeChildren(raw)) || raw.string("activity") == "idle" && raw.string("phase") == "idle" && hasCoordinationWait(session))
    }

    /// Keep the read identity while postponing reply badges and reply-only attention.
    public static func visibleUnreadReply(_ session: Session, _ online: Bool) -> Bool {
        !archived(session) && !replyInProgress(session, online) && (session.raw.bool("unread_reply") || session.raw.bool("mobile_unread_reply"))
    }

    public static func backgroundIdle(_ session: Session) -> Bool {
        guard let count = session.raw.object("process_work")?["count"]?.numberValue else { return false }
        return count.isFinite && count > 0 && count.truncatingRemainder(dividingBy: 1) == 0 &&
            session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle"
    }

    /// Jobs the agent scheduled for itself start its next turns: neither Ready nor Working.
    public static func scheduledIdle(_ session: Session) -> Bool {
        let jobs = session.raw.object("schedules")?["count"]?.numberValue ?? 0
        return session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle" && Int(jobs) > 0
    }

    public static func scheduled(_ session: Session, _ online: Bool) -> Bool {
        !archived(session) && currentActivity(session, online) && !needsAction(session) && !backgroundIdle(session) &&
            !waitingIdle(session) && scheduledIdle(session)
    }

    /// The next run as a time of day, "due" once it has passed, or empty if unknown.
    public static func nextScheduledRun(_ session: Session, now: Date = Date(), locale: Locale = .current, timeZone: TimeZone = .current) -> String {
        guard let seconds = session.raw.object("schedules")?["next_at"]?.numberValue, seconds.isFinite, seconds > 0 else { return "" }
        let next = Date(timeIntervalSince1970: seconds)
        if next <= now { return "due" }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.dateFormat = calendar.isDate(next, inSameDayAs: now) ? "HH:mm" : "d MMM HH:mm"
        return formatter.string(from: next)
    }

    static func children(_ raw: JSONObject) -> [JSONObject] {
        let roster = raw.object("subagents").map { roster in roster.keys.compactMap { roster.object($0) } } ?? []
        return roster + raw.objects("subagent_previews")
    }

    public static func needsAttention(_ session: Session, _ online: Bool) -> Bool {
        if archived(session) { return false }
        let raw = session.raw
        if raw.bool("review_later") || raw.bool("mobile_review_later") { return true }
        if !online { return false }
        let acknowledged = raw.bool("attention_acknowledged")
        let childAttention = currentActivity(session, online) && raw.string("subagent_source") != "hook_profiles" &&
            children(raw).contains { ["approval", "input", "attention"].contains($0.string("display_state", "state")) }
        return !acknowledged && (needsAction(session) || childAttention) || visibleUnreadReply(session, online) ||
            currentActivity(session, online) && session.pendingCount > 0
    }

    /// Composer drafts remain local and count even when their machine is offline.
    public static func draftedSessions(_ sessions: [Session], _ drafts: [Draft]) -> Set<String> {
        let unsent = drafts.filter {
            $0.questionId.isEmpty && $0.detachedId.isEmpty && $0.target.archiveId.isEmpty &&
                !["submitting", "submitted", "recorded"].contains($0.status) && $0.hasContent
        }
        if unsent.isEmpty { return [] }
        let parents = Set(unsent.filter { $0.target.agentId.isEmpty }.map { "\($0.target.connectionId)\u{0}\($0.target.computerId)\u{0}\($0.target.session)" })
        var children: [Target: [Draft]] = [:]
        for draft in unsent where !draft.target.agentId.isEmpty {
            children[draft.target.with(conversation: draft.target.parentConversation, agentId: "", parentConversation: ""), default: []].append(draft)
        }
        var result = Set<String>()
        for session in sessions where !archived(session) {
            let parent = session.target.agentId.isEmpty &&
                parents.contains("\(session.target.connectionId)\u{0}\(session.target.computerId)\u{0}\(session.target.session)")
            let child = session.raw.string("subagent_source") != "hook_profiles" && (children[session.target] ?? []).contains { draft in
                // `optBoolean("group") == false`: a present child that is not a profile group.
                session.raw.object("subagents")?.object(draft.target.agentId).map { !$0.bool("group") } == true
            }
            if parent || child { result.insert(session.target.key) }
        }
        return result
    }

    /// Needs attention as the status filters list and count it (desktop filterAttention, zerus-sa9qn.18): a quiet
    /// supervised child's unread reply, retry or provider limit waits for its parent, so only the person's own Review
    /// later mark lists it. Every other row keeps needsAttention: a child with a request only the person can answer is
    /// never quiet, and a muted session stays listed.
    public static func filterAttention(_ session: Session, _ online: Bool) -> Bool {
        guard ChildSessions.quiet(session.raw) else { return needsAttention(session, online) }
        return !archived(session) && (session.raw.bool("review_later") || session.raw.bool("mobile_review_later"))
    }

    public static func active(_ session: Session, _ online: Bool, hasDraft: Bool = false) -> Bool {
        !archived(session) && (hasDraft || filterAttention(session, online) || working(session, online) || scheduled(session, online) ||
            replyInProgress(session, online) && session.raw.string("activity") == "idle" && session.raw.string("phase") == "idle")
    }

    public static func matches(_ session: Session, _ filter: SessionFilter, _ online: Bool, hasDraft: Bool = false) -> Bool {
        switch filter {
        case .all: !archived(session)
        case .attention: filterAttention(session, online)
        case .active: active(session, online, hasDraft: hasDraft)
        case .saved: !archived(session) && state(session) != "running"
        case .archive: archived(session)
        }
    }

    public static func label(_ session: Session) -> String {
        let tag = session.raw.string("tag")
        if !tag.isEmpty { return tag }
        let rest = session.target.session.split(separator: "/", omittingEmptySubsequences: false).dropFirst(2).joined(separator: "/")
        if !rest.isEmpty { return rest }
        let project = session.raw.string("project")
        return project.isEmpty ? session.target.session : project
    }

    public static func archiveDate(_ session: Session, locale: Locale = .current, timeZone: TimeZone = .current) -> String {
        guard archived(session) else { return "" }
        let seconds = session.raw.double("archived_at")
        guard seconds.isFinite, seconds > 0, seconds <= 253_402_300_799 else { return "" }
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.dateStyle = .short
        formatter.timeStyle = .medium
        return formatter.string(from: Date(timeIntervalSince1970: seconds))
    }

    public static func status(_ session: Session, _ online: Bool) -> String {
        let raw = session.raw
        let recovery = raw.object("recovery")?.string("state")
        if !online { return "Offline" }
        if archived(session) { return "Archived" }
        if state(session) == "paused" { return "Paused" }
        if state(session) == "stopped" { return "Stopped" }
        if raw.string("process_state") == "exited" { return "Agent exited" }
        if !raw.bool("tracked") && raw.string("activity").isEmpty { return "Not tracked" }
        switch raw.string("phase") {
        case "approval": return "Needs approval"
        case "input": return "Needs input"
        default: break
        }
        if recovery == "waiting" { return "Waiting to retry" }
        if recovery == "dispatching" || (recovery == "retrying" && !RecoveryPresentation.resumed(raw.object("recovery"))) { return "Retrying" }
        switch raw.string("phase") {
        case "error": return "Error"
        case "interrupted": return "Interrupted"
        default: break
        }
        if keepWarmTurn(session) { return "Warming cache" }
        switch raw.string("phase") {
        case "compacting": return "Compacting"
        case "starting": return "Starting"
        case "tool": return "Using a tool"
        default: break
        }
        if raw.string("activity") == "busy" { return "Working" }
        if backgroundIdle(session) { return "Background" }
        if waitingIdle(session) { return waitingLabel(session) }
        if scheduledIdle(session) {
            let next = nextScheduledRun(session)
            return next.isEmpty ? "Scheduled" : "Scheduled · \(next)"
        }
        if raw.string("activity") == "idle" { return "Ready" }
        if ["ended", "unknown"].contains(raw.string("activity")) { return "Status unknown" }
        return "Connecting"
    }

    public static func scoped(_ sessions: [Session], _ machines: [Machine], _ selectedMachines: Set<MachineKey>, _ query: String,
                              project: ProjectKey? = nil) -> [Session] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        var names: [MachineKey: [String]] = [:]
        if !needle.isEmpty { for machine in machines { names[machine.key, default: []].append(machine.name) } }
        return sessions.filter { session in
            guard project == nil || session.projectKey == project,
                  selectedMachines.isEmpty || selectedMachines.contains(session.target.machineKey) else { return false }
            if needle.isEmpty { return true }
            var fields = [session.target.session, session.target.conversation, session.target.run, session.target.archiveId,
                          session.target.agentId, label(session), session.title, session.preview, session.agent, session.project]
            fields += childIdentifiers(session)
            fields += ["prompt", "current_tool", "git_branch", "git_worktree_name", "activity_summary", "activity_detail", "cwd"]
                .map { SessionPreview.render(session.raw.string($0)) }
            fields += names[session.target.machineKey] ?? []
            return fields.joined(separator: " ").range(of: needle, options: .caseInsensitive) != nil
        }
    }

    static func childIdentifiers(_ session: Session) -> [String] {
        if session.raw.string("subagent_source") == "hook_profiles" { return [] }
        var result: [String] = []
        if let roster = session.raw.object("subagents") {
            for id in roster.keys { result += [id, roster.object(id)?.string("conversation_id") ?? ""] }
        }
        for preview in session.raw.objects("subagent_previews") { result += [preview.string("id", "agent_id"), preview.string("conversation_id")] }
        return result
    }

    /// The one rule a status tab's list, its count and the machine chooser share (zerus-sa9qn.19): the filter, and for
    /// Archive the loaded date window [from, until) (desktop archiveListed).
    public static func listed(_ session: Session, _ filter: SessionFilter, _ online: Bool, hasDraft: Bool = false,
                              archiveRange: ArchiveRange = ArchiveRange()) -> Bool {
        guard matches(session, filter, online, hasDraft: hasDraft) else { return false }
        guard filter == .archive else { return true }
        let at = session.raw.double("archived_at", default: 0)
        return at >= Double(archiveRange.from) && at < Double(archiveRange.until)
    }

    public static func counts(_ sessions: [Session], _ machines: [Machine], drafts: Set<String> = [],
                              archiveRange: ArchiveRange = ArchiveRange()) -> [SessionFilter: Int] {
        let online = Set(machines.filter(\.online).map(\.key))
        var counts = Dictionary(uniqueKeysWithValues: SessionFilter.allCases.map { ($0, 0) })
        for row in sessions {
            let isOnline = online.contains(row.target.machineKey)
            let hasDraft = drafts.contains(row.target.key)
            for filter in SessionFilter.allCases where listed(row, filter, isOnline, hasDraft: hasDraft, archiveRange: archiveRange) { counts[filter]! += 1 }
        }
        return counts
    }

    /// Chooser counts precede machine selection and text search, retaining project/state scope.
    public static func machineCounts(_ sessions: [Session], _ machines: [Machine], _ filter: SessionFilter,
                                     drafts: Set<String> = [], project: ProjectKey? = nil,
                                     archiveRange: ArchiveRange = ArchiveRange()) -> [MachineKey: Int] {
        var result: [MachineKey: Int] = [:]
        for machine in machines { result[machine.key] = 0 }
        let online = Set(machines.filter(\.online).map(\.key))
        for session in sessions where (project == nil || session.projectKey == project) &&
            listed(session, filter, online.contains(session.target.machineKey), hasDraft: drafts.contains(session.target.key), archiveRange: archiveRange) {
            result[session.target.machineKey, default: 0] += 1
        }
        return result
    }

    public static func grouped(_ sessions: [Session], _ projects: [ProjectSummary]) -> [SessionGroup] {
        var grouped: [ProjectKey: [Session]] = [:]
        var firstSeen: [ProjectKey] = []
        for session in sessions {
            let key = session.projectKey ?? ProjectKey(connectionId: session.target.connectionId, swarmId: "unassigned:\(session.target.computerId)", id: "unassigned")
            if grouped[key] == nil { firstSeen.append(key) }
            grouped[key, default: []].append(session)
        }
        var catalog: [ProjectKey: ProjectSummary] = [:]
        for project in projects where catalog[project.key] == nil { catalog[project.key] = project }
        let order = projects.map(\.key) + firstSeen.filter { catalog[$0] == nil }
        var emitted = Set<ProjectKey>()
        return order.compactMap { key in
            guard emitted.insert(key).inserted, let members = grouped[key], !members.isEmpty else { return nil }
            let project = catalog[key]
            let fallback = members.first!.project.isEmpty ? "Unassigned sessions" : members.first!.project
            let sorted = members.allSatisfy(archived)
                ? members.enumerated().sorted { lhs, rhs in
                    let a = lhs.element.raw.double("archived_at", default: 0), b = rhs.element.raw.double("archived_at", default: 0)
                    return a != b ? a > b : lhs.offset < rhs.offset
                }.map(\.element)
                : members
            return SessionGroup(key: key, name: project?.name ?? fallback, color: project?.color ?? "", sessions: sorted)
        }
    }
}

public enum DesktopBadgeKind: Sendable, Hashable { case neutral, working, attention, error, paused, unread, scheduled, background, waiting }

public struct DesktopBadgeColors: Sendable, Hashable {
    public let background: UInt32
    public let foreground: UInt32
}

public struct DesktopSessionBadge: Sendable, Hashable {
    public let caption: String
    public let kind: DesktopBadgeKind
    public let additionalUnread: Bool
}

/// Native predicates and precedence from SessionCardDelegate.h; colors from SessionStatusBadge.h.
public enum DesktopSessionPalette {
    static func attention(_ session: Session, _ online: Bool) -> Bool {
        let raw = session.raw
        return raw.bool("review_later") || raw.bool("mobile_review_later") ||
            online && !raw.bool("attention_acknowledged") && (SessionFilters.needsAction(session) || childAttention(session, online))
    }

    /// The badge kind before a mute or a supervising parent grays it.
    public static func nativeKind(_ session: Session, _ online: Bool) -> DesktopBadgeKind {
        let working = SessionFilters.working(session, online)
        if attention(session, online) { return detailKind(session, online) == .error ? .error : .attention }
        if working { return SessionFilters.backgroundIdle(session) ? .background : .working }
        if SessionFilters.visibleUnreadReply(session, online) { return .unread }
        if online && SessionFilters.waitingIdle(session) { return .waiting }
        if SessionFilters.scheduled(session, online) { return .scheduled }
        if online && SessionFilters.state(session) == "paused" && !SessionFilters.archived(session) { return .paused }
        return .neutral
    }

    public static func badge(_ session: Session, _ online: Bool) -> DesktopSessionBadge {
        let raw = session.raw
        let state = SessionFilters.status(session, online)
        let attention = Self.attention(session, online)
        let working = SessionFilters.working(session, online)
        let unread = SessionFilters.visibleUnreadReply(session, online)
        let kind = nativeKind(session, online)
        // Muted, or quiet under a live parent (spec 2.1): requests and replies gray instead of amber.
        // A quiet child's error is its parent's to handle, gray too; an explicitly muted error stays
        // red (desktop badgeKind, zerus-nlck0).
        let muted = SessionNotificationPreferences.muted(session), quiet = ChildSessions.quiet(raw)
        let displayKind: DesktopBadgeKind = (muted || quiet) && [.attention,.unread].contains(kind) || quiet && !muted && kind == .error
            ? .neutral : kind
        let caption: String
        if working { caption = raw.string("phase") == "compacting" ? "Compacting" : (SessionFilters.backgroundIdle(session) ? "Background" : "Working") }
        else if kind == .waiting { caption = SessionFilters.waitingLabel(session, includeProducer: false, compact: true) }
        else if unread && !attention { caption = "New reply" }
        else if online && !SessionFilters.archived(session) && !["paused", "stopped"].contains(SessionFilters.state(session)) &&
            raw.string("process_state") != "exited" && !raw.bool("tracked") && raw.string("activity").isEmpty { caption = "Untracked" }
        else { caption = state }
        return DesktopSessionBadge(caption: caption, kind: displayKind, additionalUnread: unread && (working || attention))
    }

    static func childAttention(_ session: Session, _ online: Bool) -> Bool {
        guard SessionFilters.currentActivity(session, online), session.raw.string("subagent_source") != "hook_profiles" else { return false }
        return SessionFilters.children(session.raw).contains { ["approval", "input", "attention"].contains($0.string("display_state", "state")) }
    }

    public static func colors(_ kind: DesktopBadgeKind, dark: Bool) -> DesktopBadgeColors {
        switch kind {
        case .working: dark ? .init(background: 0x194D36, foreground: 0x97F0BA) : .init(background: 0xCCEFDC, foreground: 0x145B37)
        case .background: dark ? .init(background: 0x273544, foreground: 0xAAC0DF) : .init(background: 0xE9F0F8, foreground: 0x466B99)
        case .waiting: dark ? .init(background: 0x443A25, foreground: 0xEDD096) : .init(background: 0xF5E8C7, foreground: 0x795913)
        case .attention, .unread: .init(background: dark ? 0xFFDA76 : 0xF4CE65, foreground: 0x392900)
        case .error: dark ? .init(background: 0xFFABB6, foreground: 0x46202A) : .init(background: 0xB73750, foreground: 0xFFFFFF)
        case .paused: dark ? .init(background: 0x443654, foreground: 0xDEC3FF) : .init(background: 0xE8DDF5, foreground: 0x634187)
        case .scheduled: dark ? .init(background: 0x173E48, foreground: 0x9FE0F0) : .init(background: 0xD3EDF4, foreground: 0x165A6B)
        case .neutral: dark ? .init(background: 0x2B343E, foreground: 0xB6C2D0) : .init(background: 0xE7EDF2, foreground: 0x566575)
        }
    }

    enum DetailKind { case neutral, working, ready, attention, error, paused, background, waiting }

    // This follows SessionPresentation::status ordering, rather than interpreting translated labels.
    static func detailKind(_ session: Session, _ online: Bool) -> DetailKind {
        let raw = session.raw
        let phase = raw.string("phase")
        if !online || SessionFilters.archived(session) { return .neutral }
        if SessionFilters.state(session) == "paused" { return .paused }
        if SessionFilters.state(session) == "stopped" || raw.string("process_state") == "exited" { return .neutral }
        if !raw.bool("tracked") && raw.string("activity").isEmpty { return .neutral }
        if ["approval", "input"].contains(phase) { return .attention }
        if ["waiting", "dispatching", "retrying"].contains(raw.object("recovery")?.string("state") ?? ""),
           !RecoveryPresentation.resumed(raw.object("recovery")) { return .neutral }
        if phase == "error" { return .error }
        if ["interrupted", "starting"].contains(phase) { return .neutral }
        if ["compacting", "tool"].contains(phase) || raw.string("activity") == "busy" { return .working }
        if SessionFilters.backgroundIdle(session) { return .background }
        if SessionFilters.waitingIdle(session) { return .waiting }
        if raw.string("activity") == "idle" { return .ready }
        return .neutral
    }

    /// SessionsWindow.cpp tone() and its detail badge background.
    public static func detailColors(_ session: Session, _ online: Bool, dark: Bool) -> DesktopBadgeColors {
        let foreground: UInt32 = switch detailKind(session, online) {
        case .working: dark ? 0x55E39A : 0x12834E
        case .background: dark ? 0xAAC0DF : 0x466B99
        case .waiting: dark ? 0xEDD096 : 0x795913
        case .ready: dark ? 0x78DABB : 0x11775D
        case .attention: dark ? 0xF0C77B : 0x885400
        case .error: dark ? 0xFF9298 : 0xB52B3D
        case .paused: dark ? 0xB9A5ED : 0x7051A4
        case .neutral: dark ? 0x9AA4B2 : 0x646F7E
        }
        return DesktopBadgeColors(background: dark ? 0x2B323D : 0xEDF1F5, foreground: foreground)
    }
}
