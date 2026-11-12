// AuthErrorTests (M5.4) — exhaustive coverage of the typed-error → message
// mapper, the iOS mirror of Android's `AuthErrorTest`. It must branch on every
// CoreError variant and every ErrorCode (a federated, offline-prone client hits
// them all), and never leak a raw error string. Pure — no simulator UI needed,
// but it runs in the app test bundle since it imports PigeonCore types.
import XCTest
import PigeonCore
@testable import Pigeon

final class AuthErrorTests: XCTestCase {

    func testEveryErrorCodeMapsToNonEmptyMessage() {
        let codes: [ErrorCode] = [
            .forbidden, .unknownToken, .missingToken, .badJson, .notJson,
            .notFound, .limitExceeded, .badSignature, .unrecognized,
            .userInUse, .invalidUsername, .unknown, .other(code: "P_MADE_UP"),
        ]
        for code in codes {
            let msg = authErrorMessage(CoreError.Api(code: code, reason: "server said so"))
            XCTAssertFalse(msg.isEmpty, "no message for \(code)")
            // Must not surface the raw server reason.
            XCTAssertFalse(msg.contains("server said so"), "leaked raw reason for \(code)")
        }
    }

    func testDistinctWordingForKeyCodes() {
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .userInUse, reason: "x")),
                       "That username is already taken.")
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .forbidden, reason: "x")),
                       "Incorrect username or password.")
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .invalidUsername, reason: "x")),
                       "Usernames may use only a–z, 0–9, and . _ -")
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .unknownToken, reason: "x")),
                       "Your session has expired. Please sign in again.")
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .missingToken, reason: "x")),
                       "Your session has expired. Please sign in again.")
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .limitExceeded, reason: "x")),
                       "Too many attempts. Please wait a moment and try again.")
    }

    func testUnknownCodeFallsBackToGenericServerError() {
        XCTAssertEqual(authErrorMessage(CoreError.Api(code: .other(code: "P_NEW"), reason: "x")),
                       "The server reported an error.")
    }

    func testNonApiVariants() {
        XCTAssertEqual(authErrorMessage(CoreError.Network(reason: "dns")),
                       "Can't reach the server. Check the address and your connection.")
        // `.Protocol` is a keyword; the leading-dot form (with an explicit type)
        // avoids the `CoreError.Protocol` metatype-parse ambiguity.
        let proto: CoreError = .`Protocol`(reason: "weird")
        XCTAssertEqual(authErrorMessage(proto),
                       "Unexpected response from the server.")
        XCTAssertEqual(authErrorMessage(CoreError.Storage(reason: "keychain")),
                       "Couldn't access secure storage on this device.")
        XCTAssertEqual(authErrorMessage(CoreError.Crypto(reason: "mls")),
                       "A security error occurred.")
    }

    func testNonCoreErrorDegradesGracefully() {
        struct Weird: Error {}
        XCTAssertEqual(authErrorMessage(Weird()), "Something went wrong.")
    }

    // chatErrorMessage — the chat-screen mapper (mirrors Android's ChatErrorTest).

    func testChatMalformedInviteeIdExplainsTheIdFormat() {
        let msg = chatErrorMessage(CoreError.Api(code: .invalidUsername,
                                                 reason: "identifier must start with sigil '@'"))
        XCTAssertTrue(msg.contains("user ID"), msg)
        XCTAssertFalse(msg.contains("Usernames may use"), "must not read as a sign-in error")
    }

    func testChatForbiddenIsAPermissionError() {
        let msg = chatErrorMessage(CoreError.Api(code: .forbidden, reason: "not joined"))
        XCTAssertTrue(msg.contains("permission"), msg)
        XCTAssertFalse(msg.contains("password"))
    }

    func testChatNeverLeaksTheRawReason() {
        XCTAssertFalse(chatErrorMessage(CoreError.Api(code: .invalidUsername, reason: "server said so"))
            .contains("server said so"))
    }

    func testChatOtherErrorsFallBackToSharedWording() {
        let e = CoreError.Network(reason: "boom")
        XCTAssertEqual(chatErrorMessage(e), authErrorMessage(e))
    }
}

// mergeTimeline — the chat timeline merge (mirrors Android's TimelineMergeTest).
// Lives in this file because the test target lists its sources explicitly.
final class TimelineMergeTests: XCTestCase {

    private func ev(_ id: String, _ cursor: Int64, body: String? = nil, system: String? = nil) -> TimelineEvent {
        TimelineEvent(eventId: id, sender: "@a:s", originServerTs: cursor, cursor: cursor,
                      body: body, systemText: system, pending: false, failed: false, image: nil)
    }

    func testFreshCopyReplacesTheStaleOne() {
        // Loaded while pending decryption (hidden), then decrypted after a restore.
        let merged = mergeTimeline(existing: [ev("$a", 1, body: "hi"), ev("$enc", 2)],
                                   fresh: [ev("$enc", 2, body: "the secret")])
        XCTAssertEqual(merged.map(\.eventId), ["$a", "$enc"])
        XCTAssertEqual(merged[1].body, "the secret")
    }

    func testFailedDecryptReplacesTheHiddenPendingCopy() {
        let merged = mergeTimeline(existing: [ev("$enc", 2)],
                                   fresh: [ev("$enc", 2, system: "⚠ Unable to decrypt")])
        XCTAssertEqual(merged.first?.systemText, "⚠ Unable to decrypt")
    }

    func testOlderPagesMergeInCursorOrderWithoutDuplicates() {
        let merged = mergeTimeline(existing: [ev("$c", 3), ev("$d", 4)],
                                   fresh: [ev("$a", 1), ev("$b", 2), ev("$c", 3)])
        XCTAssertEqual(merged.map(\.eventId), ["$a", "$b", "$c", "$d"])
    }
}

