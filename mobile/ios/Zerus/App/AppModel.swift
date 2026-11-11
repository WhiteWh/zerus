import Observation
import UIKit
import SwiftUI
import ZerusKit

/// The application state owner, mirroring Android `ZerusViewModel`. It keeps
/// navigation and request identity on the main actor and runs parsing off it.
@MainActor
@Observable
final class AppModel {
    let client: RelayClient
    @ObservationIgnored private let connectionStore: ConnectionStore
    @ObservationIgnored private let sealedStore: SealedFileStore?

    // MARK: Private state

    @ObservationIgnored private let outboxPins = OutboxPins()
    @ObservationIgnored private(set) var messages: OrderedStatePersistence<MessageState>?
    private(set) var messageState = MessageState()
    @ObservationIgnored private var accountsStore: OrderedStatePersistence<[AccountCatalog]>?
    private(set) var accountCatalogs: [AccountCatalog] = []
    var accountsError = ""

    // MARK: Workspace catalog

    private(set) var connections: [Connection] = []
    private(set) var pendingEncryptedPairings = Set<String>()
    private(set) var machines: [Machine] = []
    private(set) var relayOperations: [String: Set<String>] = [:]
    private(set) var relayFeatures: [String: Set<String>] = [:]
    private(set) var machineOperations: [MachineKey: Set<String>] = [:]
    private(set) var machineFeatures: [MachineKey: Set<String>] = [:]
    /// Live catalog rows, before archive pages are merged.
    @ObservationIgnored private var catalogSessions: [Session] = [] { didSet { catalogRevision += 1 } }
    /// Changes whenever the live catalog is replaced, so off-main annotations can detect a newer catalog.
    @ObservationIgnored private(set) var catalogRevision = 0
    var catalogRows: [Session] { catalogSessions }
    /// The published session catalog: live rows and bounded archive pages.
    private(set) var sessions: [Session] = []
    private(set) var projects: [ProjectSummary] = []
    private(set) var projectWarnings: [String] = []
    /// For each catalogued project, every computer's tasks digest, and when the catalog named them.
    private(set) var taskDigests: [ProjectKey: [String: String]] = [:]
    @ObservationIgnored private(set) var taskDigestsAt = Date.distantPast

    // MARK: Status

    private(set) var storageReady = false
    private(set) var demo = false
    private(set) var busy = false
    private(set) var catalogBusy = false
    /// Thin progress for catalog reads: explicit refreshes show it at once, background reads after five seconds.
    private(set) var catalogProgress = false
    var error = ""
    /// A pasted or scanned `zerus://pair` link waiting for explicit confirmation.
    var invitation: PairingInvite?
    /// A conversation another screen asked to show; the root view pushes it on Sessions.
    var conversationRequest: Target?
    /// A short confirmation shown at the bottom of the screen.
    var notice = ""

    // MARK: Sessions view state

    var sessionFilter: SessionFilter = .all { didSet { if oldValue != sessionFilter { sessionScopeChanged() } } }
    var sessionQuery = "" { didSet { if oldValue != sessionQuery { sessionScopeChanged() } } }
    private(set) var selectedMachines: Set<MachineKey> = []
    private(set) var collapsedProjects: Set<String> = []
    /// Parents whose subagents and child sessions are hidden: one toggle, kept across relaunches
    /// by machine UUID and session ID (TeamCollapse, zerus-sa9qn.5).
    private(set) var teamCollapse = TeamCollapse()
    @ObservationIgnored private var teamsStore: OrderedStatePersistence<[String]>?
    @ObservationIgnored private var groupedCache: (source: [AccountCatalog], groups: [GroupedAccount])?
    private(set) var sessionProjectScope: ProjectSummary?
    @ObservationIgnored private var unscoped: (filter: SessionFilter, query: String)?
    private(set) var listPresentation = SessionListPresentation()
    private(set) var listReady = false
    @ObservationIgnored private var listGeneration = 0
    @ObservationIgnored private(set) var archive: ArchivePaging!

    // MARK: Machine labels

    private(set) var machineNameSaving: Set<MachineKey> = []
    private(set) var machineColorSaving: Set<MachineKey> = []

    @ObservationIgnored private var started = false
    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    @ObservationIgnored private var progressTask: Task<Void, Never>?
    @ObservationIgnored private var catalogGeneration = 0

