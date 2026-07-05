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
}
