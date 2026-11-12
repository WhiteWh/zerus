import CryptoKit
import Foundation

private struct WorkspaceCatalogCandidate {
    let computer: String, row: JSONObject, route: WorkspaceRoute, expiresAt: Int64
}

extension RelayClient {
    public func encryptedEnrollmentPending(_ connection: Connection) async throws -> Bool {
        try await Task.detached { try self.journal().load(connection).enrollment == nil }.value
    }
    func journal() throws -> SQLiteWorkspaceJournal {
        guard let workspace else { throw ZerusError(.unavailable, "Encrypted workspace storage is unavailable. Plaintext fallback is disabled.") }
        return workspace
    }
    static var nowSeconds: Int64 { Int64(Date().timeIntervalSince1970) }

    /// Reserves private identity, exact proof and bearer before any enrollment HTTP.
    public func prepareEncryptedPair(url: String, code: String) async throws -> Connection {
        let journal = try journal()
        return try await Task.detached {
            let normalized = try EndpointPolicy.normalize(url), invitation = try WorkspaceInvitation.parse(code.trimmingCharacters(in: .whitespacesAndNewlines))
            let identity = try WorkspaceIdentity.generate(id: UUID().uuidString.lowercased())
            let candidate = try invitation.candidate(identity: identity, now: Self.nowSeconds)
            let random = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
            let connection = Connection(id: UUID().uuidString.lowercased(), label: "Workspace", url: normalized, token: WorkspaceProtocol.encode(random), protocolVersion: 2)
            try journal.create(connection, state: WorkspaceState(identity: identity, candidate: candidate, issuer: invitation.issuer,
                pinnedRevision: invitation.membershipRevision, pinnedHead: invitation.membershipHead))
            return connection
        }.value
    }

    /// Explicit first submit or Resume uses the same persisted candidate and bearer.
    public func enrollPrepared(_ connection: Connection) async throws {
        let state = try journal().load(connection)
        guard state.enrollment == nil else { throw ZerusError.invalid("This phone enrollment is already approved. Refresh to check computers.") }
        _ = try await call(connection.url, token: "", path: "/v2/enrollments", body: ["candidate": .object(state.candidate.json), "service_token": .string(connection.token)])
    }

    /// Automatic recovery is receipt-only. Every persisted history update uses a CAS.
    func synchronizeWorkspace(_ connection: Connection) async throws -> WorkspaceState {
        let journal = try journal(); var state = try journal.load(connection)
        if state.enrollment == nil {
            let status = try await call(connection.url, token: connection.token, path: "/v2/enrollments/\(state.identity.device.deviceId)")
            guard status.string("workspace_id") == state.candidate.workspaceId else { throw ZerusError.mismatch("Enrollment belongs to a different workspace.") }
            guard status.string("state") == "active" else { return state }
            let approval = try WorkspaceApproval.parse(status["enrollment"]?.object ?? JSONObject())
            try approval.verify(candidate: state.candidate, issuer: state.issuer)
            var next = state; next.enrollment = approval
            if state.membership == nil {
                let bootstrap = try WorkspaceBootstrap.parse(status["bootstrap"]?.object ?? JSONObject())
                next.membership = try bootstrap.verify(workspaceId: state.candidate.workspaceId, revision: state.pinnedRevision, head: state.pinnedHead, issuer: state.issuer)
            }
            try journal.update(connection, expected: state, next: next); state = next
        }
        var pages = 0
        while true {
            pages += 1
            guard pages <= 83, let membership = state.membership else { throw ZerusError(.limit, "Workspace history exceeded its bounded membership limit.") }
            let page = try await call(connection.url, token: connection.token, path: "/v2/membership?after=\(membership.revision)&limit=100")
            guard let rows = page["changes"]?.array, rows.count <= 100 else { throw ZerusError.invalid("Invalid workspace history page.") }
            let changes = try rows.map { try WorkspaceMembershipChange.parse($0.object ?? JSONObject()) }
            var nextMembership = membership
            for change in changes { nextMembership = try nextMembership.applying(change) }
            let latest = try WorkspaceProtocol.number(page, "membership_revision"), latestHead = try WorkspaceProtocol.string(page, "membership_head")
            guard latest >= nextMembership.revision, latest != nextMembership.revision || latestHead == nextMembership.head else { throw ZerusError.mismatch("Workspace membership was rolled back or forked.") }
            if !changes.isEmpty {
                var next = state; next.membership = nextMembership
                next.routes = next.routes.filter { nextMembership.members[$0.value.gatewayId] != nil }
                try journal.update(connection, expected: state, next: next, changes: changes); state = next
            }
            if page["next_after"]?.isNull == true {
                guard nextMembership.revision == latest else { throw ZerusError.mismatch("Workspace history page is incomplete.") }; break
            }
            guard !changes.isEmpty, try WorkspaceProtocol.number(page, "next_after") == nextMembership.revision else { throw ZerusError.invalid("Invalid workspace history cursor.") }
        }
        guard let approval = state.enrollment, let membership = state.membership,
              try journal.head(connection, revision: approval.membership.revision) == approval.membership.head(),
              membership.members[state.identity.device.deviceId] == state.identity.device else { throw ZerusError.mismatch("This phone is not on the workspace's verified enrollment history.") }
        return state
    }

