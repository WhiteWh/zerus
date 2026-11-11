import Foundation

/// Versioned framing shared with services/mobile/crypto. JSON text is never signed.
public enum WorkspaceProtocol {
    public static let maxPayload = 30_408_704
    public static func encryptedInvitationCode(_ value: String) -> Bool { value.hasPrefix("ze") && value.contains(".") }
    // Service scheduling hints only. Every request still owns a permanent endpoint identity.
    static let reads: Set<String> = ["send_receipt", "analytics_read", "archive_list", "speech_config", "speech_config_status", "inspect", "history", "history_projection", "hooks_status", "git_status", "process_output", "project_assignment", "terminal_snapshot", "catalog", "dirs", "worktrees", "mark_read", "attachment_read", "file_read", "speech_profiles", "speech_stats", "speech_status", "voice_profiles", "voice_status"]
    static func operationClass(_ operation: String) -> String { reads.contains(operation) ? "read" : "mutation" }
    public static func encode(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    public static func decode(_ value: String, size: Int? = nil, max: Int = maxPayload) throws -> Data {
        guard value.utf8.count <= (max * 4 + 2) / 3,
              value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }),
              let data = Data(base64Encoded: value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/") + String(repeating: "=", count: (4 - value.utf8.count % 4) % 4)),
              data.count <= max, size == nil || data.count == size, encode(data) == value else {
            throw ZerusError.invalid("Invalid encrypted workspace encoding.")
        }
        return data
    }
    public static func frame(_ domain: String, _ fields: [Data]) -> Data {
        var out = Data(domain.utf8); out.append(0)
        for field in fields {
            precondition(field.count <= Int(UInt32.max))
            var count = UInt32(field.count).bigEndian
            withUnsafeBytes(of: &count) { out.append(contentsOf: $0) }
            out.append(field)
        }
        return out
    }
    public static func integer(_ value: Int64) throws -> Data {
        guard value >= 0 else { throw ZerusError.invalid("Invalid encrypted workspace integer.") }
        var number = UInt64(value).bigEndian
        return withUnsafeBytes(of: &number) { Data($0) }
    }
    static func fields(_ json: JSONObject, _ names: Set<String>) throws {
        guard Set(json.keys) == names else { throw ZerusError.invalid("Unsupported encrypted workspace state.") }
    }
    static func string(_ json: JSONObject, _ key: String) throws -> String {
        guard case .string(let value)? = json[key] else { throw ZerusError.invalid("Invalid encrypted workspace field.") }
        return value
    }
    static func number(_ json: JSONObject, _ key: String) throws -> Int64 {
        guard case .int(let value)? = json[key], value >= 0 else { throw ZerusError.invalid("Invalid encrypted workspace integer.") }
        return value
    }
    static func identifier(_ value: String) throws {
        guard (1...128).contains(value.utf8.count), value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || [45,95,46,58].contains($0) }) else {
            throw ZerusError.invalid("Invalid encrypted workspace identity.")
        }
    }
}

public struct WorkspaceDevice: Hashable, Sendable {
    public let deviceId: String
    public let role: String
    public let keyGeneration: Int64
    public let signingPublicKey: String
    public let encryptionPublicKey: String