    /// The open conversation and its delivery state.
    @ObservationIgnored private(set) var conversation: ConversationModel!
    /// The New session form.
    @ObservationIgnored private(set) var launch: LaunchModel!
    /// Sealed copies of selected files, separate from private state.
    @ObservationIgnored let attachmentStore: AttachmentStore?
    /// Opens message attachments.
    @ObservationIgnored private(set) var viewer: AttachmentViewer!
    /// Opens Markdown links to files on a conversation's computer.
    @ObservationIgnored private(set) var computerFiles: ComputerFiles!
    /// Voice and round video messages.
    @ObservationIgnored private(set) var voice: VoiceController!
    /// Plays one recording at a time.
    @ObservationIgnored private(set) var playback: VoicePlayback!
    /// Speech provider management on processing computers.
    @ObservationIgnored private(set) var speechProviders: SpeechProviders!
    /// Session alerts and push registration.
    @ObservationIgnored private(set) var notifications: SessionNotifications!
    /// The projects' task replicas and the Tasks screen.
    @ObservationIgnored private(set) var tasks: TasksModel!

    init(client: RelayClient, connectionStore: ConnectionStore, sealedStore: SealedFileStore?,
         cache: ConversationCache? = nil, pages: ConversationPageCache? = nil, attachmentStore: AttachmentStore? = nil,
         attachmentCache: AttachmentViewCache? = nil, computerFileCache: AttachmentViewCache? = nil) {
        self.client = client
        self.connectionStore = connectionStore
        self.sealedStore = sealedStore
        self.attachmentStore = attachmentStore
        archive = ArchivePaging(load: { [weak self] key, query in
            guard let self else { throw CancellationError() }
            return try await self.loadArchivePage(key, query)
        }, reset: { [weak self] in self?.publishSessions() })
        conversation = ConversationModel(app: self, cache: cache, pages: pages)
        launch = LaunchModel(app: self)
        viewer = AttachmentViewer(app: self, store: attachmentStore, cache: attachmentCache)
        computerFiles = ComputerFiles(app: self, cache: computerFileCache)
        voice = VoiceController(app: self)
        playback = VoicePlayback(app: self)
        speechProviders = SpeechProviders(app: self)
        notifications = SessionNotifications(app: self)
        tasks = TasksModel(app: self)
    }

    static func live() -> AppModel {
        AppModel(client: RelayClient(workspaceJournal: try? SQLiteWorkspaceJournal.applicationSupport()), connectionStore: KeychainConnectionStore(),
                 sealedStore: try? SealedFileStore.applicationSupport(keys: KeychainKeyProvider()),
                 cache: try? ConversationCache.live(), pages: try? ConversationPageCache.live(),
                 attachmentStore: try? AttachmentStore.live(), attachmentCache: viewCache("attachment-view"), computerFileCache: viewCache("computer-file-view"))
    }

    private static func viewCache(_ name: String) -> AttachmentViewCache? {
        guard let caches = try? FileManager.default.url(for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true) else { return nil }
        return AttachmentViewCache(root: caches.appendingPathComponent(name, isDirectory: true))
    }

    /// Whether the gateway and computer both serve an additive read operation.
    func attachmentAccess(_ target: Target, operation: String) -> AttachmentAccess {
        let relay = relayOperations[target.connectionId], machine = machineOperations[target.machineKey]
        let online = machines.first { $0.key == target.machineKey }?.online
        return operation == ComputerFileProtocol.operation ? ComputerFileProtocol.access(relay: relay, machine: machine, online: online)
            : AttachmentIdentity.access(relay: relay, machine: machine, online: online)
    }

    func start() async {
        guard !started else { return }
        started = true
        do {
            let store = connectionStore
            connections = try await Task.detached { try store.load() }.value
            for connection in connections where connection.protocolVersion == 2 {
                if (try? await client.encryptedEnrollmentPending(connection)) == true { pendingEncryptedPairings.insert(connection.id) }
            }
            try await openPrivateState()
            storageReady = true
            startNotifications()
            conversation.recoverAttachmentPick()
            let caches = [viewer.cache, computerFiles.cache].compactMap { $0 }
            Task.detached(priority: .utility) { caches.forEach { $0.trim() } }
            refresh()
        } catch {
            self.error = "Private storage could not be opened. Existing data has been preserved. \(error.localizedDescription)"
        }
    }

