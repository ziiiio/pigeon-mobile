package com.pigeon.mobile.rooms

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

    fun clearError() {
        _state.value = _state.value.copy(actionError = null)
    }
}