    public init(deviceId: String, role: String, keyGeneration: Int64, signingPublicKey: String, encryptionPublicKey: String) throws {
        try WorkspaceProtocol.identifier(deviceId)
        guard ["computer", "phone"].contains(role), keyGeneration > 0 else { throw ZerusError.invalid("Invalid workspace device.") }
        _ = try WorkspaceProtocol.decode(signingPublicKey, size: 32)
        guard !WorkspaceRejectedKeys.signing.contains(signingPublicKey), try WorkspaceProtocol.decode(encryptionPublicKey, size: 32).contains(where: { $0 != 0 }) else { throw ZerusError.invalid("Invalid workspace public key.") }
        self.deviceId = deviceId; self.role = role; self.keyGeneration = keyGeneration
        self.signingPublicKey = signingPublicKey; self.encryptionPublicKey = encryptionPublicKey
    }
    public var json: JSONObject { ["device_id": .string(deviceId), "role": .string(role), "key_generation": .int(keyGeneration),
                                  "signing_public_key": .string(signingPublicKey), "encryption_public_key": .string(encryptionPublicKey)] }
    public func frame() throws -> Data {
        try WorkspaceProtocol.frame("zerus-e2ee-device-v1", [Data(deviceId.utf8), Data(role.utf8), WorkspaceProtocol.integer(keyGeneration),
            WorkspaceProtocol.decode(signingPublicKey, size: 32), WorkspaceProtocol.decode(encryptionPublicKey, size: 32)])
    }
    public static func parse(_ json: JSONObject) throws -> WorkspaceDevice {
        try WorkspaceProtocol.fields(json, ["device_id", "role", "key_generation", "signing_public_key", "encryption_public_key"])
        return try WorkspaceDevice(deviceId: WorkspaceProtocol.string(json, "device_id"), role: WorkspaceProtocol.string(json, "role"),
                                   keyGeneration: WorkspaceProtocol.number(json, "key_generation"), signingPublicKey: WorkspaceProtocol.string(json, "signing_public_key"),
                                   encryptionPublicKey: WorkspaceProtocol.string(json, "encryption_public_key"))
    }
}

/// Private state is written only inside the existing ThisDeviceOnly connection item.
public struct WorkspaceIdentity: Sendable {
    public let device: WorkspaceDevice
    let keys: WorkspaceCrypto.Keys

    init(device: WorkspaceDevice, keys: WorkspaceCrypto.Keys) throws {
        guard device.signingPublicKey == WorkspaceProtocol.encode(keys.signingPublic),
              device.encryptionPublicKey == WorkspaceProtocol.encode(keys.encryptionPublic) else {
            throw ZerusError.mismatch("Workspace keys do not match this device.")
        }
        self.device = device; self.keys = keys
    }
    public static func generate(id: String) throws -> WorkspaceIdentity {
        let keys = WorkspaceCrypto.Keys()
        return try WorkspaceIdentity(device: WorkspaceDevice(deviceId: id, role: "phone", keyGeneration: 1,
                                                              signingPublicKey: WorkspaceProtocol.encode(keys.signingPublic),
                                                              encryptionPublicKey: WorkspaceProtocol.encode(keys.encryptionPublic)), keys: keys)
    }
    var privateJSON: JSONObject { ["version": 1, "device": .object(device.json),
                                  "signing_private_key": .string(WorkspaceProtocol.encode(keys.signingSeed)),
                                  "encryption_private_key": .string(WorkspaceProtocol.encode(keys.encryptionPrivate))] }
    static func parse(_ json: JSONObject) throws -> WorkspaceIdentity {
        try WorkspaceProtocol.fields(json, ["version", "device", "signing_private_key", "encryption_private_key"])
        guard try WorkspaceProtocol.number(json, "version") == 1, let device = json["device"]?.object else { throw ZerusError.invalid("Unsupported workspace identity.") }
        return try WorkspaceIdentity(device: WorkspaceDevice.parse(device), keys: WorkspaceCrypto.Keys(
            signingSeed: WorkspaceProtocol.decode(WorkspaceProtocol.string(json, "signing_private_key"), size: 32),
            encryptionPrivate: WorkspaceProtocol.decode(WorkspaceProtocol.string(json, "encryption_private_key"), size: 32)))
    }
}

public struct WorkspaceHeader: Hashable, Sendable {
    public let workspaceId: String
    public let senderDeviceId: String
    public let recipientDeviceId: String
    public let senderKeyGeneration: Int64
    public let recipientKeyGeneration: Int64
    public let gatewayId: String
    public let routeId: String
    public let requestId: String
    public let kind: String
    public let operationClass: String
    public let expiresAt: Int64
    public let membershipRevision: Int64
    public let membershipHead: String

