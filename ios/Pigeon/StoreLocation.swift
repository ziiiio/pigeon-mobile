// StoreLocation — where the core's SQLite store lives on iOS, and keeping it out
// of backups.
//
// The store (`pigeon.sqlite3`) caches the **decrypted text** of encrypted
// messages (Gotcha #3). Application Support is included in iCloud and device
// backups by default, so that text could leave the phone in a backup. Android
// avoids this with `android:allowBackup="false"`; this is the iOS equivalent:
//
//   • the store gets its own directory, `Application Support/PigeonStore/`,
//   • which is excluded from backup (`isExcludedFromBackup`, i.e. the
//     `com.apple.metadata:com_apple_backup_excludeItem` attribute), and
//   • protected with `FileProtectionType.completeUnlessOpen`: unreadable while the
//     device is locked, except for files already open — so the sync loop's open
//     database keeps working when the phone locks (`.complete` would break it).
//
// Existing installs kept the store directly in Application Support. `prepare`
// moves it (with its `-wal`/`-shm` siblings) into the new directory, as a set: if
// any move fails it moves the others back and keeps using the old location, so
// nobody loses their local history and decrypted cache to a half-done migration.
import Foundation
import PigeonCore

enum StoreLocation {
    /// The store's own directory, inside the base (Application Support).
    static let directoryName = "PigeonStore"

    /// The core's store file (`core/src/session.rs` → `STORE_FILE`) and the
    /// files SQLite keeps next to it in WAL mode. Moved together.
    static let storeFiles = ["pigeon.sqlite3", "pigeon.sqlite3-wal", "pigeon.sqlite3-shm"]

    /// Prepare the store directory under `base` and return the directory the core
    /// should use. On any failure it returns `base` itself (the pre-migration
    /// location) and reports why through `log` (level, message), which never sees
    /// message text — only file names and system error descriptions.
    static func prepare(
        base: URL,
        fileManager fm: FileManager = .default,
        log: (LogLevel, String) -> Void = { _, _ in }
    ) -> URL {
        var dir = base.appendingPathComponent(directoryName, isDirectory: true)
        let protection: [FileAttributeKey: Any] = [.protectionKey: FileProtectionType.completeUnlessOpen]
        do {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true, attributes: protection)
            // Re-apply on every launch: the directory may predate this code.
            try fm.setAttributes(protection, ofItemAtPath: dir.path)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try dir.setResourceValues(values)
        } catch {
            log(.warn, "could not prepare \(directoryName) (\(error.localizedDescription)); keeping the store in Application Support")
            return base
        }

        // Move a legacy store in, as a set.
        var moved: [(from: URL, to: URL)] = []
        for name in storeFiles {
            let legacy = base.appendingPathComponent(name)
            let target = dir.appendingPathComponent(name)
            guard fm.fileExists(atPath: legacy.path) else { continue }
            if fm.fileExists(atPath: target.path) {
                // Both exist: the new location is the live one. Leave the old file
                // alone rather than delete anything a user might still need.
                log(.warn, "\(name) exists in both locations; using \(directoryName), leaving the old copy")
                continue
            }
            do {
                try fm.moveItem(at: legacy, to: target)
                try? fm.setAttributes(protection, ofItemAtPath: target.path)
                moved.append((legacy, target))
            } catch {
                // Undo the moves so far; a db split from its -wal would lose data.
                for m in moved.reversed() {
                    try? fm.moveItem(at: m.to, to: m.from)
                }
                log(.warn, "could not move \(name) into \(directoryName) (\(error.localizedDescription)); keeping the store in Application Support")
                return base
            }
        }
        if !moved.isEmpty {
            log(.info, "moved \(moved.count) store file(s) into \(directoryName) (excluded from backup)")
        }
        return dir
    }
}
