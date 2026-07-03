package com.pigeon.mobile.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pigeon.mobile.R
import uniffi.pigeon_mobile_core.Session

/**
 * The signed-in landing screen (M1.4). Shows the non-secret [Session] identity;
 * real content (rooms, timeline) arrives in M2, and a Sign-out action in M1.5.
 */
@Composable
fun HomeScreen(session: Session, modifier: Modifier = Modifier) {
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
    }
}
