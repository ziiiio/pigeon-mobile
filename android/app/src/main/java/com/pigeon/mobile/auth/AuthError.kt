package com.pigeon.mobile.auth

import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.ErrorCode

/**
 * Maps a typed [CoreException] to a user-facing message.
 *
 * The UI branches on the typed code, never on error text (CLAUDE.md) — and it
 * must handle every variant, because a federated, offline-prone client will hit
 * them. A pure function so it can be unit-tested without a device.
 */
fun authErrorMessage(e: CoreException): String = when (e) {
    is CoreException.Api -> when (e.code) {
        is ErrorCode.UserInUse -> "That username is already taken."
        is ErrorCode.Forbidden -> "Incorrect username or password."
        is ErrorCode.InvalidUsername -> "Usernames may use only a–z, 0–9, and . _ -"
        is ErrorCode.UnknownToken, is ErrorCode.MissingToken ->
            "Your session has expired. Please sign in again."
        is ErrorCode.LimitExceeded -> "Too many attempts. Please wait a moment and try again."
        is ErrorCode.BadJson, is ErrorCode.NotJson -> "The server rejected the request."
        is ErrorCode.NotFound -> "Not found on this server."
        else -> "The server reported an error."
    }
    is CoreException.Network -> "Can't reach the server. Check the address and your connection."
    is CoreException.Protocol -> "Unexpected response from the server."
    is CoreException.Storage -> "Couldn't access secure storage on this device."
    is CoreException.Crypto -> "A security error occurred."
}
