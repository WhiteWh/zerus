import Foundation
import Observation
import ZerusKit

/// One inspected conversation snapshot and, once prepared, its merged presentation.
/// Identity matters: a presentation applies only to the frame it was prepared for.
/// Frames are built off the main actor; `presentation` is written only on the main actor.
final class ConversationFrame: @unchecked Sendable {
    struct Presentation {
        let messages: [OutgoingMessage]
        let events: [Event]
        let questions: [Question]
    }

    let target: Target
    let raw: JSONObject
    let events: [Event]
    let questions: [Question]
    /// Milliseconds since 1970 of the inspection; 0 for a frame restored from the sealed cache.
    let observedAt: Int64
    let bytes: Int
    let truncated: Bool
    var presentation: Presentation?
    let window: HistoryWindow?
    let indexComplete: Bool
    let indexing: Bool
    let unreadBoundary: String?
    let inspectedEvents: [Event]

    init(target: Target, raw: JSONObject, events: [Event], questions: [Question], observedAt: Int64, bytes: Int, truncated: Bool,
         window: HistoryWindow? = nil, indexComplete: Bool = false, indexing: Bool = false, unreadBoundary: String? = nil, inspectedEvents: [Event] = []) {
        self.target = target
        self.raw = raw
        self.events = events
        self.questions = questions
        self.observedAt = observedAt
        self.bytes = bytes
        self.truncated = truncated
        self.window = window
        self.indexComplete = indexComplete
        self.indexing = indexing
        self.unreadBoundary = unreadBoundary
        self.inspectedEvents = inspectedEvents
    }

    /// A new frame with replaced history; its presentation must be prepared again.
    func with(events: [Event], truncated: Bool, window: HistoryWindow?, indexComplete: Bool, indexing: Bool, unreadBoundary: String? = nil) -> ConversationFrame {
        ConversationFrame(target: target, raw: raw, events: events, questions: questions, observedAt: observedAt, bytes: bytes, truncated: truncated,
                          window: window, indexComplete: indexComplete, indexing: indexing, unreadBoundary: unreadBoundary, inspectedEvents: inspectedEvents)
    }
    func with(raw: JSONObject) -> ConversationFrame {
        let copy = ConversationFrame(target:target,raw:raw,events:events,questions:questions,observedAt:observedAt,bytes:bytes,truncated:truncated,
                                     window:window,indexComplete:indexComplete,indexing:indexing,unreadBoundary:unreadBoundary,inspectedEvents:inspectedEvents)
        copy.presentation = presentation;return copy
    }
}

/// A small access-ordered map for retained frames and history windows.
struct RecentMap<Value> {
    private(set) var values: [String: Value] = [:]
    private(set) var order: [String] = []

    subscript(key: String) -> Value? {
        get { values[key] }
        set {
            order.removeAll { $0 == key }
            if let newValue {
                values[key] = newValue
                order.append(key)
            } else {
                values[key] = nil
            }
        }
    }

    mutating func touch(_ key: String) -> Value? {
        guard let value = values[key] else { return nil }
        order.removeAll { $0 == key }
        order.append(key)
        return value
    }

    mutating func trim(while condition: (RecentMap<Value>) -> Bool) {
        while !order.isEmpty && condition(self) { values[order.removeFirst()] = nil }
    }

    mutating func removeAll(where predicate: (Value) -> Bool) {
        for (key, value) in values where predicate(value) { self[key] = nil }
    }

    var count: Int { values.count }
}

/// The open conversation, its history window, read tracking and deliveries,
/// ported from the conversation sections of Android `ZerusViewModel`.
@MainActor
@Observable
final class ConversationModel {
    @ObservationIgnored unowned let app: AppModel
    @ObservationIgnored let cache: ConversationCache?
    @ObservationIgnored let pages: ConversationPageCache?

    // MARK: Selection

    var selected: Session?
    var activity: JSONObject?
    var activityVerified = false
    var historyCached = false
    var historyTruncated = false
    var events: [Event] = [] { didSet { app.retainOutbox() } }
    var questions: [Question] = []
    var outgoing: [OutgoingMessage] = []
    var detailBusy = false
    var detailProgress = false
    var contextNotice = ""
    /// Bumped when a frame or window publishes, so views can rescroll without diffing rows.
    var revision = 0

    // MARK: History

    var olderLoading = false
    var olderError = ""
    var lastHistoryWasLatest = false
    var hasOlder = false
    var hasNewer = false
    var historyIndexing = false
    var historyEpoch = ""
    var historyComplete = false

    // MARK: Reading

    var readState = ConversationReadState()
    var readTrackingReady = false
    var readPositionReady = false
    var firstUnreadEventId = ""
    var currentHistoryHeads: Set<String> = []

