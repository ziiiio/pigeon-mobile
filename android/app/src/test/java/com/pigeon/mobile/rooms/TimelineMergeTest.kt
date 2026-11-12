package com.pigeon.mobile.rooms

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.pigeon_mobile_core.TimelineEvent

/** `mergeTimeline` — pure Kotlin over the generated record (JVM-testable). */
class TimelineMergeTest {

    private fun ev(id: String, cursor: Long, body: String? = null, system: String? = null) =
        TimelineEvent(
            eventId = id, sender = "@a:s", originServerTs = cursor, cursor = cursor,
            body = body, systemText = system, pending = false, failed = false, image = null,
        )

    @Test
    fun the_fresh_copy_of_an_event_replaces_the_stale_one() {
        // Loaded while pending decryption (no body, no system line → hidden)...
        val onScreen = listOf(ev("\$a", 1, body = "hi"), ev("\$enc", 2))
        // ...then decrypted after a key restore: the fresh read must win.
        val fresh = listOf(ev("\$enc", 2, body = "the secret"))
        val merged = mergeTimeline(onScreen, fresh)
        assertEquals(listOf("\$a", "\$enc"), merged.map { it.eventId })
        assertEquals("the secret", merged[1].body)
    }

    @Test
    fun a_failed_decrypt_replaces_the_hidden_pending_copy() {
        val merged = mergeTimeline(listOf(ev("\$enc", 2)), listOf(ev("\$enc", 2, system = "⚠ Unable to decrypt")))
        assertEquals("⚠ Unable to decrypt", merged.single().systemText)
    }

    @Test
    fun older_pages_are_prepended_in_cursor_order_without_duplicates() {
        val merged = mergeTimeline(listOf(ev("\$c", 3), ev("\$d", 4)), listOf(ev("\$a", 1), ev("\$b", 2), ev("\$c", 3)))
        assertEquals(listOf("\$a", "\$b", "\$c", "\$d"), merged.map { it.eventId })
    }
}
