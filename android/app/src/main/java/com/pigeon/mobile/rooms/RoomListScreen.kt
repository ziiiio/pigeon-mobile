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
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarVisuals
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.ui.platform.LocalContext
import android.content.ClipData
import android.content.ClipboardManager
import android.widget.Toast
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
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
            onBackup = vm::backup,
            onRestore = vm::restoreBackup,
            onRecoveryKeyDismiss = vm::clearRecoveryKey,
            onDismissError = vm::clearError,
            onDismissNotice = vm::clearNotice,
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
    onBackup: () -> Unit,
    onRestore: (String) -> Unit,
    onRecoveryKeyDismiss: () -> Unit,
    onDismissError: () -> Unit,
    onDismissNotice: () -> Unit,
    onSignOut: () -> Unit,
) {
    var showCreate by rememberSaveable { mutableStateOf(false) }
    var showJoin by rememberSaveable { mutableStateOf(false) }
    var showMenu by rememberSaveable { mutableStateOf(false) }
    var showRestore by rememberSaveable { mutableStateOf(false) }
    val busy = state.backupInProgress || state.restoreInProgress

    // Action outcomes as snackbars (above the FAB): failures in error colours
    // with a dismiss ✕ that stay until dismissed or the next attempt; success
    // notices in the default style, auto-dismissing. Keyed on the message, so a
    // new outcome replaces the old one (restarting the effect cancels and
    // removes the previous bar).
    val snackbar = remember { SnackbarHostState() }
    val error = state.actionError ?: signOutError
    LaunchedEffect(error) {
        val message = error ?: return@LaunchedEffect
        snackbar.showSnackbar(FeedbackVisuals(message, isError = true))
        onDismissError()
    }
    LaunchedEffect(state.notice) {
        val message = state.notice ?: return@LaunchedEffect
        snackbar.showSnackbar(FeedbackVisuals(message, isError = false))
        onDismissNotice()
    }

    Scaffold(
        snackbarHost = {
            SnackbarHost(snackbar) { data ->
                val isError = (data.visuals as? FeedbackVisuals)?.isError == true
                Snackbar(
                    snackbarData = data,
                    containerColor = if (isError) {
                        MaterialTheme.colorScheme.errorContainer
                    } else {
                        MaterialTheme.colorScheme.inverseSurface
                    },
                    contentColor = if (isError) {
                        MaterialTheme.colorScheme.onErrorContainer
                    } else {
                        MaterialTheme.colorScheme.inverseOnSurface
                    },
                    dismissActionContentColor = if (isError) {
                        MaterialTheme.colorScheme.onErrorContainer
                    } else {
                        MaterialTheme.colorScheme.inverseOnSurface
                    },
                )
            }
        },
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
                    // Overflow: encryption-key backup / restore (M4.3).
                    Box {
                        TextButton(onClick = { showMenu = true }) {
                            Text(stringResource(R.string.rooms_menu))
                        }
                        DropdownMenu(expanded = showMenu, onDismissRequest = { showMenu = false }) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.backup_action)) },
                                enabled = !busy,
                                onClick = {
                                    showMenu = false
                                    onBackup()
                                },
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.restore_action)) },
                                enabled = !busy,
                                onClick = {
                                    showMenu = false
                                    showRestore = true
                                },
                            )
                        }
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

            // In-flight backup/restore: a progress strip under the top bar, so
            // the tap visibly did something (the actions are disabled meanwhile).
            if (busy) {
                Column(Modifier.align(Alignment.TopCenter).fillMaxWidth()) {
                    LinearProgressIndicator(Modifier.fillMaxWidth())
                    Text(
                        text = stringResource(
                            if (state.backupInProgress) R.string.backup_in_progress else R.string.restore_in_progress,
                        ),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
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
    if (showRestore) {
        RestoreBackupDialog(
            onDismiss = { showRestore = false },
            onRestore = { key ->
                onRestore(key)
                showRestore = false
            },
        )
    }
    // Shown once after a successful backup — the user must save this key.
    state.recoveryKey?.let { key ->
        RecoveryKeyDialog(recoveryKey = key, onDismiss = onRecoveryKeyDismiss)
    }
}

@Composable
private fun RecoveryKeyDialog(recoveryKey: String, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val copiedMsg = stringResource(R.string.backup_copied)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.backup_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(stringResource(R.string.backup_message))
                SelectionContainer {
                    Text(
                        text = recoveryKey,
                        // Monospace so look-alike characters (I/l/1, O/0) stay
                        // distinguishable if the user writes the key down.
                        style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(MaterialTheme.colorScheme.surfaceVariant)
                            .padding(12.dp),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.backup_done)) }
        },
        // Selecting text inside a dialog is unreliable (it's its own window, so
        // the selection toolbar often never appears) — an explicit Copy is the
        // dependable path.
        dismissButton = {
            TextButton(onClick = {
                copySecret(context, recoveryKey)
                // Android 13+ shows its own copy confirmation; avoid a double.
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                    Toast.makeText(context, copiedMsg, Toast.LENGTH_SHORT).show()
                }
            }) { Text(stringResource(R.string.backup_copy)) }
        },
    )
}

