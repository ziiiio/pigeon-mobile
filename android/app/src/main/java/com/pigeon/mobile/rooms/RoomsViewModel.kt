package com.pigeon.mobile.rooms

import uniffi.pigeon_mobile_core.ErrorCode
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.PigeonClient
import uniffi.pigeon_mobile_core.Room

/** UI state for the room list (M2.3). */
data class RoomsState(
    val rooms: List<Room> = emptyList(),
    /** Connectivity to the homeserver, from the sync loop's `on_status`. */
    val connected: Boolean = true,
    /** True until the first room-list read completes. */
    val loading: Boolean = true,
    /** A transient action failure (create/join/reload) to surface then dismiss. */
    val actionError: String? = null,
    /** After a successful key backup, the recovery key to show the user once
     * (M4.3). The user must save it — it's the only secret that can restore the
     * backup, and it never reaches the server. Cleared when the dialog dismisses. */
    val recoveryKey: String? = null,
    /** When the backup behind [recoveryKey] was taken (Unix ms) — the dialog says
     * it only covers messages from then on. */
    val backupCreatedAtMs: Long? = null,
    /** The account's server-side backup, as last fetched for the backup/restore
     * dialogs (they say when it was taken, i.e. what it can't restore). */
    val lastBackup: LastBackup = LastBackup.Loading,
    /** A key backup is in flight — the UI shows progress and disables the action. */
    val backupInProgress: Boolean = false,
    /** A key restore is in flight — same. */
    val restoreInProgress: Boolean = false,
    /** A transient success message to surface then dismiss (e.g. "keys restored"). */
    val notice: String? = null,
)

/**
 * The thin view-model over the core's room API (M2.3). It owns no protocol or
 * crypto logic: it reads the room list from the store, drives create/join, and
 * folds the sync loop's signals into UI state. Reads are offline-first (straight
 * from the local store); the sync loop — started by the screen so its lifecycle
 * bounds it (Gotcha #6) — keeps the store current and calls back into [reload].
 */
class RoomsViewModel(private val client: PigeonClient) : ViewModel() {

    private val _state = MutableStateFlow(RoomsState())
    val state: StateFlow<RoomsState> = _state.asStateFlow()

    init {
        reload()
    }

    /** Re-read the room list from the local store (no network). */
    fun reload() {
        viewModelScope.launch {
            _state.value = try {
                val rooms = withContext(Dispatchers.IO) { client.listRooms() }
                _state.value.copy(rooms = rooms, loading = false)
            } catch (e: CoreException) {
                _state.value.copy(loading = false, actionError = e.message)
            }
        }
    }

    /** Fold the sync loop's connectivity signal into state. */
    fun setConnected(connected: Boolean) {
        _state.value = _state.value.copy(connected = connected)
    }

    /**
     * Create a room. Its state arrives via the sync loop, which reloads the list
     * on change — so there's nothing to merge here beyond surfacing an error.
     * When [encrypted], the core creates the room E2EE (marks it + hosts the MLS
     * group); the UI is otherwise identical (encryption is transparent, M3).
     */
    fun createRoom(name: String?, topic: String?, encrypted: Boolean) {
        viewModelScope.launch {
            try {
                val n = name?.ifBlank { null }
                val t = topic?.ifBlank { null }
                if (encrypted) {
                    client.createEncryptedRoom(n, t)
                } else {
                    client.createRoom(n, t)
                }
            } catch (e: CoreException) {
                _state.value = _state.value.copy(actionError = e.message)
            }
        }
    }

    /** Join a room by id; membership + timeline arrive on the next sync. */
    fun joinRoom(roomId: String) {
        viewModelScope.launch {
            try {
                client.joinRoom(roomId.trim())
            } catch (e: CoreException) {
                _state.value = _state.value.copy(actionError = e.message)
            }
        }
    }

    /** The sync loop ended fatally (e.g. the token was revoked). */
    fun onSyncFailed(message: String?) {
        _state.value = _state.value.copy(connected = false, actionError = message)
    }

