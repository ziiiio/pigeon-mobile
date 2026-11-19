package com.pigeon.mobile.auth

import android.security.NetworkSecurityPolicy
import java.net.URI

/** Shown when the user enters an `http://` homeserver the build doesn't allow. Mirrors iOS. */
internal const val CLEARTEXT_REFUSED =
    "This address uses http://, which would send your password and messages unencrypted. " +
        "Use an https:// address."

/**
 * Whether a homeserver address may be used, checked **before** it reaches the core.
 *
 * Why here and not just in the manifest: Android's network-security-config is enforced
 * by the platform HTTP stacks (HttpURLConnection, OkHttp, …), not by raw sockets — and the
 * Rust core's `reqwest` client opens its own. So the config alone would not stop a release
 * build sending the password and token over `http://`. This asks the same policy
 * ([NetworkSecurityPolicy], which reads `res/xml/network_security_config.xml`): release
 * builds permit no cleartext; debug builds permit `10.0.2.2` and `localhost` only.
 *
 * Returns the refusal message, or `null` if the address may be used. An address that
 * isn't a parseable `http://host` URL passes through, for the core to reject as usual.
 */
fun cleartextRefusal(
    server: String,
    isCleartextPermitted: (String) -> Boolean = ::platformPermitsCleartext,
): String? {
    val uri = runCatching { URI(server.trim()) }.getOrNull() ?: return null
    if (!uri.scheme.equals("http", ignoreCase = true)) return null
    val host = uri.host ?: return null
    return if (isCleartextPermitted(host)) null else CLEARTEXT_REFUSED
}

private fun platformPermitsCleartext(host: String): Boolean =
    NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(host)
