import SwiftUI
import ZerusKit

/// The open conversation: canonical history window, unread divider, native questions and the composer.
/// Reading position, following and read marks follow the Android viewport policies.
struct ConversationView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.dismiss) private var dismiss
    let route: SessionRoute

    @State private var position = ScrollPosition(idType: String.self)
    @State private var follow = ConversationFollowState()
    @State private var atEnd = true
    @State private var userScrolling = false
    @State private var latestPending = false
    @State private var unavailableSaved = false
    @State private var display = QuestionDisplayState(owner: "")
    @State private var review: Draft?
    @State private var reviewOutgoing: OutgoingMessage?
    @State private var discard: Draft?
    @State private var searching = false
    @State private var notice = ""
    /// The items the shared readable-visibility rule shows (zerus-a4mub.31), for reading.
    @State private var readLayout = ConversationReadLayout()
    @State private var historyProgress = false
    /// The conversation whose opening reads have settled; until then its first refresh shows at once.
    @State private var openedTarget = ""
    @State private var stopRequest = false
    @State private var details = false
    @State private var terminal = false
    @State private var terminalOpen = false
    @State private var terminalUnavailable: String?
    @State private var tasks = false
    @State private var tasksOpen = false

    private var model: ConversationModel { app.conversation }

    private var session: Session {
        if let selected = model.selected { return selected }
        return app.sessions.first { $0.target == route.target } ??
            Session(target: route.target, title: route.target.session, agent: "", status: "", project: "", preview: "", pendingCount: 0, raw: [:])
    }

    private func currentQuestions(_ target: Target) -> [Question] {
        !target.archiveId.isEmpty || app.demo ? [] : model.questions
    }

    private func questionPresence(questions: [Question], available: Bool) -> QuestionPresenceView? {
        QuestionPresencePolicies.resolve(display, questions: questions, verified: model.activityVerified, available: available,
                                         checking: model.detailBusy, checkedAtMillis: model.questionCheckedAt,
                                         nowMillis: ConversationModel.monotonicMillis(), complete: model.questionProjectionComplete)
    }

    var body: some View {
        let session = self.session
        let target = session.target
        let archived = !target.archiveId.isEmpty
        let events = displayedEvents(target)
        let questions = archived || app.demo ? [] : model.questions
        let raw = model.activity
        let warning = app.demo ? "" : (raw?.string("pending_questions_error", "provider_messages_error") ?? "")
        let unreadBoundary = events.contains { $0.id == model.firstUnreadEventId } ? model.firstUnreadEventId : ""
        let readReady = model.readPositionReady || app.demo
        let unread = UnreadPresentation.badge(total: model.unreadCount(target), loaded: model.loadedUnreadCount(target))
        let historyReason = app.demo ? "" : model.historyReason(target)
        let bounded = !model.historyEpoch.isEmpty
            ? (model.hasOlder || model.hasNewer ? "Showing part of the conversation. Use the history controls to load more messages." : "")
            : (model.historyTruncated ? "Showing recent activity. Older messages may be outside the available history." : "")
        let loadStatus = ConversationLoadPresentation.status(hasContent: !events.isEmpty || !questions.isEmpty, detailLoading: model.detailBusy,
                                                             detailProgress: model.detailProgress, historyLoading: model.olderLoading,
                                                             historyProgress: historyProgress, demo: app.demo, resolved: raw != nil,
                                                             opening: openedTarget != target.key)
        // Refresh progress uses the progress bar, so no status row moves cached messages.
        let openingProgress = openedTarget != target.key && loadStatus?.initial == false && loadStatus?.spinner == true
        let loading = loadStatus?.initial == true
        let offered = Set(questions.filter { QuestionPolicies.shown($0) && QuestionPolicies.submitted($0, target) == nil }.map(\.id)).count
        let keys = Self.keys(events: events, questions: questions, warning: warning, bounded: bounded, unreadBoundary: unreadBoundary, loading: loading)
        let online = model.activityVerified || app.demo
        let available = online && model.activityVerified && !model.contextBlocked(target)
        // zerus-a4mub.25: the expanded question's verified presence. Only a resolved card is a sheet; every obsolete
        // state is an inline notice, so an unrelated delivery Review stays reachable. It never enables an answer.
        let presence = questionPresence(questions: questions, available: available)

        VStack(spacing: 0) {
            ProgressView()
                .progressViewStyle(.linear)
                .tint(Theme.mint)
                .frame(height: 3)
                .opacity(!app.demo && (model.detailProgress || historyProgress || openingProgress) ? 1 : 0)
                .accessibilityHidden(loadStatus?.initial != false || loadStatus?.spinner != true)
                .accessibilityLabel(loadStatus?.text ?? "")
            SessionHeaderTools(session: session, details: $details, search: $searching, terminal: $terminal, tasks: $tasks)
            statusStrip(unread: unread, offered: offered, questions: questions, target: target)
            if !notice.isEmpty {
                HStack {
                    Text(notice).font(.footnote).foregroundStyle(Theme.amber)
                    Spacer()
                    Button { notice = "" } label: { Image(systemName: "xmark") }.accessibilityLabel("Dismiss notice")
                }
                .padding(.horizontal, 16)
            }
            ZStack(alignment: .bottomTrailing) {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        earlierRow(target: target, events: events, historyReason: historyReason, loading: loading).readingItem("history:earlier", readLayout)
                        statusRow(session: session, raw: raw, events: events, loading: loading, online: online).readingItem("history:status", readLayout)
                        if !warning.isEmpty { Text(warning).font(.footnote).foregroundStyle(Theme.amber).readingItem("history:warning", readLayout) }
                        if !bounded.isEmpty { Text(bounded).font(.footnote).foregroundStyle(Theme.muted).readingItem("history:bounded", readLayout) }
                        Group {
                            if unavailableSaved {
                                Text("Your saved reading position is outside the loaded messages. Showing the earliest available messages.")
                                    .font(.footnote).foregroundStyle(Theme.muted)
                            } else { Color.clear.frame(height: 0) }
                        }.readingItem("history:missing-anchor", readLayout)
                        // A fork's inherited rows: each source under its label, then where the fork begins.
                        ForEach(Array(events.enumerated()), id: \.element.id) { index, event in
                            let source = event.inheritedFrom ?? ""
                            if source != (index > 0 ? events[index - 1].inheritedFrom ?? "" : "") {
                                inheritedDivider(source.isEmpty ? "Forked here: this session's own activity continues below"
                                    : "Inherited from \(source): the history this fork began from")
                                    .readingItem("history:inherited:" + event.id, readLayout)
                            }
                            if event.id == unreadBoundary { unreadDivider.readingItem("history:unread", readLayout) }
                            MessageBubble(event: event, target: target, agent: agentName(session, raw), outgoing: model.outgoing.first { $0.requestId == event.outgoingId },
                                          onReviewOutgoing: { reviewOutgoing = $0 })
                                .readingItem("event:" + event.id, readLayout)
                        }
                        if events.isEmpty && !loading {
                            Text(raw != nil || app.demo ? "No messages yet." : "No recent messages are available. Refresh to load activity.")
                                .foregroundStyle(Theme.muted).readingItem("history:empty", readLayout)
                        }
                        newerRow(target: target, historyReason: historyReason).readingItem("history:newer", readLayout)
                        if !archived { NativeInputQueueCard(target: target).readingItem("history:queue", readLayout) }
                        if !questions.isEmpty {
                            QuestionsPane(target: target, questions: questions, available: available, display: $display, onReview: { review = $0 })
                                .readingItem("history:questions", readLayout)
                        }
                        Color.clear.frame(height: 1).readingItem("history:tail", readLayout)
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 12)
                    .scrollTargetLayout()
                }
                .scrollPosition($position)
                .defaultScrollAnchor(.bottom, for: .initialOffset)
                .scrollDismissesKeyboard(.interactively)
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    geometry.visibleRect.maxY >= geometry.contentSize.height + geometry.contentInsets.bottom - 24
                } action: { _, value in atEnd = value }
                .onScrollGeometryChange(for: CGFloat.self) { $0.containerSize.height } action: { old, new in
                    // Keep the newest message in view when the composer or header changes height.
                    if follow.following && follow.opened && old != new && !userScrolling { position.scrollTo(edge: .bottom) }
                }
                .onScrollPhaseChange { old, new in
                    userScrolling = new == .interacting || new == .decelerating || new == .tracking
                    if new == .idle && (old == .interacting || old == .decelerating) {
                        follow = ConversationViewport.userScrolled(follow, atEnd: atEnd)
                        follow.opened = true
                    }
                    if new == .idle { follow = ConversationViewport.settled(follow, atEnd: atEnd, hasNewer: model.hasNewer) }
                }
                .refreshable { model.refreshActivity(explicit: true) }

                if loading {
                    VStack(spacing: 12) {
                        if loadStatus?.spinner == true { ProgressView() }
                        Text(loadStatus?.text ?? "").foregroundStyle(Theme.muted)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .allowsHitTesting(false)
                }
                if (!follow.following || model.hasNewer || latestPending) && (!events.isEmpty || !questions.isEmpty) && !atEnd {
                    latestButton(target: target, unread: unread, keys: keys)
                }
            }
            if let presence, !presence.modal {
                QuestionPresenceNotice(view: presence, checkedAt: model.questionCheckedAt,
                                       onClose: { display = display.close() },
                                       onRefresh: { model.refreshActivity(explicit: true) },
                                       onOpen: { display = display.open($0) })
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
            }
            if archived {
                VStack(alignment: .leading, spacing: 6) {
                    ContextFooter(session: session)
                    Text("Archived conversation. Read-only.").font(.footnote).foregroundStyle(Theme.muted)
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.bar)
            } else {
                Composer(session: session, online: online, onReview: { review = $0 }, onReviewOutgoing: { reviewOutgoing = $0 })
                    .id(ComposerIdentity.lane(target))
            }
        }
        .background(Theme.background.ignoresSafeArea())
        .navigationTitle(SessionFilters.label(session))
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(!target.agentId.isEmpty)
        .toolbar { toolbar(session: session, raw: raw) }
        // Dismissing a resolved card closes it; a card that stops resolving leaves the sheet and keeps its
        // identity for the inline notice.
        .sheet(isPresented: Binding(get: { presence?.modal == true },
                                    set: { if !$0 && questionPresence(questions: currentQuestions(target), available: available)?.modal == true {
                                        display = display.close() } })) {
            ExpandedQuestionSheet(target: target, questions: questions, available: available, presence: presence, display: $display,
                                  onReview: { review = $0 })
        }
        .sheet(isPresented: $searching) {
            ConversationSearchSheet(events: events) { id in jump(to: id, events: model.events, target: target) }
        }
        .alert("Stop this turn?", isPresented: $stopRequest) {
            Button("Stop turn", role: .destructive) { Task { await model.interrupt(target) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Interrupt the current turn in \(session.title). The session remains available.")
        }
        .reviewDialogs(review: $review, reviewOutgoing: $reviewOutgoing, discard: $discard)
        .sheet(isPresented: $details) { SessionDetailsSheet(session: session) }
        .modifier(ConversationTools(session: session, raw: raw, archived: archived, online: online, terminal: $terminal, terminalOpen: $terminalOpen,
                                    terminalUnavailable: $terminalUnavailable, tasks: $tasks, tasksOpen: $tasksOpen))
        .onChange(of: model.returnToSessions) { dismiss() }
        .onAppear {
            openIfNeeded()
            #if DEBUG
            // `-zerus-terminal` shows a synthetic captured screen for simulator UI checks.
            if ProcessInfo.processInfo.arguments.contains("-zerus-terminal") && app.demo {
                model.terminalState = ["name": .string(target.session), "run_id": .string(target.run), "conversation_id": .string(target.conversation),
                                       "terminal_binding_id": "fixture", "cols": 60, "rows": 8, "cursor": ["x": 2, "y": 6],
                                       "screen": .string("\u{1b}[1;32muser@example\u{1b}[0m:\u{1b}[34m~/project\u{1b}[0m$ git status\nOn branch \u{1b}[36mmain\u{1b}[0m\nChanges not staged for commit:\n  \u{1b}[31mmodified:   src/orders.rs\u{1b}[0m\n\u{1b}[7m inverse \u{1b}[27m \u{1b}[4munderline\u{1b}[24m \u{1b}[2mdim\u{1b}[22m 界面 😀\n\n$ ")]
                terminalOpen = true
            }
            #endif
        }
        .onChange(of: target.key) { display = display.forTarget(target.key) }
        .task(id: OpeningKey(target: target.key, settled: model.activityVerified && model.readPositionReady && !model.detailBusy && !model.olderLoading)) {
            // The inspection and canonical history can finish in either order; later polls keep the delayed gate.
            if model.activityVerified && model.readPositionReady && !model.detailBusy && !model.olderLoading { openedTarget = target.key }
        }
        .onChange(of: ContentSignature(revision: model.revision, own: model.ownSendId(target), verified: model.activityVerified,
                                       boundary: unreadBoundary, ready: readReady, newer: model.hasNewer, questions: questions.count)) {
            applyContent(target: target, keys: keys, events: events, questions: questions, unreadBoundary: unreadBoundary, readReady: readReady)
        }
        .task(id: VisibilityKey(ids: readLayout.visible, readable: scenePhase == .active && follow.opened && readReady && !userScrolling,
                                following: follow.following && !model.hasNewer, current: model.currentReadKey(target),
                                newest: model.newestDisplayedEventId(target))) {
            guard scenePhase == .active, follow.opened, readReady, !userScrolling, !app.demo else { return }
            // The values and newest row this dwell observes; a change restarts it.
            let current = model.currentReadKey(target), newest = model.newestDisplayedEventId(target)
            // A transient row during initial positioning or a fling is not a deliberate read.
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            let visible = readLayout.visible
            let ids = ConversationReadViewport.eventIds(visible)
            model.visibleMessages(target, ids)
            model.visibleCurrent(target, key: current, flags: ConversationReadViewport.flags(visible: visible, newestId: newest))
            if let first = visible.first(where: { $0.hasPrefix("event:") }) {
                model.saveViewport(target, eventId: String(first.dropFirst(6)), offset: 0, following: follow.following && !model.hasNewer)
            }
            if !model.historyEpoch.isEmpty && visible.contains("history:earlier") && model.hasOlder && !model.olderLoading && model.olderError.isEmpty { model.loadOlder(target) }
        }
        .task(id: model.olderLoading) {
            historyProgress = false
            guard model.olderLoading, !app.demo else { return }
            try? await Task.sleep(for: .seconds(5))
            if model.olderLoading { historyProgress = true }
        }
    }

    // MARK: Rows

    /// Native history with this phone's voice receipts and private media copies merged in.
    private func displayedEvents(_ target: Target) -> [Event] {
        app.demo ? model.events : VoiceHistory.merge(target, native: model.events, outgoing: app.voice.outgoing)
    }

    private static func keys(events: [Event], questions: [Question], warning: String, bounded: String, unreadBoundary: String, loading: Bool) -> [String] {
        var keys = ["history:earlier", "history:status"]
        if !warning.isEmpty { keys.append("history:warning") }
        if !bounded.isEmpty { keys.append("history:bounded") }
        keys.append("history:missing-anchor")
        for event in events {
            if event.id == unreadBoundary { keys.append("history:unread") }
            keys.append("event:" + event.id)
        }
        if events.isEmpty && !loading { keys.append("history:empty") }
        keys.append("history:newer")
        keys.append("history:queue")
        if !questions.isEmpty { keys.append("history:questions") }
        keys.append("history:tail")
        return keys
    }

    @ViewBuilder
    private func earlierRow(target: Target, events: [Event], historyReason: String, loading: Bool) -> some View {
        VStack(spacing: 4) {
            if !loading {
                if !model.olderError.isEmpty {
                    Text(model.olderError).font(.footnote).foregroundStyle(Theme.amber).multilineTextAlignment(.center)
                    Button(model.lastHistoryWasLatest ? "Retry latest messages" : "Retry history") { model.retryHistory(target) }
                        .disabled(model.olderLoading || !model.lastHistoryWasLatest && !historyReason.isEmpty)
                } else if model.hasOlder {
                    Button("Load earlier messages") { model.loadOlder(target) }.disabled(model.olderLoading || !historyReason.isEmpty)
                } else if !model.historyEpoch.isEmpty && !model.historyIndexing {
                    Text(model.historyComplete ? "Beginning of conversation" : "No earlier page reported; history may be incomplete.")
                        .font(.caption2).foregroundStyle(Theme.muted)
                }
                if model.historyIndexing {
                    Text("History is still indexing. Unread totals may be unavailable.").font(.caption2).foregroundStyle(Theme.muted)
                }
                if !historyReason.isEmpty && !events.isEmpty { Text(historyReason).font(.caption2).foregroundStyle(Theme.muted) }
            }
        }
        .font(.footnote.weight(.semibold))
        .frame(maxWidth: .infinity)
    }

    @ViewBuilder
    private func statusRow(session: Session, raw: JSONObject?, events: [Event], loading: Bool, online: Bool) -> some View {
        if !loading && !events.isEmpty {
            HStack(spacing: 8) {
                if raw == nil && !app.demo {
                    Tag(text: "Checking…", color: Theme.muted)
                } else {
                    Tag(text: SelectedSessionPresentation.status(session, raw), color: Theme.mint)
                }
                Tag(text: app.demo ? "Preview" : (model.activityVerified ? "Connected" : (raw != nil ? "Last known" : "Checking…")),
                    color: online ? Theme.mint : Theme.amber)
            }
        } else {
            Color.clear.frame(height: 0)
        }
    }

    private func inheritedDivider(_ label: String) -> some View {
        HStack(spacing: 12) {
            Rectangle().fill(Theme.muted.opacity(0.4)).frame(height: 1)
            Text(label).font(.caption.weight(.semibold)).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
            Rectangle().fill(Theme.muted.opacity(0.4)).frame(height: 1)
        }
    }

    private var unreadDivider: some View {
        HStack(spacing: 12) {
            Rectangle().fill(Theme.mint.opacity(0.4)).frame(height: 1)
            Text("Unread messages").font(.caption.weight(.semibold)).foregroundStyle(Theme.mint).fixedSize()
            Rectangle().fill(Theme.mint.opacity(0.4)).frame(height: 1)
        }
    }

    @ViewBuilder
    private func newerRow(target: Target, historyReason: String) -> some View {
        if model.hasNewer {
            HStack {
                Spacer()
                Button("Load newer messages") { model.loadNewer(target) }.disabled(model.olderLoading || !historyReason.isEmpty)
                Button("Latest") { requestLatest(target: target) }.disabled(latestPending)
                Spacer()
            }
            .font(.footnote.weight(.semibold))
        } else {
            Color.clear.frame(height: 0)
        }
    }

    @ViewBuilder
    private func statusStrip(unread: UnreadBadge?, offered: Int, questions: [Question], target: Target) -> some View {
        if unread != nil || offered > 0 {
            HStack(spacing: 8) {
                Text(unread.map { "\($0.text) unread" } ?? "")
                    .font(.caption)
                    .foregroundStyle(SessionNotificationPreferences.parse(model.activity ?? session.raw,target)?.muted == true ? Theme.muted : Theme.mint)
                Spacer()
                if offered > 0 {
                    Button("Questions (\(offered))") {
                        if !display.expanded, let question = QuestionPolicies.queue(questions, target, chosen: display.chosen).selected {
                            display = display.open(question)
                        }
                    }
                    .font(.caption.weight(.semibold))
                }
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 28)
        }
    }

    private func latestButton(target: Target, unread: UnreadBadge?, keys: [String]) -> some View {
        Button { requestLatest(target: target) } label: {
            HStack(spacing: 4) {
                if latestPending { ProgressView().controlSize(.small) } else { Image(systemName: "chevron.down") }
                Text(latestPending ? "Loading latest…" : unread.map { "\($0.text) unread" } ?? "Latest").font(.caption.weight(.semibold))
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 44)
            .foregroundStyle(Theme.mint)
            .background(Color(hex: 0x2D4142), in: RoundedRectangle(cornerRadius: 16))
        }
        .buttonStyle(.plain)
        .disabled(latestPending)
        .padding(16)
        .accessibilityLabel("Jump to latest messages")
        .accessibilityValue(latestPending ? "Loading latest messages" : unread?.description ?? "")
    }

    @ToolbarContentBuilder
    private func toolbar(session: Session, raw: JSONObject?) -> some ToolbarContent {
        let target = session.target
        if !target.agentId.isEmpty {
            ToolbarItem(placement: .topBarLeading) {
                Button { model.back() } label: { Image(systemName: "chevron.backward") }.accessibilityLabel("Back to parent conversation")
            }
        }
        ToolbarItem(placement: .principal) {
            SessionHeaderTitle(session: session) { details = true }
        }
        if raw?.bool("interrupt_supported") == true && target.agentId.isEmpty && ["busy", "working"].contains(raw?.string("activity") ?? "") {
            ToolbarItem(placement: .topBarTrailing) {
                Button { stopRequest = true } label: { Image(systemName: "stop.circle") }
                    .disabled(!model.interruptReason(target).isEmpty)
                    .accessibilityLabel("Stop current turn")
            }
        }
    }

    private func agentName(_ session: Session, _ raw: JSONObject?) -> String {
        if !session.target.agentId.isEmpty, let provider = raw?.string("provider"), !provider.isEmpty { return provider }
        return session.agent
    }

    // MARK: Behaviour

    private func openIfNeeded() {
        let current = model.selected?.target
        if current == route.target || current?.resolves(to: route.target) == true || (current.map { route.target.resolves(to: $0) } ?? false) ||
            (current?.agentId.isEmpty == false && current?.session == route.target.session) {
            model.refreshActivity()
            return
        }
        follow = ConversationFollowState()
        display = QuestionDisplayState(owner: route.target.key)
        let session = app.sessions.first { $0.target == route.target } ??
            Session(target: route.target, title: route.target.session, agent: "", status: "", project: "", preview: "", pendingCount: 0, raw: [:])
        model.open(session)
    }

    private func applyContent(target: Target, keys: [String], events: [Event], questions: [Question], unreadBoundary: String, readReady: Bool) {
        guard !(events.isEmpty && questions.isEmpty && model.activity == nil && !app.demo) else { return }
        let ownSendId = model.ownSendId(target)
        let own = events.last { $0.outgoingId == ownSendId || $0.receiptRequestId == ownSendId }
        let ownKey = own.map { questions.isEmpty ? "history:tail" : "event:" + $0.id }
        let explicitSend = !ownSendId.isEmpty && ownSendId != follow.ownSendId && ownKey != nil
        let firstOpen = !follow.opened
        if firstOpen && !explicitSend && !readReady { return }
        if firstOpen && !explicitSend {
            let opening = UnreadPresentation.opening(memory: nil, saved: model.conversationViewport(target), boundary: unreadBoundary, keys: keys)
            unavailableSaved = opening.unavailableSaved
            follow.anchor = opening.anchor
            follow.following = opening.anchor?.following ?? true
        }
        if model.hasNewer && !explicitSend { follow.following = false }
        if explicitSend && model.hasNewer && !latestPending { requestLatest(target: target) }
        let request = ConversationViewport.content(follow, keys: keys, ownSendId: ownSendId, ownItemKey: ownKey)
        follow = request.state
        if explicitSend || firstOpen || !userScrolling, let index = request.index { scroll(to: keys[index], last: index == keys.count - 1) }
    }

    private func scroll(to key: String, last: Bool) {
        if last { position.scrollTo(edge: .bottom) } else { position.scrollTo(id: key, anchor: .top) }
    }

    private func requestLatest(target: Target) {
        guard model.selected?.target == target, !latestPending else { return }
        let needsHistory = model.hasNewer || model.olderLoading
        latestPending = true
        Task {
            defer { latestPending = false }
            if needsHistory {
                guard let read = model.jumpToLatest(target) else {
                    notice = "Could not load the latest messages. \(model.olderError.isEmpty ? "Wait for the current history read, then try again." : model.olderError)"
                    return
                }
                await read.value
                // The read prepares its presentation off the main actor; wait until it is installed.
                for _ in 0..<50 where model.olderLoading { try? await Task.sleep(for: .milliseconds(20)) }
            }
            guard model.selected?.target == target else { return }
            let keys = Self.keys(events: model.events, questions: model.questions, warning: "", bounded: "", unreadBoundary: "", loading: false)
            let request = ConversationViewport.loadedLatest(follow, keys: keys, hasNewer: needsHistory && model.hasNewer, error: needsHistory ? model.olderError : "")
            if !request.failure.isEmpty {
                notice = request.failure
            } else {
                follow = request.state
                withAnimation { position.scrollTo(edge: .bottom) }
            }
        }
    }

    private func jump(to eventId: String, events: [Event], target: Target) {
        searching = false
        guard let match = ConversationSearch.resolve(events, eventId) else {
            notice = "That message is no longer in the loaded window. Search again or load its earlier page."
            return
        }
        follow.opened = true
        follow.following = false
        follow.anchor = ConversationAnchor(itemKey: "event:" + match.id, offset: 0, following: false)
        withAnimation { position.scrollTo(id: "event:" + match.id, anchor: .top) }
    }
}