    // MARK: Delivery

    var receiptFlights: Set<String> = [] { didSet { app.retainOutbox() } }
    @ObservationIgnored var outgoingReceiptProbe = OutgoingReceiptProbe()
    var answerFlights: [String: String] = [:]
    var preparingTargets: Set<String> = []
    var attachmentPreparing = false
    var attachmentImporting = false
    @ObservationIgnored var importTask: Task<Void, Never>?

    // MARK: Phone delivery timing

    var mobileDeliveryMessage = ""
    var mobileDeliveryRequest = ""

    // MARK: Terminal

    var terminalState: JSONObject?
    var terminalLoading = false
    var terminalError = ""
    var terminalVisible = false
    @ObservationIgnored var terminalVerified = false
    @ObservationIgnored var terminalReceivedAt: TimeInterval = 0
    @ObservationIgnored var terminalTask: Task<Void, Never>?
    @ObservationIgnored var emptyTerminalBuffers: [String: TerminalBuffer] = [:]
    var deliveryFlights: Set<String> = [] { didSet { app.retainOutbox() } }
    /// Outbox rows, with interrupted sends shown as unconfirmed.
    var normalizedOutgoing: [OutgoingMessage] = []
    var pendingQuestionSend: QuestionSendIntent?
    var questionSendRefusal: QuestionSendRefusal?
    var lastOwnSend: (target: Target, id: String)?

    // MARK: Session controls

    var actionFlights: Set<String> = []
    var contextFlights: Set<String> = []
    var continuation: CompactContinuation?
    var actionResultNotice = ""
    /// Bumped when a confirmed action removed the open session, so the screen returns to Sessions.
    var returnToSessions = 0
    var sessionMenuRequest: (target: Target, operation: String, navigation: Int64)?
    var processOutputId = ""
    var processOutputResult: JSONObject?
    var processOutputError = ""
    var processOutputLoading = false

    // MARK: Ignored bookkeeping

    @ObservationIgnored var clearExpectation: (requestId: String, target: Target, navigation: Int64, confirmed: Bool)?
    @ObservationIgnored var continuationEnqueuing = ""
    @ObservationIgnored var acknowledgedContexts: Set<String> = []
    @ObservationIgnored var contextObservations: Set<String> = []
    @ObservationIgnored var contextNoticeOwner: String?
    @ObservationIgnored var pendingSettingsAttempts: Set<String> = []
    @ObservationIgnored let scheduledReadScheduler = HistoryHeadScheduler()
    @ObservationIgnored var actionResultTask: Task<Void, Never>?
    @ObservationIgnored var resolvingActionResults: Set<String> = []
    @ObservationIgnored var processOutputTask: Task<Void, Never>?

    @ObservationIgnored var navigationId: Int64 = 0
    @ObservationIgnored var frames = RecentMap<ConversationFrame>()
    @ObservationIgnored var activeFrame: ConversationFrame?
    @ObservationIgnored var activeFrameVerified = false
    @ObservationIgnored var historyWindows = RecentMap<HistoryWindow>()
    @ObservationIgnored var projectionHeads: [String: (epoch: String, generation: Int64)] = [:]
    @ObservationIgnored var projectionTasks: [String: (target: Target, token: UUID, task: Task<Void, Never>)] = [:]
    @ObservationIgnored var inspections: [String: (target: Target, task: Task<ConversationFrame, Error>)] = [:]
    @ObservationIgnored var historyFlights: [String: (target: Target, task: Task<HistoryPage, Error>)] = [:]
    @ObservationIgnored let detailGate = LoadingGate(now: { ProcessInfo.processInfo.systemUptime })
    @ObservationIgnored var detailTask: Task<Void, Never>?
    @ObservationIgnored var detailTimer: Task<Void, Never>?
    @ObservationIgnored var olderTask: Task<Void, Never>?
    @ObservationIgnored var viewportPageTask: Task<Void, Never>?
    @ObservationIgnored var presentationTask: Task<Void, Never>?
    @ObservationIgnored var historyHeadTask: Task<Void, Never>?
    @ObservationIgnored var readAttentionTask: Task<Void, Never>?
    @ObservationIgnored var presentationRevision = 0
    @ObservationIgnored var unreadBoundaryCaptured = false
    @ObservationIgnored var restoreHistoryAttempt = ""
    @ObservationIgnored var historyIntent: Int64 = 0
    @ObservationIgnored var latestHistoryIntent: Int64?
    @ObservationIgnored var pendingHistoryPosition = ""
    @ObservationIgnored var pendingHistoryPublication = ""
    @ObservationIgnored var lastHistoryObservation: Int64 = 0
    @ObservationIgnored var lastHistoryRequest: (target: Target, direction: String, cursor: String)?
    @ObservationIgnored var historySnapshotHints: [String: String] = [:]
    @ObservationIgnored let historyHeadScheduler = HistoryHeadScheduler()
    @ObservationIgnored var sharedReadsSent: Set<String> = []
    /// Marks completed without being applied wait in one bounded table, released by one owned timer.
    @ObservationIgnored var sharedReadRetries = SharedReadRetries()
    @ObservationIgnored var sharedReadRetryTimer: Task<Void, Never>?
    @ObservationIgnored let sharedReadClock = ContinuousClock.now
    @ObservationIgnored var readAllShares: Task<Void, Never>?
    @ObservationIgnored var readWriter: OrderedStatePersistence<ConversationReadState>?
    @ObservationIgnored var emptyDrafts: [String: Draft] = [:]
    @ObservationIgnored let questionSendGate = QuestionSendGate()
    @ObservationIgnored var parentSelection: Session?
    @ObservationIgnored var pendingAgentOpen: (navigation: Int64, parent: Target, agentId: String, deadline: Double)?
    @ObservationIgnored var detailReadErrorOwner: (target: Target, navigation: Int64)?

