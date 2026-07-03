package com.pigeon.mobile.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import com.pigeon.mobile.R

/**
 * The sign-in / register form. Pure UI: it renders [AuthState] and reports the
 * user's intent up via callbacks; the view-model owns all the work.
 */
@Composable
fun AuthScreen(
    state: AuthState,
    onLogin: (server: String, username: String, password: String) -> Unit,
    onRegister: (server: String, username: String, password: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    // 10.0.2.2 is the host loopback as seen from the Android emulator — a sensible
    // default for talking to a local dev homeserver.
    var server by rememberSaveable { mutableStateOf("http://10.0.2.2:8008") }
    var username by rememberSaveable { mutableStateOf("") }
    var password by rememberSaveable { mutableStateOf("") }

    val submitting = state is AuthState.Submitting
    val error = (state as? AuthState.SignedOut)?.error
    val canSubmit = !submitting &&
        server.isNotBlank() && username.isNotBlank() && password.isNotBlank()

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            text = stringResource(R.string.auth_title),
            style = MaterialTheme.typography.headlineSmall,
        )

        OutlinedTextField(
            value = server,
            onValueChange = { server = it },
            label = { Text(stringResource(R.string.auth_homeserver)) },
            singleLine = true,
            enabled = !submitting,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = username,
            onValueChange = { username = it },
            label = { Text(stringResource(R.string.auth_username)) },
            singleLine = true,
            enabled = !submitting,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text(stringResource(R.string.auth_password)) },
            singleLine = true,
            enabled = !submitting,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            modifier = Modifier.fillMaxWidth(),
        )

        if (error != null) {
            Text(text = error, color = MaterialTheme.colorScheme.error)
        }

        Spacer(Modifier.height(4.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Button(
                onClick = { onLogin(server, username, password) },
                enabled = canSubmit,
            ) { Text(stringResource(R.string.auth_login)) }
            OutlinedButton(
                onClick = { onRegister(server, username, password) },
                enabled = canSubmit,
            ) { Text(stringResource(R.string.auth_register)) }
        }

        if (submitting) {
            Spacer(Modifier.height(8.dp))
            CircularProgressIndicator(modifier = Modifier.align(Alignment.CenterHorizontally))
        }
    }
}
