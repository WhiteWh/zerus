package app.zerus.mobile

import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.util.Base64

/** Versioned wire encoding shared with services/mobile/crypto. No JSON serialization is signed. */
internal object WorkspaceProtocol {
    const val MAX_PAYLOAD = 30_408_704
    fun encryptedInvitationCode(value:String)=value.startsWith("ze") && value.contains('.')
    // Service scheduling hints only. Every request still owns a permanent endpoint identity.
    private val reads=setOf("send_receipt","analytics_read","archive_list","speech_config","speech_config_status","inspect","history","history_projection",
        "hooks_status","git_status","process_output","project_assignment","terminal_snapshot","catalog","dirs","worktrees","mark_read",
        "attachment_read","file_read","speech_profiles","speech_stats","speech_status","voice_profiles","voice_status")
    fun operationClass(operation:String)=if(operation in reads) "read" else "mutation"
    private val encoder = Base64.getUrlEncoder().withoutPadding()
    private val decoder = Base64.getUrlDecoder()
    fun encode(bytes: ByteArray): String = encoder.encodeToString(bytes)
    fun decode(value: String, size: Int? = null, max: Int = MAX_PAYLOAD): ByteArray {
        require(value.length <= ((max.toLong() * 4 + 2) / 3).toInt() && value.matches(Regex("[A-Za-z0-9_-]*"))) { "Invalid encrypted workspace encoding." }
        val bytes = decoder.decode(value)
        require((size == null || bytes.size == size) && bytes.size <= max && encode(bytes) == value) { "Invalid encrypted workspace encoding." }
        return bytes
    }
    fun frame(domain: String, vararg fields: ByteArray): ByteArray {
        val out = ByteArrayOutputStream()
        out.write(domain.toByteArray(Charsets.US_ASCII)); out.write(0)
        fields.forEach { out.write(ByteBuffer.allocate(4).putInt(it.size).array()); out.write(it) }
        return out.toByteArray()
    }
    fun integer(value: Long): ByteArray { require(value >= 0); return ByteBuffer.allocate(8).putLong(value).array() }
    fun text(value: String): ByteArray = value.toByteArray(Charsets.UTF_8)
    fun exactFields(json: JSONObject, fields: Set<String>) {
        require(json.keys().asSequence().toSet() == fields) { "Unsupported encrypted workspace state." }
    }
    fun string(json: JSONObject, key: String): String = (json.get(key) as? String)
        ?: error("Invalid encrypted workspace field.")
    fun number(json: JSONObject, key: String): Long {
        val raw = json.get(key)
        require(raw is Int || raw is Long) { "Invalid encrypted workspace integer." }
        return (raw as Number).toLong().also { require(it >= 0) }
    }
    fun identifier(value: String) {
        require(value.length in 1..128 && value.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it in "-_.:" }) { "Invalid encrypted workspace identity." }
    }
}

internal data class WorkspaceDevice(val deviceId: String, val role: String, val keyGeneration: Long,
    val signingPublicKey: String, val encryptionPublicKey: String) {
    init {
        WorkspaceProtocol.identifier(deviceId)
        require(role in setOf("computer", "phone") && keyGeneration > 0)
        WorkspaceProtocol.decode(signingPublicKey, 32)
        require(signingPublicKey !in WorkspaceRejectedKeys.signing && WorkspaceProtocol.decode(encryptionPublicKey, 32).any { it != 0.toByte() }) { "Invalid workspace public key." }
    }
    fun json(): JSONObject = JSONObject().put("device_id", deviceId).put("role", role).put("key_generation", keyGeneration)
        .put("signing_public_key", signingPublicKey).put("encryption_public_key", encryptionPublicKey)
    fun frame(): ByteArray = WorkspaceProtocol.frame("zerus-e2ee-device-v1",WorkspaceProtocol.text(deviceId),WorkspaceProtocol.text(role),
        WorkspaceProtocol.integer(keyGeneration),WorkspaceProtocol.decode(signingPublicKey,32),WorkspaceProtocol.decode(encryptionPublicKey,32))
    companion object {
        fun parse(json: JSONObject): WorkspaceDevice {
            WorkspaceProtocol.exactFields(json, setOf("device_id", "role", "key_generation", "signing_public_key", "encryption_public_key"))
            return WorkspaceDevice(WorkspaceProtocol.string(json,"device_id"), WorkspaceProtocol.string(json,"role"),
                WorkspaceProtocol.number(json,"key_generation"), WorkspaceProtocol.string(json,"signing_public_key"), WorkspaceProtocol.string(json,"encryption_public_key"))
        }
    }
}

