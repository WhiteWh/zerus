import Foundation

/// A non-2xx gateway response. Statuses 400, 401, 403, 404, 409, 413 and 429
/// prove a mutation was not accepted; other failures leave delivery uncertain.
public struct RelayError: Error, LocalizedError, Equatable, Sendable {
    public let status: Int
    public let message: String
    public init(status: Int, message: String) {
        self.status = status
        self.message = message
    }
    public var errorDescription: String? { message }
    public static let rejectionStatuses: Set<Int> = [400, 401, 403, 404, 409, 413, 429]
    public var rejected: Bool { Self.rejectionStatuses.contains(status) }
}

/// A local refusal (nothing sent), a protocol identity mismatch or a size limit.
public struct ZerusError: Error, LocalizedError, Equatable, Sendable {
    public enum Kind: Sendable, Equatable { case invalid, mismatch, limit, unavailable }
    public let kind: Kind
    public let message: String
    public init(_ kind: Kind, _ message: String) {
        self.kind = kind
        self.message = message
    }
    public var errorDescription: String? { message }
    public static func invalid(_ message: String) -> ZerusError { ZerusError(.invalid, message) }
    public static func mismatch(_ message: String) -> ZerusError { ZerusError(.mismatch, message) }
}

/// Bounds a read that may poll; the slower branch is cancelled. Nothing is retried or resent.
public enum Deadline {
    public static let expired = ZerusError(.unavailable, "The check did not finish in time.")

    public static func within<T: Sendable>(_ limit: Duration, _ operation: @escaping @Sendable () async throws -> T) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(for: limit)
                throw expired
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else { throw expired }
            return first
        }
    }
}

/// HTTPS client for legacy and encrypted connections. It never follows redirects, stores no
/// cookies or cached responses and never retries a request by itself.
public struct RelayClient: Sendable {
    public static let responseLimit = 1024 * 1024
    public static let requestEnvelopeLimit = 30_408_704
    public static let requestLimit = 1024 * 1024
    public static let deviceName = "Zerus iOS"

    let session: URLSession
    let workspace: SQLiteWorkspaceJournal?
    let catalogNowSeconds: @Sendable () -> Int64

