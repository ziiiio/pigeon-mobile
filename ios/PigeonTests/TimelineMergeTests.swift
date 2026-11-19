// TimelineMergeTests — `mergeTimeline`, the chat timeline merge.
// Mirrors Android's TimelineMergeTest.kt: same cases, same inputs, same expectations.
import XCTest
import PigeonCore
@testable import Pigeon

final class TimelineMergeTests: XCTestCase {

    private func ev(_ id: String, _ cursor: Int64, body: String? = nil, system: String? = nil) -> TimelineEvent {
        TimelineEvent(eventId: id, sender: "@a:s", originServerTs: cursor, cursor: cursor,
                      body: body, systemText: system, pending: false, failed: false, image: nil)
    }

    func testTheFreshCopyOfAnEventReplacesTheStaleOne() {
        // Loaded while pending decryption (no body, no system line → hidden)...
        let onScreen = [ev("$a", 1, body: "hi"), ev("$enc", 2)]
        // ...then decrypted after a key restore: the fresh read must win.
        let fresh = [ev("$enc", 2, body: "the secret")]
        let merged = mergeTimeline(existing: onScreen, fresh: fresh)
        XCTAssertEqual(merged.map(\.eventId), ["$a", "$enc"])
        XCTAssertEqual(merged[1].body, "the secret")
    }

    func testAFailedDecryptReplacesTheHiddenPendingCopy() {
        let merged = mergeTimeline(existing: [ev("$enc", 2)],
                                   fresh: [ev("$enc", 2, system: "⚠ Unable to decrypt")])
        XCTAssertEqual(merged.count, 1)
        XCTAssertEqual(merged.first?.systemText, "⚠ Unable to decrypt")
    }

    func testOlderPagesArePrependedInCursorOrderWithoutDuplicates() {
        let merged = mergeTimeline(existing: [ev("$c", 3), ev("$d", 4)],
                                   fresh: [ev("$a", 1), ev("$b", 2), ev("$c", 3)])
        XCTAssertEqual(merged.map(\.eventId), ["$a", "$b", "$c", "$d"])
    }
}
