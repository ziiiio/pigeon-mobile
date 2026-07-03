package com.pigeon.mobile.auth

import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.ErrorCode

/**
 * The error mapper is pure Kotlin over the generated types, so it unit-tests on
 * the JVM without a device or the native library.
 */
class AuthErrorTest {

    @Test
    fun userInUse_is_specific() {
        val msg = authErrorMessage(CoreException.Api(ErrorCode.UserInUse, "exists"))
        assertTrue(msg, msg.contains("taken", ignoreCase = true))
    }

    @Test
    fun forbidden_reads_as_bad_credentials() {
        val msg = authErrorMessage(CoreException.Api(ErrorCode.Forbidden, "nope"))
        assertTrue(msg, msg.contains("Incorrect", ignoreCase = true))
    }

    @Test
    fun network_error_mentions_reaching_the_server() {
        val msg = authErrorMessage(CoreException.Network("boom"))
        assertTrue(msg, msg.contains("reach", ignoreCase = true))
    }

    @Test
    fun unknown_future_code_still_yields_a_message() {
        val msg = authErrorMessage(CoreException.Api(ErrorCode.Other("P_FUTURE_CODE"), "x"))
        assertTrue(msg.isNotBlank())
    }

    @Test
    fun every_core_exception_variant_maps_to_a_message() {
        // If a new CoreException variant is added, `authErrorMessage`'s exhaustive
        // `when` won't compile — but assert non-blank output for the ones we know.
        val cases = listOf(
            authErrorMessage(CoreException.Api(ErrorCode.Unknown, "x")),
            authErrorMessage(CoreException.Network("x")),
            authErrorMessage(CoreException.Protocol("x")),
            authErrorMessage(CoreException.Storage("x")),
            authErrorMessage(CoreException.Crypto("x")),
        )
        cases.forEach { assertTrue(it.isNotBlank()) }
    }
}