    func encryptedComputers(_ connection: Connection) async throws -> JSONObject {
        let state = try await synchronizeWorkspace(connection), membership = try state.active(), journal = try journal()
        var candidates: [WorkspaceCatalogCandidate] = [], unavailable = Set<String>(), seen = Set<String>(), after = "", pages = 0, privateBytes = 0
        while true {
            pages += 1; guard pages <= 41 else { throw ZerusError(.limit, "The workspace computer catalog exceeds its safety limit.") }
            let escaped = after.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-_"))) ?? ""
            let page = try await call(connection.url, token: connection.token, path: "/v2/computers?limit=100" + (after.isEmpty ? "" : "&after=\(escaped)"))
            guard let rows = page["computers"]?.array, rows.count <= 100 else { throw ZerusError.invalid("Invalid encrypted computer catalog.") }
            for raw in rows {
                guard let row = raw.object else { throw ZerusError.invalid("Invalid encrypted computer row.") }
                if row["envelope"]?.isNull == true { continue }
                let envelope = try WorkspaceEnvelope.parse(row["envelope"]?.object ?? JSONObject()), h = envelope.header
                let computer = try WorkspaceProtocol.string(row, "computer_id"), gateway = try WorkspaceProtocol.string(row, "gateway_id")
                guard h.kind == "snapshot", h.operationClass == "read", h.workspaceId == membership.workspaceId, h.gatewayId == gateway,
                      h.senderDeviceId == gateway, h.routeId == row.string("route_id"), h.recipientDeviceId == state.identity.device.deviceId else { throw ZerusError.mismatch("Computer snapshot has a different route or identity.") }
                let sender = try membership.senderAtKnownHead(envelope, knownHead: journal.head(connection, revision: h.membershipRevision))
                guard sender.role == "computer" else { throw ZerusError.mismatch("Unrecognized executing computer.") }
                let bytes = try journal.openEnvelope(connection, envelope: envelope, expected: h)
                privateBytes += bytes.count
                guard privateBytes <= 16 * 1024 * 1024 else { throw ZerusError(.limit, "The encrypted computer catalog exceeds this phone's safety limit.") }
                guard bytes.count <= 1024 * 1024, var privateRow = try JSON.parse(bytes).object, let binding = privateRow["encrypted_transport"]?.object else { throw ZerusError.invalid("Invalid encrypted computer snapshot.") }
                let nativeId = try WorkspaceProtocol.string(privateRow, "machine_id")
                guard privateRow.string("id") == computer, nativeId == computer, binding.string("gateway_id") == gateway, binding.string("route_id") == h.routeId, binding.string("machine_id") == nativeId else { throw ZerusError.mismatch("Encrypted computer snapshot has a different native route.") }
                let route = try WorkspaceRoute.parse(["computer_id": .string(computer), "gateway_id": .string(gateway), "route_id": .string(h.routeId), "machine_id": .string(nativeId)])
                seen.insert(nativeId)
                guard seen.count <= 4096 else { throw ZerusError(.limit, "The workspace computer catalog exceeds its safety limit.") }
                if h.expiresAt <= catalogNowSeconds() {
                    // Parse only to authenticate the native cache identity; never publish expired content.
                    unavailable.insert(nativeId)
                    guard unavailable.count <= 4096 else { throw ZerusError(.limit, "The workspace computer catalog exceeds its safety limit.") }
                    continue
                }
                privateRow["online"] = .bool(row.bool("online"))
                candidates.append(WorkspaceCatalogCandidate(computer: computer, row: privateRow, route: route, expiresAt: h.expiresAt))
                guard candidates.count <= 4100 else { throw ZerusError(.limit, "The workspace computer catalog exceeds its safety limit.") }
            }
            if page["next_after"]?.isNull == true { break }
            let next = try WorkspaceProtocol.string(page, "next_after")
            guard (1...2048).contains(next.utf8.count), next != after else { throw ZerusError.invalid("Invalid computer catalog cursor.") }; after = next
        }
        let latest = try journal.load(connection)
        guard latest.membership == membership else { throw ZerusError.mismatch("Workspace membership changed while loading computers. Refresh again.") }
        try Task.checkCancellation()
        // Admit once after page/journal waits; alternatives survive until this instant.
        let admissionTime = catalogNowSeconds()
        var records: [String: JSONObject] = [:], routes: [String: WorkspaceRoute] = [:]
        for candidate in candidates {
            if candidate.expiresAt <= admissionTime { unavailable.insert(candidate.computer); continue }
            let computer = candidate.computer, online = candidate.row.bool("online")
            let selected = routes[computer], selectedOnline = records[computer]?.bool("online") == true
            if selected == nil || online && !selectedOnline || online == selectedOnline && candidate.route == state.routes[computer] {
                routes[computer] = candidate.route; records[computer] = candidate.row
            }
        }
        var next = latest; next.routes = routes; try journal.update(connection, expected: latest, next: next)
        return ["computers": .array(records.keys.sorted().map { .object(records[$0]!) }), "unavailable_computers": .array(unavailable.subtracting(records.keys).sorted().map(JSON.string))]
    }