    init(app: AppModel, cache: ConversationCache?, pages: ConversationPageCache?) {
        self.app = app
        self.cache = cache
        self.pages = pages
    }

    var demo: Bool { app.demo }
    var messageState: MessageState { app.messageState }
    var drafts: [Draft] { app.messageState.drafts }

    // MARK: Opening

    func open(_ session: Session, preserveProject: Bool = false) {
        contextNotice = ""
        firstUnreadEventId = ""
        unreadBoundaryCaptured = false
        restoreHistoryAttempt = ""
        pendingHistoryPosition = ""
        pendingHistoryPublication = ""
        lastHistoryObservation = 0
        hasOlder = false
        hasNewer = false
        historyEpoch = ""
        historyIndexing = false
        historyComplete = false
        olderError = ""
        lastHistoryWasLatest = false
        actionResultNotice = ""
        if session.target.agentId.isEmpty { parentSelection = nil }
        cancelCompactContinuation()
        clearExpectation = nil
        app.flushPrivateState()
        navigationId += 1
        cancelDetail()
        selected = session
        activityVerified = false
        historyCached = false
        activeFrame = nil
        activity = nil
        events = []
        questions = []
        outgoing = []
        historyTruncated = false
        revision += 1
        let key = session.target.key
        let following = readState.record(session.target)?.viewport?.following == true
        if following { historyWindows[key] = nil }
        if historyWindows[key] == nil { currentHistoryHeads.remove(ConversationReadPolicies.key(session.target)) }
        let warm: ConversationFrame? = frames.touch(key).flatMap { frame in
            if frame.observedAt == 0 { return cache?.memory(frame.target) != nil ? frame : nil }
            let age = Clock.milliseconds() - frame.observedAt
            return (0...(7 * 24 * 60 * 60 * 1000)).contains(age) ? frame : nil
        }.map { frame in
            following ? frame.with(events: frame.inspectedEvents.isEmpty ? frame.events : frame.inspectedEvents,
                                   truncated: frame.truncated, window: nil, indexComplete: false, indexing: false) : frame
        }
        if warm == nil { frames[key] = nil }
        if let warm {
            installFrame(warm, verified: false)
        } else if !demo, let cache {
            let navigation = navigationId
            Task {
                guard let decoded = await cache.read(session.target) else { return }
                let raw = decoded.raw, rows = decoded.events
                let frame = await Task.detached(priority: .userInitiated) {
                    let inspected = NativeEvents.events(raw)
                    let displayed = following && !inspected.isEmpty ? inspected : rows
                    return ConversationFrame(target: session.target, raw: raw, events: displayed, questions: NativeEvents.questions(raw), observedAt: 0,
                                             bytes: JSON.object(raw).compact.utf8.count + displayed.reduce(0) { $0 + $1.approximateBytes },
                                             truncated: decoded.truncated, inspectedEvents: inspected)
                }.value
                guard navigationId == navigation, selected?.target == session.target, !activityVerified, activeFrame == nil else { return }
                rememberFrame(frame)
                installFrame(frame, verified: false)
            }
        }
        if !demo && readTrackingReady {
            restoreSavedWindow(session.target, navigation: navigationId)
            // A normal opening reads the bounded inspection tail. Only an actual
            // saved older reading position requests its canonical page.
            restoreHistory(session.target)
        }
        if demo {
            events = DemoCatalog.events
            questions = []
            revision += 1
        } else {
            refreshActivity()
        }
    }

