package com.pigeon.mobile.rooms

import android.graphics.BitmapFactory
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.pigeon.mobile.R
import kotlinx.coroutines.flow.StateFlow
import uniffi.pigeon_mobile_core.ImageContent
import uniffi.pigeon_mobile_core.PigeonClient
import uniffi.pigeon_mobile_core.TimelineEvent

/**
 * A room's chat timeline (M2.4). Reads are offline-first from the store; the
 * screen re-reads when the sync loop signals a change ([changes], bumped by the
 * room list's [SyncObserver]). Scrolling to the top pages older history in from
 * the store; the composer sends (M2.5); the top-bar action invites a user (M2.6).
 */
@Composable
fun ChatRoute(
    client: PigeonClient,
    roomId: String,
    roomTitle: String,
    encrypted: Boolean,
    myUserId: String,
    changes: StateFlow<Long>,
    onBack: () -> Unit,
) {
    val vm: ChatViewModel = viewModel(
        key = "chat/$roomId",
        factory = viewModelFactory { initializer { ChatViewModel(client, roomId) } },
    )
    val state by vm.state.collectAsStateWithLifecycle()

    // Re-read when the sync loop reports new events landed in the store.
    val changeTick by changes.collectAsStateWithLifecycle()
    LaunchedEffect(changeTick) { vm.refresh() }

    ChatScreen(
        title = roomTitle,
        encrypted = encrypted,
        myUserId = myUserId,
        state = state,
        onBack = onBack,
        onLoadOlder = vm::loadOlder,
        onSend = vm::send,
        onInvite = vm::invite,
        onSendImage = vm::sendImage,
        download = vm::downloadImage,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(
    title: String,
    encrypted: Boolean,
    myUserId: String,
    state: ChatState,
    onBack: () -> Unit,
    onLoadOlder: () -> Unit,
    onSend: (String) -> Unit,
    onInvite: (String) -> Unit,
    onSendImage: (ByteArray, String, Int, Int) -> Unit,
    download: suspend (ImageContent) -> ByteArray?,
) {
    val listState = rememberLazyListState()
    var showInvite by rememberSaveable { mutableStateOf(false) }
    // A tapped image, shown full-screen (M4.1).
    var viewImage by remember { mutableStateOf<ImageContent?>(null) }

    // Image picker (plaintext rooms only in M4.1). Reads the picked bytes, decodes
    // its dimensions, and hands them to the core to upload + send.
    val context = LocalContext.current
    val pickImage = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            val resolver = context.contentResolver
            val bytes = resolver.openInputStream(uri)?.use { it.readBytes() }
            if (bytes != null) {
                val mimetype = resolver.getType(uri) ?: "image/*"
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
                onSendImage(bytes, mimetype, bounds.outWidth.coerceAtLeast(0), bounds.outHeight.coerceAtLeast(0))
            }
        }
    }

    // Page older when the top of the loaded range scrolls into view.
    val atTop by remember { derivedStateOf { listState.firstVisibleItemIndex == 0 } }
    LaunchedEffect(atTop, state.events.size) {
        if (atTop && !state.atTop && !state.loading && state.events.isNotEmpty()) onLoadOlder()
    }

    // Follow the tail: when the newest event changes, scroll to the bottom.
    LaunchedEffect(state.events.lastOrNull()?.eventId) {
        if (state.events.isNotEmpty()) listState.scrollToItem(state.events.lastIndex)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    // A lock prefix marks an end-to-end-encrypted room (M3.6).
                    Text(
                        text = if (encrypted) {
                            "${stringResource(R.string.rooms_lock)} $title"
                        } else {
                            title
                        },
                        maxLines = 1,
                    )
                },
                navigationIcon = {
                    TextButton(onClick = onBack) { Text(stringResource(R.string.chat_back)) }
                },
                actions = {
                    TextButton(onClick = { showInvite = true }) {
                        Text(stringResource(R.string.chat_invite))
                    }
                },
            )
        },
        bottomBar = {
            Composer(
                onSend = onSend,
                // Media attach works in both plaintext and encrypted rooms (M4.2):
                // the core encrypts before upload for encrypted rooms.
                onAttach = {
                    pickImage.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                },
            )
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            when {
                state.loading -> CircularProgressIndicator(Modifier.align(Alignment.Center))
                state.events.isEmpty() -> Text(
                    text = stringResource(R.string.chat_empty),
                    modifier = Modifier.align(Alignment.Center),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                else -> LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(8.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    items(state.events, key = { it.eventId }) { event ->
                        TimelineRow(
                            event = event,
                            mine = event.sender == myUserId,
                            download = download,
                            onImageClick = { viewImage = it },
                        )
                    }
                }
            }

            state.error?.let {
                Text(
                    text = it,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .padding(16.dp),
                )
            }
        }
    }

    if (showInvite) {
        InviteDialog(
            onDismiss = { showInvite = false },
            onInvite = { userId ->
                onInvite(userId)
                showInvite = false
            },
        )
    }
    viewImage?.let { img ->
        FullScreenImage(image = img, download = download, onDismiss = { viewImage = null })
    }
}

@Composable
private fun InviteDialog(onDismiss: () -> Unit, onInvite: (String) -> Unit) {
    var userId by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.chat_invite_title)) },
        text = {
            OutlinedTextField(
                value = userId,
                onValueChange = { userId = it },
                label = { Text(stringResource(R.string.chat_invite_user)) },
                singleLine = true,
            )
        },
        confirmButton = {
            TextButton(
                onClick = { onInvite(userId) },
                enabled = userId.isNotBlank(),
            ) {
                Text(stringResource(R.string.chat_invite_confirm))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.rooms_cancel)) }
        },
    )
}