    public init(workspaceId: String, senderDeviceId: String, recipientDeviceId: String, senderKeyGeneration: Int64,
                recipientKeyGeneration: Int64, gatewayId: String, routeId: String, requestId: String, kind: String,
                operationClass: String, expiresAt: Int64, membershipRevision: Int64, membershipHead: String) throws {
        for id in [workspaceId, senderDeviceId, recipientDeviceId, gatewayId, routeId, requestId] { try WorkspaceProtocol.identifier(id) }
        guard senderKeyGeneration > 0, recipientKeyGeneration > 0, expiresAt > 0, membershipRevision >= 0,
              ["request", "result", "snapshot", "membership"].contains(kind), ["read", "mutation", "control"].contains(operationClass) else {
            throw ZerusError.invalid("Invalid encrypted workspace header.")
        }
        self.workspaceId = workspaceId; self.senderDeviceId = senderDeviceId; self.recipientDeviceId = recipientDeviceId
        self.senderKeyGeneration = senderKeyGeneration; self.recipientKeyGeneration = recipientKeyGeneration
        self.gatewayId = gatewayId; self.routeId = routeId; self.requestId = requestId; self.kind = kind
        self.operationClass = operationClass; self.expiresAt = expiresAt
        _ = try WorkspaceProtocol.decode(membershipHead, size: 32)
        self.membershipRevision = membershipRevision; self.membershipHead = membershipHead
    }
    public func frame() throws -> Data {
        try WorkspaceProtocol.frame("zerus-e2ee-header-v1", [Data(workspaceId.utf8), Data(senderDeviceId.utf8), Data(recipientDeviceId.utf8),
            WorkspaceProtocol.integer(senderKeyGeneration), WorkspaceProtocol.integer(recipientKeyGeneration), Data(gatewayId.utf8),
            Data(routeId.utf8), Data(requestId.utf8), Data(kind.utf8), Data(operationClass.utf8), WorkspaceProtocol.integer(expiresAt),
            WorkspaceProtocol.integer(membershipRevision), WorkspaceProtocol.decode(membershipHead, size: 32)])
    }
    public var json: JSONObject { ["workspace_id": .string(workspaceId), "sender_device_id": .string(senderDeviceId), "recipient_device_id": .string(recipientDeviceId),
                                  "sender_key_generation": .int(senderKeyGeneration), "recipient_key_generation": .int(recipientKeyGeneration), "gateway_id": .string(gatewayId),
                                  "route_id": .string(routeId), "request_id": .string(requestId), "kind": .string(kind), "operation_class": .string(operationClass), "expires_at": .int(expiresAt),
                                  "membership_revision": .int(membershipRevision), "membership_head": .string(membershipHead)] }
    public static func parse(_ json: JSONObject) throws -> WorkspaceHeader {
        try WorkspaceProtocol.fields(json, ["workspace_id", "sender_device_id", "recipient_device_id", "sender_key_generation", "recipient_key_generation", "gateway_id", "route_id", "request_id", "kind", "operation_class", "expires_at", "membership_revision", "membership_head"])
        return try WorkspaceHeader(workspaceId: WorkspaceProtocol.string(json, "workspace_id"), senderDeviceId: WorkspaceProtocol.string(json, "sender_device_id"),
            recipientDeviceId: WorkspaceProtocol.string(json, "recipient_device_id"), senderKeyGeneration: WorkspaceProtocol.number(json, "sender_key_generation"),
            recipientKeyGeneration: WorkspaceProtocol.number(json, "recipient_key_generation"), gatewayId: WorkspaceProtocol.string(json, "gateway_id"),
            routeId: WorkspaceProtocol.string(json, "route_id"), requestId: WorkspaceProtocol.string(json, "request_id"), kind: WorkspaceProtocol.string(json, "kind"),
            operationClass: WorkspaceProtocol.string(json, "operation_class"), expiresAt: WorkspaceProtocol.number(json, "expires_at"),
            membershipRevision: WorkspaceProtocol.number(json, "membership_revision"), membershipHead: WorkspaceProtocol.string(json, "membership_head"))
    }
}