    private func openPrivateState() async throws {
        guard let sealed = sealedStore else { throw ZerusError.invalid("Private storage is unavailable.") }
        let loaded = try await Task.detached { () -> (MessageState, [AccountCatalog]) in
            let state = try sealed.read("message_state", as: MessageState.self) ?? MessageState()
            let accounts = (try? sealed.read("account_catalog", as: [AccountCatalog].self)) ?? []
            return (OutboxRetention.normalize(state.recovered()), AccountSnapshots.restored(accounts))
        }.value
        let pins = outboxPins
        let messageStore = OrderedStatePersistence(initial: loaded.0, normalize: { OutboxRetention.normalize($0, pinned: pins.snapshot()) }) { try sealed.write("message_state", $0) }
        messageStore.onChange = { [weak self] in self?.messageStateChanged($0) }
        messageStore.onResult = { [weak self] failure in
            if failure != nil {
                self?.error = "Could not save private changes. Your text remains available here; a message is sent only after its outgoing record is saved."
            }
        }
        messages = messageStore
        messageState = loaded.0
        let connected = Set(connections.map(\.id))
        let accounts = OrderedStatePersistence(initial: loaded.1.filter { connected.contains($0.key.connectionId) }) { try sealed.write("account_catalog", $0) }
        accounts.onChange = { [weak self] in self?.accountCatalogs = $0 }
        await conversation.openReadState(sealed)
        await voice.open(sealed)
        await speechProviders.open(sealed)
        await notifications.open(sealed)
        accounts.onResult = { [weak self] failure in
            self?.accountsError = failure == nil ? "" : "Account snapshots could not be saved. Existing private data is preserved."
        }
        accountsStore = accounts
        accountCatalogs = accounts.state
        let teams = await Task.detached { ChildSessions.parseTeams((try? sealed.read("collapsed_teams", as: [String].self)) ?? []) }.value
        teamsStore = OrderedStatePersistence(initial: teams) { try sealed.write("collapsed_teams", $0) }
        teamCollapse = teamCollapse.loaded(teams)
        // Offline navigation index: last known sessions and computers until the catalog refreshes.
        // A preview opened while storage loads keeps its own catalog.
        guard !demo else { return }
        let indexed = messageState.conversationIndex.filter { connected.contains($0.target.connectionId) }
        catalogSessions = indexed.map { $0.session() }
        var seen = Set<MachineKey>()
        machines = indexed.compactMap { row in
            guard seen.insert(row.target.machineKey).inserted else { return nil }
            return MachineNames.apply(Machine(connectionId: row.target.connectionId, id: row.target.computerId, name: row.computerName,
                                              online: false, lastKnown: true), messageState.machineAliases)
        }
        publishSessions()
    }

    /// Saves pending debounced edits, as Android does when the app stops.
    func flushPrivateState() {
        retainOutbox()
        messages?.flushAsync()
        accountsStore?.flushAsync()
        teamsStore?.flushAsync()
        conversation.flushRead()
    }

    private func messageStateChanged(_ value: MessageState) {
        let old = messageState
        let namesChanged = old.machineAliases != value.machineAliases
        let draftsChanged = old.drafts != value.drafts
        messageState = value
        if namesChanged { machines = machines.map { MachineNames.apply($0, value.machineAliases) } }
        if namesChanged || draftsChanged { scheduleList() }
        conversation.messageStateChanged(old: old, new: value)
    }

    /// Retain identities still referenced by live delivery or the current presentation.
    func retainOutbox() {
        guard let conversation else { return }
        outboxPins.replace(conversation.deliveryFlights.union(conversation.receiptFlights.map { String($0.split(separator: ":").last ?? "") })
            .union(conversation.events.map(\.outgoingId).filter { !$0.isEmpty }))
    }

    /// Applies a private-state reducer now and schedules its debounced write.
    func editMessages(_ reduce: @escaping @Sendable (MessageState) -> MessageState) {
        guard let messages else {
            error = "Private storage is not ready."
            return
        }
        retainOutbox()
        messages.edit(reduce)
    }

    /// Writes a reducer before returning; transport may follow only after this succeeds.
    @discardableResult
    func durableMessages(_ reduce: @escaping @Sendable (MessageState) throws -> MessageState,
                         acknowledge: (@Sendable (MessageState, MessageState) -> MessageState)? = nil) async throws -> MessageState {
        guard let messages else { throw ZerusError.invalid("Private storage is not ready.") }
        retainOutbox()
        return try await messages.durable(reduce, acknowledge: acknowledge)
    }

    @discardableResult
    func flushMessages() async throws -> MessageState {
        guard let messages else { throw ZerusError.invalid("Private storage is not ready.") }
        retainOutbox()
        return try await messages.flush()
    }

    /// Replaces live rows with their read annotations when the catalog has not changed meanwhile.
    func annotateCatalog(_ rows: [Session], revision: Int) {
        guard revision == catalogRevision else { return }
        catalogSessions = rows
        publishSessions()
    }

