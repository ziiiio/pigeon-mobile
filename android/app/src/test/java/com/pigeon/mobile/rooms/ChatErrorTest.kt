package com.pigeon.mobile.rooms

import com.pigeon.mobile.auth.authErrorMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.ErrorCode

/** The chat error mapper — pure Kotlin over the generated types (JVM-testable). */
class ChatErrorTest {

    @Test
    fun malformed_invitee_id_explains_the_id_format() {
        val msg = chatErrorMessage(
            CoreException.Api(ErrorCode.InvalidUsername, "identifier must start with sigil '@'"),
        )
        assertTrue(msg, msg.contains("user ID"))
        assertFalse("must not read as a sign-in error", msg.contains("Usernames may use"))
    }

    @Test
    fun forbidden_is_a_permission_error_not_bad_credentials() {
        val msg = chatErrorMessage(CoreException.Api(ErrorCode.Forbidden, "not joined"))
        assertTrue(msg, msg.contains("permission"))
        assertFalse(msg.contains("password"))
    }

    @Test
    fun never_leaks_the_raw_reason_or_exception_text() {
        val e = CoreException.Api(ErrorCode.InvalidUsername, "server said so")
        val msg = chatErrorMessage(e)
        assertFalse(msg.contains("server said so"))
        assertFalse(msg.contains("uniffi"))
    }

    @Test
    fun other_errors_fall_back_to_the_shared_wording() {
        val e = CoreException.Network("boom")
        assertEquals(authErrorMessage(e), chatErrorMessage(e))
    }
}
