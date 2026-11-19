// BackupFeedbackTests — backup/restore outcome messages.
// Mirrors Android's BackupFeedbackTest.kt: same cases, same inputs, same expected wording.
// One iOS-only extra, `testWordingMatchesAndroidForTheKeyCases`, pins Android's exact
// strings, so a wording change that lands on one platform only fails here.
import XCTest
import PigeonCore
@testable import Pigeon

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

    private let fmt: (Int64) -> String = { "T\($0)" }

    func testEveryFailureSaysWhichActionFailedAndNeverLeaksRawText() {
        for e in everyVariant {
            let b = backupErrorMessage(e), r = restoreErrorMessage(e)
            XCTAssertTrue(b.hasPrefix("Backup failed"), b)
            XCTAssertTrue(r.hasPrefix("Restore failed"), r)
            for m in [b, r] {
                XCTAssertFalse(m.contains("server said so"), m)
                XCTAssertFalse(m.contains("uniffi"), m)
            }
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
        // The sign-in mapper's wording for Forbidden must not leak into backup UX.
        let e = CoreError.Api(code: .forbidden, reason: "x")
        XCTAssertFalse(backupErrorMessage(e).contains("password"))
        XCTAssertFalse(restoreErrorMessage(e).contains("password"))
    }

    func testRestoreSuccessSaysWhatCameBackAndWhatCannot() {
        let dated = restoreSuccessMessage(42, format: fmt)
        XCTAssertTrue(dated.contains("backup of T42"), dated)
        XCTAssertTrue(dated.contains("New messages will decrypt"), dated)
        XCTAssertTrue(dated.contains("can't be recovered"), dated)
        XCTAssertFalse(dated.contains("all your messages"), dated)
        // A backup stored without a time (reference CLI) still gets the caveat.
        let undated = restoreSuccessMessage(nil, format: fmt)
        XCTAssertTrue(undated.contains("can't be recovered"), undated)
        XCTAssertFalse(undated.contains("nil"), undated)
    }

    func testTheBackupExplainerStatesItsLimits() {
        // Said before every backup: it's a snapshot, older messages are gone for
        // good, later rooms aren't covered, and a new backup retires the old key.
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

    func testCoverageAndRestoreScopeNameTheBackupTime() {
        XCTAssertTrue(backupCoverageText(7, format: fmt).contains("from T7 onward"))
        XCTAssertTrue(restoreScopeText(.at(7), format: fmt).contains("from T7 onward"))
        let unknown = restoreScopeText(.at(nil), format: fmt)
        XCTAssertTrue(unknown.contains("when the backup was made"), unknown)
    }

    func testLastBackupLineCoversEveryState() {
        XCTAssertTrue(lastBackupText(.at(9), format: fmt).contains("T9"))
        XCTAssertTrue(lastBackupText(.noBackup, format: fmt).contains("no key backup"))
        XCTAssertTrue(lastBackupText(.at(nil), format: fmt).contains("date is unknown"))
        XCTAssertTrue(lastBackupText(.unavailable, format: fmt).contains("Couldn't check"))
        XCTAssertTrue(lastBackupText(.loading, format: fmt).contains("Checking"))
    }

    // iOS-only extra (see the header): Android's exact strings.
    func testWordingMatchesAndroidForTheKeyCases() {
        XCTAssertEqual(restoreErrorMessage(CoreError.Crypto(reason: "x")),
                       "Restore failed — that recovery key doesn't match your backup. Check it and try again.")
        XCTAssertEqual(backupErrorMessage(CoreError.Network(reason: "x")),
                       "Backup failed — couldn't reach the server. Check your connection and try again.")
        XCTAssertEqual(restoreSuccessMessage(42, format: fmt),
                       "Encryption keys restored from your backup of T42. New messages will decrypt, but "
                       + "messages from before the backup can't be recovered.")
        XCTAssertEqual(backupCoverageText(7, format: fmt),
                       "This backup covers messages from T7 onward. Messages from before then can't be restored with it.")
    }
}