    /// Leaves the conversation, or returns from a child agent to its parent.
    @discardableResult
    func back() -> Bool {
        if selected?.target.agentId.isEmpty == false, let parent = parentSelection {
            parentSelection = nil
            open(parent, preserveProject: true)
            return false
        }
        cancelCompactContinuation()
        clearExpectation = nil
        app.flushPrivateState()
        if selected != nil {
            navigationId += 1
            cancelDetail()
            selected = nil
            activity = nil
            activeFrame = nil
            activityVerified = false
            // Tasks opens over a conversation and closes with it.
            app.tasks.closeTasks()
        }
        return true
    }

    /// Leaves the conversation entirely, including a child opened from it.
    func close() {
        parentSelection = nil
        pendingAgentOpen = nil
        cancelCompactContinuation()
        clearExpectation = nil
        app.flushPrivateState()
        guard selected != nil else { return }
        navigationId += 1
        cancelDetail()
        selected = nil
        activity = nil
        activeFrame = nil
        activityVerified = false
        app.tasks.closeTasks()
    }

    func cancelDetail() {
        if detailReadErrorOwner != nil { app.error = "" }
        detailReadErrorOwner = nil
        detailGate.cancel()
        detailTask?.cancel()
        detailTimer?.cancel()
        detailBusy = false
        detailProgress = false
        presentationRevision += 1
        presentationTask?.cancel()
        olderTask?.cancel()
        latestHistoryIntent = nil
        olderLoading = false
        readPositionReady = readTrackingReady
        processOutputTask?.cancel()
        processOutputLoading = false
        processOutputResult = nil
        processOutputError = ""
        terminalTask?.cancel()
        terminalVisible = false
        terminalLoading = false
        terminalVerified = false
        terminalState = nil
    }

    // MARK: Inspection

