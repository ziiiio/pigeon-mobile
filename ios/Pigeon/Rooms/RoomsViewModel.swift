// RoomsViewModel (M5.4) — the thin view-model over the core's room API, the iOS
// mirror of Android's `RoomsViewModel.kt`. No protocol/crypto logic: it reads the
// room list from the local store (offline-first), drives create/join/backup, and
// folds the sync loop's signals into UI state. The sync loop itself is owned by
// the room-list view so its lifecycle bounds it (Gotcha #6).
import SwiftUI
import PigeonCore

/// UI state for the room list.
struct RoomsState {
    var rooms: [Room] = []
    /// Connectivity to the homeserver, from the sync loop's `onStatus`.
    var connected = true
    /// True until the first room-list read completes.
    var loading = true
    /// A transient action failure (create/join/reload) to surface then dismiss.
    var actionError: String?
    /// After a successful key backup, the recovery key to show the user once
    /// (M4.3) — the only secret that can restore the backup; it never reaches the
    /// server. Cleared when the sheet dismisses.
    var recoveryKey: String?
    /// When the backup behind `recoveryKey` was taken (Unix ms) — the sheet says
    /// it only covers messages from then on.
    var backupCreatedAtMs: Int64?
    /// The account's server-side backup, as last fetched for the backup/restore
    /// sheets (they say when it was taken, i.e. what it can't restore).
    var lastBackup: LastBackup = .loading
    /// A key backup / restore is in flight — the UI shows progress and disables
    /// the actions.
    var backupInProgress = false
    var restoreInProgress = false
    /// A transient success message to surface then dismiss (e.g. "keys restored").
    var notice: String?
}

@MainActor
final class RoomsViewModel: ObservableObject {
    @Published private(set) var state = RoomsState()
    private let client: PigeonClient

    init(client: PigeonClient) {
        self.client = client
        reload()
    }

    /// Re-read the room list from the local store (no network).
    func reload() {
        do {
            state.rooms = try client.listRooms()
            state.loading = false
        } catch {
            state.loading = false
            state.actionError = message(error)
        }
    }

    /// Fold the sync loop's connectivity signal into state.
    func setConnected(_ connected: Bool) { state.connected = connected }

    /// The sync loop ended fatally (e.g. the token was revoked).
    func onSyncFailed(_ message: String?) {
        state.connected = false
        state.actionError = message
    }

    /// Create a room; its state arrives via the sync loop (which reloads the
    /// list on change). When `encrypted`, the core creates it E2EE — the UI is
    /// otherwise identical (encryption is transparent, M3).
    func createRoom(name: String?, topic: String?, encrypted: Bool) {
        let n = name?.blankAsNil, t = topic?.blankAsNil
        Task {
            do {
                _ = encrypted ? try await client.createEncryptedRoom(name: n, topic: t)
                              : try await client.createRoom(name: n, topic: t)
            } catch { state.actionError = message(error) }
        }
    }

    /// Join a room by id; membership + timeline arrive on the next sync.
    func joinRoom(_ roomId: String) {
        Task {
            do { try await client.joinRoom(roomId: roomId.trimmed) }
            catch { state.actionError = message(error) }
        }
    }

    /// Back up the device's encryption keys (M4.3). The returned recovery key is
    /// shown once for the user to save; it never reaches the server (Gotcha #1).
    func backup() {
        guard !state.backupInProgress else { return }
        // Clear any stale outcome so the user sees *this* attempt's result.
        state.backupInProgress = true
        state.actionError = nil
        state.notice = nil
        Task {
            // Success: the recovery-key sheet opens, titled "Backup saved".
            do {
                let made = try await client.backup()
                state.backupCreatedAtMs = made.createdAtMs
                state.lastBackup = .at(made.createdAtMs)
                state.recoveryKey = made.recoveryKey
            } catch { state.actionError = backupErrorMessage(error) }
            state.backupInProgress = false
        }
    }

    /// Restore the device's encryption keys from the server-side backup using the
    /// user's recovery key (M4.3). Recovered groups/identity surface via sync.
    func restoreBackup(_ recoveryKey: String) {
        guard !state.restoreInProgress else { return }
        state.restoreInProgress = true
        state.actionError = nil
        state.notice = nil
        Task {
            do {
                let restored = try await client.restoreBackup(recoveryKey: recoveryKey.trimmed)
                state.notice = restoreSuccessMessage(restored.backedUpAtMs, format: formatBackupTime)
            } catch { state.actionError = restoreErrorMessage(error) }
            state.restoreInProgress = false
        }
    }

    /// Fetch whether a backup exists and when it was taken, for the backup and
    /// restore sheets. A failure isn't something the user must act on — the
    /// sheet just says the date is unavailable.
    func loadBackupStatus() {
        state.lastBackup = .loading
        Task {
            do {
                state.lastBackup = try await client.backupStatus().map { .at($0.backedUpAtMs) } ?? .noBackup
            } catch { state.lastBackup = .unavailable }
        }
    }

    func clearRecoveryKey() {
        state.recoveryKey = nil
        state.backupCreatedAtMs = nil
    }
    func clearError() { state.actionError = nil }
    func clearNotice() { state.notice = nil }

    private func message(_ error: Error) -> String { authErrorMessage(error) }
}