private struct ContentSignature: Equatable {
    let revision: Int
    let own: String
    let verified: Bool
    let boundary: String
    let ready: Bool
    let newer: Bool
    let questions: Int
}

private struct OpeningKey: Equatable {
    let target: String
    let settled: Bool
}

private struct VisibilityKey: Equatable {
    let ids: [String]
    let readable: Bool
    let following: Bool
    /// The open inspection's current read values and newest displayed row (zerus-a4mub.9).
    let current: String
    let newest: String
}

/// Searches loaded messages only; results never advance read state.
struct ConversationSearchSheet: View {
    @Environment(\.dismiss) private var dismiss
    let events: [Event]
    let onSelect: (String) -> Void
    @State private var query = ""

    var body: some View {
        let prepared = ConversationSearch.prepare(events)
        let results = ConversationSearch.find(prepared, query)
        NavigationStack {
            List {
                if !query.trimmingCharacters(in: .whitespaces).isEmpty {
                    Section {
                        ForEach(results.hits, id: \.eventId) { hit in
                            Button { onSelect(hit.eventId) } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack {
                                        Text(hit.role).font(.caption.weight(.semibold)).foregroundStyle(Theme.muted)
                                        Spacer()
                                        if hit.at > 0 { Text(MessageTime.text(hit.at)).font(.caption2).foregroundStyle(Theme.muted) }
                                    }
                                    Text(hit.snippet).font(.footnote).foregroundStyle(Theme.onSurface).lineLimit(4)
                                }
                            }
                        }
                    } footer: {
                        Text(results.matches == 0 ? "No loaded message matches." :
                             results.matches > results.hits.count ? "\(results.matches) loaded messages match; showing the first \(results.hits.count)." :
                             "\(results.matches) loaded \(results.matches == 1 ? "message matches" : "messages match").")
                    }
                } else {
                    Text("Search the messages loaded on this phone.").foregroundStyle(Theme.muted)
                }
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search messages")
            .navigationTitle("Search")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
            .zerusScreen()
        }
    }
}

