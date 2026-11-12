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
                _state.value.copy(backupInProgress = false, recoveryKey = client.backup())
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
                client.restoreBackup(recoveryKey.trim())
                _state.value.copy(restoreInProgress = false, notice = RESTORE_OK)
            } catch (e: CoreException) {
                _state.value.copy(restoreInProgress = false, actionError = restoreErrorMessage(e))
            }
        }
    }

    /** Dismiss a shown success notice. */
    fun clearNotice() {
        _state.value = _state.value.copy(notice = null)
    }

    /** Dismiss the shown recovery key (the user has saved it). */
    fun clearRecoveryKey() {
        _state.value = _state.value.copy(recoveryKey = null)
    }

    fun clearError() {
        _state.value = _state.value.copy(actionError = null)
    }
}

/** Shown after a successful restore. Honest about scope: restoring brings back the
 * device's identity + room memberships (new messages decrypt), not history that
 * forward secrecy already destroyed — see design/history-backup.md in ../pigeon. */
internal const val RESTORE_OK =
    "Encryption keys restored. New messages in your encrypted rooms will decrypt on this device."

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

