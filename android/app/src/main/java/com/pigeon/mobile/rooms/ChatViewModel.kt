package com.pigeon.mobile.rooms

import com.pigeon.mobile.auth.authErrorMessage
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
import uniffi.pigeon_mobile_core.ImageContent
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
                // Re-read the whole loaded range, not just the newest page, so every
                // row on screen picks up its current state (decrypted / failed /
                // confirmed) — not only the most recent PAGE.
                val window = maxOf(PAGE, _state.value.events.size.toUInt())
                val fresh = withContext(Dispatchers.IO) { client.timeline(roomId, window, null) }
                _state.value.copy(loading = false, events = mergeTimeline(_state.value.events, fresh))
            } catch (e: CoreException) {
                _state.value.copy(loading = false, error = chatErrorMessage(e))
            }
        }
    }

    /**
     * Drop the shown error. Called when the user taps it away, and at the start of
     * every user action (send / invite / attach) so a failure from an earlier
     * action doesn't linger over a later one — the error is about the last thing
     * the user did, not a sticky banner.
     */
    fun clearError() {
        _state.value = _state.value.copy(error = null)
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
                    _state.value.copy(events = mergeTimeline(_state.value.events, older))
                }
            } catch (e: CoreException) {
                _state.value.copy(error = chatErrorMessage(e))
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
        clearError()
        viewModelScope.launch {
            try {
                client.sendMessage(roomId, text)
            } catch (e: CoreException) {
                _state.value = _state.value.copy(error = chatErrorMessage(e))
            }
            refresh()
        }
    }

    /**
     * Invite a user to this room (M2.6). The invite is a membership event; it
     * surfaces in the timeline via the sync loop (rendered as a system line by
     * the core), so [refresh] afterwards catches it once the store folds it in.
     * The invitee accepts by joining the room id.
     */
    fun invite(userId: String) {
        val id = userId.trim()
        if (id.isEmpty()) return
        clearError()
        viewModelScope.launch {
            try {
                client.invite(roomId, id)
            } catch (e: CoreException) {
                _state.value = _state.value.copy(error = chatErrorMessage(e))
            }
            refresh()
        }
    }

    /**
     * Attach an image (M4.1/M4.2): hand the raw bytes to the core, which uploads
     * and sends the right message for the room — a `p.image` in a plaintext room,
     * or (for an encrypted room) an encrypted-then-uploaded blob with the per-file
     * key carried inside the E2EE'd message. All crypto/protocol stays in the core.
     * The image appears once the sync loop delivers it.
     */
    fun sendImage(bytes: ByteArray, mimetype: String, width: Int, height: Int) {
        clearError()
        viewModelScope.launch {
            try {
                withContext(Dispatchers.IO) {
                    client.sendImage(roomId, bytes, mimetype, width.toUInt(), height.toUInt(), "")
                }
            } catch (e: CoreException) {
                _state.value = _state.value.copy(error = chatErrorMessage(e))
            }
            refresh()
        }
    }

    /** Download an image's displayable bytes for inline rendering (M4.1/M4.2);
     * the core decrypts encrypted images (the key never leaves it). `null` on
     * failure (offline / gone). Off the main thread. */
    suspend fun downloadImage(image: ImageContent): ByteArray? =
        try {
            withContext(Dispatchers.IO) { client.downloadImage(image) }
        } catch (_: CoreException) {
            null
        }

    /** Union two pages by event id and order by the opaque cursor (DAG depth). */
}

/**
 * Merge a freshly-read page into what's on screen, by event id, ordered by
 * cursor. **The fresh copy wins**: an event's rendering changes after it first
 * loads — pending decryption (hidden) → decrypted or "unable to decrypt", sending
 * → confirmed — and keeping the stale copy left those rows hidden for as long as
 * the (cached, per-room) view-model lived, e.g. every message after a key restore.
 */
internal fun mergeTimeline(existing: List<TimelineEvent>, fresh: List<TimelineEvent>): List<TimelineEvent> =
    (fresh + existing).distinctBy { it.eventId }.sortedBy { it.cursor }

/**
 * The user-facing message for a failed chat action (send / invite / attach /
 * timeline read). Branches on the typed `P_` code, never on error text
 * (CLAUDE.md), and never shows the raw exception. Reuses [authErrorMessage] except
 * where its sign-in wording would mislead in a chat: a malformed invitee id comes
 * back as `InvalidUsername` ("Usernames may use only…"), and a refused action as
 * `Forbidden` ("Incorrect username or password"). Mirrors iOS `chatErrorMessage`.
 */
fun chatErrorMessage(e: CoreException): String = when {
    e is CoreException.Api && e.code is ErrorCode.InvalidUsername ->
        "That isn't a valid user ID. Use the full form, e.g. @bob:localhost."
    e is CoreException.Api && e.code is ErrorCode.Forbidden ->
        "You don't have permission to do that in this room."
    else -> authErrorMessage(e)
}
