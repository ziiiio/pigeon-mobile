package com.pigeon.mobile.rooms

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.foundation.clickable
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.pigeon.mobile.R
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.PigeonClient
import uniffi.pigeon_mobile_core.Room
import uniffi.pigeon_mobile_core.Session
import uniffi.pigeon_mobile_core.SyncObserver

/**
 * The signed-in room list (M2.3). Owns the sync loop for the session's lifetime:
 * a [LaunchedEffect] keyed on the client runs `runSync`, so leaving this screen
 * (sign-out) or a new session cancels the coroutine — which drops the core
 * future and cancels the in-flight `/sync` (CLAUDE.md Gotcha #6). The observer
 * bridges the core's change-stream back into the view-model, which re-reads the
 * store (offline-first).
 */
@Composable
fun RoomListRoute(
    session: Session,
    client: PigeonClient,
    signingOut: Boolean,
    signOutError: String?,
    onSignOut: () -> Unit,
) {
    // Key the VM on the session identity (device id is fresh per login) so a new
    // session gets a fresh VM bound to the new client, not a stale handle.
    val vm: RoomsViewModel = viewModel(
        key = "rooms/${session.userId}/${session.deviceId}",
        factory = viewModelFactory { initializer { RoomsViewModel(client) } },
    )
    val state by vm.state.collectAsStateWithLifecycle()

    // A monotonic "the store changed" signal the open chat also observes, so a
    // room's timeline refreshes on the same sync events that refresh the list.
    val changes = remember { MutableStateFlow(0L) }

    LaunchedEffect(client) {
        val observer = object : SyncObserver {
            override fun onChange() {
                vm.reload()
                changes.update { it + 1 }
            }
            override fun onStatus(connected: Boolean) = vm.setConnected(connected)
        }
        try {
            client.runSync(observer)
        } catch (e: CoreException) {
            // Fatal (e.g. revoked token) — the loop ended. Surface it; a full
            // "kick to sign-in" is refined alongside membership handling in M2.6.
            vm.onSyncFailed(e.message)
        }
    }

    // Simple state-based navigation: a selected room shows its chat over the list.
    // Plain `remember` — the selection resets on rotation (acceptable for M2).
    var openRoom by remember { mutableStateOf<Room?>(null) }
    val current = openRoom
    if (current != null) {
        ChatRoute(
            client = client,
            roomId = current.roomId,
            roomTitle = current.name ?: current.roomId,
            encrypted = current.encrypted,
            myUserId = session.userId,
            changes = changes,
            onBack = { openRoom = null },
        )
    } else {
        RoomListScreen(
            session = session,
            state = state,
            signingOut = signingOut,
            signOutError = signOutError,
            onOpenRoom = { openRoom = it },
            onCreateRoom = vm::createRoom,
            onJoinRoom = vm::joinRoom,
            onSignOut = onSignOut,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RoomListScreen(
    session: Session,
    state: RoomsState,
    signingOut: Boolean,
    signOutError: String?,
    onOpenRoom: (Room) -> Unit,
    onCreateRoom: (String?, String?, Boolean) -> Unit,
    onJoinRoom: (String) -> Unit,
    onSignOut: () -> Unit,
) {
    var showCreate by rememberSaveable { mutableStateOf(false) }
    var showJoin by rememberSaveable { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.rooms_title)) },
                actions = {
                    if (!state.connected) {
                        Text(
                            text = stringResource(R.string.rooms_offline),
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(end = 8.dp),
                        )
                    }
                    TextButton(onClick = { showJoin = true }) {
                        Text(stringResource(R.string.rooms_join))
                    }
                    TextButton(onClick = onSignOut, enabled = !signingOut) {
                        Text(stringResource(R.string.home_sign_out))
                    }
                },
            )
        },
        floatingActionButton = {
            FloatingActionButton(onClick = { showCreate = true }) {
                Text("+", style = MaterialTheme.typography.headlineMedium)
            }
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            when {
                state.loading -> CircularProgressIndicator(Modifier.align(Alignment.Center))
                state.rooms.isEmpty() -> Column(
                    modifier = Modifier.align(Alignment.Center).padding(24.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(
                        text = stringResource(R.string.rooms_empty),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        text = stringResource(R.string.rooms_empty_hint),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                else -> LazyColumn(Modifier.fillMaxSize()) {
                    items(state.rooms, key = { it.roomId }) { room ->
                        RoomRow(room, onClick = { onOpenRoom(room) })
                        HorizontalDivider()
                    }
                }
            }

            val error = state.actionError ?: signOutError
            if (error != null) {
                Text(
                    text = error,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .padding(16.dp),
                )
            }
        }
    }

    if (showCreate) {
        CreateRoomDialog(
            onDismiss = { showCreate = false },
            onCreate = { name, topic, encrypted ->
                onCreateRoom(name, topic, encrypted)
                showCreate = false
            },
        )
    }
    if (showJoin) {
        JoinRoomDialog(
            onDismiss = { showJoin = false },
            onJoin = { roomId ->
                onJoinRoom(roomId)
                showJoin = false
            },
        )
    }
}

@Composable
private fun RoomRow(room: Room, onClick: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = room.name ?: room.roomId,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            if (room.encrypted) {
                Text(
                    text = stringResource(R.string.rooms_encrypted),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }
        room.topic?.let {
            Text(
                text = it,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun CreateRoomDialog(onDismiss: () -> Unit, onCreate: (String?, String?, Boolean) -> Unit) {
    var name by rememberSaveable { mutableStateOf("") }
    var topic by rememberSaveable { mutableStateOf("") }
    var encrypted by rememberSaveable { mutableStateOf(true) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.rooms_create_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text(stringResource(R.string.rooms_create_name)) },
                    singleLine = true,
                )
                OutlinedTextField(
                    value = topic,
                    onValueChange = { topic = it },
                    label = { Text(stringResource(R.string.rooms_create_topic)) },
                    singleLine = true,
                )
                // End-to-end encryption is on by default; the room's messages are
                // MLS-encrypted end to end (transparent to the rest of the UI, M3).
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { encrypted = !encrypted },
                ) {
                    Text(
                        text = stringResource(R.string.rooms_create_encrypted),
                        modifier = Modifier.weight(1f),
                    )
                    Switch(checked = encrypted, onCheckedChange = { encrypted = it })
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onCreate(name, topic, encrypted) }) {
                Text(stringResource(R.string.rooms_create_confirm))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.rooms_cancel)) }
        },
    )
}

@Composable
private fun JoinRoomDialog(onDismiss: () -> Unit, onJoin: (String) -> Unit) {
    var roomId by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.rooms_join_title)) },
        text = {
            OutlinedTextField(
                value = roomId,
                onValueChange = { roomId = it },
                label = { Text(stringResource(R.string.rooms_join_id)) },
                singleLine = true,
            )
        },
        confirmButton = {
            TextButton(
                onClick = { onJoin(roomId) },
                enabled = roomId.isNotBlank(),
            ) {
                Text(stringResource(R.string.rooms_join))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.rooms_cancel)) }
        },
    )
}