    /// A phone's signed latch statement for the gateway that routes `computer` (zerus-sa9qn.50): built and signed once,
    /// when the person's action is sent; that gateway's own service verifies it. The payload of session_question_route_person.
    public func signRoutePerson(_ connection: Connection, computer: String, sessionId: String, run: String, conversation: String,
                         personOff: Bool, requestId: String) async throws -> JSONObject {
        let state = try await synchronizeWorkspace(connection), membership = try state.active()
        guard let route = state.routes[computer], membership.members[route.gatewayId]?.role == "computer" else {
            throw ZerusError.mismatch("Refresh this computer's encrypted route before sending.")
        }
        let now = Self.nowSeconds
        let statement = try RoutePersonStatement(workspaceId: membership.workspaceId, deviceId: state.identity.device.deviceId,
            gatewayId: route.gatewayId, sessionId: sessionId, runId: run, conversationId: conversation, personOff: personOff,
            requestId: requestId, issuedAt: now, expiresAt: now + RoutePersonStatement.lifetime)
        let signature = try WorkspaceCrypto.sign(state.identity.keys, message: statement.frame())
        return ["statement": .object(statement.json), "signature": .string(WorkspaceProtocol.encode(signature))]
    }

    func encryptedPost(_ connection: Connection, body: JSONObject) async throws -> JSONObject {
        let state = try await synchronizeWorkspace(connection), membership = try state.active(), journal = try journal()
        let id = try WorkspaceProtocol.string(body, "request_id"); try WorkspaceProtocol.identifier(id)
        let computer = try WorkspaceProtocol.string(body, "computer_id"), previous = try journal.recordedRequest(connection, id: id)
        let route = previous.map { WorkspaceRoute(computerId: computer, gatewayId: $0.gatewayId, routeId: $0.routeId, machineId: computer) } ?? state.routes[computer]
        guard let route, let recipient = membership.members[route.gatewayId], recipient.role == "computer" else {
            throw ZerusError.mismatch("Refresh this computer's encrypted route before sending.")
        }
        let bound = body.putting("encrypted_transport", .object(["gateway_id": .string(route.gatewayId), "route_id": .string(route.routeId), "machine_id": .string(route.machineId)]))
        let clear = try JSON.object(bound).data()
        guard clear.count <= WorkspaceProtocol.maxPayload else { throw ZerusError(.limit, "The request exceeds the encrypted transport limit.") }
        let header = try WorkspaceHeader(workspaceId: membership.workspaceId, senderDeviceId: state.identity.device.deviceId, recipientDeviceId: recipient.deviceId,
            senderKeyGeneration: state.identity.device.keyGeneration, recipientKeyGeneration: recipient.keyGeneration, gatewayId: route.gatewayId, routeId: route.routeId,
            requestId: id, kind: "request", operationClass: WorkspaceProtocol.operationClass(body.string("operation")), expiresAt: Self.nowSeconds + 300, membershipRevision: membership.revision, membershipHead: membership.head)
        let envelope = try WorkspaceEnvelope.seal(identity: state.identity, recipient: recipient, header: header, plaintext: clear)
        let retained = try journal.reserve(connection, id: id, plainHash: WorkspaceProtocol.encode(Data(SHA256.hash(data: clear))), envelope: envelope)
        let result = try await call(connection.url, token: connection.token, path: "/v2/requests", body: ["envelope": .object(retained.json)])
        return try decodeEncryptedReceipt(connection, state: state, outer: result, id: id, request: retained.header)
    }

