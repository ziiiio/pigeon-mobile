// StoreLocationTests — the iOS store directory: created, excluded from backup,
// legacy store moved in as a set, idempotent, and a safe fallback on failure.
import XCTest
import PigeonCore
@testable import Pigeon

final class StoreLocationTests: XCTestCase {
    private var base: URL!
    private let fm = FileManager.default

    override func setUpWithError() throws {
        base = fm.temporaryDirectory.appendingPathComponent("StoreLocationTests-\(UUID().uuidString)")
        try fm.createDirectory(at: base, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? fm.removeItem(at: base)
    }

    private func excludedFromBackup(_ url: URL) throws -> Bool {
        try url.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup ?? false
    }

    private func write(_ name: String, _ contents: String, in dir: URL) throws {
        try contents.write(to: dir.appendingPathComponent(name), atomically: true, encoding: .utf8)
    }

    private func read(_ name: String, in dir: URL) throws -> String {
        try String(contentsOf: dir.appendingPathComponent(name), encoding: .utf8)
    }

    func testCreatesTheDirectoryAndExcludesItFromBackup() throws {
        let dir = StoreLocation.prepare(base: base, fileManager: fm)
        XCTAssertEqual(dir.lastPathComponent, StoreLocation.directoryName)
        var isDir: ObjCBool = false
        XCTAssertTrue(fm.fileExists(atPath: dir.path, isDirectory: &isDir) && isDir.boolValue)
        XCTAssertTrue(try excludedFromBackup(dir), "the store directory must be excluded from backup")
        // File protection isn't enforced on the simulator, but where the attribute
        // is reported it must be the class that keeps an open database usable.
        if let p = try fm.attributesOfItem(atPath: dir.path)[.protectionKey] as? FileProtectionType {
            XCTAssertEqual(p, .completeUnlessOpen)
        }
    }

    func testMovesTheLegacyStoreAndItsSiblings() throws {
        // An install from before this change: the store sits in the base itself.
        for name in StoreLocation.storeFiles { try write(name, "data:\(name)", in: base) }
        var logs: [String] = []
        let dir = StoreLocation.prepare(base: base, fileManager: fm) { _, m in logs.append(m) }
        for name in StoreLocation.storeFiles {
            XCTAssertFalse(fm.fileExists(atPath: base.appendingPathComponent(name).path), "\(name) left behind")
            XCTAssertEqual(try read(name, in: dir), "data:\(name)", "\(name) contents preserved")
        }
        XCTAssertTrue(logs.contains { $0.contains("moved 3 store file(s)") }, "\(logs)")
    }

    func testASecondCallIsANoOp() throws {
        try write("pigeon.sqlite3", "db", in: base)
        let first = StoreLocation.prepare(base: base, fileManager: fm)
        var logs: [String] = []
        let second = StoreLocation.prepare(base: base, fileManager: fm) { _, m in logs.append(m) }
        XCTAssertEqual(first, second)
        XCTAssertEqual(try read("pigeon.sqlite3", in: second), "db")
        XCTAssertTrue(try excludedFromBackup(second))
        XCTAssertTrue(logs.isEmpty, "nothing to do the second time, but logged \(logs)")
    }

    func testAnOldCopyNextToALiveStoreIsLeftAlone() throws {
        let dir = StoreLocation.prepare(base: base, fileManager: fm)
        try write("pigeon.sqlite3", "live", in: dir)
        try write("pigeon.sqlite3", "stale", in: base)
        let again = StoreLocation.prepare(base: base, fileManager: fm)
        XCTAssertEqual(try read("pigeon.sqlite3", in: again), "live", "the live store wins")
        XCTAssertEqual(try read("pigeon.sqlite3", in: base), "stale", "nothing is deleted")
    }

    func testFallsBackToTheBaseWhenTheDirectoryCantBeMade() throws {
        // A *file* where the directory should be: creating it fails.
        try write(StoreLocation.directoryName, "in the way", in: base)
        try write("pigeon.sqlite3", "db", in: base)
        var levels: [LogLevel] = []
        let dir = StoreLocation.prepare(base: base, fileManager: fm) { l, _ in levels.append(l) }
        XCTAssertEqual(dir.standardizedFileURL, base.standardizedFileURL, "keep using the old location")
        XCTAssertEqual(try read("pigeon.sqlite3", in: base), "db", "the store is untouched")
        XCTAssertEqual(levels, [.warn])
    }
}
