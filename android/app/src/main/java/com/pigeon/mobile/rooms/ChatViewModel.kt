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
import uniffi.pigeon_mobile_core.TimelineEvent

/** How many timeline events to read per page. */
private const val PAGE: UInt = 50u

/** UI state for a room's timeline (M2.4). Events are oldest-first. */
data class ChatState(
    val loading: Boolean = true,
    val events: List<TimelineEvent> = emptyList(),
    /** True once backward pagination has reached the start of stored history. */
    val atTop: Boolean = false,
    val error: String? = null,
)

/**
 * The thin view-model over a room's timeline (M2.4). Reads are offline-first from
 * the local store; on open it also backfills recent history from the server
 * ([PigeonClient.fetchMessages]) so the room isn't empty before the sync loop has
 * covered it. The screen calls [refresh] when the sync loop signals a change.
 *
 * No protocol logic here: bodies and system lines are pre-rendered by the core
 * ([TimelineEvent]); this only pages and merges by the opaque `cursor`.
 */
class ChatViewModel(
    private val client: PigeonClient,
    private val roomId: String,
) : ViewModel() {

    private val _state = MutableStateFlow(ChatState())
    val state: StateFlow<ChatState> = _state.asStateFlow()

    init {
        refresh()
        // Top up recent history from the server; the reload afterwards folds it in.
        viewModelScope.launch {
            try {
                withContext(Dispatchers.IO) { client.fetchMessages(roomId, PAGE) }
            } catch (_: CoreException) {
                // Offline / transient — the store read already showed what we have.
            }
            refresh()
        }
    }

    /** Re-read the newest page and merge it with what's already loaded. */
    fun refresh() {
        viewModelScope.launch {
            _state.value = try {
                val newest = withContext(Dispatchers.IO) { client.timeline(roomId, PAGE, null) }
                _state.value.copy(loading = false, events = merge(_state.value.events, newest))
            } catch (e: CoreException) {
                _state.value.copy(loading = false, error = e.message)
            }
        }
    }

    /** Page backwards from the oldest loaded event (scroll-to-load-older). */
    fun loadOlder() {
        val state = _state.value
        if (state.atTop || state.loading || state.events.isEmpty()) return
        val before = state.events.first().cursor
        viewModelScope.launch {
            _state.value = try {
                val older = withContext(Dispatchers.IO) { client.timeline(roomId, PAGE, before) }
                if (older.isEmpty()) {
                    _state.value.copy(atTop = true)
                } else {
                    _state.value.copy(events = merge(older, _state.value.events))
                }
            } catch (e: CoreException) {
                _state.value.copy(error = e.message)
            }
        }
    }

    /**
     * Send a plaintext message. The core writes a local echo and queues it
     * (offline-first), so [refresh] afterwards shows the message immediately —
     * pending, then confirmed once the server acks (or failed on rejection).
     */
    fun send(body: String) {
        val text = body.trim()
        if (text.isEmpty()) return
        viewModelScope.launch {
            try {
                client.sendMessage(roomId, text)
            } catch (e: CoreException) {
                _state.value = _state.value.copy(error = e.message)
            }
            refresh()
        }
    }

    /** Union two pages by event id and order by the opaque cursor (DAG depth). */
    private fun merge(a: List<TimelineEvent>, b: List<TimelineEvent>): List<TimelineEvent> =
        (a + b).distinctBy { it.eventId }.sortedBy { it.cursor }
}
