package app.zerus.mobile

import org.json.JSONObject

/**
 * The person's latch statement on a child's question route (zerus-sa9qn.50), byte for byte as
 * services/mobile/crypto RoutePersonStatement frames it: a phone of the encrypted workspace signs it for
 * one gateway with its workspace signing key; the gateway's desktop service verifies it through its own
 * connector and sets the latch at most once (by request ID) before it expires.
 * Shared vector: services/mobile/crypto/tests/vectors/question-route-person-v1.json.
 */
internal data class RoutePersonStatement(val workspaceId:String,val deviceId:String,val gatewayId:String,val sessionId:String,
    val runId:String,val conversationId:String,val personOff:Boolean,val requestId:String,val issuedAt:Long,val expiresAt:Long) {
    init {
        listOf(workspaceId,deviceId,gatewayId,sessionId,requestId).forEach(WorkspaceProtocol::identifier)
        require(runId.isNotEmpty() && runId.toByteArray().size <= 128 && conversationId.isNotEmpty() && conversationId.toByteArray().size <= 256 &&
            (runId+conversationId).none { it < ' ' || it == '\u007f' }) { "Invalid question route identity." }
        require(issuedAt in 0 until expiresAt && expiresAt-issuedAt <= LIFETIME) { "Invalid question route statement time." }
    }
    fun frame():ByteArray = WorkspaceProtocol.frame(DOMAIN,WorkspaceProtocol.text(workspaceId),WorkspaceProtocol.text(deviceId),
        WorkspaceProtocol.text(gatewayId),WorkspaceProtocol.text(sessionId),WorkspaceProtocol.text(runId),WorkspaceProtocol.text(conversationId),
        byteArrayOf(if(personOff) 1 else 0),WorkspaceProtocol.text(requestId),WorkspaceProtocol.integer(issuedAt),WorkspaceProtocol.integer(expiresAt))
    fun json():JSONObject = JSONObject().put("workspace_id",workspaceId).put("device_id",deviceId).put("gateway_id",gatewayId)
        .put("session_id",sessionId).put("run_id",runId).put("conversation_id",conversationId).put("person_off",personOff)
        .put("request_id",requestId).put("issued_at",issuedAt).put("expires_at",expiresAt)
    companion object {
        const val DOMAIN = "zerus-question-route-person-v1"
        /** The longest a statement stays valid, in seconds. */
        const val LIFETIME = 300L
        fun parse(json:JSONObject):RoutePersonStatement {
            WorkspaceProtocol.exactFields(json,setOf("workspace_id","device_id","gateway_id","session_id","run_id","conversation_id",
                "person_off","request_id","issued_at","expires_at"))
            return RoutePersonStatement(WorkspaceProtocol.string(json,"workspace_id"),WorkspaceProtocol.string(json,"device_id"),
                WorkspaceProtocol.string(json,"gateway_id"),WorkspaceProtocol.string(json,"session_id"),WorkspaceProtocol.string(json,"run_id"),
                WorkspaceProtocol.string(json,"conversation_id"),json.get("person_off") as? Boolean ?: error("Invalid question route switch."),
                WorkspaceProtocol.string(json,"request_id"),WorkspaceProtocol.number(json,"issued_at"),WorkspaceProtocol.number(json,"expires_at"))
        }
    }
}
