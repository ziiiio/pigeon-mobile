// ChatErrorTests — the chat-screen error mapper.
// Mirrors Android's ChatErrorTest.kt: same cases, same inputs, same expected wording.
import XCTest
import PigeonCore
@testable import Pigeon

final class ChatErrorTests: XCTestCase {

    func testMalformedInviteeIdExplainsTheIdFormat() {
        let msg = chatErrorMessage(CoreError.Api(code: .invalidUsername,
                                                 reason: "identifier must start with sigil '@'"))
        XCTAssertTrue(msg.contains("user ID"), msg)
        XCTAssertFalse(msg.contains("Usernames may use"), "must not read as a sign-in error")
    }

    func testForbiddenIsAPermissionErrorNotBadCredentials() {
        let msg = chatErrorMessage(CoreError.Api(code: .forbidden, reason: "not joined"))
        XCTAssertTrue(msg.contains("permission"), msg)
        XCTAssertFalse(msg.contains("password"))
    }

    func testNeverLeaksTheRawReasonOrExceptionText() {
        let msg = chatErrorMessage(CoreError.Api(code: .invalidUsername, reason: "server said so"))
        XCTAssertFalse(msg.contains("server said so"))
        XCTAssertFalse(msg.contains("uniffi"))
    }

    func testOtherErrorsFallBackToTheSharedWording() {
        let e = CoreError.Network(reason: "boom")
        XCTAssertEqual(authErrorMessage(e), chatErrorMessage(e))
    }
}
