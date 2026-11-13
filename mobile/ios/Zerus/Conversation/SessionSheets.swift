import SwiftUI
import ZerusKit

/// Exact-identity confirmation; its action stays disabled until a fresh inspection of this session permits it.
struct SessionActionSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let session: Session
    let operation: String
    @State private var tag = ""
    @State private var notificationPreference: SessionNotificationPreference?
    @State private var questionRouting: QuestionRouting?
    @State private var routingRaw: JSONObject?

    var body: some View {
        let model = app.conversation!
        let target = session.target
        let parts = target.session.components(separatedBy: "/")
        let args: JSONObject = operation == SessionNotificationPreferences.operation ? ["muted":.bool(notificationPreference?.muted != true),"expected_revision":.int(notificationPreference?.revision ?? -1)]
            : operation == SessionQuestionRouting.operation ? SessionQuestionRouting.toggle(questionRouting)
            : operation == "rename" ? ["new_name": .string(parts.prefix(2).joined(separator: "/") + "/" + tag)]
            : operation == "fork" ? ["tag": .string(tag)] : [:]
        let label = operation == SessionNotificationPreferences.operation ? SessionNotificationPreferences.label(notificationPreference)
            : operation == SessionQuestionRouting.operation ? SessionQuestionRouting.label(routingRaw ?? session.raw) : ActionText.label(operation,target)
        let reason: String = model.actionReason(target, operation).nonEmpty ?? SessionControlPresentation.reason(target, operation, model.activity).nonEmpty ?? {
            do {
                _ = try SessionActionPolicies.arguments(operation, args)
                return SessionActionPolicies.inspectReason(target, operation, args, model.activity ?? [:])
            } catch { return AppModel.message(error, fallback: "Enter a valid name.") }
        }()
        NavigationStack {
            Form {
                Section {
                    Text("\(target.session) on \(app.machineName(target.machineKey))").font(.footnote).foregroundStyle(Theme.muted)
                    Text(Self.explanation(operation, target)).font(.callout)
                    if operation == "rename" || operation == "fork" {
                        TextField(operation == "rename" ? "Session tag" : "New session tag", text: $tag)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                } footer: {
                    if !reason.isEmpty { Text(reason) }
                }
            }
            .navigationTitle(operation == SessionNotificationPreferences.operation || operation == SessionQuestionRouting.operation ? "\(label) / \(session.title)" : "\(label) \(session.title)?")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(label, role: ["terminate", "forget"].contains(operation) ? .destructive : nil) {
                        app.flushPrivateState()
                        model.sessionAction(target, operation, args)
                        dismiss()
                    }
                    .disabled(!reason.isEmpty)
                }
            }
            .zerusScreen()
        }
        .presentationDetents([.medium, .large])
        .onAppear {
            if operation == "rename" && parts.count == 3 { tag = parts[2] }
        }
        .onChange(of:model.revision,initial:true) {
            if notificationPreference == nil && model.activityVerified { notificationPreference = SessionNotificationPreferences.parse(model.activity,target) }
            if questionRouting == nil && model.activityVerified { questionRouting = SessionQuestionRouting.parse(model.activity);routingRaw = model.activity }
        }
    }

    static func explanation(_ operation: String, _ target: Target) -> String {
        switch operation {
        case SessionNotificationPreferences.operation: "Applies to this conversation on all devices. Unread replies and pending requests stay unchanged. Unmuting does not replay earlier notifications."
        case SessionQuestionRouting.operation: "Applies to this child session on all devices. A blocking question is sent once to its parent session, which can answer it or ask you. Turning this off never cancels or resends a question already sent."
        case "pause": "Pause keeps the exact native conversation for Resume. Open network connections, child processes and unsent terminal input are not kept."
        case "resume": "Start the agent again with its saved native conversation."
        case "archive": "Move this saved session to Archive. Your private drafts stay on this phone."
        case "restore": "Start the agent again with this archived conversation. The archive is removed only after the agent confirms that conversation."
        case "terminate": "The agent and its running processes will stop. Native conversation history is kept where supported. Your private drafts stay on this phone."
        case "forget": target.archiveId.isEmpty
            ? "Stop this session and its running processes, then remove its Zerus record without keeping a new Zerus archive. Native provider history may remain. Your private drafts stay on this phone."
            : "Remove this selected archived version from Zerus on this machine. Your private drafts stay on this phone."
        case "rename": "Choose a new tag. The provider and project stay the same."
        case "fork": "Create a separate native conversation. Your current draft is kept without sending."
        default: ""
        }
    }
}

