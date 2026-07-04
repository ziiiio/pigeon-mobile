package com.pigeon.mobile.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.PigeonClient
import uniffi.pigeon_mobile_core.Session
import uniffi.pigeon_mobile_core.login as coreLogin
import uniffi.pigeon_mobile_core.register as coreRegister
import uniffi.pigeon_mobile_core.restoreSession as coreRestoreSession

/** UI state for the auth flow. */
sealed interface AuthState {
    /** Checking for a persisted session on launch. */
    data object Restoring : AuthState

    /** Signed out — showing the form. [error] holds the last failure, if any. */
    data class SignedOut(val error: String? = null) : AuthState

    /** A register/login is in flight. */
    data object Submitting : AuthState

    /**
     * Signed in with this session identity. [client] is the opaque core handle
     * the rooms/sync flows hang off (the token stays inside it — Gotcha #1).
     * [signingOut] is true while a logout is in flight; [error] carries a logout
     * failure that left the session intact (so the user can retry rather than see
     * a signed-out UI over a live session).
     */
    data class SignedIn(
        val session: Session,
        val client: PigeonClient,
        val signingOut: Boolean = false,
        val error: String? = null,
    ) : AuthState
}

/**
 * The thin view-model over the core's session API. It owns no protocol or crypto
 * logic — it calls the core's suspend functions, holds the resulting client
 * handle, and exposes UI state. (CLAUDE.md: native = UI + a thin view-model.)
 */
class AuthViewModel : ViewModel() {

    private val _state = MutableStateFlow<AuthState>(AuthState.Restoring)
    val state: StateFlow<AuthState> = _state.asStateFlow()

    // The logged-in client handle. The token stays inside it (in the core) — this
    // is an opaque handle, kept for the flows that hang off it in later phases
    // (sync, logout); never unwrapped into app-level secret state.
    private var client: PigeonClient? = null

    init {
        // Restore a persisted session on launch.
        viewModelScope.launch {
            _state.value = try {
                coreRestoreSession()?.let { restored ->
                    client = restored
                    AuthState.SignedIn(restored.session(), restored)
                } ?: AuthState.SignedOut()
            } catch (e: CoreException) {
                // A restore fault (e.g. storage) must not wedge launch — fall
                // back to the sign-in form and surface the reason.
                AuthState.SignedOut(authErrorMessage(e))
            }
        }
    }

    fun login(server: String, username: String, password: String) =
        submit { coreLogin(server.trim(), username.trim(), password) }

    fun register(server: String, username: String, password: String) =
        submit { coreRegister(server.trim(), username.trim(), password) }

    /**
     * Sign out: revoke the token server-side and clear the persisted session
     * (both in the core). On success the handle is dropped and we return to the
     * form. The core clears local state even if the server revoke fails; the only
     * error it surfaces is a keystore fault — in that case the session is still
     * live, so we stay signed in and show the reason for a retry.
     */
    fun logout() {
        val current = _state.value
        if (current !is AuthState.SignedIn || current.signingOut) return
        val c = client ?: return
        _state.value = current.copy(signingOut = true, error = null)
        viewModelScope.launch {
            _state.value = try {
                c.logout()
                client = null
                AuthState.SignedOut()
            } catch (e: CoreException) {
                current.copy(signingOut = false, error = authErrorMessage(e))
            }
        }
    }

    private fun submit(call: suspend () -> PigeonClient) {
        if (_state.value == AuthState.Submitting) return
        _state.value = AuthState.Submitting
        viewModelScope.launch {
            _state.value = try {
                val c = call()
                client = c
                AuthState.SignedIn(c.session(), c)
            } catch (e: CoreException) {
                AuthState.SignedOut(authErrorMessage(e))
            }
        }
    }
}