    public init(session: URLSession = URLSession(configuration: RelayClient.configuration()), workspaceJournal: SQLiteWorkspaceJournal? = nil,
                catalogNowSeconds: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970) }) {
        self.session = session
        self.workspace = workspaceJournal
        self.catalogNowSeconds = catalogNowSeconds
    }

    public static func configuration() -> URLSessionConfiguration {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = 35
        configuration.timeoutIntervalForResource = 300
        configuration.waitsForConnectivity = false
        configuration.httpMaximumConnectionsPerHost = 16
        return configuration
    }

    // MARK: Transport

    public func call(_ url: String, token: String, path: String, body: JSONObject? = nil, method: String? = nil) async throws -> JSONObject {
        guard let endpoint = URL(string: ManagedRelay.transport(url) + path) else { throw ZerusError.invalid("Invalid gateway address.") }
        var request = URLRequest(url: endpoint)
        request.httpMethod = method ?? (body == nil ? "GET" : "POST")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if !token.trimmingCharacters(in: .whitespaces).isEmpty { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let body {
            let data = try JSON.object(body).data()
            let limit = path == "/v2/requests" ? 41 * 1024 * 1024 : (path == "/v1/requests" ? Self.requestEnvelopeLimit : Self.requestLimit)
            guard data.count <= limit else { throw ZerusError(.limit, "The request exceeds the gateway's size limit.") }
            request.httpBody = data
            request.setValue("application/json; charset=utf-8", forHTTPHeaderField: "Content-Type")
        }
        let (bytes, response) = try await session.bytes(for: request, delegate: NoRedirects.shared)
        guard let http = response as? HTTPURLResponse else { throw ZerusError(.unavailable, "The gateway returned an invalid response.") }
        let catalog = path == "/v2/computers" || path.hasPrefix("/v2/computers?")
        let responseLimit = catalog ? 3 * Self.responseLimit : (path.hasPrefix("/v2/") ? 2 * Self.responseLimit : Self.responseLimit)
        if http.expectedContentLength > Int64(responseLimit) { throw Self.responseTooLarge }
        var data = Data()
        data.reserveCapacity(Int(max(0, min(http.expectedContentLength, Int64(responseLimit)))))
        for try await byte in bytes {
            guard data.count < responseLimit else { throw Self.responseTooLarge }
            data.append(byte)
        }
        guard (200..<300).contains(http.statusCode) else {
            let parsed = try? JSON.parse(data)
            let message = parsed?.string("error", "message") ?? ""
            throw RelayError(status: http.statusCode, message: message.isEmpty ? "Server returned \(http.statusCode)." : message)
        }
        if data.isEmpty || data.allSatisfy({ $0 == 0x20 || $0 == 0x0A || $0 == 0x0D || $0 == 0x09 }) { return JSONObject() }
        guard let object = try JSON.parse(data).object else { throw ZerusError(.unavailable, "The gateway returned an invalid response.") }
        return object
    }

    static let responseTooLarge = ZerusError(.limit, "Gateway response exceeded its safety limit.")

    // MARK: Device endpoints

    public func pair(url: String, code: String, deviceName: String = RelayClient.deviceName) async throws -> Connection {
        let normalized = try EndpointPolicy.normalize(url)
        let trimmed = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !WorkspaceProtocol.encryptedInvitationCode(trimmed) else { throw ZerusError.invalid("Use the encrypted phone invitation pairing flow.") }
        guard !trimmed.isEmpty else { throw ZerusError.invalid("Enter the invitation code shown on your computer.") }
        let result = try await call(normalized, token: "", path: "/v1/pair", body: ["code": .string(trimmed), "device_name": .string(deviceName)])
        guard case .string(let token)? = result["device_token"], !token.isEmpty else {
            throw ZerusError(.unavailable, "The gateway did not return a device credential.")
        }
        let host = URLComponents(string: normalized)?.host ?? "Workspace"
        let name = result.string("workspace_name")
        return Connection(id: UUID().uuidString.lowercased(), label: name.isEmpty ? host : name, url: normalized, token: token)
    }

    public func computers(_ connection: Connection) async throws -> JSONObject {
        if connection.protocolVersion == 2 { return try await encryptedComputers(connection) }
        return try await call(connection.url, token: connection.token, path: "/v1/computers")
    }

    public func capabilities(_ connection: Connection) async throws -> JSONObject {
        try await call(connection.url, token: connection.token, path: "/v\(connection.protocolVersion)/capabilities")
    }

    /// Revokes this phone's credential, queued requests and push registration.
    public func revoke(_ connection: Connection) async throws {
        guard connection.protocolVersion == 1 else { throw ZerusError.invalid("Encrypted connections are forgotten locally. Remove this phone from an enrolled computer to revoke it globally.") }
        _ = try await call(connection.url, token: connection.token, path: "/v1/device", method: "DELETE")
    }

    /// Registers this phone's APNs device token for generic wakes.
    public func registerPush(_ connection: Connection, provider: String, token: String) async throws {
        guard PushRegistration.providers.contains(provider), PushRegistration.validToken(token) else { throw ZerusError.invalid("Invalid push registration.") }
        _ = try await call(connection.url, token: connection.token, path: "/v\(connection.protocolVersion)/push", body: ["provider": .string(provider), "token": .string(token)])
    }

    public func events(_ connection: Connection, after: Int64, wait: Int = 25) async throws -> JSONObject {
        if connection.protocolVersion == 2 { return try await encryptedEvents(connection, after: after, wait: wait) }
        return try await call(connection.url, token: connection.token, path: "/v1/events?after=\(after)&wait=\(wait)")
    }

    // MARK: Requests

    static let childOperations: Set<String> = ["inspect", "history", "history_projection", "send", "send_receipt", "attachment_read", "file_read"]
    static let archiveOperations: Set<String> = ["inspect", "history", "history_projection", "process_output", "restore", "forget", "rename", "fork", "attachment_read", "file_read"]

    /// Submits one request with an immutable UUID. The caller has already made it durable.
    public func submit(_ connection: Connection, target: Target, operation: String, payload: JSONObject, id: String) async throws -> JSONObject {
        guard target.agentId.isEmpty || Self.childOperations.contains(operation) else {
            throw ZerusError.invalid("This control belongs to the parent session.")
        }
        let archive = payload.string("archive_id")
        guard (target.archiveId.isEmpty && archive.isEmpty) ||
              (Self.archiveOperations.contains(operation) && !target.archiveId.isEmpty && archive == target.archiveId) else {
            throw ZerusError.invalid("Archived conversations are read-only.")
        }
        return try await post(connection, computerId: target.computerId, session: target.session, operation: operation, payload: payload, id: id)
    }

    /// A computer-scoped request (empty session), such as catalog or speech operations.
    public func post(_ connection: Connection, computerId: String, session: String = "", operation: String, payload: JSONObject, id: String) async throws -> JSONObject {
        let body: JSONObject = ["request_id": .string(id), "computer_id": .string(computerId), "session": .string(session),
                                "operation": .string(operation), "payload": .object(payload)]
        let result: JSONObject
        if connection.protocolVersion == 2 { result = try await encryptedPost(connection, body: body) }
        else { result = try await call(connection.url, token: connection.token, path: "/v1/requests", body: body) }
        guard result.string("request_id") == id else { throw Self.mismatchedReceipt }
        return result
    }

    static let mismatchedReceipt = ZerusError.mismatch("Gateway returned a mismatched receipt. Delivery remains unconfirmed.")

    /// The status-only check of an unconfirmed request: its original receipt, polled until it settles, within `limit`.
    /// It never submits anything; on expiry the read or poll in progress is cancelled.
    public func boundedReceipt(_ connection: Connection, id: String, limit: Duration = .seconds(45),
                               pollDelay: Duration = .milliseconds(800)) async throws -> JSONObject {
        try await Deadline.within(limit) {
            try await self.waitForReceipt(connection, initial: try await self.receipt(connection, id: id), pollDelay: pollDelay)
        }
    }

    public func sendReceipt(_ connection: Connection, message: OutgoingMessage, intent: SendIntent) async throws -> SendReceiptProof? {
        try Task.checkCancellation()
        guard connection.id == message.target.connectionId, SendReceiptRecovery.matches(message, intent) else { return nil }
        let id = UUID().uuidString.lowercased()
        let payload = try SendReceiptRecovery.request(id, intent)
        let initial = try await submit(connection, target: message.target, operation: SendReceiptRecovery.operation, payload: payload, id: id)
        let receipt = try await waitForReceipt(connection, initial: initial, pollDelay: .milliseconds(100))
        guard receipt.string("request_id") == id, receipt.string("state") == "completed", let result = receipt.object("result") else { return nil }
        return SendReceiptRecovery.proof(id, intent, result)
    }

    /// One campaign, including the original journal read, fallback and durable settlement.
    public func checkDelivery(_ connection: Connection, message: OutgoingMessage, intent: SendIntent?, limit: Duration = .seconds(45)) async throws -> SendReceiptCheck {
        guard limit > .zero, limit <= .seconds(45) else { throw ZerusError.invalid("Invalid receipt deadline.") }
        let parts = limit.components
        let deadline = SendReceiptRecovery.now + Double(parts.seconds) + Double(parts.attoseconds) / 1e18
        return try await Deadline.within(limit) {
            var original: JSONObject?
            do { original = try await self.waitForReceipt(connection, initial: try await self.receipt(connection, id: message.requestId)) }
            catch is CancellationError { throw CancellationError() }
            catch { original = nil }
            if let original, original.string("state") == "failed" { return .original(original, deadline: deadline) }
            if let original, original.string("state") == "completed", let result = original.object("result"), ["submitted", "queued"].contains(result.string("status")) {
                if !message.hasOriginalSendIdentity { return .original(original, deadline: deadline) }
                if let saved = message.originalSendIntent(), let proof = SendReceiptRecovery.acknowledgement(saved, result) { return .native(saved, proof, deadline: deadline) }
            }
            try Task.checkCancellation()
            guard let intent, (!message.hasOriginalSendIdentity || message.originalSendIntent() == intent), SendReceiptRecovery.matches(message, intent), SendReceiptRecovery.now < deadline else { return .unconfirmed }
            let proof: SendReceiptProof?
            do { proof = try await self.sendReceipt(connection, message: message, intent: intent) }
            catch is CancellationError { throw CancellationError() }
            catch { proof = nil }
            guard let proof, SendReceiptRecovery.now < deadline else { return .unconfirmed }
            return .native(intent, proof, deadline: deadline)
        }
    }

    /// Reads the original receipt; it never creates a replacement request.
    public func receipt(_ connection: Connection, id: String) async throws -> JSONObject {
        if connection.protocolVersion == 2 { return try await encryptedReceipt(connection, id: id) }
        guard let escaped = id.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-_"))) else {
            throw ZerusError.invalid("Invalid request identity.")
        }
        let result = try await call(connection.url, token: connection.token, path: "/v1/requests/\(escaped)")
        guard result.string("request_id") == id else { throw Self.mismatchedReceipt }
        return result
    }

    /// Polls a queued or claimed receipt until it settles or the poll budget ends.
    public func waitForReceipt(_ connection: Connection, initial: JSONObject, maxPolls: Int = 35,
                               pollDelay: Duration = .milliseconds(800)) async throws -> JSONObject {
        var result = initial
        let delay = min(max(pollDelay, .milliseconds(50)), .seconds(1))
        for _ in 0..<maxPolls {
            guard ["queued", "claimed"].contains(result.string("state")) else { return result }
            try await Task.sleep(for: delay)
            result = try await receipt(connection, id: result.string("request_id"))
        }
        return result
    }

    // MARK: Conversations

    /// Reads one native conversation and verifies that the computer answered for this exact target.
    public func inspect(_ connection: Connection, target: Target) async throws -> JSONObject {
        var payload: JSONObject = [:]
        if !target.archiveId.isEmpty { payload["archive_id"] = .string(target.archiveId) }
        if !target.agentId.isEmpty {
            payload["agent_id"] = .string(target.agentId)
            payload["expected_run_id"] = .string(target.run)
            payload["expected_conversation_id"] = .string(target.parentConversation)
        }
        let activity = try await completed(connection, target: target, operation: "inspect", payload: payload,
                                           fallback: "Computer has not returned its activity yet. Refresh to check again.")
        if !target.agentId.isEmpty {
            guard activity.string("agent_id") == target.agentId, activity.string("parent_conversation_id") == target.parentConversation,
                  activity.string("run_id") == target.run, activity.string("name") == target.session else {
                throw ZerusError.mismatch("Computer returned a different child conversation.")
            }
        } else if !activity.string("agent_id").isEmpty {
            throw ZerusError.mismatch("Computer returned a child conversation for the parent.")
        }
        if !target.archiveId.isEmpty {
            guard activity.string("archive_id") == target.archiveId, activity.string("name") == target.session else {
                throw ZerusError.mismatch("Computer returned a different archived conversation.")
            }
        } else {
            let name = activity.string("name")
            guard activity.string("archive_id").isEmpty, activity.string("state") != "archived", name.isEmpty || name == target.session else {
                throw ZerusError.mismatch("Computer returned a different conversation for this session.")
            }
        }
        return activity
    }

    /// One canonical history page; its receipt must echo this exact request and conversation.
    public func history(_ connection: Connection, target: Target, direction: String = "", cursor: String = "", limit: Int = 100) async throws -> HistoryPage {
        let id = UUID().uuidString.lowercased()
        let payload = try HistoryRequests.payload(target, id: id, direction: direction, cursor: cursor, limit: limit)
        let receipt = try await waitForReceipt(connection, initial: submit(connection, target: target, operation: "history", payload: payload, id: id))
        guard receipt.string("state") == "completed", let result = receipt.object("result") else {
            let message = receipt.string("error")
            throw ZerusError(.unavailable, message.isEmpty ? "History is not available yet. Try again." : message)
        }
        return try await Task.detached(priority: .userInitiated) { try HistoryPage.parse(target: target, id: id, raw: result) }.value
    }

    /// One intact projection page; applying it is a separate display-cache operation.
    public func historyProjection(_ connection: Connection, target: Target, epoch: String, after: Int64, until: Int64) async throws -> HistoryProjection.Page {
        let id = UUID().uuidString.lowercased()
        let payload = try HistoryProjection.payload(target, id: id, epoch: epoch, after: after, until: until)
        let receipt = try await waitForReceipt(connection, initial: submit(connection, target: target, operation: "history_projection", payload: payload, id: id))
        guard receipt.string("state") == "completed", let result = receipt.object("result") else {
            throw ZerusError(.unavailable, "History changes are not available yet.")
        }
        return try await Task.detached(priority: .userInitiated) {
            try HistoryProjection.parse(target: target, id: id, epoch: epoch, after: after, until: until, raw: result)
        }.value
    }

    /// Shared read mark; the computer applies only current values, so each attempt has its own request.
    public func markRead(_ connection: Connection, target: Target, payload: JSONObject) async throws -> JSONObject {
        guard target.archiveId.isEmpty, target.agentId.isEmpty else { throw ZerusError.invalid("Read marks belong to the live parent conversation.") }
        let id = UUID().uuidString.lowercased()
        return try await waitForReceipt(connection, initial: submit(connection, target: target, operation: "mark_read", payload: payload, id: id))
    }

    /// A voice operation on the exact live conversation; the receipt must complete for this request.
    public func voice(_ connection: Connection, target: Target, operation: String, payload: JSONObject, id: String) async throws -> JSONObject {
        guard connection.id == target.connectionId, VoiceProtocol.operations.contains(operation), !target.session.isEmpty,
              payload.string("request_id") == id, payload.string("expected_run_id") == target.run,
              payload.string("expected_conversation_id") == target.conversation else { throw ZerusError.invalid("Invalid voice request.") }
        let receipt = try await waitForReceipt(connection, initial: submit(connection, target: target, operation: operation, payload: payload, id: id))
        guard receipt.string("request_id") == id, receipt.string("state") == "completed", let result = receipt.object("result") else {
            throw ZerusError(.unavailable, receipt.string("error").isEmpty ? "The computer has not confirmed this voice request. Check its status." : receipt.string("error"))
        }
        return result
    }

    /// A computer-scoped speech request (statistics, profiles or provider configuration) with an empty session.
    public func speech(_ connection: Connection, host: MachineKey, operation: String, payload: JSONObject,
                       id: String = UUID().uuidString.lowercased(), fallback: String) async throws -> JSONObject {
        guard host.connectionId == connection.id,
              ["speech_stats", "speech_profiles"].contains(operation) || SpeechConfigProtocol.operations.contains(operation) else {
            throw ZerusError.invalid("Invalid speech request.")
        }
        let receipt = try await waitForReceipt(connection, initial: post(connection, computerId: host.computerId, operation: operation, payload: payload, id: id))
        guard receipt.string("state") == "completed", let result = receipt.object("result") else {
            throw ZerusError(.unavailable, receipt.string("error").isEmpty ? fallback : receipt.string("error"))
        }
        return result
    }

    /// Submits and waits; a request that does not complete reports its error.
    public func completed(_ connection: Connection, target: Target, operation: String, payload: JSONObject,
                          id: String = UUID().uuidString.lowercased(), fallback: String) async throws -> JSONObject {
        let receipt = try await waitForReceipt(connection, initial: submit(connection, target: target, operation: operation, payload: payload, id: id))
        guard receipt.string("state") == "completed", let result = receipt.object("result") else {
            let message = receipt.string("error")
            throw ZerusError(.unavailable, message.isEmpty ? fallback : message)
        }
        return result
    }
}

/// Refuses every HTTP redirect so credentials never follow a Location header.
final class NoRedirects: NSObject, URLSessionTaskDelegate, Sendable {
    static let shared = NoRedirects()
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest) async -> URLRequest? { nil }
}