    /**
     * Back up the device's encryption keys (M4.3). On success the returned
     * recovery key lands in [RoomsState.recoveryKey] for the UI to show once —
     * it's the only secret that can restore the backup and never reaches the
     * server. The token never crosses into UI state (Gotcha #1); this is the
     * recovery key, a distinct user-facing secret the user must save.
     */
    fun backup() {
        if (_state.value.backupInProgress) return
        // Clear any stale outcome so the user sees *this* attempt's result.
        _state.value = _state.value.copy(backupInProgress = true, actionError = null, notice = null)
        viewModelScope.launch {
            _state.value = try {
                // Success: the recovery-key dialog opens, headed "Backup saved".
                val made = client.backup()
                _state.value.copy(
                    backupInProgress = false,
                    recoveryKey = made.recoveryKey,
                    backupCreatedAtMs = made.createdAtMs,
                    lastBackup = LastBackup.At(made.createdAtMs),
                )
            } catch (e: CoreException) {
                _state.value.copy(backupInProgress = false, actionError = backupErrorMessage(e))
            }
        }
    }

    /** Restore the device's encryption keys from the server-side backup using the
     * user's recovery key (M4.3). Recovered groups/identity surface via sync. */
    fun restoreBackup(recoveryKey: String) {
        if (_state.value.restoreInProgress) return
        _state.value = _state.value.copy(restoreInProgress = true, actionError = null, notice = null)
        viewModelScope.launch {
            _state.value = try {
                val restored = client.restoreBackup(recoveryKey.trim())
                _state.value.copy(
                    restoreInProgress = false,
                    notice = restoreSuccessMessage(restored.backedUpAtMs, ::formatBackupTime),
                )
            } catch (e: CoreException) {
                _state.value.copy(restoreInProgress = false, actionError = restoreErrorMessage(e))
            }
        }
    }

    /**
     * Fetch whether a backup exists and when it was taken, for the backup and
     * restore dialogs. Failure isn't an error the user must act on — the dialog
     * just says the date is unavailable.
     */
    fun loadBackupStatus() {
        _state.value = _state.value.copy(lastBackup = LastBackup.Loading)
        viewModelScope.launch {
            val last = try {
                client.backupStatus()?.let { LastBackup.At(it.backedUpAtMs) } ?: LastBackup.None
            } catch (e: CoreException) {
                LastBackup.Unavailable
            }
            _state.value = _state.value.copy(lastBackup = last)
        }
    }

    /** Dismiss a shown success notice. */
    fun clearNotice() {
        _state.value = _state.value.copy(notice = null)
    }

    /** Dismiss the shown recovery key (the user has saved it). */
    fun clearRecoveryKey() {
        _state.value = _state.value.copy(recoveryKey = null, backupCreatedAtMs = null)
    }

    fun clearError() {
        _state.value = _state.value.copy(actionError = null)
    }
}

/** The account's key backup, as the backup/restore dialogs know it. */
sealed interface LastBackup {
    data object Loading : LastBackup
    /** No backup on the server yet. */
    data object None : LastBackup
    /** A backup exists; [atMs] is null when it was stored without a time (CLI). */
    data class At(val atMs: Long?) : LastBackup
    /** Couldn't ask the server. */
    data object Unavailable : LastBackup
}

/*
 * What a key backup can and can't do, said up front (M4.3). A backup is a
 * snapshot of this device's MLS keys at one moment: it restores rooms and
 * messages from that moment on, never messages the device had already read —
 * MLS deletes those keys after use (forward secrecy), so nothing can recover
 * them. See design/history-backup.md in ../pigeon. All wording mirrors iOS
 * `RoomsViewModel.swift` — keep them in lockstep.
 */

/** Shown before every backup, beside the last-backup line. */
internal const val BACKUP_SCOPE =
    "A key backup is a snapshot of this device's encryption keys at the moment you make it. " +
        "Restoring it on a new or reset device:\n" +
        "• lets you read messages sent after the backup, in rooms you were in when you made it;\n" +
        "• can't bring back messages from before the backup — Pigeon deletes those keys once " +
        "they've been used, so nobody can recover them, not even the server;\n" +
        "• doesn't include rooms you join later — back up again after joining one.\n" +
        "A new backup replaces the previous one, and the old recovery key stops working."

