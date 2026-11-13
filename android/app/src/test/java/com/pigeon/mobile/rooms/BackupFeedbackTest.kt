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

    private val fmt: (Long) -> String = { "T$it" }

    @Test
    fun restore_success_says_what_came_back_and_what_cannot() {
        val dated = restoreSuccessMessage(42L, fmt)
        assertTrue(dated, dated.contains("backup of T42"))
        assertTrue(dated, dated.contains("New messages will decrypt"))
        assertTrue(dated, dated.contains("can't be recovered"))
        assertFalse(dated, dated.contains("all your messages"))
        // A backup stored without a time (reference CLI) still gets the caveat.
        val undated = restoreSuccessMessage(null, fmt)
        assertTrue(undated, undated.contains("can't be recovered"))
        assertFalse(undated, undated.contains("null"))
    }

    @Test
    fun the_backup_explainer_states_its_limits() {
        // Said before every backup: it's a snapshot, older messages are gone for
        // good, later rooms aren't covered, and a new backup retires the old key.
        assertTrue(BACKUP_SCOPE.contains("snapshot"))
        assertTrue(BACKUP_SCOPE.contains("can't bring back messages from before the backup"))
        assertTrue(BACKUP_SCOPE.contains("rooms you join later"))
        assertTrue(BACKUP_SCOPE.contains("old recovery key stops working"))
    }

    @Test
    fun coverage_and_restore_scope_name_the_backup_time() {
        assertTrue(backupCoverageText(7L, fmt).contains("from T7 onward"))
        assertTrue(restoreScopeText(LastBackup.At(7L), fmt).contains("from T7 onward"))
        val unknown = restoreScopeText(LastBackup.At(null), fmt)
        assertTrue(unknown, unknown.contains("when the backup was made"))
    }

    @Test
    fun last_backup_line_covers_every_state() {
        assertTrue(lastBackupText(LastBackup.At(9L), fmt).contains("T9"))
        assertTrue(lastBackupText(LastBackup.None, fmt).contains("no key backup"))
        assertTrue(lastBackupText(LastBackup.At(null), fmt).contains("date is unknown"))
        assertTrue(lastBackupText(LastBackup.Unavailable, fmt).contains("Couldn't check"))
        assertTrue(lastBackupText(LastBackup.Loading, fmt).contains("Checking"))
    }
}