/// File links, Terminal and voice observation, kept out of the main body so older compilers type-check it quickly.
private struct ConversationTools: ViewModifier {
    @Environment(AppModel.self) private var app
    let session: Session
    let raw: JSONObject?
    let archived: Bool
    let online: Bool
    @Binding var terminal: Bool
    @Binding var terminalOpen: Bool
    @Binding var terminalUnavailable: String?
    @Binding var tasks: Bool
    @Binding var tasksOpen: Bool

    func body(content: Content) -> some View {
        let target = session.target
        content
            // While Terminal covers the conversation, it presents its own file links.
            .sheet(item: Binding(get: { terminalOpen ? nil : app.computerFiles.selection }, set: { if $0 == nil { app.computerFiles.close() } })) { ComputerFileSheet(selection: $0) }
            .environment(\.computerFileLink, ComputerFileLink { [weak app] reference in app?.computerFiles.show(target, reference) })
            .onChange(of: terminal) { _, requested in
                guard requested else { return }
                terminal = false
                if let reason = reason(target) { terminalUnavailable = reason } else { terminalOpen = true }
            }
            .alert("Terminal unavailable", isPresented: Binding(get: { terminalUnavailable != nil }, set: { if !$0 { terminalUnavailable = nil } })) {
                Button("Close", role: .cancel) { terminalUnavailable = nil }
            } message: {
                Text(terminalUnavailable ?? "")
            }
            .fullScreenCover(isPresented: $terminalOpen) { TerminalView(session: session) }
            .onChange(of: tasks) { _, requested in
                guard requested else { return }
                tasks = false
                app.tasks.openTasks(session)
                tasksOpen = app.tasks.tasksKey != nil
            }
            // Tasks opens over this conversation and closes with it.
            .onChange(of: target) { tasksOpen = false }
            .fullScreenCover(isPresented: $tasksOpen, onDismiss: { app.tasks.closeTasks() }) { TasksView() }
            .task(id: VoiceObservation(revision: app.conversation.revision, verified: app.conversation.activityVerified, ready: app.voice.ready)) {
                if !app.demo && app.conversation.activityVerified { app.voice.observe(target, events: app.conversation.events) }
            }
    }

    private func reason(_ target: Target) -> String? {
        let supported = (app.relayOperations[target.connectionId] ?? []).intersection(app.machineOperations[target.machineKey] ?? [])
        let facts = SessionMenuFacts(target: target, row: raw ?? [:], state: SessionFilters.state(session), archived: archived, online: online, supported: supported)
        return SessionActionsMenu.terminalReason(facts).nonEmpty ?? app.conversation.terminalOpenReason(target).nonEmpty
    }
}

/// Voice rows are observed again whenever verified history publishes.
private struct VoiceObservation: Equatable {
    let revision: Int
    let verified: Bool
    let ready: Bool
}