/** Local secret state. It is only serialized into a Keystore-encrypted connection record. */
internal class WorkspaceIdentity(val device: WorkspaceDevice, val keys: WorkspaceCrypto.Keys) {
    init {
        require(device.signingPublicKey == WorkspaceProtocol.encode(keys.signingPublic) &&
            device.encryptionPublicKey == WorkspaceProtocol.encode(keys.encryptionPublic)) { "Workspace keys do not match this device." }
    }
    fun privateJson(): JSONObject = JSONObject().put("version",1).put("device",device.json())
        .put("signing_private_key",WorkspaceProtocol.encode(keys.signingSeed))
        .put("encryption_private_key",WorkspaceProtocol.encode(keys.encryptionPrivate))
    companion object {
        fun generate(id: String): WorkspaceIdentity {
            val keys = WorkspaceCrypto.generate()
            return WorkspaceIdentity(WorkspaceDevice(id,"phone",1,WorkspaceProtocol.encode(keys.signingPublic),WorkspaceProtocol.encode(keys.encryptionPublic)),keys)
        }
        fun parse(json: JSONObject): WorkspaceIdentity {
            WorkspaceProtocol.exactFields(json, setOf("version", "device", "signing_private_key", "encryption_private_key"))
            require(WorkspaceProtocol.number(json,"version") == 1L)
            val device = WorkspaceDevice.parse(json.getJSONObject("device"))
            return WorkspaceIdentity(device,WorkspaceCrypto.restore(WorkspaceProtocol.decode(WorkspaceProtocol.string(json,"signing_private_key"),32),
                WorkspaceProtocol.decode(WorkspaceProtocol.string(json,"encryption_private_key"),32), WorkspaceProtocol.decode(device.encryptionPublicKey,32)))
        }
    }
}

internal data class WorkspaceHeader(val workspaceId: String, val senderDeviceId: String, val recipientDeviceId: String,
    val senderKeyGeneration: Long, val recipientKeyGeneration: Long, val gatewayId: String, val routeId: String,
    val requestId: String, val kind: String, val operationClass: String, val expiresAt: Long,
    val membershipRevision: Long, val membershipHead: String) {
    init {
        listOf(workspaceId,senderDeviceId,recipientDeviceId,gatewayId,routeId,requestId).forEach(WorkspaceProtocol::identifier)
        require(senderKeyGeneration > 0 && recipientKeyGeneration > 0 && expiresAt > 0 && membershipRevision >= 0)
        WorkspaceProtocol.decode(membershipHead,32)
        require(kind in setOf("request","result","snapshot","membership") && operationClass in setOf("read","mutation","control"))
    }
    fun frame(): ByteArray = WorkspaceProtocol.frame("zerus-e2ee-header-v1", WorkspaceProtocol.text(workspaceId),
        WorkspaceProtocol.text(senderDeviceId),WorkspaceProtocol.text(recipientDeviceId),WorkspaceProtocol.integer(senderKeyGeneration),
        WorkspaceProtocol.integer(recipientKeyGeneration),WorkspaceProtocol.text(gatewayId),WorkspaceProtocol.text(routeId),
        WorkspaceProtocol.text(requestId),WorkspaceProtocol.text(kind),WorkspaceProtocol.text(operationClass),WorkspaceProtocol.integer(expiresAt),
        WorkspaceProtocol.integer(membershipRevision),WorkspaceProtocol.decode(membershipHead,32))
    fun json(): JSONObject = JSONObject().put("workspace_id",workspaceId).put("sender_device_id",senderDeviceId)
        .put("recipient_device_id",recipientDeviceId).put("sender_key_generation",senderKeyGeneration)
        .put("recipient_key_generation",recipientKeyGeneration).put("gateway_id",gatewayId).put("route_id",routeId)
        .put("request_id",requestId).put("kind",kind).put("operation_class",operationClass).put("expires_at",expiresAt)
        .put("membership_revision",membershipRevision).put("membership_head",membershipHead)
    companion object {
        fun parse(json: JSONObject): WorkspaceHeader {
            WorkspaceProtocol.exactFields(json,setOf("workspace_id","sender_device_id","recipient_device_id","sender_key_generation",
                "recipient_key_generation","gateway_id","route_id","request_id","kind","operation_class","expires_at","membership_revision","membership_head"))
            return WorkspaceHeader(WorkspaceProtocol.string(json,"workspace_id"),WorkspaceProtocol.string(json,"sender_device_id"),
                WorkspaceProtocol.string(json,"recipient_device_id"),WorkspaceProtocol.number(json,"sender_key_generation"),
                WorkspaceProtocol.number(json,"recipient_key_generation"),WorkspaceProtocol.string(json,"gateway_id"),WorkspaceProtocol.string(json,"route_id"),
                WorkspaceProtocol.string(json,"request_id"),WorkspaceProtocol.string(json,"kind"),WorkspaceProtocol.string(json,"operation_class"),WorkspaceProtocol.number(json,"expires_at"),
                WorkspaceProtocol.number(json,"membership_revision"),WorkspaceProtocol.string(json,"membership_head"))
        }
    }
}

