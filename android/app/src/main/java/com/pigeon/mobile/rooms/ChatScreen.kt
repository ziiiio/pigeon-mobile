package com.pigeon.mobile.rooms

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.pigeon.mobile.R
import kotlinx.coroutines.flow.StateFlow
import uniffi.pigeon_mobile_core.PigeonClient
import uniffi.pigeon_mobile_core.TimelineEvent

/**
 * A room's chat timeline (M2.4). Reads are offline-first from the store; the
 * screen re-reads when the sync loop signals a change ([changes], bumped by the
 * room list's [SyncObserver]). Scrolling to the top pages older history in from
 * the store. The composer (send) arrives in M2.5.
 */
@Composable
fun ChatRoute(
    client: PigeonClient,
    roomId: String,
    roomTitle: String,
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
        myUserId = myUserId,
        state = state,
        onBack = onBack,
        onLoadOlder = vm::loadOlder,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(
    title: String,
    myUserId: String,
    state: ChatState,
    onBack: () -> Unit,
    onLoadOlder: () -> Unit,
) {
    val listState = rememberLazyListState()

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
                title = { Text(title, maxLines = 1) },
                navigationIcon = {
                    TextButton(onClick = onBack) { Text(stringResource(R.string.chat_back)) }
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
                        TimelineRow(event, mine = event.sender == myUserId)
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
}

@Composable
private fun TimelineRow(event: TimelineEvent, mine: Boolean) {
    val body = event.body
    val systemText = event.systemText
    when {
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
            ) {
                Text(
                    text = body,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                )
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