/** The "last backup" line in the backup and restore dialogs. */
fun lastBackupText(last: LastBackup, format: (Long) -> String): String = when (last) {
    LastBackup.Loading -> "Checking for an existing backup…"
    LastBackup.None -> "You have no key backup yet."
    is LastBackup.At ->
        last.atMs?.let { "Your last backup was made ${format(it)}." }
            ?: "You have a backup, but its date is unknown."
    LastBackup.Unavailable -> "Couldn't check for an existing backup."
}

/** In the restore dialog: what restoring *this* backup will and won't recover. */
fun restoreScopeText(last: LastBackup, format: (Long) -> String): String {
    val at = (last as? LastBackup.At)?.atMs
    return if (at != null) {
        "Restoring recovers your rooms and messages from ${format(at)} onward. " +
            "Messages from before then can't be recovered."
    } else {
        "Restoring recovers your rooms and messages from when the backup was made onward. " +
            "Messages from before then can't be recovered."
    }
}

/** In the "Backup saved" dialog: the period this backup covers. */
fun backupCoverageText(createdAtMs: Long, format: (Long) -> String): String =
    "This backup covers messages from ${format(createdAtMs)} onward. " +
        "Messages from before then can't be restored with it."

/** Shown after a successful restore: what came back, and what can't. */
fun restoreSuccessMessage(backedUpAtMs: Long?, format: (Long) -> String): String {
    val since = backedUpAtMs?.let { "your backup of ${format(it)}" } ?: "your backup"
    return "Encryption keys restored from $since. New messages will decrypt; " +
        "messages from before the backup can't be recovered."
}

/** The user-facing date for a backup time, in the device's locale + zone. */
fun formatBackupTime(ms: Long): String =
    java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.MEDIUM, java.text.DateFormat.SHORT)
        .format(java.util.Date(ms))

/**
 * The message for a failed key **backup** (M4.3). Branches on the typed error
 * (never on text, CLAUDE.md) and never shows the raw exception. Mirrors iOS
 * `backupErrorMessage` — keep the wording in lockstep.
 */
fun backupErrorMessage(e: CoreException): String = when (e) {
    is CoreException.Network -> "Backup failed — couldn't reach the server. Check your connection and try again."
    is CoreException.Api -> when (e.code) {
        is ErrorCode.UnknownToken, is ErrorCode.MissingToken -> "Backup failed — your session has expired. Sign in again, then retry."
        is ErrorCode.LimitExceeded -> "Backup failed — too many attempts. Wait a moment and try again."
        else -> "Backup failed — the server rejected it. Try again later."
    }
    is CoreException.Crypto -> "Backup failed — this device's encryption keys couldn't be prepared."
    is CoreException.Storage -> "Backup failed — couldn't access secure storage on this device."
    is CoreException.Protocol -> "Backup failed — unexpected response from the server."
}

/**
 * The message for a failed key **restore** (M4.3). A missing backup (`NotFound`)
 * and a wrong recovery key (`Crypto`) are distinct typed errors from the core, so
 * each gets its own advice. Mirrors iOS `restoreErrorMessage`.
 */
fun restoreErrorMessage(e: CoreException): String = when (e) {
    is CoreException.Crypto -> "Restore failed — that recovery key doesn't match your backup. Check it and try again."
    is CoreException.Network -> "Restore failed — couldn't reach the server. Check your connection and try again."
    is CoreException.Api -> when (e.code) {
        is ErrorCode.NotFound -> "Restore failed — there's no key backup for this account yet. Back up from a device that still has your keys."
        is ErrorCode.UnknownToken, is ErrorCode.MissingToken -> "Restore failed — your session has expired. Sign in again, then retry."
        is ErrorCode.LimitExceeded -> "Restore failed — too many attempts. Wait a moment and try again."
        else -> "Restore failed — the server rejected it. Try again later."
    }
    is CoreException.Storage -> "Restore failed — couldn't access secure storage on this device."
    is CoreException.Protocol -> "Restore failed — unexpected response from the server."
}

