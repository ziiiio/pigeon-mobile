package com.pigeon.mobile.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** `cleartextRefusal` — pure Kotlin; the platform policy is injected. Mirrored by iOS `CleartextPolicyTests`. */
class CleartextPolicyTest {

    private val onlyDevHost: (String) -> Boolean = { it == "10.0.2.2" }
    private val never: (String) -> Boolean = { error("the policy must not be consulted for $it") }

    @Test
    fun https_is_always_allowed_without_consulting_the_policy() {
        assertNull(cleartextRefusal("https://pigeon.example", never))
    }

    @Test
    fun http_to_a_permitted_host_is_allowed() {
        assertNull(cleartextRefusal("http://10.0.2.2:8008", onlyDevHost))
    }

    @Test
    fun http_to_any_other_host_is_refused() {
        assertEquals(CLEARTEXT_REFUSED, cleartextRefusal("http://pigeon.example", onlyDevHost))
    }

    @Test
    fun the_scheme_check_ignores_case_and_surrounding_space() {
        assertEquals(CLEARTEXT_REFUSED, cleartextRefusal("  HTTP://pigeon.example  ", onlyDevHost))
    }

    @Test
    fun an_unparseable_address_is_left_for_the_core_to_reject() {
        assertNull(cleartextRefusal("not a url", never))
    }
}