    func refreshActivity(explicit: Bool = false) {
        guard !demo, !terminalVisible, latestHistoryIntent == nil, let original = selected else { return }
        if let action = pendingActionResult(original.target) {
            openSessionActionResult(action)
            return
        }
        guard let connection = app.connections.first(where: { $0.id == original.target.connectionId }) else { return }
        let navigation = navigationId
        let intent = historyIntent
        let start = detailGate.begin("\(navigation):\(original.target.key)", explicit: explicit)
        detailProgress = detailGate.visible()
        guard start.newRequest else { return }
        let operation = start.operation
        detailBusy = true
        detailTask?.cancel()
        detailTimer?.cancel()
        detailTimer = Task { [weak self] in
            guard let self else { return }
            try? await Task.sleep(for: .seconds(self.detailGate.remaining(operation.id)))
            if self.detailGate.owns(operation.id) && self.navigationId == navigation { self.detailProgress = self.detailGate.visible() }
        }
        detailTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if self.detailGate.finish(operation.id) {
                    self.detailTimer?.cancel()
                    self.detailBusy = false
                    self.detailProgress = false
                }
            }
            do {
                let frame = try await self.inspectOnce(connection, original.target).value
                guard self.detailGate.owns(operation.id), self.navigationId == navigation, self.historyIntent == intent,
                      self.selected?.target == original.target else { return }
                if let owner = self.detailReadErrorOwner, owner.target == original.target, owner.navigation == navigation { self.app.error = "" }
                let clear = self.clearExpectation.flatMap { $0.navigation == navigation && ContextPolicies.sameSessionRun($0.target, frame.target) ? $0 : nil }
                if let clear, frame.target.conversation != clear.target.conversation, !clear.confirmed {
                    // A new conversation is accepted only after the original clear receipt confirms it.
                    self.activityVerified = false
                    self.activeFrameVerified = false
                    self.historyCached = self.activity != nil
                    return
                }
                if let clear, clear.confirmed, !frame.target.conversation.isEmpty, frame.target.conversation != clear.target.conversation,
                   let operation = self.messageState.contextOperations.first(where: { $0.requestId == clear.requestId && $0.status == "completed" }) {
                    self.app.editMessages { ContextPolicies.promoteClearDraft($0, operation, to: frame.target) }
                    var updated = original
                    updated.target = frame.target
                    updated.status = frame.raw.string("activity", "phase", "state")
                    updated.raw = frame.raw
                    self.selected = updated
                    self.installFrame(frame, verified: true)
                    self.clearExpectation = nil
                    self.contextNotice = self.drafts.contains { $0.target == operation.target && $0.questionId.isEmpty && $0.detachedId.isEmpty }
                        ? "Context cleared. The previous composition remains in Saved drafts; restore it explicitly when ready." : "Context cleared. Your composition was kept."
                    return
                }
                if let clear = self.clearExpectation, !ContextPolicies.sameSessionRun(clear.target, frame.target) { self.clearExpectation = nil }
                var updated = original
                updated.target = frame.target
                updated.status = frame.raw.string("activity", "phase", "state")
                updated.raw = frame.raw
                self.selected = updated
                self.app.editMessages { $0.resolveConversation(from: original.target, to: frame.target) }
                self.installFrame(frame, verified: true)
            } catch is CancellationError {
            } catch {
                guard self.detailGate.owns(operation.id), self.navigationId == navigation, self.historyIntent == intent else { return }
                self.activeFrameVerified = false
                self.activityVerified = false
                self.historyCached = self.activity != nil
                self.app.error = ReadFailure.message(error, "conversation")
                self.detailReadErrorOwner = (original.target, navigation)
                if !self.olderLoading {
                    self.readPositionReady = true
                    self.captureUnreadBoundary(original.target, self.events)
                }
            }
        }
    }

    /// One shared inspection per exact target; at most four at a time.
    func inspectOnce(_ connection: Connection, _ target: Target, fresh: Bool = false, current: @MainActor () -> Bool = { true }) async throws -> Task<ConversationFrame, Error> {
        if fresh, let existing = inspections[target.key] {
            _ = try? await existing.task.value
            try Task.checkCancellation()
        }
        if fresh && !current() { throw CancellationError() }
        while inspections.count >= 4 && inspections[target.key] == nil {
            if let first = inspections.values.first { _ = try? await first.task.value }
            try Task.checkCancellation()
        }
        if fresh && !current() { throw CancellationError() }
        if !fresh, let existing = inspections[target.key] { return existing.task }
        let observedAt = Clock.milliseconds()
        let client = app.client
        let task = Task<ConversationFrame, Error> { [weak self] in
            let raw = try await client.inspect(connection, target: target)
            guard let self else { throw CancellationError() }
            let resolved = target.archiveId.isEmpty ? target.with(run: raw.string("run_id"), conversation: raw.string("conversation_id")) : target
            let previous = self.frames[resolved.key]
            let currentWindow = self.historyWindows[resolved.key]
            let complete = self.historyComplete, indexing = self.historyIndexing
            let previousTarget = previous?.target, previousEvents = previous?.events ?? [], previousInspected = previous?.inspectedEvents ?? []
            let previousTruncated = previous?.truncated == true
            let prepared = await Task.detached(priority: .userInitiated) { () -> (ConversationFrame, [String: String]) in
                let incoming = NativeEvents.events(raw)
                let retained = RollingConversationHistory.merge(target: resolved, previousTarget: previousTarget, previous: previousEvents, incoming: incoming,
                                                                inspection: true)
                let window = currentWindow.flatMap { $0.atHead ? $0.inspection(incoming, previous: previousInspected, scope: resolved) : nil }
                let rows = window?.events ?? retained.events
                let unknown = currentWindow.map { HistoryInspection.changed($0, incoming, previousInspected) } ?? false
                let bytes = JSON.object(raw).compact.utf8.count + rows.reduce(0) { $0 + $1.approximateBytes }
                let frame = ConversationFrame(target: resolved, raw: raw, events: rows, questions: NativeEvents.questions(raw), observedAt: observedAt,
                                              bytes: bytes, truncated: retained.truncated || previousTruncated || raw.bool("mobile_history_truncated") ||
                                                  raw.bool("history_truncated") || raw.bool("message_events_truncated") || raw.bool("provider_messages_truncated"),
                                              window: window, indexComplete: complete && !unknown, indexing: indexing, inspectedEvents: incoming)
                return (frame, retained.supersededIds)
            }.value
            var frame = prepared.0
            let aliases = prepared.1
            // A history page can publish while parsing. Rebase on the latest window so it is never erased.
            let latest = self.historyWindows[resolved.key]
            if latest != currentWindow || (latest != nil && frame.window == nil) {
                let guarded = latest.map { HistoryInspection.retain($0, nil, frame.inspectedEvents, previousInspected, scope: frame.target) }
                let changed = latest.map { HistoryInspection.changed($0, frame.inspectedEvents, previousInspected) } ?? false
                frame = frame.with(events: guarded?.events ?? frame.events, truncated: frame.truncated, window: guarded,
                                   indexComplete: self.historyComplete && !changed, indexing: self.historyIndexing)
            }
            guard self.app.connections.contains(where: { $0.id == connection.id }) else { throw CancellationError() }
            if frame.window == nil { self.currentHistoryHeads.remove(ConversationReadPolicies.key(resolved)) }
            if let window = frame.window {
                self.historyWindows[resolved.key] = window
                if let base = latest, HistoryInspection.changed(base, frame.inspectedEvents, previousInspected) {
                    self.currentHistoryHeads.remove(ConversationReadPolicies.key(resolved))
                }
            }
            if !aliases.isEmpty { self.editRead { $0.supersede(resolved, aliases) } }
            self.rememberFrame(frame)
            if let cache = self.cache {
                let rows = frame.events, truncated = frame.truncated
                Task { await cache.write(resolved, raw: raw, events: rows, truncated: truncated, observedAt: observedAt) }
            }
            return frame
        }
        inspections[target.key] = (target, task)
        Task { [weak self] in
            _ = try? await task.value
            if let self, self.inspections[target.key]?.task == task { self.inspections[target.key] = nil }
        }
        return task
    }

    func rememberFrame(_ frame: ConversationFrame) {
        frames[frame.target.key] = frame
        frames.trim { map in map.count > 20 || map.values.values.reduce(0) { $0 + $1.bytes } > 20 * 1024 * 1024 }
    }

    func installFrame(_ source: ConversationFrame, verified: Bool) {
        let retained = [activeFrame.flatMap { SessionNotificationPreferences.sameConversation($0.target,source.target) ? $0.raw : nil },
                        selected.flatMap { SessionNotificationPreferences.sameConversation($0.target,source.target) ? $0.raw : nil },
                        app.catalogRows.first { SessionNotificationPreferences.sameConversation($0.target,source.target) }?.raw].compactMap { $0 }
        let raw = SessionNotificationPreferences.mergeDetail(source.raw,source.target,retained:retained,actions:messageState.sessionActions)
        let incoming = raw == source.raw ? source : source.with(raw:raw)
        let key = incoming.target.key
        let window = historyWindows[key]
        let priorInspection = activeFrame.flatMap { $0.target == incoming.target ? $0.inspectedEvents : nil } ?? []
        let changed = window.map { incoming.window != $0 && HistoryInspection.changed($0, incoming.inspectedEvents, priorInspection) } ?? false
        if changed { currentHistoryHeads.remove(ConversationReadPolicies.key(incoming.target)) }
        let guarded = window.map { HistoryInspection.retain($0, incoming.window, incoming.inspectedEvents, priorInspection, scope: incoming.target) }
        var frame = incoming
        if let guarded, incoming.window != guarded || incoming.events != guarded.events {
            frame = incoming.with(events: guarded.events, truncated: incoming.truncated || guarded.hasBefore || guarded.hasAfter, window: guarded,
                                  indexComplete: historyComplete && !changed, indexing: historyIndexing)
        }
        if let guarded { historyWindows[key] = guarded }
        activeFrame = frame
        activeFrameVerified = verified
        if frame.presentation == nil {
            activityVerified = false
            historyCached = activity != nil
        }
        if let prepared = frame.presentation {
            events = prepared.events
            outgoing = prepared.messages
            questions = prepared.questions
            activity = frame.raw
            activityVerified = verified
            historyCached = !verified
            historyTruncated = frame.truncated
            revision += 1
            publishPagingMetadata(frame)
            captureUnreadBoundary(frame.target, prepared.events)
        }
        preparePresentation()
    }

    func confirmNotificationPreference(_ action: SessionAction) {
        if let row = selected {
            var copy = row
            copy.raw = SessionNotificationPreferences.mergeDetail(row.raw,row.target,retained:[],actions:messageState.sessionActions + [action])
            selected = copy
        }
        if let frame = activeFrame { installFrame(frame,verified:activeFrameVerified) }
    }

    func publishPagingMetadata(_ frame: ConversationFrame) {
        if let window = frame.window {
            hasOlder = window.hasBefore
            hasNewer = window.hasAfter
            historyEpoch = window.epoch
            historyComplete = frame.indexComplete
            historyIndexing = frame.indexing
        } else {
            // The bounded inspection does not prove that no older rows exist.
            // Older installed CLIs may omit their provider-tail truncation flag.
            hasOlder = historyNetworkReason(frame.target).isEmpty
            hasNewer = false
            historyEpoch = ""
            historyComplete = false
            historyIndexing = false
        }
        if pendingHistoryPublication == frame.target.key {
            // A page may retarget an existing opening divider through an alias; never restore a retired one.
            if !firstUnreadEventId.isEmpty, let boundary = frame.unreadBoundary { firstUnreadEventId = boundary }
            pendingHistoryPublication = ""
            olderLoading = false
            readPositionReady = true
        }
    }

    /// Merges native rows with this phone's outbox and restores submitted-answer locks, off the main actor.
    func preparePresentation() {
        guard let frame = activeFrame, let target = selected?.target, frame.target == target else { return }
        let messages = normalizedOutgoing.filter { $0.belongs(to: target) }
        let questionState = messageState
        presentationRevision += 1
        let version = presentationRevision
        presentationTask?.cancel()
        presentationTask = Task { [weak self] in
            let frameEvents = frame.events, frameQuestions = frame.questions
            let (merged, restored) = await Task.detached(priority: .userInitiated) {
                (target.archiveId.isEmpty ? ConversationMerge.merge(target: target, native: frameEvents, outgoing: messages)
                    : ConversationMerge.Result(events: frameEvents, recordedRequestIds: []),
                 QuestionTracking.restore(questionState, target, frameQuestions))
            }.value
            guard let self, version == self.presentationRevision, self.selected?.target == target, self.activeFrame === frame else { return }
            frame.presentation = .init(messages: messages, events: merged.events, questions: restored)
            if self.questions != restored { self.questions = restored }
            if self.events != merged.events { self.events = merged.events }
            if self.outgoing != messages { self.outgoing = messages }
            self.activity = frame.raw
            self.activityVerified = self.activeFrameVerified
            self.historyCached = !self.activeFrameVerified
            self.historyTruncated = frame.truncated
            self.revision += 1
            self.publishPagingMetadata(frame)
            if self.pendingHistoryPosition == frame.target.key {
                self.pendingHistoryPosition = ""
                self.readPositionReady = true
            }
            self.captureUnreadBoundary(frame.target, merged.events)
            guard self.activityVerified else { return }
            self.observeContext(frame)
            self.observeScheduledSettings(frame)
            if QuestionTracking.remember(self.messageState, frame.target, frame.questions) != self.messageState {
                self.app.editMessages { QuestionTracking.remember($0, frame.target, frame.questions) }
            }
            self.restoreHistory(frame.target)
            if let window = self.historyWindows[frame.target.key], window.atHead, !self.olderLoading, frame.observedAt > 0,
               frame.observedAt != self.lastHistoryObservation {
                self.lastHistoryObservation = frame.observedAt
                if let request = HistoryRequests.refreshAtHead(window) { self.loadHistory(frame.target, request.direction, request.cursor) }
            }
            if messages.contains(where: { row in merged.recordedRequestIds.contains(row.requestId) &&
                (row.status != "recorded" || merged.exactRepresentations[row.requestId].map { $0 != row.coverage } == true) }) {
                let recorded = merged.recordedRequestIds, exact = merged.exactRepresentations
                self.app.editMessages { OutboxRetention.recorded($0, recorded, exact: exact) }
            }
            // One status-only receipt read per navigation for the newest unconfirmed message; it never resends.
            if let message = messages.last(where: { $0.status == "uncertain" && !merged.recordedRequestIds.contains($0.requestId) }),
               self.outgoingReceiptProbe.claim(target, navigation: self.navigationId, message: message,
                                               busy: self.deliveryFlights.contains(message.requestId) || self.receiptFlights.contains("outgoing:\(message.requestId)")) {
                Task { await self.checkOutgoing(message, automatic: true) }
            }
            // Last: opening a child replaces this parent presentation.
            self.openPendingAgent()
        }
    }

    /// Outbox changes refresh the merged presentation.
    func messageStateChanged(old: MessageState, new: MessageState) {
        let submittedLocks: (MessageState) -> [Draft] = { $0.drafts.filter { $0.status == "submitted" && !$0.questionId.isEmpty } }
        if let intent = continuation, continuationEnqueuing != intent.requestId, !intent.unchanged(draft(intent.draft.target)) {
            cancelCompactContinuation("Draft changed. Compaction may finish, but your message has not been sent.")
        }
        if old.outgoing != new.outgoing || submittedLocks(old) != submittedLocks(new) { syncOutgoing() }
    }

    func syncOutgoing() {
        normalizedOutgoing = messageState.outgoing.map { $0.status == "sending" && !deliveryFlights.contains($0.requestId) ? $0.recover() : $0 }
        preparePresentation()
    }

    // MARK: Freshness

    /// Only a verified inspection of this exact live conversation authorizes a native action.
    func fresh(_ target: Target) -> Bool {
        guard let raw = activity, !terminalVisible, activityVerified, selected?.target == target, target.archiveId.isEmpty else { return false }
        let name = raw.string("name")
        return raw.string("run_id") == target.run && raw.string("conversation_id") == target.conversation && raw.string("archive_id").isEmpty &&
            raw.string("state") != "archived" && raw.string("agent_id") == target.agentId &&
            (target.agentId.isEmpty || raw.string("parent_conversation_id") == target.parentConversation) && (name.isEmpty || name == target.session)
    }

    // MARK: Child agents

    func childSendReason(_ target: Target) -> String {
        guard !target.agentId.isEmpty else { return "" }
        if !fresh(target) { return "Refresh this child conversation before sending." }
        if app.relayFeatures[target.connectionId]?.contains("send_agent") != true || app.machineFeatures[target.machineKey]?.contains("send_agent") != true {
            return "This native adapter supports child history only."
        }
        if activity?.bool("can_send") == true { return "" }
        let reason = activity?.string("send_reason", "can_send_reason") ?? ""
        return reason.isEmpty ? "This child conversation is read-only." : reason
    }

    func agentInspectReason(_ parent: Target, _ agentId: String) -> String {
        if !parent.agentId.isEmpty || !fresh(parent) { return "Refresh the live parent conversation first." }
        if app.relayFeatures[parent.connectionId]?.contains("inspect_agent") != true || app.machineFeatures[parent.machineKey]?.contains("inspect_agent") != true {
            return "This computer does not support child inspection."
        }
        if !SessionCardPresentation.openableAgent(agentId) { return "Invalid child identity." }
        let listed = activity?.object("subagents")?.has(agentId) == true ||
            (activity?.objects("subagents") ?? []).contains { $0.string("id", "agent_id") == agentId }
        return listed ? "" : "This child agent is no longer present."
    }

    func inspectAgent(_ parent: Target, _ agentId: String) {
        let reason = agentInspectReason(parent, agentId)
        guard reason.isEmpty else {
            app.error = reason
            return
        }
        guard let current = selected else { return }
        parentSelection = current
        var child = current
        child.target = parent.with(conversation: parent.conversation + "/" + agentId, agentId: agentId, parentConversation: parent.conversation)
        child.title = agentId
        open(child, preserveProject: true)
    }

    func agentListReason(_ parent: Target, _ agentId: String, online: Bool) -> String {
        if demo { return "Preview sessions have no subagent activity." }
        if !parent.agentId.isEmpty || !parent.archiveId.isEmpty { return "Archived subagent activity is not available on this phone." }
        if !online { return "This computer is unavailable." }
        if app.relayFeatures[parent.connectionId]?.contains("inspect_agent") != true || app.machineFeatures[parent.machineKey]?.contains("inspect_agent") != true {
            return "This computer does not support child inspection."
        }
        return SessionCardPresentation.openableAgent(agentId) ? "" : "Invalid child identity."
    }

    /// As in the desktop session tree, a child row opens its parent first, then the child once verified.
    func openAgent(_ parent: Session, _ agentId: String, online: Bool) {
        let reason = agentListReason(parent.target, agentId, online: online)
        guard reason.isEmpty else {
            app.error = reason
            return
        }
        open(parent, preserveProject: true)
        pendingAgentOpen = (navigationId, parent.target, agentId, ProcessInfo.processInfo.systemUptime + 30)
    }

    func openPendingAgent() {
        guard let pending = pendingAgentOpen else { return }
        guard pending.navigation == navigationId, let target = selected?.target else {
            pendingAgentOpen = nil
            return
        }
        guard fresh(target) else { return }
        pendingAgentOpen = nil
        if ProcessInfo.processInfo.systemUptime > pending.deadline {
            app.error = "The session took too long to load. Open the subagent again."
        } else if target.run != pending.parent.run || target.conversation != pending.parent.conversation {
            app.error = "This session has a new conversation. Its earlier subagent was not opened."
        } else {
            inspectAgent(target, pending.agentId)
        }
    }

    // MARK: Workspace removal

    func forget(connection: String) async {
        sharedReadRetries.forget(connection: connection)
        scheduleSharedReadRetries()
        for (_, flight) in inspections where flight.target.connectionId == connection { flight.task.cancel() }
        inspections = inspections.filter { $0.value.target.connectionId != connection }
        frames.removeAll { $0.target.connectionId == connection }
        let removed = Set(readState.records.filter { $0.target.connectionId == connection }.map { ConversationReadPolicies.key($0.target) })
        editRead { state in
            var copy = state
            copy.records = state.records.filter { $0.target.connectionId != connection }
            return copy
        }
        for (_, flight) in projectionTasks where flight.target.connectionId == connection { flight.task.cancel() }
        projectionTasks = projectionTasks.filter { $0.value.target.connectionId != connection }
        projectionHeads = projectionHeads.filter { historyWindows[$0.key]?.target.connectionId != connection }
        historyWindows.removeAll { $0.target.connectionId == connection }
        historySnapshotHints = historySnapshotHints.filter { !removed.contains($0.key) }
        currentHistoryHeads.subtract(removed)
        for (_, flight) in historyFlights where flight.target.connectionId == connection { flight.task.cancel() }
        historyFlights = historyFlights.filter { $0.value.target.connectionId != connection }
        if selected?.target.connectionId == connection {
            navigationId += 1
            cancelDetail()
            selected = nil
            activity = nil
            activeFrame = nil
            activityVerified = false
            events = []
            questions = []
            outgoing = []
        }
        await cache?.purgeWorkspace(connection)
        await pages?.purgeWorkspace(connection)
    }
}