extension String {
    /// nil if blank after trimming; else the trimmed string. Mirrors Android's
    /// `ifBlank { null }` for optional name/topic fields.
    var blankAsNil: String? {
        let t = trimmed
        return t.isEmpty ? nil : t
    }
}

/// The account's key backup, as the backup/restore sheets know it.
enum LastBackup: Equatable {
    case loading
    /// No backup on the server yet.
    case noBackup
    /// A backup exists; nil when it was stored without a time (reference CLI).
    case at(Int64?)
    /// Couldn't ask the server.
    case unavailable
}

// What a key backup can and can't do, said up front (M4.3). A backup is a
// snapshot of this device's MLS keys at one moment: it restores rooms and
// messages from that moment on, never messages the device had already read —
// MLS deletes those keys after use (forward secrecy), so nothing can recover
// them. See design/history-backup.md in ../pigeon. All wording mirrors Android's
// `RoomsViewModel.kt` — keep them in lockstep.

/// Shown before every backup, beside the last-backup line.
let backupScope =
    "A key backup is a snapshot of this device's encryption keys at the moment you make it. "
    + "Restoring it on a new or reset device:\n"
    + "• lets you read messages sent after the backup, in rooms you were in when you made it;\n"
    + "• can't bring back messages from before the backup — Pigeon deletes those keys once "
    + "they've been used, so nobody can recover them, not even the server;\n"
    + "• doesn't include rooms you join later — back up again after joining one.\n"
    + "A new backup replaces the previous one, and the old recovery key stops working."

/// The "last backup" line in the backup and restore sheets.
func lastBackupText(_ last: LastBackup, format: (Int64) -> String) -> String {
    switch last {
    case .loading: return "Checking for an existing backup…"
    case .noBackup: return "You have no key backup yet."
    case let .at(ms):
        guard let ms else { return "You have a backup, but its date is unknown." }
        return "Your last backup was made \(format(ms))."
    case .unavailable: return "Couldn't check for an existing backup."
    }
}

/// In the restore sheet: what restoring *this* backup will and won't recover.
func restoreScopeText(_ last: LastBackup, format: (Int64) -> String) -> String {
    if case let .at(ms?) = last {
        return "Restoring recovers your rooms and messages from \(format(ms)) onward. "
            + "Messages from before then can't be recovered."
    }
    return "Restoring recovers your rooms and messages from when the backup was made onward. "
        + "Messages from before then can't be recovered."
}

/// In the "Backup saved" sheet: the period this backup covers.
func backupCoverageText(_ createdAtMs: Int64, format: (Int64) -> String) -> String {
    "This backup covers messages from \(format(createdAtMs)) onward. "
        + "Messages from before then can't be restored with it."
}

/// Shown after a successful restore: what came back, and what can't.
func restoreSuccessMessage(_ backedUpAtMs: Int64?, format: (Int64) -> String) -> String {
    let since = backedUpAtMs.map { "your backup of \(format($0))" } ?? "your backup"
    return "Encryption keys restored from \(since). New messages will decrypt; "
        + "messages from before the backup can't be recovered."
}

/// The user-facing date for a backup time, in the device's locale + zone.
func formatBackupTime(_ ms: Int64) -> String {
    Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
        .formatted(date: .abbreviated, time: .shortened)
}

/// The message for a failed key **backup** (M4.3). Branches on the typed error
/// (never on text) and never shows the raw error. Mirrors Android's
/// `backupErrorMessage` — keep the wording in lockstep.
func backupErrorMessage(_ error: Error) -> String {
    guard let core = error as? CoreError else { return "Backup failed — something went wrong." }
    switch core {
    case .Network: return "Backup failed — couldn't reach the server. Check your connection and try again."
    case let .Api(code, _):
        switch code {
        case .unknownToken, .missingToken: return "Backup failed — your session has expired. Sign in again, then retry."
        case .limitExceeded: return "Backup failed — too many attempts. Wait a moment and try again."
        default: return "Backup failed — the server rejected it. Try again later."
        }
    case .Crypto: return "Backup failed — this device's encryption keys couldn't be prepared."
    case .Storage: return "Backup failed — couldn't access secure storage on this device."
    case .Protocol: return "Backup failed — unexpected response from the server."
    }
}

/// The message for a failed key **restore** (M4.3). A missing backup (`.notFound`)
/// and a wrong recovery key (`.Crypto`) are distinct typed errors from the core,
/// so each gets its own advice. Mirrors Android's `restoreErrorMessage`.
func restoreErrorMessage(_ error: Error) -> String {
    guard let core = error as? CoreError else { return "Restore failed — something went wrong." }
    switch core {
    case .Crypto: return "Restore failed — that recovery key doesn't match your backup. Check it and try again."
    case .Network: return "Restore failed — couldn't reach the server. Check your connection and try again."
    case let .Api(code, _):
        switch code {
        case .notFound: return "Restore failed — there's no key backup for this account yet. Back up from a device that still has your keys."
        case .unknownToken, .missingToken: return "Restore failed — your session has expired. Sign in again, then retry."
        case .limitExceeded: return "Restore failed — too many attempts. Wait a moment and try again."
        default: return "Restore failed — the server rejected it. Try again later."
        }
    case .Storage: return "Restore failed — couldn't access secure storage on this device."
    case .Protocol: return "Restore failed — unexpected response from the server."
    }
}