    func scenePhaseChanged(_ phase: ScenePhase) {
        switch phase {
        case .active:
            if storageReady {
                refresh()
                startNotifications()
            }
        case .background:
            tasks.stopTaskSync()
            voice.background()
            notifications.setActive(false)
            AppDelegate.scheduleRefresh()
            flushPrivateState()
        default: break
        }
    }

    // MARK: Notifications

    /// Asks once for permission after a workspace exists, registers for wakes and drains events while active.
    private func startNotifications() {
        guard storageReady, !demo, !connections.isEmpty else { return }
        Task {
            await notifications.refreshAuthorization()
            let active = UIApplication.shared.applicationState == .active
            if notifications.authorization == .notDetermined && notifications.preferences.master {
                // The system prompt only appears in the foreground; a background launch asks later.
                if active { await notifications.requestAuthorization() }
            } else if notifications.allowed {
                UIApplication.shared.registerForRemoteNotifications()
                await notifications.registerAll()
            }
            if active { notifications.setActive(true) }
        }
    }

    /// Opens the freshly resolved current live session for a tapped card, never an archive.
    func openNotification(connection: String, computer: String, session: String) {
        func match() -> Session? {
            sessions.first { $0.target.connectionId == connection && $0.target.computerId == computer && $0.target.session == session && $0.target.archiveId.isEmpty }
        }
        if let found = match() {
            conversationRequest = found.target
            return
        }
        refresh(explicit: true)
        Task {
            for _ in 0..<20 {
                try? await Task.sleep(for: .milliseconds(250))
                if let found = match() {
                    conversationRequest = found.target
                    return
                }
            }
            notice = "The session is no longer available."
        }
    }

    // MARK: Pairing

    func handle(url: URL) {
        guard let invite = PairingInvite.parse(url.absoluteString) else {
            if url.scheme == "zerus" && url.host == "pair" { error = "This pairing link is not valid. Ask for a new invitation." }
            return
        }
        invitation = invite
    }

    /// Pairs and saves the credential; returns true on success.
    func pair(url: String, code: String) async -> Bool {
        guard storageReady, !busy else { return false }
        busy = true
        defer { busy = false }
        var saved = false
        do {
            let encrypted = WorkspaceProtocol.encryptedInvitationCode(code.trimmingCharacters(in: .whitespacesAndNewlines))
            let connection: Connection
            if encrypted { connection = try await client.prepareEncryptedPair(url: url, code: code) }
            else { connection = try await client.pair(url: url, code: code) }
            let next = connections + [connection]
            let store = connectionStore
            try await Task.detached { try store.save(next) }.value
            connections = next
            saved = true
            if demo { stopPreview() }
            if encrypted { pendingEncryptedPairings.insert(connection.id); try await client.enrollPrepared(connection) }
            refresh(explicit: true)
            startNotifications()
            return true
        } catch {
            self.error = Self.message(error, fallback: "Pairing failed.")
            return saved
        }
    }

    func resumeEncryptedPair(_ connection: Connection) async {
        guard !busy, connections.contains(connection), connection.protocolVersion == 2 else { return }
        busy = true; defer { busy = false }
        do { try await client.enrollPrepared(connection); refresh(explicit: true) }
        catch { self.error = Self.message(error, fallback: "Could not resume encrypted phone enrollment.") }
    }

