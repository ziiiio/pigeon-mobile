package com.pigeon.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.pigeon.mobile.auth.AuthScreen
import com.pigeon.mobile.auth.AuthState
import com.pigeon.mobile.auth.AuthViewModel
import com.pigeon.mobile.auth.HomeScreen

/**
 * Hosts the auth flow (M1.4). The core's host callbacks (log sink, key store)
 * are installed in [PigeonApp]; this activity only renders view-model state and
 * routes between the sign-in form and the signed-in screen.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    AuthFlow()
                }
            }
        }
    }
}

@Composable
private fun AuthFlow() {
    val vm: AuthViewModel = viewModel()
    val state by vm.state.collectAsStateWithLifecycle()

    when (val s = state) {
        is AuthState.SignedIn -> HomeScreen(s.session)
        // Restoring, SignedOut, and Submitting all render through the form
        // (which shows a spinner while submitting and any error while signed out).
        AuthState.Restoring,
        AuthState.Submitting,
        is AuthState.SignedOut -> AuthScreen(
            state = state,
            onLogin = vm::login,
            onRegister = vm::register,
        )
    }
}
