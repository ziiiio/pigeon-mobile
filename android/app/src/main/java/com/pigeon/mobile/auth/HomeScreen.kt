package com.pigeon.mobile.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pigeon.mobile.R
import uniffi.pigeon_mobile_core.Session

/**
 * The signed-in landing screen (M1.4). Shows the non-secret [Session] identity
 * and a Sign-out action (M1.5); real content (rooms, timeline) arrives in M2.
 *
 * Pure UI: the actual logout (server revoke + local clear) is the core's job,
 * driven through the view-model's [onSignOut]. [signingOut] disables the action
 * while it's in flight, and [error] surfaces a logout that failed to clear the
 * session (so the user can retry).
 */
@Composable
fun HomeScreen(
    session: Session,
    onSignOut: () -> Unit,
    signingOut: Boolean = false,
    error: String? = null,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = stringResource(R.string.home_signed_in_as),
            style = MaterialTheme.typography.labelMedium,
        )
        Text(text = session.userId, style = MaterialTheme.typography.titleLarge)
        Text(text = "${stringResource(R.string.home_device)}: ${session.deviceId}")
        Text(text = "${stringResource(R.string.home_server)}: ${session.server}")

        if (error != null) {
            Text(text = error, color = MaterialTheme.colorScheme.error)
        }

        Spacer(Modifier.height(8.dp))
        OutlinedButton(onClick = onSignOut, enabled = !signingOut) {
            Text(stringResource(R.string.home_sign_out))
        }
        if (signingOut) {
            CircularProgressIndicator()
        }
    }
}
