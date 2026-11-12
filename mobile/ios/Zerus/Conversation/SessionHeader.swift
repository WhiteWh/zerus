import SwiftUI
import ZerusKit

enum ActionText {
    static func label(_ operation: String, _ target: Target? = nil) -> String {
        switch operation {
        case SessionNotificationPreferences.operation: "Shared notification settings"
        case SessionQuestionRouting.operation: "Questions to parent"
        case "pause": "Pause"
        case "resume": "Resume"
        case "archive": "Archive"
        case "rename": "Rename"
        case "fork": "Fork"
        case "terminate": "Terminate"
        case "restore": "Restore"
        case "forget": target.map { !$0.archiveId.isEmpty } == true ? "Forget archived version" : "Forget session"
        case "send_now": "Send queued message"
        case "directory_create": "Create folder"
        case "set_mobile_delivery": "Phone notification timing"
        case "settings": "Change settings"
        case "process_stop": "Stop process"
        case "recovery_action": "Recovery action"
        case "launch": "New session"
        case "worktree_create": "Create worktree"
        default: "Native action"
        }
    }

    /// Computer-scoped actions have no session, so the operation names the card.
    static func title(_ action: SessionAction) -> String {
        if action.needsProjectReview { return action.resultTarget?.session ?? "" }
        return action.target.session.isEmpty ? label(action.operation) : action.target.session
    }

    static func status(_ action: SessionAction) -> String {
        switch action.status {
        case "sending": "Sending"
        case "uncertain": "Outcome unknown"
        case "failed": "Not applied"
        case "scheduled": "Pending on machine"
        case "applied": "Applied"
        case "completed": action.operation == "process_stop" ? "Stop requested" : "Completed"
        case "reviewed": "Reviewed"
        default: "Status not reported"
        }
    }

    static func context(_ operation: String, _ status: String) -> String {
        let name = operation == "clear_context" ? "Context clear" : "Compaction"
        let state: String
        switch status {
        case "sending": state = "Sending command…"
        case "submitted": state = "Command accepted / Waiting for native result"
        case "completed", "confirmed": state = "Completed"
        case "uncertain": state = "Result unknown"
        case "failed": state = "Not completed"
        case "unchanged": state = "No history compacted"
        case "cancelled": state = "Cancelled"
        default: state = status
        }
        return name + " / " + state
    }

    static func effort(_ value: String) -> String { EffortOption.text(value) }
}

/// Name, owning computer and desktop-coloured status; tapping opens session details.
struct SessionHeaderTitle: View {
    @Environment(AppModel.self) private var app
    let session: Session
    let onDetails: () -> Void