    /// Legacy disconnect revokes the bearer; encrypted Forget removes local keys.
    func disconnect(_ connection: Connection) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        do {
            if connection.protocolVersion == 1 { do { try await client.revoke(connection) } catch let failure as RelayError where failure.status == 401 {} }
            let store = connectionStore
            let client = client
            let next = try await ConnectionRemoval.forget(connection, from: connections,
                eraseProtected: { try await client.forgetEncrypted($0) },
                saveIndex: { next in try await Task.detached { try store.save(next) }.value })
            connections = next
            pendingEncryptedPairings.remove(connection.id)
            accountsStore?.edit { rows in rows.filter { $0.key.connectionId != connection.id } }
            editMessages { state in
                var copy = state
                copy.conversationIndex = state.conversationIndex.filter { $0.target.connectionId != connection.id }
                return copy
            }
            flushPrivateState()
            catalogSessions.removeAll { $0.target.connectionId == connection.id }
            projects.removeAll { $0.key.connectionId == connection.id }
            if sessionProjectScope?.key.connectionId == connection.id { clearSessionScope() }
            machines.removeAll { $0.connectionId == connection.id }
            selectedMachines = selectedMachines.filter { $0.connectionId != connection.id }
            relayOperations[connection.id] = nil
            relayFeatures[connection.id] = nil
            machineOperations = machineOperations.filter { $0.key.connectionId != connection.id }
            machineFeatures = machineFeatures.filter { $0.key.connectionId != connection.id }
            publishSessions()
            playback.stop()
            viewer.clear()
            computerFiles.clear()
            notifications.forget(connection: connection.id)
            await voice.forget(connection: connection.id)
            await conversation.forget(connection: connection.id)
            taskDigests = taskDigests.filter { $0.key.connectionId != connection.id }
            await tasks.purge(connection.id)
        } catch {
            self.error = connection.protocolVersion == 2
                ? "Could not finish forgetting this encrypted connection. Try Forget again. \(Self.message(error, fallback: ""))"
                : "Could not revoke this phone on the gateway. The connection is preserved. \(Self.message(error, fallback: ""))"
        }
    }

    // MARK: Catalog

    @ObservationIgnored private var notificationPreferenceRevision = 0
    func confirmNotificationPreference(_ action: SessionAction) async {
        notificationPreferenceRevision += 1
        while true {
            let rows = catalogSessions, revision = catalogRevision, version = notificationPreferenceRevision
            let actions = messageState.sessionActions + [action]
            let merged = await Task.detached { SessionNotificationPreferences.mergeRows(rows,retained:rows,actions:actions) }.value
            guard revision == catalogRevision,version == notificationPreferenceRevision else { continue }
            catalogSessions = merged
            publishSessions()
            conversation.confirmNotificationPreference(action)
            return
        }
    }

    func refresh(explicit: Bool = false) {
        guard !demo, storageReady else { return }
        if explicit { showCatalogProgress() }
        if refreshTask != nil && !explicit { return }
        refreshTask?.cancel()
        catalogGeneration += 1
        let generation = catalogGeneration
        let original = connections
        catalogBusy = true
        if !explicit {
            progressTask?.cancel()
            progressTask = Task { [weak self] in
                try? await Task.sleep(for: .seconds(5))
                guard let self, !Task.isCancelled, self.catalogGeneration == generation, self.catalogBusy else { return }
                self.catalogProgress = true
            }
        }
        refreshTask = Task { [weak self] in
            await self?.loadCatalog(original, explicit: explicit, generation: generation)
        }
    }

    private func showCatalogProgress() { catalogProgress = true }

    private func loadCatalog(_ original: [Connection], explicit: Bool, generation: Int) async {
        defer {
            if generation == catalogGeneration {
                catalogBusy = false
                catalogProgress = false
                progressTask?.cancel()
                refreshTask = nil
                if connections != original { refresh() }
            }
        }
        var nextSessions: [Session] = []
        var nextMachines: [Machine] = []
        var nextProjects: [ProjectSummary] = []
        var nextAccounts: [AccountCatalog] = []
        var warnings: [String] = []
        var operations: [MachineKey: Set<String>] = [:]
        var features: [MachineKey: Set<String>] = [:]
        var digests: [ProjectKey: [String: String]] = [:]
        var reached = Set<String>()
        for connection in original {
            if explicit || relayOperations[connection.id] == nil {
                // Session discovery remains available if optional capability discovery fails.
                if let capabilities = try? await client.capabilities(connection) {
                    relayOperations[connection.id] = Set(capabilities.strings("operations"))
                    relayFeatures[connection.id] = Set(capabilities.strings("features"))
                }
            }
            do {
                let computers = try await client.computers(connection)
                if connection.protocolVersion == 2 { pendingEncryptedPairings.remove(connection.id) }
                let retained = catalogSessions, actions = messageState.sessionActions
                let previousMachines = machines, previousProjects = projects, previousAccounts = accountCatalogs
                let catalog = await Task.detached {
                    WorkspaceCatalogCache.preserve(connection, response: computers,
                        fresh: SessionCatalogParser.parse(connection, computers.objects("computers"),retained:retained,actions:actions),
                        sessions: retained, machines: previousMachines, projects: previousProjects, accounts: previousAccounts)
                }.value
                guard generation == catalogGeneration, connections == original else { return }
                nextMachines += catalog.machines
                nextAccounts += catalog.accounts
                operations.merge(catalog.operations) { $1 }
                features.merge(catalog.features) { $1 }
                nextSessions += catalog.projects.sessions
                nextProjects += catalog.projects.projects
                digests.merge(catalog.projects.taskDigests) { $1 }
                warnings += catalog.projects.unavailableComputers
                reached.insert(connection.id)
            } catch is CancellationError {
                return
            } catch {
                if generation == catalogGeneration {
                    self.error = "\(connection.displayName): \(ReadFailure.message(error, "sessions"))"
                }
            }
        }
        guard generation == catalogGeneration, connections == original else { return }
        let unavailable = Set(original.map(\.id)).subtracting(reached)
        if !unavailable.isEmpty {
            nextSessions += catalogSessions.filter { unavailable.contains($0.target.connectionId) }.map { row in
                var copy = row
                copy.raw["last_known"] = true
                return copy
            }
            nextMachines += machines.filter { unavailable.contains($0.connectionId) }.map { machine in
                var copy = machine
                copy.online = false
                copy.lastKnown = true
                return copy
            }
            nextProjects += projects.filter { unavailable.contains($0.key.connectionId) }
            digests.merge(taskDigests.filter { unavailable.contains($0.key.connectionId) }) { $1 }
        }
        let accounts = nextAccounts, successful = reached, connected = Set(original.map(\.id))
        while true {
            let incoming = nextSessions,retained = catalogSessions,actions = messageState.sessionActions,revision = catalogRevision,version = notificationPreferenceRevision
            let merged = await Task.detached { SessionNotificationPreferences.mergeRows(incoming,retained:retained,actions:actions) }.value
            guard generation == catalogGeneration,connections == original else { return }
            if revision != catalogRevision || version != notificationPreferenceRevision { continue }
            nextSessions = merged;break
        }
        accountsStore?.edit { old in AccountSnapshots.merge(old, accounts, successful: successful, connected: connected) }
        catalogSessions = nextSessions
        let aliases = messageState.machineAliases
        machines = nextMachines.map { MachineNames.apply($0, aliases) }
        machineOperations = operations
        machineFeatures = features
        projects = nextProjects
        projectWarnings = warnings
        taskDigests = digests
        taskDigestsAt = Date()
        if let scope = sessionProjectScope { sessionProjectScope = projects.first { $0.key == scope.key } ?? scope }
        publishSessions()
        conversation.refreshReadAttention()
        conversation.adoptSharedReads()
        let rows = nextSessions, hosts = nextMachines
        let index = await Task.detached { ConversationIndex.capture(rows, hosts) }.value
        if connections == original {
            editMessages { state in
                var copy = state
                copy.conversationIndex = index
                return copy
            }
        }
        configureArchive()
        conversation.watchHistoryHeads()
    }

    /// Merges live rows with archive pages and rebuilds the visible list.
    func publishSessions() {
        sessions = SessionCatalogProjection.prepare(catalog: catalogSessions, archives: archive?.rows ?? [], native: machineOperations, relay: relayOperations)
        scheduleList()
    }

    private func loadArchivePage(_ key: MachineKey, _ query: JSONObject) async throws -> ArchivePage {
        guard let connection = connections.first(where: { $0.id == key.connectionId }) else { throw ZerusError.invalid("Workspace disconnected") }
        let id = UUID().uuidString.lowercased()
        let initial = try await client.post(connection, computerId: key.computerId, operation: "archive_list", payload: query, id: id)
        let receipt = try await client.waitForReceipt(connection, initial: initial, maxPolls: 150, pollDelay: .milliseconds(200))
        guard receipt.string("request_id") == id, receipt.string("state") == "completed", let result = receipt.object("result") else {
            let message = receipt.string("error")
            throw ZerusError(.unavailable, message.isEmpty ? "Archive page is not available. Refresh to retry." : message)
        }
        guard result.string("request_id") == id, result.int("version") == 1, result.int64("from") == query.int64("from"),
              result.int64("until") == query.int64("until"), result.string("query") == query.string("query") else {
            throw ZerusError.mismatch("Computer returned another archive query")
        }
        let name = machines.first { $0.key == key }?.name ?? ""
        let machine: JSONObject = ["id": .string(key.computerId), "name": .string(name), "snapshot": .object(result)]
        let range = archive.range
        let rows = NativeSessions.sessions(connection, machine)
        guard rows.count == (result.array("sessions")?.count ?? -1), rows.allSatisfy({ row in
            let at = row.raw.double("archived_at", default: -1)
            return SessionFilters.archived(row) && at >= Double(range.from) && at < Double(range.until)
        }) else { throw ZerusError.mismatch("Invalid archived session identity") }
        let window = result.has("position") || result.has("previous_cursor")
        if window {
            guard case .string = result["previous_cursor"], let position = result["position"]?.numberValue, position == position.rounded() else {
                throw ZerusError.mismatch("Invalid archive window")
            }
        }
        let catalog = ProjectParser.parse(connection, [machine], rows)
        guard case .string(let cursor)? = result["next_cursor"], let total = result["total"]?.numberValue else {
            throw ZerusError.mismatch("Invalid archive page")
        }
        return ArchivePage(sessions: catalog.sessions, cursor: cursor, total: Int(total), previous: result.string("previous_cursor"),
                           position: window ? result.int("position") : nil)
    }

    var archiveMachines: [Machine] { machines.filter { selectedMachines.isEmpty || selectedMachines.contains($0.key) } }
    var archiveSupportedMachines: [Machine] {
        archiveMachines.filter {
            $0.online && machineOperations[$0.key]?.contains("archive_list") == true && relayOperations[$0.connectionId]?.contains("archive_list") == true
        }
    }

    func configureArchive() {
        archive.configure(machines: Set(archiveSupportedMachines.map(\.key)), query: sessionQuery.trimmingCharacters(in: .whitespaces),
                          visible: sessionFilter == .archive && !demo)
    }

    // MARK: Session list

    private func sessionScopeChanged() {
        scheduleList()
        configureArchive()
    }

    func toggleMachine(_ key: MachineKey) {
        if selectedMachines.contains(key) { selectedMachines.remove(key) } else { selectedMachines.insert(key) }
        sessionScopeChanged()
    }

    func allMachines() {
        selectedMachines = []
        sessionScopeChanged()
    }

    func toggleProject(_ key: ProjectKey) {
        if collapsedProjects.contains(key.key) { collapsedProjects.remove(key.key) } else { collapsedProjects.insert(key.key) }
    }

    /// The machine UUID of a target's computer, which parent links and collapsed parents name.
    func machineUuid(_ target: Target) -> String? { machine(target.machineKey)?.machineId }

    func teamCollapsed(_ session: Session) -> Bool { teamCollapse.collapsed(session, machine: machineUuid(session.target)) }

    /// Shows or hides a parent's subagents and child sessions, and remembers it across relaunches.
    func toggleTeam(_ session: Session) {
        let collapsed = !teamCollapsed(session)
        let saved = ChildSessions.teamId(machineUuid(session.target), session).flatMap { id in
            teamsStore?.edit { ChildSessions.withTeam($0, id, collapsed: collapsed) }
        }
        teamCollapse = teamCollapse.choose(session.target.key, collapsed: collapsed, saved: saved)
        scheduleList()
    }

    /// Grouped accounts, which label the cards' account marks; regrouped only when the catalogs change.
    var groupedAccounts: [GroupedAccount] {
        let source = displayedAccounts
        if let cached = groupedCache, cached.source == source { return cached.groups }
        let groups = AccountGrouping.group(source)
        groupedCache = (source, groups)
        return groups
    }

    func viewProjectSessions(_ project: ProjectSummary) {
        if sessionProjectScope == nil { unscoped = (sessionFilter, sessionQuery) }
        sessionProjectScope = project
        sessionFilter = .all
        sessionQuery = ""
        scheduleList()
    }

    func clearSessionScope() {
        guard sessionProjectScope != nil else { return }
        sessionProjectScope = nil
        if let unscoped {
            sessionFilter = unscoped.filter
            sessionQuery = unscoped.query
        }
        unscoped = nil
        scheduleList()
    }

    /// Prepares filters, counters and groups off the main actor; a newer request wins.
    func scheduleList() {
        listGeneration += 1
        let generation = listGeneration
        let sessions = self.sessions, machines = self.machines, projects = self.projects
        let selected = selectedMachines, query = sessionQuery, project = sessionProjectScope?.key, filter = sessionFilter
        let drafts = messageState.drafts, range = archive?.range ?? ArchiveRange(), teams = teamCollapse
        let later = Set((conversation?.readState.records ?? []).filter(\.reviewLater).map { ConversationReadPolicies.key($0.target) })
        Task { [weak self] in
            let prepared = await Task.detached(priority: .userInitiated) {
                SessionListPresentation.prepare(sessions: sessions, machines: machines, projects: projects, selectedMachines: selected,
                                                query: query, project: project, filter: filter, drafts: drafts, archiveRange: range,
                                                readAllCandidate: { row, online in
                                                    ReadAllPolicies.listCandidate(row, online: online, reviewLater: later.contains(ConversationReadPolicies.key(row.target)))
                                                }, teams: teams)
            }.value
            guard let self, self.listGeneration == generation else { return }
            self.listPresentation = prepared
            self.listReady = true
        }
    }

    func online(_ session: Session) -> Bool { machines.contains { $0.key == session.target.machineKey && $0.online } }
    func machine(_ key: MachineKey) -> Machine? { machines.first { $0.key == key } }

    // MARK: Machine labels

    func machineName(_ key: MachineKey, fallback: String = "") -> String {
        machines.first { $0.key == key }?.name ?? messageState.machineAliases.first { $0.key == key }?.name ?? (fallback.isEmpty ? key.computerId : fallback)
    }

    func machineAlias(_ key: MachineKey) -> String { messageState.machineAliases.first { $0.key == key }?.name ?? "" }
    func machineColor(_ key: MachineKey) -> String { MachineColors.color(messageState.machineColors, key) }
    func machineColorOverride(_ key: MachineKey) -> String? { messageState.machineColors.first { $0.key == key }?.hex }

    func renameMachine(_ key: MachineKey, _ name: String?) async -> Bool {
        guard storageReady, !demo, !machineNameSaving.contains(key), messages != nil else { return false }
        guard machines.contains(where: { $0.key == key }), connections.contains(where: { $0.id == key.connectionId }) else {
            error = "This machine is no longer connected to this workspace."
            return false
        }
        let checked: String?
        do { checked = try name.map(MachineNames.checked) } catch {
            self.error = (error as? LocalizedError)?.errorDescription ?? ""
            return false
        }
        machineNameSaving.insert(key)
        defer { machineNameSaving.remove(key) }
        let reduce: @Sendable (MessageState) -> MessageState = { state in
            var copy = state
            copy.machineAliases = state.machineAliases.filter { $0.key != key } + (checked.map { [MachineAlias(key: key, name: $0)] } ?? [])
            return copy
        }
        do {
            _ = try await durableMessages(reduce, acknowledge: { current, _ in reduce(current) })
            return true
        } catch {
            self.error = "Could not save the machine name. \(Self.message(error, fallback: ""))"
            return false
        }
    }

    func setMachineColor(_ key: MachineKey, _ hex: String?) async -> Bool {
        guard storageReady, !demo, !machineColorSaving.contains(key), messages != nil else { return false }
        let checked: String?
        do { checked = try hex.map(MachineColors.checked) } catch {
            self.error = (error as? LocalizedError)?.errorDescription ?? ""
            return false
        }
        machineColorSaving.insert(key)
        defer { machineColorSaving.remove(key) }
        let reduce: @Sendable (MessageState) -> MessageState = { state in
            var copy = state
            copy.machineColors = state.machineColors.filter { $0.key != key } + (checked.map { [MachineColorOverride(key: key, hex: $0)] } ?? [])
            return copy
        }
        do {
            _ = try await durableMessages(reduce, acknowledge: { current, _ in reduce(current) })
            return true
        } catch {
            self.error = "Could not save the machine color. \(Self.message(error, fallback: ""))"
            return false
        }
    }

    // MARK: Preview

    var displayedAccounts: [AccountCatalog] { demo ? AccountSnapshots.demo(now: Clock.seconds()) : accountCatalogs }

    func preview() {
        conversation.back()
        demo = true
        let names = [("demo", "Design workstation"), ("demo-laptop", "Preview laptop"), ("demo-build", "Preview build machine with a long display name")]
        machines = names.enumerated().map { index, item in
            Machine(connectionId: "demo", id: item.0, name: item.1, online: index < 2,
                    route: index == 0 ? .direct : .via(gatewayId: "demo", gatewayName: "Design workstation"),
                    hgsVersion: index == 0 ? "1.49.0" : "", lastSeenAt: index == 2 ? Clock.seconds() - 3600 : nil)
        }
        let preview = DemoCatalog.make()
        projects = preview.projects
        catalogSessions = preview.sessions
        machineOperations = [:]
        publishSessions()
    }

    func stopPreview() {
        conversation.back()
        demo = false
        machines = []
        projects = []
        catalogSessions = []
        publishSessions()
        refresh(explicit: true)
    }

    static func message(_ error: Error, fallback: String) -> String {
        if let urlError = error as? URLError {
            switch urlError.code {
            case .notConnectedToInternet, .networkConnectionLost: return "No network connection."
            case .timedOut: return "The gateway did not respond in time."
            case .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed: return "The gateway is unreachable."
            case .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateNotYetValid, .serverCertificateHasUnknownRoot:
                return "The gateway's HTTPS certificate is not trusted."
            default: break
            }
        }
        let text = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        return text.isEmpty ? fallback : text
    }
}