/// One staged form: the model list with a pinned Effort row, and a separate effort page.
struct NativeSettingsSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let session: Session
    @State private var snapshot: NativeSettings?
    @State private var selectedModel = ""
    @State private var selectedEffort = ""
    @State private var effortPage = false
    @State private var modelQuery = ""

    var body: some View {
        let model = app.conversation!
        let target = session.target
        let current = SessionDetailsPresentation.settings(model.activity)
        let snapshot = self.snapshot ?? current
        let choice = snapshot.models.first { $0.id == selectedModel }
        // The selected model's own variants (OpenCode `effort_options`, named by `effort_labels`); an older relay or
        // connector accepts only its fixed levels.
        let effortFeature = LaunchPresentation.effortSupported(relay: app.relayFeatures[target.connectionId], computer: app.machineFeatures[target.machineKey])
        let effortChoices = snapshot.effortOptions(selectedModel, effortFeature: effortFeature)
        let efforts = effortChoices.map(\.value)
        let effortLabel = effortChoices.first { $0.value == selectedEffort }?.label ?? ActionText.effort(selectedEffort)
        let applyingPending = !snapshot.pendingId.isEmpty && selectedModel == snapshot.pendingModel && selectedEffort == snapshot.pendingEffort
        let reason: String = model.actionReason(target, "settings").nonEmpty ?? {
            if current.signature != snapshot.signature { return "Native settings changed. Close and reopen this dialog to review them." }
            if !snapshot.supported { return snapshot.reason.isEmpty ? "Native settings are unavailable for this session." : snapshot.reason }
            if choice == nil { return "Choose a model from the native catalog." }
            if !selectedEffort.isEmpty && !efforts.contains(selectedEffort) { return "Choose a supported effort for this model." }
            return ""
        }()
        NavigationStack {
            List {
                Section {
                    Text("Settings apply to \(target.session).").font(.footnote).foregroundStyle(Theme.muted)
                    if !snapshot.pendingId.isEmpty {
                        let pending = snapshot.models.first { $0.id == snapshot.pendingModel }?.label ?? snapshot.pendingModel
                        let effort = snapshot.effortLabel(snapshot.pendingEffort, model: snapshot.pendingModel) ?? ActionText.effort(snapshot.pendingEffort)
                        Text("Pending: \(pending) / \(effort)").font(.footnote)
                    }
                }
                if effortPage {
                    Section(choice?.label ?? selectedModel) {
                        ForEach(effortChoices, id: \.value) { effort in
                            row(effort.label, selected: selectedEffort == effort.value) { selectedEffort = effort.value }
                        }
                    }
                } else {
                    Section {
                        Button { effortPage = true } label: {
                            HStack {
                                Text("Effort").foregroundStyle(Theme.onSurface)
                                Spacer()
                                Text(effortLabel).foregroundStyle(Theme.mint)
                                Image(systemName: "chevron.right").foregroundStyle(Theme.muted)
                            }
                        }
                        .disabled(!(effortChoices.count > 1 || !selectedEffort.isEmpty))
                        .accessibilityLabel("Choose effort, \(effortLabel)")
                    }
                    if snapshot.models.contains(where: { !$0.provider.isEmpty }) {
                        let models = snapshot.models.map { option in
                            OpenCodeModel(value: option.id, label: option.label, provider: option.provider, providerLabel: option.providerLabel,
                                free: option.free, favorite: option.favorite, isDefault: option.isDefault)
                        }
                        OpenCodeModelSections(models: OpenCodeCatalog.filter(models, query: modelQuery), selected: selectedModel) { value in
                            selectedModel = value
                            selectedEffort = snapshot.keptEffort(selectedEffort, model: value, effortFeature: effortFeature)
                        }
                    } else {
                        Section("Model") {
                            ForEach(snapshot.models, id: \.id) { option in
                                row(option.label, selected: selectedModel == option.id) {
                                    selectedModel = option.id
                                    selectedEffort = snapshot.keptEffort(selectedEffort, model: option.id, effortFeature: effortFeature)
                                }
                            }
                        }
                    }
                    let notice = OpenCodeCatalog.truncationMessage(models: snapshot.modelsTruncated, providers: false)
                    if !notice.isEmpty { Section { Text(notice).font(.footnote).foregroundStyle(Theme.muted) } }
                }
                Section {
                    Text(Self.applyText(snapshot.applyWhen)).font(.footnote).foregroundStyle(Theme.muted)
                    if snapshot.supported && !snapshot.notice.isEmpty { Text(snapshot.notice).font(.footnote) }
                    if !reason.isEmpty { Text(reason).font(.footnote).foregroundStyle(Theme.amber) }
                }
            }
            .modifier(OpenCodeModelSearch(enabled: !effortPage && snapshot.models.count > 8 && snapshot.models.contains(where: { !$0.provider.isEmpty }), query: $modelQuery))
            .navigationTitle(effortPage ? "Effort" : "Select model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if effortPage {
                    ToolbarItem(placement: .topBarLeading) {
                        Button { effortPage = false } label: { Image(systemName: "chevron.backward") }.accessibilityLabel("Back to models")
                    }
                } else {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(Self.applyLabel(snapshot.applyWhen, pending: applyingPending)) {
                        model.updateSettings(target, model: selectedModel, effort: selectedEffort, expectedPendingId: applyingPending ? snapshot.pendingId : "")
                        dismiss()
                    }
                    .disabled(!reason.isEmpty)
                }
            }
            .zerusScreen()
        }
        .presentationDetents([.large])
        .onAppear {
            guard self.snapshot == nil else { return }
            let first = SessionDetailsPresentation.settings(model.activity)
            self.snapshot = first
            selectedModel = first.pendingModel.isEmpty ? first.model : first.pendingModel
            selectedEffort = first.pendingId.isEmpty ? first.effort : first.pendingEffort
        }
    }

    private func row(_ label: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Text(label).foregroundStyle(selected ? Theme.mint : Theme.onSurface)
                Spacer()
                if selected { Image(systemName: "checkmark").foregroundStyle(Theme.mint) }
            }
        }
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    static func applyText(_ when: String) -> String {
        switch when {
        case "ready": "Apply when the agent is ready."
        case "resume": "Save for the next resume."
        case "now": "Apply now."
        default: "The machine determines when these settings apply."
        }
    }

    static func applyLabel(_ when: String, pending: Bool) -> String {
        switch when {
        case "ready": "Apply when ready"
        case "resume": "Save for resume"
        default: pending ? "Apply pending" : "Apply"
        }
    }
}