/**
 * Put a secret (the recovery key) on the clipboard, flagged **sensitive** so
 * Android 13+ hides it from the clipboard preview overlay and the keyboard's
 * clipboard suggestions. Older releases ignore the extra (no harm). Only ever on
 * an explicit user tap — the key is the user's to save (Gotcha #1 still holds:
 * nothing reaches the server).
 */
private fun copySecret(context: Context, secret: String) {
    val clip = ClipData.newPlainText("Recovery key", secret).apply {
        description.extras = PersistableBundle().apply {
            // ClipDescription.EXTRA_IS_SENSITIVE (API 33); the literal keeps minSdk 24 compiling.
            putBoolean("android.content.extra.IS_SENSITIVE", true)
        }
    }
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(clip)
}

@Composable
private fun RestoreBackupDialog(onDismiss: () -> Unit, onRestore: (String) -> Unit) {
    var key by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.restore_title)) },
        text = {
            OutlinedTextField(
                value = key,
                onValueChange = { key = it },
                label = { Text(stringResource(R.string.restore_key_hint)) },
                singleLine = true,
            )
        },
        confirmButton = {
            TextButton(onClick = { onRestore(key) }, enabled = key.isNotBlank()) {
                Text(stringResource(R.string.restore_confirm))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.rooms_cancel)) }
        },
    )
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RoomRow(room: Room, onClick: () -> Unit) {
    val context = LocalContext.current
    val copiedMsg = stringResource(R.string.rooms_id_copied)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            // Tap opens the room; long-press copies its id. Joining is by room id
            // (invites don't surface in the list), so the id must be shareable
            // without retyping — ids mix look-alike characters (I/l).
            .combinedClickable(
                onClick = onClick,
                onLongClick = {
                    context.getSystemService(ClipboardManager::class.java)
                        ?.setPrimaryClip(ClipData.newPlainText("Room id", room.roomId))
                    Toast.makeText(context, copiedMsg, Toast.LENGTH_SHORT).show()
                },
            )
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
        // The room id, as iOS shows it — what an invitee needs to join.
        if (room.name != null) {
            Text(
                text = room.roomId,
                style = MaterialTheme.typography.labelSmall,
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

/**
 * Snackbar content for an action outcome: the message plus whether it's a
 * failure (error colours, stays until dismissed) or a success notice (default
 * colours, auto-dismisses). Both carry a dismiss ✕.
 */
private class FeedbackVisuals(
    override val message: String,
    val isError: Boolean,
) : SnackbarVisuals {
    override val actionLabel: String? = null
    override val withDismissAction: Boolean = true
    override val duration: SnackbarDuration =
        if (isError) SnackbarDuration.Indefinite else SnackbarDuration.Long
}