    var body: some View {
        let model = app.conversation!
        let target = session.target
        let raw = model.activity
        let status = raw == nil && !app.demo ? "Checking…" : SelectedSessionPresentation.status(session, raw)
        let freshness = app.demo ? " / Preview" : (model.activityVerified || raw == nil ? "" : " / Last known")
        let kind = raw == nil && !app.demo ? DesktopBadgeKind.neutral : DesktopSessionPalette.badge(SelectedSessionPresentation.session(session, raw), true).kind
        let colors = DesktopSessionPalette.colors(kind, dark: true)
        Button(action: onDetails) {
            VStack(spacing: 4) {
                Text(target.agentId.isEmpty ? session.title : target.agentId).font(.headline).lineLimit(1)
                HStack(spacing: 6) {
                    MachineLabel(name: app.machineName(target.machineKey), colorHex: app.machineColor(target.machineKey))
                        .frame(maxWidth: 150)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(status + freshness)
                        .font(.caption2.weight(.medium))
                        .lineLimit(1)
                        .foregroundStyle(Color(rgb: colors.foreground))
                        .padding(.horizontal, 7)
                        .padding(.vertical, 3)
                        .background(Color(rgb: colors.background), in: RoundedRectangle(cornerRadius: 5))
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows session details")
    }
}

/// Provider and model, Read all, Terminal, Tasks and the session menu, as on Android.
struct SessionHeaderTools: View {
    @Environment(AppModel.self) private var app
    let session: Session
    @Binding var details: Bool
    @Binding var search: Bool
    @Binding var terminal: Bool
    @Binding var tasks: Bool
    @State private var settings = false
    @State private var request: String?
    @State private var notice = ""

    var body: some View {
        let model = app.conversation!
        let target = session.target
        let native = SessionDetailsPresentation.settings(model.activity)
        let provider = !target.agentId.isEmpty ? (model.activity?.string("provider")).flatMap { $0.isEmpty ? nil : $0 } ?? session.agent : session.agent
        let canReadAll = target.archiveId.isEmpty && !app.demo && model.canReadAll(target)
        VStack(spacing: 6) {
            HStack(spacing: 4) {
                Button {
                    if target.agentId.isEmpty { settings = true } else { details = true }
                } label: {
                    HStack(spacing: 6) {
                        ProviderLabel(agent: provider)
                        Text([native.model.isEmpty ? "Model not reported" : native.model, native.effortLabel(native.effort, model: native.model) ?? native.effort]
                            .filter { !$0.isEmpty }.joined(separator: " / "))
                            .font(.caption.weight(.medium))
                            .lineLimit(1)
                            .foregroundStyle(Theme.onSurface)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: 10))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Model and effort")
                Button {
                    let result = model.markAllRead([target])
                    notice = ConversationLoadPresentation.readAllOutcome(marked: result.marked, skipped: result.skipped, partial: result.partial)
                } label: { Image(systemName: "checkmark.circle").frame(width: 40, height: 40) }
                    .disabled(!canReadAll)
                    .accessibilityLabel("Read all")
                Button { terminal = true } label: { Image(systemName: "apple.terminal").frame(width: 40, height: 40) }
                    .accessibilityLabel("Terminal")
                // Tasks belong to catalogued projects; the badge counts the tasks In review.
                if let key = app.tasks.taskProject(session), target.agentId.isEmpty, target.archiveId.isEmpty, !app.demo {
                    let review = app.tasks.reviewCounts[key] ?? 0
                    Button { tasks = true } label: {
                        Image(systemName: "checklist")
                            .frame(width: 40, height: 40)
                            .overlay(alignment: .topTrailing) {
                                if review > 0 {
                                    Text(review > 99 ? "99+" : "\(review)")
                                        .font(.caption2.weight(.bold))
                                        .foregroundStyle(Theme.background)
                                        .padding(.horizontal, 4)
                                        .frame(minWidth: 16, minHeight: 16)
                                        .background(Theme.mint, in: Capsule())
                                }
                            }
                    }
                    .accessibilityLabel("Tasks")
                    .accessibilityValue(review > 0 ? "\(review) In review" : "")
                    .task(id: key) { app.tasks.refreshReviewCount(key) }
                }
                if SessionNotificationPreferences.parse(model.activity ?? session.raw,target)?.muted == true {
                    Image(systemName:"bell.slash").foregroundStyle(Theme.muted).accessibilityLabel("Notifications muted on all devices")
                }
                menu(model: model, target: target)
            }
            .padding(.horizontal, 10)
            if let action = model.pendingActionResult(target) {
                HStack {
                    Text(model.actionResultNotice.isEmpty ? "The machine confirmed this action. Its destination is available for verification." : model.actionResultNotice)
                        .font(.caption)
                    Spacer()
                    Button("Open confirmed result") { model.openSessionActionResult(action) }.font(.caption.weight(.semibold))
                }
                .padding(.horizontal, 16)
            }
            NativeRecoveryCard(target: target)
            if !notice.isEmpty {
                HStack {
                    Text(notice).font(.caption).foregroundStyle(Theme.muted)
                    Spacer()
                    Button { notice = "" } label: { Image(systemName: "xmark") }.accessibilityLabel("Dismiss")
                }
                .padding(.horizontal, 16)
            }
        }
        .sheet(isPresented: $settings) { NativeSettingsSheet(session: session) }
        .sheet(item: Binding(get: { request.map(ActionRequest.init) }, set: { request = $0?.operation })) { item in
            SessionActionSheet(session: session, operation: item.operation)
        }
        .onChange(of: model.sessionMenuRequest?.operation, initial: true) {
            // A Sessions long-press action opened this session; confirm it here against fresh native details.
            if let operation = model.takeSessionMenuRequest(target) {
                if operation == "terminal" { terminal = true } else { request = operation }
            }
        }
    }

    private func menu(model: ConversationModel, target: Target) -> some View {
        let reference = SessionReference.resolve(target, app.machines)
        let evidence = target.agentId.isEmpty ? model.captureReadAttention(target) : nil
        let folder = model.activity?.string("cwd") ?? ""
        return Menu {
            Button("Search loaded messages", systemImage: "magnifyingglass") { search = true }
            Button("Refresh", systemImage: "arrow.clockwise") { model.refreshActivity(explicit: true) }
            let preference = SessionNotificationPreferences.parse(model.activity ?? session.raw,target)
            let notificationReason = model.actionReason(target,SessionNotificationPreferences.operation)
            Button { request = SessionNotificationPreferences.operation } label: {
                Label(SessionNotificationPreferences.label(preference),systemImage:preference?.muted == true ? "bell" : "bell.slash")
                if !notificationReason.isEmpty { Text(notificationReason) }
            }.disabled(!notificationReason.isEmpty)
            // The person's per-child question route switch (zerus-sa9qn.33).
            let routingRaw = model.activity ?? session.raw
            if SessionQuestionRouting.applies(target,routingRaw) {
                let routingReason = model.actionReason(target,SessionQuestionRouting.operation)
                Button { request = SessionQuestionRouting.operation } label: {
                    Label(SessionQuestionRouting.label(routingRaw),systemImage:"questionmark.bubble")
                    if !routingReason.isEmpty { Text(routingReason) }
                }.disabled(!routingReason.isEmpty)
            }
            if let evidence {
                Section {
                    let emptyReminder = model.reviewLater(target) && !evidence.authoritative && evidence.loadedIds.isEmpty
                    Button(emptyReminder ? "Clear review reminder" : evidence.authoritative ? "Mark read on this phone" : "Mark loaded replies read",
                           systemImage: "checkmark.circle") { model.markRead(target, evidence) }
                        .disabled(!(model.readPositionReady && (evidence.authoritative || !evidence.loadedIds.isEmpty || emptyReminder)))
                    Button("Review later on this phone", systemImage: "bookmark") { model.reviewLater(target, evidence) }
                        .disabled(!model.readPositionReady)
                }
            }
            Section {
                Button("Copy session name", systemImage: "doc.on.doc") { UIPasteboard.general.string = target.session }
                Button("Copy session reference", systemImage: "link") { if let uri = reference.uri { UIPasteboard.general.string = uri } }
                    .disabled(reference.uri == nil)
                if !folder.isEmpty { Button("Copy folder", systemImage: "folder") { UIPasteboard.general.string = folder } }
            }
            if target.agentId.isEmpty {
                Section {
                    let operations = !target.archiveId.isEmpty ? ["restore", "rename", "fork", "forget"] : ["pause", "resume", "archive", "rename", "fork", "terminate", "forget"]
                    ForEach(operations, id: \.self) { operation in
                        let reason = model.actionReason(target, operation).nonEmpty ?? SessionControlPresentation.reason(target, operation, model.activity)
                        Button { request = operation } label: {
                            Text(ActionText.label(operation, target))
                            if !reason.isEmpty { Text(reason) }
                        }
                        .disabled(!reason.isEmpty)
                    }
                }
            }
        } label: {
            Image(systemName: "ellipsis.circle").frame(width: 40, height: 40)
        }
        .accessibilityLabel("Session actions")
    }

    private struct ActionRequest: Identifiable {
        let operation: String
        var id: String { operation }
    }
}

extension String {
    var nonEmpty: String? { isEmpty ? nil : self }
}

/// The literal native recovery job; its one-second countdown changes only this card.
struct NativeRecoveryCard: View {
    @Environment(AppModel.self) private var app
    let target: Target
    @State private var history = false

    var body: some View {
        let model = app.conversation!
        if let job = RecoveryPresentation.from(model.activity, target, verified: model.activityVerified), job.state != "succeeded", !job.resumed {
            TimelineView(.periodic(from: .now, by: 1)) { timeline in
                let now = timeline.date.timeIntervalSince1970
                let nowReason = model.recoveryActionReason(target, jobId: job.id, choice: "now")
                let cancelReason = model.recoveryActionReason(target, jobId: job.id, choice: "cancel")
                VStack(alignment: .leading, spacing: 4) {
                    Text(job.status(now: now) + (job.recorded ? " / Last known" : "")).font(.subheadline.weight(.semibold))
                    Text(job.attempts).font(.caption)
                    if !job.reason.isEmpty { Text(job.reason).font(.caption).lineLimit(2) }
                    else if job.action == "retry_request" { Text("Retry the failed provider request").font(.caption) }
                    else if job.action == "continue_message" { Text("Send a continuation message in this conversation").font(.caption) }
                    HStack(spacing: 16) {
                        if job.state == "waiting" {
                            Button("Retry now") { model.recoveryAction(target, jobId: job.id, choice: "now") }
                                .disabled(job.recorded || !nowReason.isEmpty || !job.retryTimeAllowed(now: now))
                            Button("Cancel retry") { model.recoveryAction(target, jobId: job.id, choice: "cancel") }
                                .disabled(job.recorded || !cancelReason.isEmpty)
                        }
                        Button("Details") { history = true }
                    }
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.borderless)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: 12))
                .padding(.horizontal, 12)
                .sheet(isPresented: $history) {
                    NavigationStack {
                        List {
                            Section {
                                Text(job.status(now: now))
                                Text(job.attempts)
                                if !job.reason.isEmpty { Text(job.reason) }
                                if job.recorded { Text("Last known job. Actions require a fresh matching session and recovery job.") }
                                if job.state == "waiting" && !job.retryTimeAllowed(now: now) { Text("Retry now respects the provider's reported minimum retry time.") }
                                ForEach(Array(Set([nowReason, cancelReason].filter { !$0.isEmpty })), id: \.self) { Text($0).font(.footnote) }
                            }
                            Section("Recorded attempts") {
                                if job.history.isEmpty { Text("No attempt history reported.") }
                                ForEach(Array(job.history.enumerated()), id: \.offset) { _, attempt in
                                    VStack(alignment: .leading) {
                                        Text([attempt.at.map { Date(timeIntervalSince1970: $0).formatted(date: .abbreviated, time: .standard) }, attempt.status,
                                              attempt.attempt.map { "\($0) attempts" }].compactMap { $0 }.joined(separator: " / ")).font(.caption.weight(.semibold))
                                        if !attempt.reason.isEmpty { Text(attempt.reason).font(.footnote) }
                                    }
                                }
                            }
                        }
                        .textSelection(.enabled)
                        .navigationTitle("Native recovery")
                        .navigationBarTitleDisplayMode(.inline)
                        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Close") { history = false } } }
                        .zerusScreen()
                    }
                }
            }
        }
    }
}