@Composable
private fun TimelineRow(
    event: TimelineEvent,
    mine: Boolean,
    download: suspend (ImageContent) -> ByteArray?,
    onImageClick: (ImageContent) -> Unit,
) {
    val body = event.body
    val systemText = event.systemText
    val image = event.image
    when {
        // An image message (M4.1) → an inline thumbnail (+ optional caption).
        image != null -> Column(
            modifier = Modifier.fillMaxWidth(),
            horizontalAlignment = if (mine) Alignment.End else Alignment.Start,
        ) {
            if (!mine) {
                Text(
                    text = event.sender,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 12.dp),
                )
            }
            RemoteImage(
                image = image,
                download = download,
                modifier = Modifier
                    .padding(horizontal = 8.dp)
                    .sizeIn(maxWidth = 240.dp, maxHeight = 240.dp)
                    .clickable { onImageClick(image) },
            )
            if (!body.isNullOrBlank()) {
                Text(
                    text = body,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(horizontal = 12.dp),
                )
            }
            TimeLabel(event.originServerTs)
        }
        // A text message → a bubble aligned by sender.
        body != null -> Column(
            modifier = Modifier.fillMaxWidth(),
            horizontalAlignment = if (mine) Alignment.End else Alignment.Start,
        ) {
            if (!mine) {
                Text(
                    text = event.sender,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 12.dp),
                )
            }
            Surface(
                color = if (mine) {
                    MaterialTheme.colorScheme.primaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceVariant
                },
                shape = MaterialTheme.shapes.medium,
                // Dim a message whose send is still in flight (local echo).
                modifier = Modifier.alpha(if (event.pending) 0.5f else 1f),
            ) {
                Text(
                    text = body,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                )
            }
            // Send status for the user's own messages (M2.5) — else the time.
            val status = when {
                event.failed -> stringResource(R.string.chat_not_sent)
                event.pending -> stringResource(R.string.chat_sending)
                else -> null
            }
            if (status != null) {
                Text(
                    text = status,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (event.failed) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                    modifier = Modifier.padding(horizontal = 12.dp),
                )
            } else {
                TimeLabel(event.originServerTs)
            }
        }
        // A state/membership event → a centered muted system line.
        systemText != null -> Text(
            text = systemText,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 2.dp),
        )
        // Nothing renderable (hidden event type) — draw nothing.
        else -> Unit
    }
}

/** A small muted timestamp under a message (M4.5). Display-only formatting of the
 * event's `origin_server_ts` (millis) — no protocol logic. */
@Composable
private fun TimeLabel(originServerTs: Long) {
    if (originServerTs <= 0L) return
    val text = remember(originServerTs) {
        Instant.ofEpochMilli(originServerTs)
            .atZone(ZoneId.systemDefault())
            .format(DateTimeFormatter.ofPattern("HH:mm"))
    }
    Text(
        text = text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 12.dp),
    )
}

@Composable
private fun Composer(onSend: (String) -> Unit, onAttach: (() -> Unit)?) {
    var text by rememberSaveable { mutableStateOf("") }
    Column {
        HorizontalDivider()
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .imePadding()
                .padding(8.dp),
            verticalAlignment = Alignment.Bottom,
        ) {
            // Attach an image (plaintext rooms only in M4.1).
            onAttach?.let {
                TextButton(onClick = it) { Text(stringResource(R.string.chat_attach)) }
            }
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                modifier = Modifier.weight(1f),
                placeholder = { Text(stringResource(R.string.chat_message_hint)) },
                maxLines = 4,
            )
            TextButton(
                onClick = {
                    if (text.isNotBlank()) {
                        onSend(text)
                        text = ""
                    }
                },
                enabled = text.isNotBlank(),
            ) {
                Text(stringResource(R.string.chat_send))
            }
        }
    }
}

/** Download an image's bytes off the main thread (the core decrypts encrypted
 * ones), decode, and render. Spinner while loading, placeholder if undecodable
 * (M4.1/M4.2). */
@Composable
private fun RemoteImage(
    image: ImageContent,
    download: suspend (ImageContent) -> ByteArray?,
    modifier: Modifier = Modifier,
) {
    val bytes by produceState<ByteArray?>(initialValue = null, image.uri) { value = download(image) }
    val bitmap = remember(bytes) {
        bytes?.let { BitmapFactory.decodeByteArray(it, 0, it.size)?.asImageBitmap() }
    }
    when {
        bitmap != null -> Image(
            bitmap = bitmap,
            contentDescription = stringResource(R.string.chat_image_alt),
            modifier = modifier,
        )
        bytes == null -> Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        else -> Text(
            text = stringResource(R.string.chat_image_alt),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = modifier.padding(12.dp),
        )
    }
}

/** Full-screen image viewer, dismissed by tapping anywhere (M4.1). */
@Composable
private fun FullScreenImage(
    image: ImageContent,
    download: suspend (ImageContent) -> ByteArray?,
    onDismiss: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .clickable(onClick = onDismiss),
            contentAlignment = Alignment.Center,
        ) {
            RemoteImage(image = image, download = download, modifier = Modifier.fillMaxWidth())
        }
    }
}
