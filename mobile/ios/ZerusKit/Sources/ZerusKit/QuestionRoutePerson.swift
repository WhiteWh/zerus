import Foundation

/// The person's latch statement on a child's question route (zerus-sa9qn.50), byte for byte as
/// services/mobile/crypto `RoutePersonStatement` frames it: a phone of the encrypted workspace signs it for
/// one gateway with its workspace signing key; the gateway's desktop service verifies it through its own
/// connector and sets the latch at most once (by request ID) before it expires.
/// Shared vector: services/mobile/crypto/tests/vectors/question-route-person-v1.json.
public struct RoutePersonStatement: Equatable, Sendable {
    public static let domain = "zerus-question-route-person-v1"
    /// The longest a statement stays valid, in seconds.
    public static let lifetime: Int64 = 300
    public let workspaceId: String, deviceId: String, gatewayId: String, sessionId: String
    public let runId: String, conversationId: String
    public let personOff: Bool
    public let requestId: String
    public let issuedAt: Int64, expiresAt: Int64

    public init(workspaceId: String, deviceId: String, gatewayId: String, sessionId: String, runId: String, conversationId: String,
                personOff: Bool, requestId: String, issuedAt: Int64, expiresAt: Int64) throws {
        for id in [workspaceId, deviceId, gatewayId, sessionId, requestId] { try WorkspaceProtocol.identifier(id) }
        guard !runId.isEmpty, runId.utf8.count <= 128, !conversationId.isEmpty, conversationId.utf8.count <= 256,
              !(runId + conversationId).unicodeScalars.contains(where: { $0.value < 0x20 || $0.value == 0x7f }) else {
            throw ZerusError.invalid("Invalid question route identity.")
        }
        guard issuedAt >= 0, issuedAt < expiresAt, expiresAt - issuedAt <= Self.lifetime else {
            throw ZerusError.invalid("Invalid question route statement time.")
        }
        self.workspaceId = workspaceId; self.deviceId = deviceId; self.gatewayId = gatewayId; self.sessionId = sessionId
        self.runId = runId; self.conversationId = conversationId; self.personOff = personOff; self.requestId = requestId
        self.issuedAt = issuedAt; self.expiresAt = expiresAt
    }

    public func frame() throws -> Data {
        WorkspaceProtocol.frame(Self.domain, [Data(workspaceId.utf8), Data(deviceId.utf8), Data(gatewayId.utf8), Data(sessionId.utf8),
                                              Data(runId.utf8), Data(conversationId.utf8), Data([personOff ? 1 : 0]), Data(requestId.utf8),
                                              try WorkspaceProtocol.integer(issuedAt), try WorkspaceProtocol.integer(expiresAt)])
    }

    public var json: JSONObject {
        ["workspace_id": .string(workspaceId), "device_id": .string(deviceId), "gateway_id": .string(gatewayId),
         "session_id": .string(sessionId), "run_id": .string(runId), "conversation_id": .string(conversationId),
         "person_off": .bool(personOff), "request_id": .string(requestId), "issued_at": .int(issuedAt), "expires_at": .int(expiresAt)]
    }

    public static func parse(_ json: JSONObject) throws -> RoutePersonStatement {
        try WorkspaceProtocol.fields(json, ["workspace_id", "device_id", "gateway_id", "session_id", "run_id", "conversation_id",
                                            "person_off", "request_id", "issued_at", "expires_at"])
        guard case .bool(let off)? = json["person_off"] else { throw ZerusError.invalid("Invalid question route switch.") }
        return try RoutePersonStatement(workspaceId: WorkspaceProtocol.string(json, "workspace_id"), deviceId: WorkspaceProtocol.string(json, "device_id"),
            gatewayId: WorkspaceProtocol.string(json, "gateway_id"), sessionId: WorkspaceProtocol.string(json, "session_id"),
            runId: WorkspaceProtocol.string(json, "run_id"), conversationId: WorkspaceProtocol.string(json, "conversation_id"), personOff: off,
            requestId: WorkspaceProtocol.string(json, "request_id"), issuedAt: WorkspaceProtocol.number(json, "issued_at"),
            expiresAt: WorkspaceProtocol.number(json, "expires_at"))
    }
}
