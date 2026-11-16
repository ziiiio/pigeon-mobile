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

// Backup/restore outcome messages (mirrors Android's BackupFeedbackTest).
final class BackupFeedbackTests: XCTestCase {

    private let everyVariant: [CoreError] = [
        .Network(reason: "boom"),
        .Api(code: .notFound, reason: "server said so"),
        .Api(code: .forbidden, reason: "server said so"),
        .Api(code: .unknownToken, reason: "server said so"),
        .Api(code: .limitExceeded, reason: "server said so"),
        .Api(code: .other(code: "P_FUTURE"), reason: "server said so"),
        .Crypto(reason: "server said so"),
        .Storage(reason: "server said so"),
        .Protocol(reason: "server said so"),
    ]

    func testEveryFailureNamesTheActionAndNeverLeaksRawText() {
        for e in everyVariant {
            let b = backupErrorMessage(e), r = restoreErrorMessage(e)
            XCTAssertTrue(b.hasPrefix("Backup failed"), b)
            XCTAssertTrue(r.hasPrefix("Restore failed"), r)
            XCTAssertFalse(b.contains("server said so"))
            XCTAssertFalse(r.contains("server said so"))
        }
    }

    func testRestoreDistinguishesNoBackupFromAWrongRecoveryKey() {
        let noBackup = restoreErrorMessage(CoreError.Api(code: .notFound, reason: "x"))
        let wrongKey = restoreErrorMessage(CoreError.Crypto(reason: "x"))
        XCTAssertTrue(noBackup.contains("no key backup"), noBackup)
        XCTAssertTrue(wrongKey.contains("recovery key doesn't match"), wrongKey)
        XCTAssertNotEqual(noBackup, wrongKey)
    }

    func testForbiddenNeverReadsAsABadPassword() {
        let e = CoreError.Api(code: .forbidden, reason: "x")
        XCTAssertFalse(backupErrorMessage(e).contains("password"))
        XCTAssertFalse(restoreErrorMessage(e).contains("password"))
    }

    func testWordingMatchesAndroidForTheKeyCases() {
        // Lockstep with Android's RoomsViewModel.kt (same codes -> same wording).
        XCTAssertEqual(restoreErrorMessage(CoreError.Crypto(reason: "x")),
                       "Restore failed — that recovery key doesn't match your backup. Check it and try again.")
        XCTAssertEqual(backupErrorMessage(CoreError.Network(reason: "x")),
                       "Backup failed — couldn't reach the server. Check your connection and try again.")
        let fmt: (Int64) -> String = { "T\($0)" }
        XCTAssertEqual(restoreSuccessMessage(42, format: fmt),
                       "Encryption keys restored from your backup of T42. New messages will decrypt, but "
                       + "messages from before the backup can't be recovered.")
        XCTAssertEqual(backupCoverageText(7, format: fmt),
                       "This backup covers messages from T7 onward. Messages from before then can't be restored with it.")
    }

    private let fmt: (Int64) -> String = { "T\($0)" }

    func testRestoreSuccessSaysWhatCameBackAndWhatCannot() {
        let dated = restoreSuccessMessage(42, format: fmt)
        XCTAssertTrue(dated.contains("backup of T42"), dated)
        XCTAssertTrue(dated.contains("can't be recovered"), dated)
        XCTAssertFalse(dated.contains("all your messages"), dated)
        let undated = restoreSuccessMessage(nil, format: fmt)
        XCTAssertTrue(undated.contains("can't be recovered"), undated)
        XCTAssertFalse(undated.contains("nil"), undated)
    }

    func testTheBackupExplainerStatesItsLimits() {
        XCTAssertTrue(backupScope.contains("as they are right now"))
        XCTAssertTrue(backupScope.contains("can't read messages from before the backup"))
        XCTAssertTrue(backupScope.contains("Rooms you join after the backup aren't included"))
        XCTAssertTrue(backupScope.contains("old recovery key stops working"))
    }

    func testBackupMessagesArePlainSentences() {
        // Short sentences read better than semicolon-chained clauses.
        let states: [LastBackup] = [.loading, .noBackup, .at(1), .at(nil), .unavailable]
        let all = [backupScope,
                   restoreSuccessMessage(1, format: fmt), restoreSuccessMessage(nil, format: fmt),
                   backupCoverageText(1, format: fmt),
                   restoreScopeText(.at(1), format: fmt), restoreScopeText(.at(nil), format: fmt)]
            + states.map { lastBackupText($0, format: fmt) }
        for m in all { XCTAssertFalse(m.contains(";"), m) }
    }

    func testLastBackupLineCoversEveryState() {
        XCTAssertTrue(lastBackupText(.at(9), format: fmt).contains("T9"))
        XCTAssertTrue(lastBackupText(.noBackup, format: fmt).contains("no key backup"))
        XCTAssertTrue(lastBackupText(.at(nil), format: fmt).contains("date is unknown"))
        XCTAssertTrue(lastBackupText(.unavailable, format: fmt).contains("Couldn't check"))
        XCTAssertTrue(restoreScopeText(.at(7), format: fmt).contains("from T7 onward"))
        XCTAssertTrue(restoreScopeText(.at(nil), format: fmt).contains("when the backup was made"))
    }
}