    func encryptedReceipt(_ connection: Connection, id: String) async throws -> JSONObject {
        try WorkspaceProtocol.identifier(id)
        let state = try await synchronizeWorkspace(connection); _ = try state.active()
        let expected = try journal().request(connection, id: id)
        return try await decodeEncryptedReceipt(connection, state: state, outer: call(connection.url, token: connection.token, path: "/v2/requests/\(id)"), id: id, request: expected)
    }

    func decodeEncryptedReceipt(_ connection: Connection, state: WorkspaceState, outer: JSONObject, id: String, request: WorkspaceHeader) throws -> JSONObject {
        guard outer.string("request_id") == id else { throw Self.mismatchedReceipt }
        if outer["envelope"]?.isNull == true {
            let status = outer.string("state")
            return ["request_id": .string(id), "state": .string(["queued", "claimed"].contains(status) ? status : "uncertain")]
        }
        let envelope = try WorkspaceEnvelope.parse(outer["envelope"]?.object ?? JSONObject()), h = envelope.header
        guard h.workspaceId == request.workspaceId, h.requestId == id, h.senderDeviceId == request.recipientDeviceId, h.recipientDeviceId == request.senderDeviceId,
              h.gatewayId == request.gatewayId, h.routeId == request.routeId, h.kind == "result", h.operationClass == request.operationClass, h.expiresAt > Self.nowSeconds else { throw ZerusError.mismatch("Encrypted result belongs to a different request or route.") }
        let sender = try state.active().senderAtKnownHead(envelope, knownHead: journal().head(connection, revision: h.membershipRevision))
        guard sender.role == "computer" else { throw ZerusError.mismatch("Unrecognized executing computer.") }
        let clear = try journal().openEnvelope(connection, envelope: envelope, expected: h)
        guard clear.count <= 1024 * 1024, let result = try JSON.parse(clear).object, result.string("request_id") == id,
              ["completed", "failed", "uncertain"].contains(result.string("state")) else { throw Self.mismatchedReceipt }
        try journal().settle(connection, id: id); return result
    }