public struct WorkspaceEnvelope: Sendable {
    public let header: WorkspaceHeader
    public let enc: String
    public let ciphertext: String
    public let signature: String
    public var json: JSONObject { ["version": 1, "header": .object(header.json), "enc": .string(enc), "ciphertext": .string(ciphertext), "signature": .string(signature)] }
    func signatureFrame() throws -> Data {
        try WorkspaceProtocol.frame("zerus-e2ee-envelope-v1", [header.frame(), WorkspaceProtocol.decode(enc, size: 32), WorkspaceProtocol.decode(ciphertext, max: WorkspaceProtocol.maxPayload + 16)])
    }
    /// Caller supplies a verified member and the complete locally expected context.
    public func open(identity: WorkspaceIdentity, sender: WorkspaceDevice, expected: WorkspaceHeader) throws -> Data {
        guard header == expected, header.senderDeviceId == sender.deviceId, header.senderKeyGeneration == sender.keyGeneration,
              header.recipientDeviceId == identity.device.deviceId, header.recipientKeyGeneration == identity.device.keyGeneration else {
            throw ZerusError.mismatch("Workspace envelope has a different identity.")
        }
        try WorkspaceCrypto.verify(publicKey: WorkspaceProtocol.decode(sender.signingPublicKey, size: 32), signature: WorkspaceProtocol.decode(signature, size: 64), message: signatureFrame())
        return try WorkspaceCrypto.open(identity.keys, info: header.frame(), sealed: WorkspaceProtocol.decode(enc, size: 32) + WorkspaceProtocol.decode(ciphertext, max: WorkspaceProtocol.maxPayload + 16))
    }
    public static func seal(identity: WorkspaceIdentity, recipient: WorkspaceDevice, header: WorkspaceHeader, plaintext: Data) throws -> WorkspaceEnvelope {
        guard plaintext.count <= WorkspaceProtocol.maxPayload, header.senderDeviceId == identity.device.deviceId,
              header.senderKeyGeneration == identity.device.keyGeneration, header.recipientDeviceId == recipient.deviceId,
              header.recipientKeyGeneration == recipient.keyGeneration else { throw ZerusError.mismatch("Workspace envelope has a different identity.") }
        let sealed = try WorkspaceCrypto.seal(recipientPublic: WorkspaceProtocol.decode(recipient.encryptionPublicKey, size: 32), info: header.frame(), plaintext: plaintext)
        let unsigned = WorkspaceEnvelope(header: header, enc: WorkspaceProtocol.encode(Data(sealed.prefix(32))), ciphertext: WorkspaceProtocol.encode(Data(sealed.dropFirst(32))), signature: "")
        return try WorkspaceEnvelope(header: header, enc: unsigned.enc, ciphertext: unsigned.ciphertext, signature: WorkspaceProtocol.encode(WorkspaceCrypto.sign(identity.keys, message: unsigned.signatureFrame())))
    }
    public static func parse(_ json: JSONObject) throws -> WorkspaceEnvelope {
        try WorkspaceProtocol.fields(json, ["version", "header", "enc", "ciphertext", "signature"])
        guard try WorkspaceProtocol.number(json, "version") == 1, let header = json["header"]?.object else { throw ZerusError.invalid("Unsupported encrypted workspace envelope.") }
        let result = try WorkspaceEnvelope(header: WorkspaceHeader.parse(header), enc: WorkspaceProtocol.string(json, "enc"), ciphertext: WorkspaceProtocol.string(json, "ciphertext"), signature: WorkspaceProtocol.string(json, "signature"))
        _ = try WorkspaceProtocol.decode(result.enc, size: 32)
        guard try WorkspaceProtocol.decode(result.ciphertext, max: WorkspaceProtocol.maxPayload + 16).count >= 16 else { throw ZerusError.invalid("Invalid encrypted workspace payload.") }
        _ = try WorkspaceProtocol.decode(result.signature, size: 64)
        return result
    }
}