internal data class WorkspaceEnvelope(val header: WorkspaceHeader, val enc: String, val ciphertext: String, val signature: String) {
    fun json(): JSONObject = JSONObject().put("version",1).put("header",header.json()).put("enc",enc).put("ciphertext",ciphertext).put("signature",signature)
    private fun signatureFrame(): ByteArray = WorkspaceProtocol.frame("zerus-e2ee-envelope-v1",header.frame(),
        WorkspaceProtocol.decode(enc,32),WorkspaceProtocol.decode(ciphertext,max=WorkspaceProtocol.MAX_PAYLOAD+16))
    /** Caller supplies an already authenticated member and the complete locally expected context. */
    fun open(identity: WorkspaceIdentity, sender: WorkspaceDevice, expected: WorkspaceHeader): ByteArray {
        require(header == expected && header.senderDeviceId == sender.deviceId && header.senderKeyGeneration == sender.keyGeneration &&
            header.recipientDeviceId == identity.device.deviceId && header.recipientKeyGeneration == identity.device.keyGeneration) { "Workspace envelope has a different identity." }
        WorkspaceCrypto.verify(WorkspaceProtocol.decode(sender.signingPublicKey,32),WorkspaceProtocol.decode(signature,64),signatureFrame())
        return WorkspaceCrypto.open(identity.keys,header.frame(),WorkspaceProtocol.decode(enc,32)+WorkspaceProtocol.decode(ciphertext,max=WorkspaceProtocol.MAX_PAYLOAD+16))
    }
    companion object {
        fun seal(identity: WorkspaceIdentity, recipient: WorkspaceDevice, header: WorkspaceHeader, plaintext: ByteArray): WorkspaceEnvelope {
            require(plaintext.size <= WorkspaceProtocol.MAX_PAYLOAD && header.senderDeviceId == identity.device.deviceId &&
                header.senderKeyGeneration == identity.device.keyGeneration && header.recipientDeviceId == recipient.deviceId &&
                header.recipientKeyGeneration == recipient.keyGeneration)
            val sealed = WorkspaceCrypto.seal(WorkspaceProtocol.decode(recipient.encryptionPublicKey,32),header.frame(),plaintext)
            val unsigned = WorkspaceEnvelope(header,WorkspaceProtocol.encode(sealed.copyOfRange(0,32)),WorkspaceProtocol.encode(sealed.copyOfRange(32,sealed.size)),"")
            return unsigned.copy(signature=WorkspaceProtocol.encode(WorkspaceCrypto.sign(identity.keys,unsigned.signatureFrame())))
        }
        fun parse(json: JSONObject): WorkspaceEnvelope {
            WorkspaceProtocol.exactFields(json,setOf("version","header","enc","ciphertext","signature"))
            require(WorkspaceProtocol.number(json,"version") == 1L)
            return WorkspaceEnvelope(WorkspaceHeader.parse(json.getJSONObject("header")),WorkspaceProtocol.string(json,"enc"),
                WorkspaceProtocol.string(json,"ciphertext"),WorkspaceProtocol.string(json,"signature")).also {
                WorkspaceProtocol.decode(it.enc,32);require(WorkspaceProtocol.decode(it.ciphertext,max=WorkspaceProtocol.MAX_PAYLOAD+16).size>=16);WorkspaceProtocol.decode(it.signature,64)
            }
        }
    }
}
