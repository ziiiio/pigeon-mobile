package com.pigeon.mobile.rooms

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.ErrorCode

/** Backup/restore outcome messages — pure Kotlin over the generated types. */
class BackupFeedbackTest {

    private val everyVariant = listOf(
        CoreException.Network("boom"),
        CoreException.Api(ErrorCode.NotFound, "server said so"),
        CoreException.Api(ErrorCode.Forbidden, "server said so"),
        CoreException.Api(ErrorCode.UnknownToken, "server said so"),
        CoreException.Api(ErrorCode.LimitExceeded, "server said so"),
        CoreException.Api(ErrorCode.Other("P_FUTURE"), "server said so"),
        CoreException.Crypto("server said so"),
        CoreException.Storage("server said so"),
        CoreException.Protocol("server said so"),
    )

    @Test
    fun every_failure_says_which_action_failed_and_never_leaks_raw_text() {
        for (e in everyVariant) {
            val b = backupErrorMessage(e)
            val r = restoreErrorMessage(e)
            assertTrue(b, b.startsWith("Backup failed"))
            assertTrue(r, r.startsWith("Restore failed"))
            for (m in listOf(b, r)) {
                assertFalse(m, m.contains("server said so"))
                assertFalse(m, m.contains("uniffi"))
            }
        }
    }

    @Test
    fun restore_distinguishes_no_backup_from_a_wrong_recovery_key() {
        val noBackup = restoreErrorMessage(CoreException.Api(ErrorCode.NotFound, "x"))
        val wrongKey = restoreErrorMessage(CoreException.Crypto("x"))
        assertTrue(noBackup, noBackup.contains("no key backup"))
        assertTrue(wrongKey, wrongKey.contains("recovery key doesn't match"))
        assertNotEquals(noBackup, wrongKey)
    }

    @Test
    fun forbidden_never_reads_as_a_bad_password() {
        // The sign-in mapper's wording for Forbidden must not leak into backup UX.
        val e = CoreException.Api(ErrorCode.Forbidden, "x")
        assertFalse(backupErrorMessage(e).contains("password"))
        assertFalse(restoreErrorMessage(e).contains("password"))
    }

    @Test
    fun restore_success_notice_is_honest_about_history() {
        // It promises new messages decrypt — not that old history comes back.
        assertTrue(RESTORE_OK.contains("New messages"))
        assertFalse(RESTORE_OK.contains("all your messages"))
    }
}