/// Native details: identity, goal, tasks, subagents and processes. Cached values are read-only.
struct SessionDetailsSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let session: Session
    @State private var tab = 0
    @State private var stop: (process: DetailProcess, generation: String)?
    @State private var output: DetailProcess?
    @State private var reviewAction: SessionAction?

    var body: some View {
        let model = app.conversation!
        let target = session.target
        let details = SessionDetailsPresentation.parse(model.activity, target, verified: model.activityVerified)
        NavigationStack {
            List {
                Picker("Section", selection: $tab) {
                    Text("Overview").tag(0)
                    Text("Plan").tag(1)
                    Text("Agents").tag(2)
                    Text("Processes").tag(3)
                }
                .pickerStyle(.segmented)
                .listRowBackground(Color.clear)
                switch tab {
                case 0: overview(details, model: model, target: target)
                case 1: tasks(details)
                case 2: agents(details, model: model, target: target)
                default: processes(details, model: model, target: target)
                }
            }
            .textSelection(.enabled)
            .navigationTitle("Session details")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if details.recorded { ToolbarItem(placement: .topBarLeading) { Tag(text: "Last known / Read-only", color: Theme.amber) } }
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .zerusScreen()
        }
        .presentationDetents([.large])
        .alert("Stop this process?", isPresented: Binding(get: { stop != nil }, set: { if !$0 { stop = nil } }), presenting: stop) { item in
            Button("Stop process", role: .destructive) { model.processStop(target, processId: item.process.id, generation: item.generation.nonEmpty) }
                .disabled(!stopReason(item.process, item.generation, model: model, target: target).isEmpty)
            Button("Cancel", role: .cancel) {}
        } message: { item in
            Text("\(target.session) on \(app.machineName(target.machineKey))\n\n\(item.process.command.nonEmpty ?? item.process.description)\n\nOnly the selected native process is targeted. The stop must be confirmed by the machine.")
        }
        .sheet(item: Binding(get: { output.map(ProcessItem.init) }, set: { output = $0?.process })) { item in
            ProcessOutputSheet(target: target, process: item.process)
        }
        .alert("Review action?", isPresented: Binding(get: { reviewAction != nil }, set: { if !$0 { reviewAction = nil } }), presenting: reviewAction) { action in
            Button("Mark reviewed") { model.reviewSessionAction(action) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("This action may already have changed the machine. Marking it reviewed does not undo or repeat it. Check the original receipt and native state first.")
        }
    }

    private struct ProcessItem: Identifiable {
        let process: DetailProcess
        var id: String { process.id }
    }

    @ViewBuilder
    private func overview(_ details: SessionDetailsView, model: ConversationModel, target: Target) -> some View {
        Section {
            ForEach(details.fields, id: \.self) { field in
                LabeledContent(field.label) {
                    HStack {
                        Text(field.value).multilineTextAlignment(.trailing)
                        if ["Session ID", "Run ID", "Agent ID", "Archive ID"].contains(field.label) {
                            Button { UIPasteboard.general.string = field.value } label: { Image(systemName: "doc.on.doc") }
                                .buttonStyle(.borderless)
                                .accessibilityLabel("Copy \(field.label)")
                        }
                    }
                }
            }
        }
        Section {
            ForEach(details.goal, id: \.self) { LabeledContent($0.label, value: $0.value) }
            Text(details.goalNotice).font(.footnote).foregroundStyle(Theme.muted)
        }
        let actions = model.sessionActions.filter { action in
            action.target == target && (["sending", "uncertain", "failed", "scheduled"].contains(action.status) ||
                (action.status == "completed" && action.resultTarget != nil && ["fork", "restore", "rename", "resume"].contains(action.operation)))
        }
        if !actions.isEmpty {
            Section("Native actions") {
                ForEach(actions, id: \.requestId) { action in
                    VStack(alignment: .leading, spacing: 6) {
                        Text("\(ActionText.label(action.operation)) / \(ActionText.status(action))").font(.subheadline.weight(.semibold))
                        if !action.error.isEmpty { Text(action.error).font(.footnote).foregroundStyle(Theme.muted) }
                        HStack(spacing: 16) {
                            if action.status == "completed" && action.resultTarget != nil {
                                Button("Open confirmed result") { model.openSessionActionResult(action); dismiss() }
                            }
                            if action.status == "uncertain" {
                                Button("Check original receipt") { model.checkSessionAction(action) }
                                Button("Review") { reviewAction = action }
                            }
                        }
                        .buttonStyle(.borderless)
                        .font(.footnote.weight(.semibold))
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func tasks(_ details: SessionDetailsView) -> some View {
        if details.tasks.isEmpty {
            Text("No plan reported yet. It appears when the agent updates its plan.").foregroundStyle(Theme.muted)
        }
        ForEach(details.tasks, id: \.owner) { list in
            Section {
                if list.tasks.isEmpty { Text("The agent cleared this list.").foregroundStyle(Theme.muted) }
                ForEach(Array(list.tasks.enumerated()), id: \.offset) { _, task in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(task.title)
                        Text(SessionDetailsPresentation.taskStatus(task.status) + (task.owner.isEmpty ? "" : " / \(task.owner)"))
                            .font(.caption).foregroundStyle(Theme.muted)
                    }
                }
                if list.truncated { Text("Showing the first 100 tasks.").font(.footnote) }
            } header: {
                Text(list.label)
            } footer: {
                Text("\(list.tasks.filter { $0.status == "completed" }.count) of \(list.tasks.count) completed" + (list.recorded ? " / Last recorded" : ""))
            }
        }
    }

    @ViewBuilder
    private func agents(_ details: SessionDetailsView, model: ConversationModel, target: Target) -> some View {
        if !details.agentNotice.isEmpty { Text(details.agentNotice).font(.footnote).foregroundStyle(Theme.muted) }
        if details.agents.isEmpty { Text("No subagent activity reported.").foregroundStyle(Theme.muted) }
        ForEach(details.agents, id: \.id) { agent in
            VStack(alignment: .leading, spacing: 6) {
                Text(agent.label).font(.subheadline.weight(.semibold))
                Text(SessionDetailsPresentation.agentStatus(agent.state) + (agent.recorded ? " / Last recorded" : "")).font(.caption).foregroundStyle(Theme.muted)
                if agent.group, let active = agent.active, let total = agent.total { Text("\(active) active / \(total) total").font(.caption) }
                if !agent.task.isEmpty { Text(agent.task) }
                if !agent.preview.isEmpty { Text(agent.preview).foregroundStyle(Theme.muted) }
                if !agent.group {
                    let reason = model.agentInspectReason(target, agent.id)
                    Button("Open agent activity") {
                        dismiss()
                        model.inspectAgent(target, agent.id)
                    }
                    .disabled(!reason.isEmpty)
                    .buttonStyle(.borderless)
                    if !reason.isEmpty { Text(reason).font(.footnote).foregroundStyle(Theme.muted) }
                }
            }
        }
    }

    @ViewBuilder
    private func processes(_ details: SessionDetailsView, model: ConversationModel, target: Target) -> some View {
        if details.processes.isEmpty { Text("No native processes reported.").foregroundStyle(Theme.muted) }
        if !details.processComplete {
            Text("Process inventory is incomplete. Missing entries are not proof that a process stopped.").font(.footnote).foregroundStyle(Theme.muted)
        }
        ForEach(details.processNotes, id: \.self) { Text($0).font(.footnote) }
        ForEach(details.processes, id: \.id) { process in
            let outputReason = model.processOutputReason(target, process.id)
            let reason = stopReason(process, details.generation, model: model, target: target)
            VStack(alignment: .leading, spacing: 6) {
                Text(process.command.nonEmpty ?? process.description.nonEmpty ?? "Native process").font(.subheadline.weight(.semibold))
                if !process.description.isEmpty && !process.command.isEmpty { Text(process.description) }
                Text(process.status + (process.stale ? " / Last recorded" : "")).font(.caption).foregroundStyle(Theme.muted)
                if !process.directory.isEmpty { Text(process.directory).font(.caption) }
                HStack(spacing: 16) {
                    Button("Output") {
                        output = process
                        model.processOutput(target, process.id, generation: details.generation.nonEmpty)
                    }
                    .disabled(!outputReason.isEmpty)
                    Button("Stop process") { stop = (process, details.generation) }.disabled(!reason.isEmpty)
                }
                .buttonStyle(.borderless)
                .font(.footnote.weight(.semibold))
                if !outputReason.isEmpty { Text(outputReason).font(.footnote).foregroundStyle(Theme.muted) }
                if !reason.isEmpty { Text(reason).font(.footnote).foregroundStyle(Theme.muted) }
            }
        }
    }

    private func stopReason(_ process: DetailProcess, _ generation: String, model: ConversationModel, target: Target) -> String {
        var args: JSONObject = ["process_id": .string(process.id)]
        if !generation.isEmpty { args["generation"] = .string(generation) }
        if let reason = model.actionReason(target, "process_stop").nonEmpty { return reason }
        if !process.stop || process.stale { return "This recorded process cannot be stopped here." }
        return SessionActionPolicies.inspectReason(target, "process_stop", args, model.activity ?? [:])
    }
}

struct ProcessOutputSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let target: Target
    let process: DetailProcess

    var body: some View {
        let model = app.conversation!
        let result = model.processOutputResult.flatMap { value in
            model.processOutputId == process.id && value.string("name") == target.session && value.string("run_id") == target.run &&
                value.string("conversation_id") == target.conversation && value.string("archive_id") == target.archiveId ? value : nil
        }
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if model.processOutputLoading { ProgressView("Reading output…") }
                    if !model.processOutputError.isEmpty { Text(model.processOutputError).foregroundStyle(Theme.amber) }
                    if let result {
                        let text = result.string("output")
                        Text(verbatim: text.isEmpty ? "No output reported." : text).font(.footnote.monospaced()).textSelection(.enabled)
                        if result.bool("output_truncated") || result.bool("truncated") { Text("Output is truncated.").font(.footnote).foregroundStyle(Theme.muted) }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
            }
            .navigationTitle("Process output")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Close") { dismiss() } } }
            .zerusScreen()
        }
    }
}
