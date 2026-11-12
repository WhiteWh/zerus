import XCTest
@testable import ZerusKit

/// The person's latch statement (zerus-sa9qn.50) against the shared crypto vector, and its session action;
/// ported from Android QuestionRoutePersonTest.
final class QuestionRoutePersonTests: XCTestCase {
    private func vector() throws -> JSONObject { try XCTUnwrap(try SharedFixture.json("question-route-person-v1.json").object) }
    private func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    func testTheStatementFramesAndVerifiesAsTheSharedVector() throws {
        let v = try vector()
        let statement = try RoutePersonStatement.parse(try XCTUnwrap(v.object("statement")))
        XCTAssertEqual(v.string("domain"), RoutePersonStatement.domain)
        XCTAssertEqual(v.string("canonical_hex"), hex(try statement.frame()))
        XCTAssertEqual(Set(statement.json.keys), Set(try XCTUnwrap(v.object("statement")).keys))
        XCTAssertEqual(try RoutePersonStatement.parse(statement.json), statement)
        let publicKey = try WorkspaceProtocol.decode(v.string("phone_signing_public_key"), size: 32)
        try WorkspaceCrypto.verify(publicKey: publicKey, signature: WorkspaceProtocol.decode(v.string("signature"), size: 64), message: statement.frame())
        // The vector's seed is the phone's key; CryptoKit's own signature also verifies.
        let keys = try WorkspaceCrypto.Keys(signingSeed: WorkspaceProtocol.decode(v.string("phone_signing_seed"), size: 32),
                                            encryptionPrivate: Data(repeating: 7, count: 32))
        XCTAssertEqual(keys.signingPublic, publicKey)
        try WorkspaceCrypto.verify(publicKey: publicKey, signature: WorkspaceCrypto.sign(keys, message: statement.frame()), message: statement.frame())
        // Every field is signed.
        let changed = try RoutePersonStatement(workspaceId: statement.workspaceId, deviceId: statement.deviceId, gatewayId: statement.gatewayId,
            sessionId: statement.sessionId, runId: statement.runId, conversationId: statement.conversationId, personOff: !statement.personOff,
            requestId: statement.requestId, issuedAt: statement.issuedAt, expiresAt: statement.expiresAt)
        XCTAssertThrowsError(try WorkspaceCrypto.verify(publicKey: publicKey,
            signature: WorkspaceProtocol.decode(v.string("signature"), size: 64), message: changed.frame()))
    }

    func testOnlyAValidStatementWithinItsLifetimeIsMade() throws {
        let good = try XCTUnwrap(try vector().object("statement"))
        for (key, value) in [("expires_at", JSON.int(good.int64("issued_at"))), ("expires_at", .int(good.int64("issued_at") + 301)),
                             ("run_id", .string("")), ("conversation_id", .string("a\nb")), ("session_id", .string("a b")), ("extra", .int(1))] {
            XCTAssertThrowsError(try RoutePersonStatement.parse(good.putting(key, value)), key)
        }
    }

    func testThePhoneUsesItOnlyWhereAnEncryptedConnectionItsRelayAndTheComputerOfferIt() {
        let encrypted = Connection(id: "c", label: "label", url: "https://relay.example.invalid", token: "token", protocolVersion: 2)
        let legacy = Connection(id: "c", label: "label", url: "https://relay.example.invalid", token: "token", protocolVersion: 1)
        let both: Set<String> = [SessionQuestionRouting.personOperation]
        XCTAssertTrue(SessionQuestionRouting.person(encrypted, relay: both, machine: both))
        XCTAssertFalse(SessionQuestionRouting.person(legacy, relay: both, machine: both))
        XCTAssertFalse(SessionQuestionRouting.person(encrypted, relay: [], machine: both))
        XCTAssertFalse(SessionQuestionRouting.person(encrypted, relay: both, machine: []))
        XCTAssertFalse(SessionQuestionRouting.person(nil, relay: both, machine: both))
    }

    private let sessionId = "00000000-0000-4000-8000-0000000000c1"
    private let id = "00000000-0000-4000-8000-0000000000aa"
    private func args() -> JSONObject { ["send_to_parent": .bool(false), "expected_revision": .int(2), "session_id": .string(sessionId)] }
    private func target() -> Target {
        Target(computerId: "00000000-0000-4000-8000-000000000001", session: "claude/project/child", run: "run-1", conversation: "conversation-1",
               connectionId: "workspace")
    }
    private func confirmed() -> JSONObject {
        ["request_id": .string(id), "op": .string("question_route_child"), "accepted": .bool(true), "durable": .bool(true),
         "session_id": .string(sessionId), "person_off": .bool(true), "revision": .int(3), "applied": .bool(true), "code": .string("")]
    }
    private func settle(_ result: JSONObject) -> SessionAction {
        let action = SessionAction(requestId: id, target: target(), operation: SessionQuestionRouting.personOperation, arguments: args())
        return SessionActionPolicies.result(action, ["request_id": .string(id), "state": .string("completed"), "result": .object(result)])
    }

    func testOnlyTheServicesExactReceiptSettlesIt() throws {
        XCTAssertTrue(SessionActionPolicies.operations.contains(SessionQuestionRouting.personOperation))
        _ = try SessionActionPolicies.arguments(SessionQuestionRouting.personOperation, args())
        for bad in [args().putting("extra", .int(1)), args().putting("session_id", .string("a b")), args().putting("session_id", nil)] {
            XCTAssertThrowsError(try SessionActionPolicies.arguments(SessionQuestionRouting.personOperation, bad))
        }
        XCTAssertEqual(settle(confirmed()).status, "completed")
        for (key, value) in [("person_off", JSON.bool(false)), ("session_id", .string("00000000-0000-4000-8000-0000000000c2")),
                             ("request_id", .string("x")), ("op", .string("question_routes")), ("durable", .bool(false)),
                             ("applied", .null), ("code", .string("x")), ("extra", .int(1))] {
            XCTAssertEqual(settle(confirmed().putting(key, value)).status, "uncertain", key)
        }
        for (code, message) in SessionQuestionRouting.personRefusals {
            let refusal: JSONObject = ["request_id": .string(id), "op": .string("question_route_child"), "accepted": .bool(false),
                                       "durable": .bool(false), "code": .string(code)]
            let settled = settle(refusal)
            XCTAssertEqual(settled.status, "failed", code)
            XCTAssertEqual(settled.error, message, code)
        }
        let store: JSONObject = ["request_id": .string(id), "op": .string("question_route_child"), "accepted": .bool(false),
                                 "durable": .bool(false), "code": .string("question_route_store_refused")]
        XCTAssertEqual(settle(store).status, "uncertain")
    }

    func testTheLatchActionChecksTheChildsDurableIdentityBeforeSending() {
        let raw: JSONObject = ["name": .string("claude/project/child"), "cmd": .string("claude"), "state": .string("running"),
                               "session_id": .string(sessionId), "parent": .object(["name": .string("codex/project/lead")]),
                               "question_routing": .object(["send_to_parent": .bool(true), "revision": .int(2)])]
        XCTAssertEqual(SessionActionPolicies.inspectReason(target(), SessionQuestionRouting.personOperation, args(), raw), "")
        XCTAssertEqual(SessionActionPolicies.inspectReason(target(), SessionQuestionRouting.personOperation, args(),
                                                           raw.putting("session_id", .string("00000000-0000-4000-8000-0000000000c2"))),
                       SessionQuestionRouting.refusals["stale_identity"])
    }
}