    /// Opaque service hints map only through an authenticated, freshly decrypted catalog.
    func encryptedEvents(_ connection: Connection, after: Int64, wait: Int) async throws -> JSONObject {
        guard after >= 0, (0...25).contains(wait) else { throw ZerusError.invalid("Invalid event cursor.") }
        let page = try await call(connection.url, token: connection.token, path: "/v2/events?after=\(after)&wait=\(wait)")
        guard let raw = page["events"]?.array, raw.count <= 100 else { throw ZerusError.invalid("Invalid encrypted event page.") }
        let records = raw.isEmpty ? [] : try await encryptedComputers(connection).objects("computers")
        return try WorkspaceNotifications.events(connection, records: records, page: page, after: after, now: Date().timeIntervalSince1970)
    }

    public func forgetEncrypted(_ connection: Connection) async throws { try await Task.detached { try self.journal().forget(connection) }.value }
}

/// Preserve prior display data without restoring expired action capabilities.
public enum WorkspaceCatalogCache {
    public static func preserve(_ connection: Connection, response: JSONObject, fresh: SessionCatalog, sessions: [Session], machines: [Machine], projects: [ProjectSummary], accounts: [AccountCatalog]) -> SessionCatalog {
        guard connection.protocolVersion == 2 else { return fresh }
        let unavailable = Set(response.strings("unavailable_computers")).subtracting(fresh.machines.map(\.id))
        let oldSessions = sessions.filter { $0.target.connectionId == connection.id && unavailable.contains($0.target.computerId) }.map { row in
            var copy = row; copy.raw["last_known"] = true; return copy
        }
        let oldMachines = machines.filter { $0.connectionId == connection.id && unavailable.contains($0.id) }.map { row in
            var copy = row; copy.online = false; copy.lastKnown = true; return copy
        }
        let oldAccounts = accounts.filter { $0.key.connectionId == connection.id && unavailable.contains($0.key.computerId) }.map { row in
            var copy = row; copy.available = false; copy.online = false; copy.stale = true; return copy
        }
        var merged = fresh.projects.projects, positions = Dictionary(uniqueKeysWithValues: fresh.projects.projects.enumerated().map { ($0.element.key, $0.offset) })
        for project in projects where project.key.connectionId == connection.id {
            let folders = project.folders.filter { unavailable.contains($0.computerId) }, computers = project.computerOrder.filter(unavailable.contains)
            if folders.isEmpty && computers.isEmpty { continue }
            if let position = positions[project.key] {
                var current = merged[position]
                for folder in folders where !current.folders.contains(where: { $0.computerId == folder.computerId && ($0.id.isEmpty ? $0.path : $0.id) == (folder.id.isEmpty ? folder.path : folder.id) }) { current.folders.append(folder) }
                for id in computers { current.addComputer(id, project.computers[id]!) }
                current.stale = true; merged[position] = current
            } else {
                var copy = project; copy.folders = folders; copy.computers = project.computers.filter { unavailable.contains($0.key) }; copy.computerOrder = computers; copy.stale = true
                positions[project.key] = merged.count; merged.append(copy)
            }
        }
        var result = fresh
        result.machines += oldMachines; result.accounts += oldAccounts
        result.projects.projects = merged; result.projects.sessions += oldSessions
        return result
    }
}

extension WorkspaceMembership {
    func senderAtKnownHead(_ envelope: WorkspaceEnvelope, knownHead: String?) throws -> WorkspaceDevice {
        let h = envelope.header
        guard h.workspaceId == workspaceId, h.membershipRevision <= revision, h.membershipHead == knownHead,
              h.membershipRevision != revision || h.membershipHead == head, let sender = members[h.senderDeviceId], let recipient = members[h.recipientDeviceId],
              sender.keyGeneration == h.senderKeyGeneration, recipient.keyGeneration == h.recipientKeyGeneration else { throw ZerusError.mismatch("Workspace payload belongs to an unknown membership history.") }
        return sender
    }
}
