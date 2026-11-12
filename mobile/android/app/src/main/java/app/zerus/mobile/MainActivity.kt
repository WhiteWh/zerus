package app.zerus.mobile

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.KeyEvent
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.SystemBarStyle
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.Image
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.horizontalScroll
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.Saver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.RectangleShape
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONArray
import org.json.JSONObject

private val Mint = Color(0xFF67E8CB)
private val Amber = Color(0xFFFFCB7D)
private val Background = Color(0xFF101517)
private val Surface = Color(0xFF1A2225)
private val Muted = Color(0xFF9AACB2)
/** Desktop's notice violet. */
private val Notice = Color(0xFFC5A8F5)
private val Scheme = darkColorScheme(primary = Mint, onPrimary = Background, secondary = Amber,
    background = Background, surface = Surface, surfaceContainer = Surface, onSurface = Color(0xFFE9F0F2),
    onSurfaceVariant = Muted, outline = Color(0xFF354449))

class MainActivity : ComponentActivity() {
    private val model: ZerusViewModel by viewModels()
    // Public Activity callback; AndroidX's inherited core bridge has a class-level restriction.
    @SuppressLint("RestrictedApi")
    override fun dispatchKeyEvent(event: KeyEvent): Boolean = when (model.speech.recordingKeys.handle(
        event.keyCode, event.deviceId, event.action, event.repeatCount, model.speech.recording, event.modifiers != 0)) {
        RecordingKeys.Result.Stop -> { model.speech.stop(); true }
        RecordingKeys.Result.Consume -> true
        RecordingKeys.Result.Pass -> super.dispatchKeyEvent(event)
    }
    private var invitation by mutableStateOf<PairingInvite?>(null)
    private var updateRequest by mutableIntStateOf(0)
    private val requestNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) SessionNotifications.initialize(this)
    }
    private lateinit var filePicker: AttachmentPickerHost
    private companion object { const val FILE_SELECTION = "zerus.fileSelection" }
    override fun onStop() { model.speech.background(); model.attachments.stopAudio(); super.onStop() }
    override fun onDestroy() { if (::filePicker.isInitialized) filePicker.close(); super.onDestroy() }
    override fun onSaveInstanceState(outState: Bundle) {
        outState.putString(FILE_SELECTION, filePicker.launchedId)
        super.onSaveInstanceState(outState)
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        sanitizeLaunchIntent(intent)
        super.onCreate(savedInstanceState)
        filePicker = AttachmentPickerHost(activityResultRegistry, savedInstanceState?.getString(FILE_SELECTION).orEmpty()) { uris, id ->
            model.finishAttachmentPick(uris, id)
        }
        AppTelemetry.opened()
        SessionNotifications.initialize(this)
        enableEdgeToEdge(statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.rgb(16, 21, 23)))
        readIntent(intent)
        setContent { MaterialTheme(colorScheme = Scheme) {
            LaunchedEffect(model.storageReady, model.pickerId) {
                if (model.storageReady) {
                    val id = model.pickerId
                    if (model.pickerDraft == null || !filePicker.restore(id)) model.recoverAttachmentPick(id, filePicker.launchedId)
                    filePicker.retain(model.pickerId)
                }
            }
            ZerusApp(model, invitation, updateRequest, onInvitationUsed = { invitation = null }, onNotifications = {
                if (Build.VERSION.SDK_INT >= 33) requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
            }, onLive = { enabled ->
                val service = Intent(this, LiveConnectionService::class.java)
                if (enabled) startForegroundService(service) else stopService(service)
            }, onAttach = { target ->
                model.beginAttachmentPick(target) { selectionId ->
                    if (isDestroyed || isFinishing) { model.cancelAttachmentPick(selectionId); return@beginAttachmentPick }
                    try { filePicker.launch(selectionId) }
                    catch (_: Exception) { model.cancelAttachmentPick(selectionId) }
                }
            })
        } }
    }
    override fun onNewIntent(intent: Intent) { sanitizeLaunchIntent(intent); super.onNewIntent(intent); setIntent(intent); readIntent(intent) }
    private fun sanitizeLaunchIntent(intent: Intent) {
        intent.data?.toString()?.let { PairingInvite.parse(it) }?.let { invitation = it }
        intent.data = null
        // Analytics lifecycle must never see pairing URIs or campaign/referrer extras.
        val retained = Bundle()
        listOf("session", "computer", "connection").forEach { key -> intent.getStringExtra(key)?.let { retained.putString(key, it) } }
        if(intent.getBooleanExtra("open_updates", false)) retained.putBoolean("open_updates", true)
        intent.replaceExtras(retained)
    }
    private fun readIntent(intent: Intent) {
        if(intent.getBooleanExtra("open_updates", false)) { updateRequest++;intent.removeExtra("open_updates") }
        intent.data?.toString()?.let { PairingInvite.parse(it) }?.let { invitation = it;intent.data = null }
        val session = intent.getStringExtra("session")
        val computer = intent.getStringExtra("computer")
        if (session != null) model.openFromNotification(intent.getStringExtra("connection").orEmpty(), computer.orEmpty(), session)
    }
}

@Composable private fun ZerusApp(model: ZerusViewModel, invitation: PairingInvite?, updateRequest: Int, onInvitationUsed: () -> Unit,
    onNotifications: () -> Unit, onLive: (Boolean) -> Unit, onAttach: (Target) -> Unit) {
    val obscuration = remember { ConversationObscuration() }
    CompositionLocalProvider(LocalConversationObscuration provides obscuration) {
        ZerusAppContent(model,invitation,updateRequest,onInvitationUsed,onNotifications,onLive,onAttach)
        AttachmentViewerHost(model.attachments)
        ComputerFileHost(model.computerFiles)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun ZerusAppContent(model: ZerusViewModel, invitation: PairingInvite?, updateRequest: Int, onInvitationUsed: () -> Unit,
    onNotifications: () -> Unit, onLive: (Boolean) -> Unit, onAttach: (Target) -> Unit) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    var handledReturnSequence by remember(model) { mutableLongStateOf(model.returnToSessionsSequence) }
    LaunchedEffect(model.returnToSessionsSequence) {
        val sequence = model.returnToSessionsSequence
        if(sequence > 0 && sequence != handledReturnSequence) tab = 0
        handledReturnSequence = sequence
    }
    var settings by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(updateRequest) { if(updateRequest > 0) settings = true }
    val updater: AndroidUpdateViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    val updateState by updater.state.collectAsState()
    val keyboard = LocalSoftwareKeyboardController.current
    if (settings) AppSettingsScreen(model, updater) { settings = false }
    LaunchedEffect(tab, model.selected) { if (model.selected == null) AppTelemetry.screen(when (tab) { 0 -> TelemetryScreen.Sessions; 1 -> TelemetryScreen.Projects; 2 -> TelemetryScreen.Drafts; 4 -> TelemetryScreen.Accounts; else -> TelemetryScreen.Machines }) }
    var pairing by rememberSaveable { mutableStateOf(false) }
    var server by rememberSaveable { mutableStateOf("https://relay.zerus.dev") }
    var code by remember { mutableStateOf("") }
    var disconnect by remember { mutableStateOf<Connection?>(null) }
    var review by remember { mutableStateOf<Draft?>(null) }
    var reviewOutgoing by remember { mutableStateOf<OutgoingMessage?>(null) }
    var discard by remember { mutableStateOf<Draft?>(null) }
    var stopRequest by remember { mutableStateOf<Pair<Session, Double>?>(null) }
    val readingPositions = remember { mutableMapOf<String, ConversationAnchor>() }
    var messageJump by remember { mutableStateOf<ConversationMessageJump?>(null) }
    if(pairing || disconnect != null || review != null || reviewOutgoing != null || discard != null || stopRequest != null) ObscureConversation()
    val live = LiveConnectionService.running
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(model, lifecycle, updater) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) { model.flushDrafts(); model.stopTaskSync() }
            if (event == Lifecycle.Event.ON_RESUME) updater.recover()
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(invitation) { invitation?.let { server = it.server; code = it.code; tab = 3; pairing = true; onInvitationUsed() } }
    val refreshTarget = model.selected?.target
    val reviewingNativeInput = refreshTarget != null && model.conversationQuestions.isNotEmpty()
    LaunchedEffect(model, lifecycle, tab, refreshTarget, reviewingNativeInput, model.demo) { lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
        if (refreshTarget?.archiveId?.isBlank() == true && !reviewingNativeInput && !model.demo) {
            var started = android.os.SystemClock.elapsedRealtime()
            model.awaitActivityRead()
            while (true) {
                // At most one inspection: wait for its actual completion before the
                // next turn, with a two-second floor and a short idle yield.
                delay(if (model.error.isNotBlank()) 5_000L else
                    maxOf(200L, 2_000L - (android.os.SystemClock.elapsedRealtime() - started)))
                started = android.os.SystemClock.elapsedRealtime()
                model.refreshActivity()
                model.awaitActivityRead()
            }
        }
        while (true) {
            delay(5_000)
            if (model.selected != null) model.refreshActivity()
            else if (tab != 2) model.refresh()
        }
    } }
    // The phone's task replicas follow their computers while the app is visible, on every tab.
    // The first check waits for the catalog the loop above reads after five seconds.
    LaunchedEffect(model, lifecycle) { lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
        delay(6_000)
        while (true) {
            model.syncTasks()
            delay(15_000)
        }
    } }
    fun back() {
        if (model.selected == null && tab == 0 && model.sessionProjectScope != null) { model.clearSessionScope(); tab = 1 }
        else model.back()
    }
    BackHandler(model.selected != null || tab == 1 && model.selectedProject != null || tab == 0 && model.sessionProjectScope != null) { back() }
    val selected = model.selected
    val conversationLoad = selected?.let { rememberConversationLoad(model, it.target) }
    val conversationObscured = LocalConversationObscuration.current?.obscured == true
    var sessionDetails by remember(selected?.target?.key) { mutableStateOf(false) }
    if (selected != null && sessionDetails) {
        ObscureConversation()
        SessionDetailsSheet(model,selected) { sessionDetails = false }
    }
    val sessionHeaderHeight = with(LocalDensity.current) {
        maxOf(56.dp,22.sp.toDp() + MaterialTheme.typography.labelSmall.lineHeight.toDp() + 18.dp)
    }
    val project = if (tab == 1) model.selectedProject else model.sessionProjectScope.takeIf { tab == 0 }
    Scaffold(modifier = Modifier.fillMaxSize(), containerColor = Background,
        topBar = { TopAppBar(title = {
            if (selected == null && project != null) Text(project.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
            else if (selected == null) Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Image(painterResource(R.drawable.zerus_brand), "Zerus", Modifier.size(32.dp))
                Row(Modifier.weight(1f, fill = false), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Zerus", Modifier.alignByBaseline(), fontWeight = FontWeight.SemiBold, letterSpacing = (-0.5).sp, maxLines = 1, softWrap = false)
                    Text("v${BuildConfig.VERSION_NAME}", Modifier.alignByBaseline().weight(1f, fill = false),
                        style = MaterialTheme.typography.labelSmall, color = Muted, maxLines = 1, softWrap = false, overflow = TextOverflow.Ellipsis)
                }
                if (model.demo) Tag("PREVIEW", Amber)
            } else SessionHeaderTitle(model,selected) { sessionDetails = true }
        }, navigationIcon = { if (selected != null || project != null) IconButton(onClick = { back() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            actions = {
                if (selected == null) NewSessionButton(model)
                if (selected != null && selected.target.agentId.isBlank() && selected.target.archiveId.isBlank() && model.activityVerified && model.activity?.optBoolean("interrupt_supported") == true)
                    IconButton(onClick = { stopRequest = selected to model.activity!!.optDouble("turn_started") }) {
                        Icon(Icons.Default.StopCircle, "Stop turn")
                    }
                IconButton(onClick = { if (selected == null) model.refresh(explicit = true) else model.refreshActivity(explicit = true) },
                    modifier = Modifier.semantics {
                        contentDescription = "Refresh"
                        if (!conversationObscured) conversationLoad?.let { stateDescription = it.text }
                    }) {
                    if(selected != null && conversationLoad?.spinner == true && !conversationObscured)
                        CircularProgressIndicator(Modifier.size(20.dp),color = Mint,strokeWidth = 2.dp)
                    else Icon(Icons.Default.Refresh, null)
                }
                if (selected == null) IconButton(onClick = { keyboard?.hide(); settings = true }, modifier = Modifier.semantics {
                    if (updateState.sessionId >= 0) stateDescription = "Installation pending"
                    else if (updateState.available != null) stateDescription = "Update available"
                }) {
                    BadgedBox(badge = { if (updateState.available != null || updateState.sessionId >= 0) Badge() }) {
                        Icon(Icons.Default.Settings, "Settings")
                    }
                }
            }, expandedHeight = if (selected == null) 64.dp else sessionHeaderHeight,
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Background)) },
        bottomBar = { if (selected == null) NavigationBar(containerColor = Background) {
            listOf(Triple(0, "Sessions", DesktopIcons.Sessions), Triple(1, "Projects", DesktopIcons.Projects),
                Triple(2, "Drafts", Icons.Default.EditNote), Triple(4, "Accounts", Icons.Default.AccountCircle),
                Triple(3, "Machines", DesktopIcons.Machines)).forEach { (id, label, icon) ->
                NavigationBarItem(selected = tab == id, onClick = { tab = id; model.clearSessionScope(); model.clearProject() }, icon = { Icon(icon, label) }, label = { Text(label) })
            }
        } }) { padding ->
        Column(Modifier.padding(padding).consumeWindowInsets(padding).fillMaxSize()) {
            if (selected == null) Box(Modifier.fillMaxWidth().height(4.dp)) {
                if (!model.demo && (model.busy || model.catalogProgress))
                    LinearProgressIndicator(Modifier.fillMaxWidth(), color = Mint)
            }
            if (selected == null) CatalogRefreshStatus(model)
            // Give the request disclosure room above the IME. The title still opens details;
            // the secondary tools return when editing ends, while refresh stays visible.
            if (selected != null && WindowInsets.ime.getBottom(LocalDensity.current) == 0)
                SessionHeaderTools(model,selected,onDetails = { sessionDetails = true }) { messageJump = it }
            if (selected != null) ConversationReadTiming(model, conversationLoad)
            // The open conversation's own read failure is shown, with its details, by its status strip.
            if (model.error.isNotBlank() && (selected == null || model.conversationReadFailure(selected.target).isBlank()))
                Surface(color = Color(0xFF3A2821), modifier = Modifier.fillMaxWidth()) {
                Row(Modifier.padding(start = 16.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(model.error, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
                    IconButton(onClick = { model.clearError() }) { Icon(Icons.Default.Close, "Dismiss error") }
                }
            }
            if (selected != null) Conversation(model, selected, conversationLoad, onReview = { review = it }, onReviewOutgoing = { reviewOutgoing = it }, onAttach = onAttach, positions = readingPositions,jumpRequest = messageJump,onJumpConsumed = { messageJump = null })
            else MainListsRefresh(model,tab to project?.key?.key) { when (tab) {
                0 -> SessionsScreen(model, onPair = { tab = 3 })
                1 -> if (project != null) ProjectDetails(model, project, onViewSessions = { model.viewProjectSessions(project); tab = 0 })
                    else ProjectsScreen(model, onPair = { tab = 3 })
                2 -> DraftsScreen(model, onReview = { review = it }, onReviewOutgoing = { reviewOutgoing = it }, onDiscard = { discard = it })
                3 -> MachinesScreen(model, live, onPair = { pairing = true }, onDisconnect = { disconnect = it },
                    onNotifications = { model.updateNotifications(it); if (it) onNotifications() }, onLive = {
                        if (it) onNotifications(); onLive(it)
                    })
                else -> AccountsScreen(model,onMachines = { tab=3 })
            } }
        }
    }
    LaunchSessionDialog(model)
    if (pairing) ZerusAlertDialog(onDismissRequest = { if (!model.busy) { pairing = false;code = "" } }, title = { Text("Pair a workspace") },
        text = { Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Text("Scan an invitation from your machine, or enter its code. Review the gateway before pairing.", color = Muted)
            ScanPairingButton(enabled = !model.busy) { invite -> server = invite.server;code = invite.code }
            OutlinedTextField(server, { server = it }, label = { Text("Gateway URL") }, placeholder = { Text("https://zerus.example") }, singleLine = true)
            OutlinedTextField(code, { code = it }, label = { Text("Invitation code") }, singleLine = true, visualTransformation = PasswordVisualTransformation())
            Text(if(WorkspaceProtocol.encryptedInvitationCode(code)) "Messages are encrypted between enrolled devices. The computer that executes a request can read its content." else "Legacy invitations trust the relay with relayed content. Use a new phone invitation from an enrolled computer for encrypted transport.", style = MaterialTheme.typography.bodySmall, color = Muted)
        } }, confirmButton = { TextButton(enabled = !model.busy && model.storageReady && code.isNotBlank() && server.isNotBlank(),
            onClick = { model.pair(server, code) { pairing = false; code = ""; onNotifications() } }) { Text("Pair") } },
        dismissButton = { TextButton(onClick = { pairing = false;code = "" }, enabled = !model.busy) { Text("Cancel") } })
    disconnect?.let { connection -> ZerusAlertDialog(onDismissRequest = { disconnect = null }, title = { Text("${if(connection.protocolVersion==2) "Forget" else "Disconnect"} ${connection.displayName}?") },
        text = { Text(if(connection.protocolVersion==2) "This erases this phone's connection keys. Saved drafts remain available. To revoke this phone across the workspace, remove it from an enrolled computer." else "This removes the gateway credential from this phone. Saved drafts remain available.") },
        confirmButton = { TextButton(onClick = { model.disconnect(connection); disconnect = null }) { Text(if(connection.protocolVersion==2) "Forget connection" else "Disconnect") } },
        dismissButton = { TextButton(onClick = { disconnect = null }) { Text("Cancel") } }) }
    review?.let { draft -> ZerusAlertDialog(onDismissRequest = { review = null }, title = { Text("Review unconfirmed delivery") },
        text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Your message may already have reached ${draft.target.session}. Check its conversation and receipt before preparing another send.")
            SelectionContainer { Text(draft.text.ifBlank { draft.answers }, maxLines = 8, overflow = TextOverflow.Ellipsis) }
            Text("Preparing again keeps the draft. Sending remains a separate action.", color = Muted, style = MaterialTheme.typography.bodySmall)
        } }, confirmButton = {
            TextButton(onClick = { model.review(draft); review = null }) { Text("Prepare again") }
            TextButton(onClick = { model.checkReceipt(draft); review = null }) { Text("Check receipt") }
        }, dismissButton = { TextButton(onClick = { review = null }) { Text("Cancel") } }) }
    reviewOutgoing?.let { message ->
        val canDismiss = !message.deliveryDismissed && message.status in listOf("uncertain", "failed")
        ZerusAlertDialog(onDismissRequest = { reviewOutgoing = null }, title = { Text("Review delivery") },
        text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(if (message.status == "uncertain") "This message may already have reached ${message.target.session}. Check its receipt and conversation for confirmation."
                else if (message.error.isNotBlank()) message.error
                else "This message was not accepted. Review it before trying again.")
            if (message.text.isNotBlank()) SelectionContainer { Text(message.text, maxLines = 8, overflow = TextOverflow.Ellipsis) }
            AttachmentRows(message.attachments.map(Attachment::displayed), message.target, model)
            if (canDismiss) Text(OUTGOING_WARNING_DISMISSAL_NOTICE, color = Muted, style = MaterialTheme.typography.bodySmall)
            if (message.status == "failed") Text("Your current composition is kept in Saved drafts. Preparing again does not send the message.", color = Muted, style = MaterialTheme.typography.bodySmall)
        } }, confirmButton = {
            if (message.status == "failed") TextButton(enabled = "outgoing:${message.requestId}" !in model.receiptFlights && "dismiss:${message.requestId}" !in model.receiptFlights,
                onClick = { model.prepareOutgoing(message); reviewOutgoing = null }) { Text("Prepare again") }
            TextButton(enabled = "outgoing:${message.requestId}" !in model.receiptFlights,
                onClick = { model.checkOutgoing(message); reviewOutgoing = null }) { Text("Check receipt") }
            if (canDismiss) TextButton(enabled = "dismiss:${message.requestId}" !in model.receiptFlights,
                onClick = { model.dismissOutgoing(message); reviewOutgoing = null }) { Text("Dismiss warning") }
        }, dismissButton = { TextButton(onClick = { reviewOutgoing = null }) { Text("Cancel") } }) }
    discard?.let { draft -> ZerusAlertDialog(onDismissRequest = { discard = null }, title = { Text("Discard saved draft?") },
        text = { Text("This removes the private draft for ${draft.target.session} from this phone.") },
        confirmButton = { TextButton(onClick = { model.discard(draft); discard = null }) { Text("Discard") } },
        dismissButton = { TextButton(onClick = { discard = null }) { Text("Cancel") } }) }
    stopRequest?.let { (session, turn) -> ZerusAlertDialog(onDismissRequest = { stopRequest = null }, title = { Text("Stop this turn?") },
        text = { Text("Interrupt the current turn in " + session.title + ". The session remains available.") },
        confirmButton = { TextButton(onClick = { model.interrupt(session.target, turn); stopRequest = null }) { Text("Stop turn") } },
        dismissButton = { TextButton(onClick = { stopRequest = null }) { Text("Cancel") } }) }
    QuestionSendConfirmationDialog(model)
}

@Composable private fun Tag(text: String, color: Color = Mint) {
    Surface(color = color.copy(alpha = .12f), shape = RoundedCornerShape(6.dp)) {
        Text(text, Modifier.padding(horizontal = 8.dp, vertical = 4.dp), color = color, style = MaterialTheme.typography.labelSmall)
    }
}
@Composable internal fun SessionBadge(session: Session, online: Boolean, modifier: Modifier = Modifier) {
    val badge = DesktopSessionPalette.badge(session, online)
    val dark = MaterialTheme.colorScheme.background.luminance() < .5f
    val colors = DesktopSessionPalette.colors(badge.kind, dark)
    Row(modifier,horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
        Surface(color = Color(colors.background), shape = RoundedCornerShape(5.dp)) {
            Row(Modifier.padding(horizontal = 8.dp, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(5.dp),
                verticalAlignment = Alignment.CenterVertically) {
                when (badge.kind) {
                    DesktopBadgeKind.Working, DesktopBadgeKind.Background -> Box(Modifier.size(5.dp).background(Color(colors.foreground), CircleShape))
                    DesktopBadgeKind.Attention, DesktopBadgeKind.Error -> Text("!", color = Color(colors.foreground),
                        fontWeight = FontWeight.Bold, style = MaterialTheme.typography.labelSmall)
                    DesktopBadgeKind.Paused -> Icon(DesktopIcons.Paused, null, Modifier.size(12.dp), tint = Color(colors.foreground))
                    DesktopBadgeKind.Unread -> Icon(Icons.Default.MarkEmailUnread, null, Modifier.size(12.dp), tint = Color(colors.foreground))
                    DesktopBadgeKind.Scheduled, DesktopBadgeKind.Waiting -> Icon(Icons.Default.Schedule, null, Modifier.size(12.dp), tint = Color(colors.foreground))
                    DesktopBadgeKind.Neutral -> Unit
                }
                Text(badge.caption, color = Color(colors.foreground), style = MaterialTheme.typography.labelSmall,
                    maxLines=if(badge.kind==DesktopBadgeKind.Waiting) 1 else Int.MAX_VALUE,
                    overflow=if(badge.kind==DesktopBadgeKind.Waiting) TextOverflow.Ellipsis else TextOverflow.Clip)
            }
        }
        if (badge.additionalUnread) {
            val unread = DesktopSessionPalette.colors(if(ChildSessions.silenced(session)) DesktopBadgeKind.Neutral else DesktopBadgeKind.Unread,dark)
            Surface(color = Color(unread.background), shape = RoundedCornerShape(5.dp)) {
                Icon(Icons.Default.MarkEmailUnread, "New reply", Modifier.padding(4.dp).size(12.dp), tint = Color(unread.foreground))
            }
        }
        SessionOptionalQuestionChip(session, online)
    }
}
@Composable private fun SessionDetailBadge(session: Session,raw: JSONObject?) {
    val displayed = SelectedSessionPresentation.session(session,raw)
    val kind = if (session.target.agentId.isNotBlank()) DesktopBadgeKind.Neutral else DesktopSessionPalette.badge(displayed, true).kind
    val colors = DesktopSessionPalette.colors(kind,
        MaterialTheme.colorScheme.background.luminance() < .5f)
    Surface(color = Color(colors.background), shape = RoundedCornerShape(5.dp)) {
        Text(selectedStatus(session, raw), Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
            color = Color(colors.foreground), style = MaterialTheme.typography.labelSmall)
    }
}
private fun selectedStatus(session: Session, raw: JSONObject?): String = SelectedSessionPresentation.status(session,raw)
@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun MainListsRefresh(model:ZerusViewModel,screen:Any,content:@Composable BoxScope.()->Unit) {
    if(model.demo || !model.storageReady) Box(Modifier.fillMaxSize(),content=content)
    else {
        val state=rememberPullToRefreshState()
        val scope=rememberCoroutineScope()
        val maximum=with(LocalDensity.current) { 28.dp.toPx() }
        val motion=remember(scope,maximum) { MainListPullMotion(scope,maximum) }
        val refreshing by rememberUpdatedState(model.catalogProgress)
        // Observe without consuming: Material3 still owns the refresh threshold and gesture.
        val before=remember(motion,state) { object:NestedScrollConnection {
            override fun onPreScroll(available:Offset,source:NestedScrollSource):Offset {
                if(source==NestedScrollSource.UserInput && available.y<0 && !refreshing && !state.isAnimating) motion.drag(available.y)
                return Offset.Zero
            }
        } }
        val after=remember(motion,state) { object:NestedScrollConnection {
            override fun onPostScroll(consumed:Offset,available:Offset,source:NestedScrollSource):Offset {
                if(source==NestedScrollSource.UserInput && available.y>0 && !refreshing && !state.isAnimating) motion.drag(available.y)
                return Offset.Zero
            }
        } }
        LaunchedEffect(screen,motion) { motion.reset() }
        val lifecycle=LocalLifecycleOwner.current.lifecycle
        DisposableEffect(lifecycle,motion) {
            val observer=LifecycleEventObserver { _,event -> if(event==Lifecycle.Event.ON_STOP) motion.release() }
            lifecycle.addObserver(observer)
            onDispose { lifecycle.removeObserver(observer); motion.reset() }
        }
        Box(Modifier.fillMaxSize().clipToBounds().nestedScroll(before).pointerInput(motion,screen) {
            try {
                awaitPointerEventScope {
                    while(true) {
                        val event=awaitPointerEvent(PointerEventPass.Initial)
                        if(event.changes.any { it.pressed }) motion.begin() else motion.release()
                    }
                }
            } finally { motion.release() }
        }) {
            PullToRefreshBox(isRefreshing=model.catalogProgress,onRefresh={ model.refresh(explicit=true) },
                state=state,modifier=Modifier.fillMaxSize()) {
                Box(Modifier.fillMaxSize().nestedScroll(after).graphicsLayer { translationY=motion.offset },content=content)
            }
        }
    }
}

/** Touch-only visual resistance; refresh-state animations never move the content. */
private class MainListPullMotion(private val scope:CoroutineScope,private val maximum:Float) {
    var offset by mutableFloatStateOf(0f)
        private set
    private var distance=0f
    private var held=false
    private var returning:Job?=null
    fun begin() {
        if(held) return
        returning?.cancel()
        distance=offset*4f
        held=true
    }
    fun drag(delta:Float) {
        if(!held) return
        distance=(distance+delta).coerceIn(0f,maximum*4f)
        offset=distance*.25f
    }
    fun release() {
        if(!held) return
        held=false
        returning=scope.launch {
            animate(offset,0f,animationSpec=tween(180)) { value,_ -> offset=value }
            distance=0f
        }
    }
    fun reset() {
        returning?.cancel()
        held=false
        distance=0f
        offset=0f
    }
}

@Composable private fun ProjectsScreen(model: ZerusViewModel, onPair: () -> Unit) {
    var query by rememberSaveable { mutableStateOf("") }
    if (model.connections.isEmpty() && !model.demo) { SessionsScreen(model, onPair); return }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Text("Projects", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold) }
        item { Text("Your work across connected machines", color = Muted) }
        item { OutlinedTextField(query, { query = it }, Modifier.fillMaxWidth(), singleLine = true, placeholder = { Text("Search projects") },
            leadingIcon = { Icon(Icons.Default.Search, "Search projects") }, shape = RoundedCornerShape(14.dp)) }
        if (model.demo) item { Text("Preview data. Pair a workspace to view your projects.", color = Amber, style = MaterialTheme.typography.bodySmall) }
        if (model.projectWarnings.isNotEmpty()) item { Text("Project catalog unavailable on ${model.projectWarnings.joinToString(", ")}. Sessions and ordinary folders remain available.", color = Amber, style = MaterialTheme.typography.bodySmall) }
        val visible = model.projects.filter { it.catalogued }.filter { query.isBlank() || it.name.contains(query, true) || it.folders.any { folder -> (folder.path + model.machineName(it.key.connectionId,folder.computerId,folder.computerName)).contains(query, true) } }
        items(visible, key = { it.key.key }) { project ->
            val count = model.sessions.count { it.projectKey == project.key && !SessionFilters.archived(it) }
            val accent = runCatching { Color(android.graphics.Color.parseColor(project.color)) }.getOrDefault(Mint)
            Card(onClick = { model.openProject(project) }, colors = CardDefaults.cardColors(containerColor = Surface), shape = RoundedCornerShape(18.dp)) {
                Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Icon(DesktopIcons.Projects, null, tint = accent)
                        Text(project.name, Modifier.weight(1f), fontWeight = FontWeight.SemiBold, fontSize = 19.sp)
                        Icon(Icons.Default.ChevronRight, "Open project", tint = Muted)
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) { Tag("$count ${if (count == 1) "session" else "sessions"}", accent)
                        if (project.stale) Tag("Last known catalog", Amber)
                        if (!project.catalogued) Tag("Folder", Muted)
                    }
                    ProjectFoldersPreview(model,project,Muted)
                    if (model.connections.size > 1) Text(model.connections.find { it.id == project.key.connectionId }?.displayName.orEmpty(), color = Muted, style = MaterialTheme.typography.labelSmall)
                }
            }
        }
        if (visible.isEmpty()) item { Text(if (model.catalogBusy && model.projects.isEmpty()) "Checking your projects…" else "No projects match this view. Sessions remain available in Sessions.", color = Muted, modifier = Modifier.padding(vertical = 24.dp)) }
    }
}

@Composable private fun ProjectDetails(model: ZerusViewModel, project: ProjectSummary, onViewSessions: () -> Unit) {
    val count = model.sessions.count { it.projectKey == project.key && !SessionFilters.archived(it) }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp), verticalArrangement = Arrangement.spacedBy(18.dp)) {
        Text(project.name, style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold)
        Text(model.connections.find { it.id == project.key.connectionId }?.displayName.orEmpty(), color = Muted)
        if (project.stale) Text("Last known project catalog", color = Amber, style = MaterialTheme.typography.bodySmall)
        Text("$count ${if (count == 1) "session" else "sessions"} across ${project.computers.size} ${if (project.computers.size == 1) "machine" else "machines"}", color = Muted)
        Button(onClick = onViewSessions, modifier = Modifier.fillMaxWidth()) { Icon(DesktopIcons.Sessions, null); Spacer(Modifier.width(8.dp)); Text("View sessions") }
        Text("Folders", style = MaterialTheme.typography.titleLarge)
        ProjectFolderDetails(model,project,Surface,Muted)
    }
}

@Composable internal fun SessionsScreen(model: ZerusViewModel, onPair: () -> Unit) {
    val project = model.sessionProjectScope
    if (model.connections.isEmpty() && !model.demo) {
        BoxWithConstraints(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).heightIn(min=maxHeight).padding(28.dp), verticalArrangement = Arrangement.Center) {
            Image(painterResource(R.drawable.zerus_brand), "", Modifier.size(72.dp))
            Spacer(Modifier.height(24.dp))
            Text("Your agents.\nWithin reach.", fontSize = 34.sp, lineHeight = 40.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(16.dp))
            Text("Follow sessions across your machines, answer questions, and pick up the conversation from your phone.", color = Muted, lineHeight = 24.sp)
            Spacer(Modifier.height(28.dp))
            Button(onClick = onPair, modifier = Modifier.fillMaxWidth()) { Text("Open Machines", Modifier.padding(6.dp)) }
            TextButton(onClick = { model.preview() }, modifier = Modifier.fillMaxWidth()) { Text("Try demo") }
        } }; return
    }
    DisposableEffect(model) { onDispose { model.archive.pause() } }
    LaunchedEffect(model.sessionFilter,model.sessionQuery,model.selectedMachines,model.archiveSupportedMachines.map { MachineKey(it.connectionId,it.id) }) { model.configureArchive() }
    val sessions = model.sessions
    val machines = model.machines
    val projects = model.projects
    val drafts = model.drafts
    val selectedMachines = model.selectedMachines
    val query = model.sessionQuery
    val filter = model.sessionFilter
    val archiveRange = model.archive.range
    val teams = model.teamCollapse
    val projectionGeneration = remember(sessions, machines, projects, drafts, selectedMachines, query, project?.key, filter, archiveRange, teams) { Any() }
    val preparedProjection by produceState<Pair<Any, SessionListPresentation>?>(null, projectionGeneration) {
        val prepared = withContext(Dispatchers.Default) {
            SessionListPresentation.prepare(sessions, machines, projects, selectedMachines, query, project?.key, filter, drafts, archiveRange, teams)
        }
        value = projectionGeneration to prepared
    }
    // Do not mount a restored LazyListState against an empty placeholder catalog:
    // LazyColumn would clamp the saved position before the first projection arrives.
    val (generation, projection) = preparedProjection ?: run {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { Text("Preparing sessions…", color = Muted) }
        return
    }
    val counts = projection.counts
    val machineCounts = projection.machineCounts
    val groups = projection.groups
    val readAllTargets = projection.readAllTargets
    val canReadAll = filter != SessionFilter.Archive && !model.demo && generation === projectionGeneration && readAllTargets.any(model::canReadAll)
    val context = LocalContext.current
    var sessionActions by remember { mutableStateOf<Session?>(null) }
    val listState = rememberLazyListState(model.sessionListIndex, model.sessionListOffset)
    var machineDialog by remember { mutableStateOf(false) }
    val gitViewport = rememberGitViewport(model.gitStatus, listState,
        model.storageReady && !model.demo && filter != SessionFilter.Archive && sessionActions == null && !machineDialog)
    val scrollReset = model.sessionScrollReset
    val scrollScope = project?.key?.key.orEmpty()
    var appliedReset by remember { mutableLongStateOf(scrollReset) }
    LaunchedEffect(scrollReset) {
        if (appliedReset != scrollReset) { appliedReset = scrollReset; listState.scrollToItem(model.sessionListIndex, model.sessionListOffset) }
    }
    DisposableEffect(listState, scrollScope, scrollReset) { onDispose { model.saveSessionScroll(listState.firstVisibleItemIndex, listState.firstVisibleItemScrollOffset, scrollScope, scrollReset) } }
    LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        item { Column {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(project?.name ?: if(model.sessionFilter==SessionFilter.Archive) "Archive" else "Sessions", Modifier.weight(1f), style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold)
                if(model.sessionFilter!=SessionFilter.Archive) TextButton(onClick = {
                    val capturedTargets = readAllTargets.toList()
                    val result = model.markAllRead(capturedTargets,visibleRepliesOnly = true)
                    Toast.makeText(context, ConversationLoadPresentation.readAllOutcome(result.marked, result.skipped, result.partial), Toast.LENGTH_LONG).show()
                }, enabled = canReadAll, modifier = Modifier.semantics {
                    contentDescription = if (canReadAll) "Read all conversations in the current project, machine and search scope on this phone"
                        else if (model.demo) "Read all unavailable in preview"
                        else if (generation !== projectionGeneration) "Read all unavailable while the session scope is updating"
                        else if (readAllTargets.none(model::readAllPending)) "Read all unavailable. No unread replies or review reminders are shown in the current scope"
                        else "Read all unavailable. The current scope needs verified history or loaded replies on this phone"
                }) { Text("Read all") }
            }
        } }
        item { OutlinedTextField(model.sessionQuery, { model.changeSessionQuery(it) }, Modifier.fillMaxWidth(), singleLine = true, placeholder = { Text(if(model.sessionFilter==SessionFilter.Archive) "Search archive names and IDs" else "Search sessions and IDs") },
            leadingIcon = { Icon(Icons.Default.Search, "Search sessions") },
            trailingIcon = { if (model.sessionQuery.isNotEmpty()) IconButton(onClick = { model.changeSessionQuery("") }) { Icon(Icons.Default.Close, "Clear search") } },
            shape = RoundedCornerShape(14.dp)) }
        item { Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            SessionFilter.entries.forEach { filter ->
                val selected = model.sessionFilter == filter
                val count = if(filter==SessionFilter.Archive && !model.demo && model.archiveSupportedMachines.isNotEmpty()) {
                    if(!model.archive.totalsKnown && counts.getValue(filter)==0) "…"
                    else counts.getValue(filter).toString() + if(model.archive.partial) "+" else ""
                } else counts.getValue(filter).toString()
                val accent = if (filter == SessionFilter.Attention && counts.getValue(filter) > 0) Amber else if (selected) Mint else Muted
                OutlinedButton(onClick = { model.chooseSessionFilter(filter) }, modifier = Modifier.height(48.dp).widthIn(min = 56.dp).semantics {
                    contentDescription = "${filter.caption}, $count sessions${if (selected) ", selected" else ""}"
                }, contentPadding = PaddingValues(horizontal = 9.dp), shape = RoundedCornerShape(12.dp),
                    colors = ButtonDefaults.outlinedButtonColors(containerColor = if (selected) Color(0xFF243136) else Color.Transparent, contentColor = accent)) {
                    Icon(DesktopIcons.forName(filter.id), null, Modifier.size(20.dp)); Spacer(Modifier.width(5.dp)); Text(count, style = MaterialTheme.typography.labelMedium)
                }
            }
            OutlinedButton(onClick = { machineDialog = true }, modifier = Modifier.height(48.dp).widthIn(min = 56.dp).semantics { contentDescription = "Filter machines" },
                contentPadding = PaddingValues(horizontal = 9.dp), shape = RoundedCornerShape(12.dp)) {
                Icon(DesktopIcons.Machines, null, Modifier.size(20.dp)); Spacer(Modifier.width(5.dp))
                Text(if (model.selectedMachines.isEmpty()) model.machines.size.toString() else model.selectedMachines.size.toString(), style = MaterialTheme.typography.labelMedium)
            }
        } }
        if(model.sessionFilter==SessionFilter.Archive) item { ArchiveFiltersUi(model.archive,model.archiveMachines.associate { MachineKey(it.connectionId,it.id) to it.name },model.archiveMachines.filter { it !in model.archiveSupportedMachines }.map { it.name },projection.visibleCount) }
        item { Text("${model.sessionFilter.caption} (${projection.visibleCount})" + if (model.selectedMachines.isEmpty()) "" else " on selected machines", color = Muted, style = MaterialTheme.typography.bodyMedium) }
        if (model.demo) item { Text("Preview data. Pair a workspace to interact with real sessions.", color = Amber, style = MaterialTheme.typography.bodySmall) }
        groups.forEach { group ->
            val accent = runCatching { Color(android.graphics.Color.parseColor(group.color)) }.getOrDefault(Mint)
            val collapsed = group.key.key in model.collapsedProjects && model.sessionQuery.isBlank()
            item(key = "group:${group.key.key}") { Surface(onClick = { model.toggleProject(group.key) }, color = Surface, shape = RoundedCornerShape(10.dp), modifier = Modifier.fillMaxWidth().semantics {
                contentDescription = "${group.name}, ${group.sessions.size} sessions, ${if (collapsed) "collapsed" else "expanded"}"
            }) {
                Row(Modifier.padding(horizontal = 12.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    Box(Modifier.width(4.dp).height(24.dp).background(accent, RoundedCornerShape(2.dp)))
                    Text(group.name, Modifier.weight(1f), fontWeight = FontWeight.SemiBold)
                    Text(group.sessions.size.toString(), color = Muted, style = MaterialTheme.typography.labelMedium)
                    Icon(if (collapsed) Icons.Default.ExpandMore else Icons.Default.ExpandLess, if (collapsed) "Expand project" else "Collapse project", tint = Muted)
                }
            } }
            if (!collapsed) items(group.cards, key = { "session:${it.session.target.key}" }) { entry ->
                val session = entry.session
                val online = SessionFilters.online(session, model.machines)
                val machine = model.machines.find { it.connectionId == session.target.connectionId && it.id == session.target.computerId }
                val gitBadge = rememberGitBadge(model.gitStatus, GitStatusAccessPolicies.candidate(session),
                    "session:${session.target.key}" in gitViewport)
                // A child session sits under its parent, indented with a tree connector (spec 3.3).
                Box(Modifier.fillMaxWidth().childTree(entry, MaterialTheme.colorScheme.outline).padding(start = childIndent(entry.depth))) {
                Card(Modifier.sessionCardGestures(RoundedCornerShape(14.dp), onOpen = { model.open(session, preserveProject = true) }, onActions = { sessionActions = session }),
                    colors = CardDefaults.cardColors(containerColor = Surface), shape = RoundedCornerShape(14.dp)) {
                    Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(9.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            ProviderBadge(session.agent, Modifier.size(24.dp))
                            SupervisedMark(session)
                            Text(SessionFilters.label(session), Modifier.weight(1f), fontWeight = FontWeight.SemiBold, fontSize = 17.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            if(SessionNotificationPreferences.muted(session)) Icon(Icons.Default.NotificationsOff,"Notifications muted on all devices",tint=Muted,modifier=Modifier.size(18.dp))
                            // A reply read on another device needs no badge here.
                            (if (!SharedReadPolicies.canShowReplyBadge(session) || SessionFilters.replyInProgress(session, online)) null else UnreadPresentation.badge(model.unreadCount(session.target), model.loadedUnreadCount(session.target),
                                SessionFilters.visibleUnreadReply(session, online)))?.let { unread ->
                                val muted = ChildSessions.silenced(session)
                                Surface(color = if(muted) Color(0xFF2B343E) else Mint, shape = CircleShape, modifier = Modifier.semantics { contentDescription = unread.description }) {
                                    Text(unread.text, Modifier.padding(horizontal = 8.dp, vertical = 3.dp), color = if(muted) Muted else Background,
                                        style = MaterialTheme.typography.labelSmall, fontWeight = FontWeight.Bold)
                                }
                            }
                            Icon(Icons.Default.ChevronRight, "Open", tint = Muted, modifier = Modifier.size(20.dp))
                        }
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                            SessionBadge(session, online, if(online && SessionFilters.waitingIdle(session)) Modifier.weight(1.5f,fill=false) else Modifier)
                            Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                MachineLabel(machine?.name ?: session.target.computerId,Modifier.weight(1f,fill=false),colorHex = model.machineColor(MachineKey(session.target.connectionId,session.target.computerId)))
                                ChildSessions.accountMark(session, model.groupedAccounts)?.let { AccountMarkChip(it) }
                            }
                            SessionTeamCounter(model, entry, online)
                        }
                        entry.fromParent?.let { parent -> FromParentLine(parent, entry.fromParentKey?.let { key ->
                            model.sessions.find { it.target.key == key }?.let { listed -> { model.open(listed, preserveProject = true) } } }) }
                        SessionCheckoutLine(session, gitBadge)
                        SessionFilters.archiveDate(session).takeIf { it.isNotBlank() }?.let { archivedDate ->
                            Text("Archived $archivedDate", color = Muted, style = MaterialTheme.typography.labelSmall)
                        }
                        Text(if(online && SessionFilters.waitingIdle(session)) SessionFilters.waitingLabel(session) else session.preview.ifBlank { "Open to view activity" }, maxLines = 2, overflow = TextOverflow.Ellipsis, color = Muted, style = MaterialTheme.typography.bodyMedium, lineHeight = 20.sp)
                        if (model.connections.size > 1) Text(model.connections.find { it.id == session.target.connectionId }?.displayName.orEmpty(), color = Muted, style = MaterialTheme.typography.labelSmall)
                        SessionSubagentRows(model, session, online)
                    }
                }
                }
            }
        }
        if (projection.visibleCount == 0 && !(model.sessionFilter==SessionFilter.Archive && model.archive.loading)) item { Text(if (model.catalogBusy && model.sessions.isEmpty()) "Checking your sessions…" else if(model.sessionFilter==SessionFilter.Archive && model.archive.partial) "No sessions in this page match. Browse older/newer sessions or narrow the date range." else "No sessions match this view.", color = Muted, modifier = Modifier.padding(vertical = 24.dp)) }
    }
    sessionActions?.let { SessionActionsSheet(model, it) { sessionActions = null } }
    if (machineDialog) ZerusAlertDialog(onDismissRequest = { machineDialog = false }, title = { Text("Filter machines") },
        text = { Column(Modifier.heightIn(max = 420.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("${model.sessionFilter.caption} before search", color = Muted, style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { model.allMachines() }) { Text("All machines (${machineCounts.values.sum()})") }
            model.machines.forEach { machine ->
                val key = MachineKey(machine.connectionId, machine.id)
                val count = machineCounts.getOrDefault(key, 0)
                Surface(onClick = { model.toggleMachine(key) }, color = Color.Transparent, modifier = Modifier.semantics {
                    contentDescription = "${machine.name}, $count sessions"
                }) { Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(key in model.selectedMachines, null)
                    Column(Modifier.weight(1f)) { MachineLabel(machine.name,colorHex = model.machineColor(key)); if (model.connections.size > 1) Text(model.connections.find { it.id == machine.connectionId }?.displayName.orEmpty(), color = Muted, style = MaterialTheme.typography.labelSmall) }
                    Text(count.toString(), color = Muted, style = MaterialTheme.typography.labelLarge, modifier = Modifier.padding(start = 12.dp, end = 8.dp))
                } }
            }
            Text("An empty selection shows all machines.", color = Muted, style = MaterialTheme.typography.bodySmall)
        } }, confirmButton = { TextButton(onClick = { machineDialog = false }) { Text("Done") } })
}

@Composable private fun rememberConversationLoad(model: ZerusViewModel, target: Target): ConversationLoadPresentation.Status? {
    var opening by remember(target.key) { mutableStateOf(true) }
    // The opening inspection and canonical history can finish in either order.
    // Keep their first refresh visible until the owned window is ready; later
    // automatic polls retain the ordinary five-second progress threshold.
    LaunchedEffect(target.key, model.detailBusy, model.olderLoading, model.readPositionReady, model.activityVerified) {
        if (model.activityVerified && model.readPositionReady && !model.detailBusy && !model.olderLoading) opening = false
    }
    val historyGate = remember(target.key) { LoadingGate(android.os.SystemClock::elapsedRealtime) }
    var historyProgress by remember(target.key) { mutableStateOf(false) }
    LaunchedEffect(target.key, model.olderLoading, model.demo) {
        historyProgress = false
        if (!model.olderLoading || model.demo) { historyGate.cancel(); return@LaunchedEffect }
        val operation = historyGate.begin(target.key).operation
        delay(historyGate.remaining(operation.id))
        if (historyGate.owns(operation.id)) historyProgress = historyGate.visible()
    }
    val hasContent = model.conversationEvents.isNotEmpty() ||
        target.archiveId.isBlank() && model.conversationQuestions.isNotEmpty() || model.speech.outgoing.any { it.belongsTo(target) }
    return ConversationLoadPresentation.status(hasContent, model.detailBusy, model.detailProgress,
        model.olderLoading, historyProgress, model.demo, resolved = model.activity != null, opening = opening)
}

@Composable private fun ConversationReadTiming(model: ZerusViewModel, status: ConversationLoadPresentation.Status?) {
    if (BuildConfig.READ_TIMING_ENABLED) {
        val stage = when {
            status?.spinner == true -> RelayReadTiming.Stage.VisibleChecking
            model.activityVerified -> RelayReadTiming.Stage.VisibleChecked
            else -> null
        }
        val historyReady = model.historyComplete && model.readPositionReady
        LaunchedEffect(model.selected?.target, stage, historyReady) {
            if (stage != null) withFrameNanos {
                RelayReadTiming.point(stage)
                if (stage == RelayReadTiming.Stage.VisibleChecked && historyReady)
                    RelayReadTiming.point(RelayReadTiming.Stage.VisibleHistory)
            }
        }
    }
}

@Composable private fun Conversation(model: ZerusViewModel, session: Session, loadStatus: ConversationLoadPresentation.Status?, onReview: (Draft) -> Unit,
    onReviewOutgoing: (OutgoingMessage) -> Unit, onAttach: (Target) -> Unit,
    positions: MutableMap<String, ConversationAnchor>,jumpRequest: ConversationMessageJump?,onJumpConsumed: () -> Unit) {
    val target = session.target
    val raw = model.activity
    val archived = target.archiveId.isNotBlank()
    val events = if (model.demo) Demo.events else VoiceHistory.merge(target, model.conversationEvents, model.speech.outgoing)
    LaunchedEffect(target, model.speech.ready) { if (!model.demo && model.speech.ready) model.speech.refreshProfiles() }
    LaunchedEffect(target, model.conversationEvents, model.speech.ready, model.activityVerified) {
        if (!model.demo && model.activityVerified) model.speech.observe(target, model.conversationEvents)
    }
    val questions = if (archived || model.demo) emptyList() else model.conversationQuestions
    var savedQuestionDisplay by rememberSaveable(stateSaver = Saver< QuestionDisplayState, String>(
        save = { it.encode() }, restore = { QuestionDisplayState.decode(it) })) { mutableStateOf(QuestionDisplayState(target.key)) }
    val questionDisplay = savedQuestionDisplay.forTarget(target.key)
    fun updateQuestionDisplay(change: (QuestionDisplayState) -> QuestionDisplayState) {
        savedQuestionDisplay = change(savedQuestionDisplay.forTarget(target.key))
    }
    val questionQueue = remember(questions, target, questionDisplay.chosen) { QuestionPolicies.queue(questions, target, questionDisplay.chosen) }
    val offeredQuestionCount = questions.filter { QuestionPolicies.shown(it) && QuestionPolicies.submitted(it, target) == null }.distinctBy { it.id }.size
    val outgoing = model.conversationOutgoing
    val ownSendId = model.ownSendId(target)
    // Exact inspection evidence supersedes the older computer catalog while reading.
    val online = model.activityVerified || model.demo
    // BEGIN QUESTION_AVAILABILITY_PROBE committed
    if (QuestionUiProbe.enabled) {
        val observedAvailability = model.questionAvailabilityProbe(target, online)
        if (observedAvailability != null) SideEffect { QuestionUiProbe.availability(target, observedAvailability) }
    }
    // END QUESTION_AVAILABILITY_PROBE committed
    val warning = if (model.demo) "" else raw?.string("pending_questions_error", "provider_messages_error").orEmpty()
    val queueVisible = target.agentId.isBlank() && !archived && raw?.optJSONObject("input_queue")?.string("id").orEmpty().isNotBlank()
    val unreadBoundary = model.firstUnreadEventId.takeIf { id -> events.any { it.id == id } }.orEmpty()
    val savedViewport = model.conversationViewport(target)
    val readReady = model.readPositionReady || model.demo
    val unread = UnreadPresentation.conversationBadge(model.unreadCount(target), model.loadedUnreadCount(target))
    var unavailableSaved by remember(target.key) { mutableStateOf(false) }
    var latestPending by remember(target.key) { mutableStateOf(false) }
    var searchNotice by remember(target.key) { mutableStateOf("") }
    val historyReason = if(model.demo) "" else model.historyReason(target)
    val boundedNotice = if(model.historyEpoch.isNotBlank()) {
        if(model.hasOlder || model.hasNewer) "Showing part of the conversation. Use the history controls to load more messages." else ""
    } else if(model.conversationHistoryTruncated) "Showing recent activity. Older messages may be outside the available history." else ""
    val conversationLoading = loadStatus?.initial == true
    val context = LocalContext.current
    var previousRowKeys by remember(target.key) { mutableStateOf<ConversationRowKeys.State?>(null) }
    val rowKeys = remember(target.key, events, model.conversationSupersededIds) {
        ConversationRowKeys.reconcile(target.key, previousRowKeys, events, model.conversationSupersededIds)
    }
    SideEffect { previousRowKeys = rowKeys }
    // The bounded-window notice is a status detail now (zerus-a4mub.10), not a list item.
    val keys = ConversationViewport.keys(events, unreadBoundary, warning.isNotBlank(), hasBoundedNotice = false,
        conversationLoading = conversationLoading, queueVisible = queueVisible, questionsVisible = questions.isNotEmpty(), eventKey = { rowKeys.key(it.id) })
    val rowKeysHolder = remember(target.key) { mutableStateOf(rowKeys) }
    val eventIdsHolder = remember(target.key) { mutableStateOf(events.mapTo(mutableSetOf()) { it.id }) }
    SideEffect {
        rowKeysHolder.value = rowKeys
        eventIdsHolder.value = events.mapTo(mutableSetOf()) { it.id }
    }
    val currentRowKeys by rowKeysHolder
    val currentEventIds by eventIdsHolder
    fun displayedEventId(key: String): String? {
        if (!key.startsWith("event:")) return null
        return (currentRowKeys.eventIdByKey[key] ?: key.removePrefix("event:")).takeIf { it in currentEventIds }
    }
    val listState = rememberLazyListState()
    val cardVisible by remember(listState) { derivedStateOf {
        listState.layoutInfo.visibleItemsInfo.any { it.key == "history:questions" &&
            it.offset < listState.layoutInfo.viewportEndOffset && it.offset + it.size > listState.layoutInfo.viewportStartOffset }
    } }
    val initialAnchor = remember(target.key) { positions[target.key] }
    var follow by remember(target.key) { mutableStateOf(ConversationFollowState(
        following = initialAnchor?.following ?: true, ownSendId = ownSendId, anchor = initialAnchor)) }
    var programmatic by remember { mutableStateOf(false) }
    val scrollMutex = remember { Mutex() }
    var viewportHeight by remember { mutableIntStateOf(0) }
    var viewportBounds by remember { mutableStateOf<androidx.compose.ui.geometry.Rect?>(null) }
    // Keep history above the card unless the IME has already reduced its measured viewport.
    val density = LocalDensity.current
    val imeVisible = WindowInsets.ime.getBottom(density) > 0
    val cardMaxHeight = with(density) {
        if (imeVisible) viewportHeight.toDp().coerceAtLeast(0.dp)
        else (viewportHeight.toDp() * .75f).coerceAtLeast(160.dp)
    }
    val scope = rememberCoroutineScope()
    val currentHistoryTarget by rememberUpdatedState(target.key)
    val window = LocalWindowInfo.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var foreground by remember(lifecycle) { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) }
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, _ -> foreground = lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED) }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    suspend fun jump(index: Int, offset: Int = 0) {
        scrollMutex.withLock {
            if (target.key != currentHistoryTarget || model.selected?.target != target) return@withLock
            programmatic = true
            try { listState.scrollToItem(index.coerceAtLeast(0), offset.coerceAtLeast(0)) }
            finally { programmatic = false }
        }
    }
    val obscured = LocalConversationObscuration.current?.obscured == true
    LaunchedEffect(jumpRequest,obscured,events,target.key) {
        val request = jumpRequest ?: return@LaunchedEffect
        if(request.target != target) { onJumpConsumed();return@LaunchedEffect }
        if(obscured) return@LaunchedEffect
        val match = ConversationSearch.resolve(events,request.eventId)
        if(match == null) searchNotice = "That message is no longer in the loaded window. Search again or load its earlier page."
        else {
            follow = follow.copy(opened = true,following = false,anchor = ConversationAnchor(rowKeys.key(match.id),0,false))
            jump(keys.indexOf(rowKeys.key(match.id)))
        }
        onJumpConsumed()
    }
    val latestKeys by rememberUpdatedState(keys)
    val renderedHistory by rememberUpdatedState(Triple(model.conversationEvents,model.hasOlder,model.hasNewer))
    fun requestLatest() {
        if(model.selected?.target != target || latestPending) return
        val needsHistory = model.hasNewer || model.olderLoading
        // Await this accepted navigation, rather than hoping a content effect sees
        // a loading transition. A cached/identical read can finish in one frame.
        latestPending = true
        scope.launch {
            try {
                if(needsHistory) {
                    val read = model.jumpToLatest(target)
                    if(read == null) {
                        if(currentHistoryTarget == target.key && model.selected?.target == target) Toast.makeText(context,
                            "Could not load the latest messages. ${model.olderError.ifBlank { "Wait for the current history read, then try again." }}",
                            Toast.LENGTH_LONG).show()
                        return@launch
                    }
                    read.join()
                    // The read job prepares presentation off-main; its rows and
                    // paging flags must be installed before choosing the tail.
                    snapshotFlow { model.selected?.target != target || currentHistoryTarget != target.key || !model.olderLoading &&
                        renderedHistory == Triple(model.conversationEvents,model.hasOlder,model.hasNewer) }.first { it }
                }
                if(model.selected?.target != target || currentHistoryTarget != target.key) return@launch
                val request = ConversationViewport.loadedLatest(follow,latestKeys,
                    needsHistory && model.hasNewer,if(needsHistory) model.olderError else "")
                if(request.failure.isNotBlank()) Toast.makeText(context,request.failure,Toast.LENGTH_LONG).show()
                else {
                    follow = request.state
                    request.index?.let { jump(it) }
                }
            } finally { latestPending = false }
        }
    }
    LaunchedEffect(target.key, events, questions, ownSendId, model.activityVerified, warning, boundedNotice,
        queueVisible, unreadBoundary, readReady, model.hasNewer, model.olderLoading) {
        if (events.isEmpty() && questions.isEmpty() && raw == null && !model.demo) return@LaunchedEffect
        val own = ConversationViewport.own(events, ownSendId)
        // With no cards below the message, align its bottom to the composer.
        val ownKey = if (own == null) null else if (questions.isEmpty() && !queueVisible) "history:tail" else rowKeys.key(own.id)
        val explicitSend = ownSendId.isNotBlank() && ownSendId != follow.ownSendId && ownKey != null
        val firstOpen = !follow.opened
        // Ledger hydration must not cause a second jump after the reader deliberately scrolls.
        if (firstOpen && !explicitSend && !readReady) return@LaunchedEffect
        if (firstOpen && !explicitSend) {
            val opening = UnreadPresentation.opening(initialAnchor, savedViewport, unreadBoundary, keys, rowKeys::key)
            unavailableSaved = opening.unavailableSaved
            follow = follow.copy(anchor = opening.anchor, following = opening.anchor?.following ?: true)
        }
        if(model.hasNewer && !explicitSend) follow = follow.copy(following = false)
        if(explicitSend && model.hasNewer && !latestPending) requestLatest()
        val request = ConversationViewport.content(follow, keys, ownSendId, ownKey)
        follow = request.state
        if (explicitSend || firstOpen || !listState.isScrollInProgress || programmatic)
            request.index?.let { jump(it, request.offset) }
    }
    LaunchedEffect(listState, target.key) {
        var userMovement = false
        snapshotFlow {
            val layout = listState.layoutInfo
            val tail = layout.visibleItemsInfo.lastOrNull()
            Triple(listState.isScrollInProgress, tail?.key == "history:tail" &&
                tail.offset + tail.size <= layout.viewportEndOffset + 2, programmatic)
        }.distinctUntilChanged().collect { (moving, atEnd, automatic) ->
            if (!automatic && moving) userMovement = true
            if (!automatic && userMovement) {
                follow = ConversationViewport.userScrolled(follow, atEnd).copy(opened = true)
                if (!moving) userMovement = false
            }
        }
    }
    // Follow actual newly measured content, rather than every refresh or
    // verification transition. A no-op poll leaves the reader's pixels alone.
    LaunchedEffect(listState, target.key) {
        snapshotFlow {
            val layout = listState.layoutInfo
            val tail = layout.visibleItemsInfo.lastOrNull()
            follow.opened && follow.following && !programmatic && !listState.isScrollInProgress &&
                layout.visibleItemsInfo.isNotEmpty() && layout.totalItemsCount == latestKeys.size &&
                (tail?.key != "history:tail" || tail.offset + tail.size > layout.viewportEndOffset + 2)
        }.distinctUntilChanged().collect { needsTail ->
            if (needsTail) jump(latestKeys.lastIndex)
        }
    }
    val settledAtTail by remember(listState) { derivedStateOf {
        val layout = listState.layoutInfo
        val tail = layout.visibleItemsInfo.lastOrNull()
        tail?.key == "history:tail" && tail.offset + tail.size <= layout.viewportEndOffset + 2
    } }
    LaunchedEffect(settledAtTail,listState.isScrollInProgress,programmatic,follow.opened,follow.following,follow.anchor,model.hasNewer) {
        if(!listState.isScrollInProgress && !programmatic) follow = ConversationViewport.settled(follow,settledAtTail,model.hasNewer)
    }
    val videoCapture = model.speech.recording && model.speech.videoRecorder != null
    var videoOverlayVisible by remember { mutableStateOf(false) }
    val readable = foreground && window.isWindowFocused && follow.opened && readReady &&
        !obscured && !videoCapture && !videoOverlayVisible
    LaunchedEffect(target.key,listState,readable,model.historyEpoch,model.hasOlder,model.olderLoading,model.olderError) {
        // Entering canonical history is an explicit choice; scrolling a short recent
        // inspection must never start indexing the entire conversation.
        if(!readable || model.historyEpoch.isBlank() || !model.hasOlder || model.olderLoading || model.olderError.isNotBlank()) return@LaunchedEffect
        snapshotFlow { listState.isScrollInProgress && !programmatic && listState.firstVisibleItemIndex <= 3 }
            .distinctUntilChanged().collect { nearTop -> if(nearTop) model.loadOlder(target) }
    }
    // The shared readable-visibility rule (zerus-a4mub.31) in this screen's pixels.
    val readableHeight = with(LocalDensity.current) { ConversationViewport.READABLE_HEIGHT_DP.dp.roundToPx() }
    LaunchedEffect(target.key, listState, readable, readableHeight) {
        if (!readable) return@LaunchedEffect
        snapshotFlow {
            val layout = listState.layoutInfo
            if (programmatic || listState.isScrollInProgress) null else layout.visibleItemsInfo.filter {
                it.key.toString().startsWith("event:") && it.offset < layout.viewportEndOffset &&
                    it.offset + it.size > layout.viewportStartOffset
            }.let { rows ->
                val items=layout.visibleItemsInfo.map { ConversationViewport.Item(it.key.toString(),it.offset,it.size) }
                val start=layout.viewportStartOffset;val end=layout.viewportEndOffset
                val ids=ConversationViewport.visibleEventIds(items,start,end,readableHeight,::displayedEventId)
                ReadViewport(ids,rows.firstOrNull()?.let { row -> displayedEventId(row.key.toString())?.let { it to (-row.offset).coerceAtLeast(0) } },
                    model.visibleReadEvidence(target,ids),model.currentReadKey(target),
                    ConversationViewport.currentFlags(items,start,end,readableHeight,model.newestDisplayedEventId(target),::displayedEventId))
            }
        }.distinctUntilChanged().collectLatest { viewport ->
            if (viewport == null) return@collectLatest
            // A transient row during initial positioning or a fling is not a deliberate read.
            delay(350)
            if (currentHistoryTarget == target.key && foreground && window.isWindowFocused && !programmatic) {
                model.visibleMessages(target, viewport.ids)
                model.visibleCurrent(target,viewport.current,viewport.flags)
                viewport.first?.let { (id, offset) -> model.saveConversationViewport(target,id,offset,follow.following && !model.hasNewer) }
            }
        }
    }
    DisposableEffect(target.key, listState) {
        onDispose {
            listState.layoutInfo.visibleItemsInfo.firstOrNull()?.let { item ->
                val dataKey = displayedEventId(item.key.toString())?.let { "event:$it" } ?: item.key.toString()
                positions[target.key] = ConversationAnchor(dataKey, listState.firstVisibleItemScrollOffset, follow.following)
            }
        }
    }
    // zerus-a4mub.10: one time-bound conversation status with details on demand. Presentation
    // only: nothing below takes question, send or read authority from it.
    val syncFacts = ConversationSyncStatus.Facts(now = 0L, demo = model.demo, archived = archived,
        loaded = raw != null || events.isNotEmpty() || questions.isNotEmpty(),
        verified = model.activityVerified, observedAt = model.conversationObservedAt,
        refreshing = model.detailBusy, refreshVisible = model.detailBusy && model.detailProgress,
        failure = model.conversationReadFailure(target),
        offline = model.conversationReadOffline(target) || model.computerReportedOffline(target),
        // Newer messages outside the loaded window; older ones are history, below.
        tailPartial = model.hasNewer,
        history = when {
            model.olderLoading -> ConversationSyncStatus.History.Loading
            model.olderError.isNotBlank() -> ConversationSyncStatus.History.Failed
            model.historyIndexing -> ConversationSyncStatus.History.Indexing
            model.hasOlder -> ConversationSyncStatus.History.MoreAvailable
            model.historyEpoch.isBlank() && model.conversationHistoryTruncated -> ConversationSyncStatus.History.Truncated
            model.historyEpoch.isNotBlank() && model.historyComplete -> ConversationSyncStatus.History.Complete
            else -> ConversationSyncStatus.History.Unknown
        }, historyMessage = model.olderError,
        atLatest = follow.following && !model.hasNewer, unreadLabel = unread?.label.orEmpty(),
        read = model.sharedReadStatus(target))
    Column(Modifier.fillMaxSize().imePadding()) {
        ConversationStatusStrip(syncFacts, refreshing = model.detailBusy && model.detailProgress,
            onRefresh = { model.refreshActivity(explicit = true) }) {
            // Compact here ("17 unread", "17+ unread"); the full wording is its description and the Position detail.
            if (unread != null) Text("${unread.text} unread", Modifier.semantics { contentDescription = unread.description },
                color = if (SessionNotificationPreferences.parse(raw ?: session.raw,target)?.muted != true) Mint else Muted,
                style = MaterialTheme.typography.labelMedium, maxLines = 1)
            if (offeredQuestionCount > 0) TextButton(onClick = {
                if (!questionDisplay.expanded) questionQueue.selected?.let { question -> updateQuestionDisplay { it.open(question) } }
            }) { Text("Questions ($offeredQuestionCount)") }
        }
        if(searchNotice.isNotBlank()) Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp),verticalAlignment = Alignment.CenterVertically) {
            Text(searchNotice,Modifier.weight(1f),color = Amber,style = MaterialTheme.typography.bodySmall)
            IconButton(onClick = { searchNotice = "" }) { Icon(Icons.Default.Close,"Dismiss search notice") }
        }
        Box(Modifier.weight(1f).fillMaxWidth().onSizeChanged { viewportHeight = it.height }
            .onGloballyPositioned { viewportBounds = it.boundsInWindow() }) {
            Box(Modifier.fillMaxSize().semantics { if (videoCapture || videoOverlayVisible) hideFromAccessibility() }) {
            ConversationTailRefresh(model, target.key, Modifier.fillMaxSize()) {
            // The tail item already adds one item gap above the composer.
            LazyColumn(Modifier.fillMaxSize(), state = listState, contentPadding = PaddingValues(start = 16.dp, top = 16.dp, end = 16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)) {
                item(key = "history:earlier") { if (loadStatus?.initial != true) Column(Modifier.fillMaxWidth(),horizontalAlignment = Alignment.CenterHorizontally) {
                    if(model.olderError.isNotBlank()) {
                        Text(model.olderError,color = Amber,style = MaterialTheme.typography.bodySmall)
                        TextButton(onClick = { model.retryHistory(target) },enabled = !model.olderLoading &&
                            (model.historyRetryIsLatest(target) || historyReason.isBlank())) { Text(model.historyRetryLabel(target)) }
                    } else if(model.hasOlder) TextButton(onClick = { model.loadOlder(target) },enabled = !model.olderLoading && historyReason.isBlank()) { Text("Load earlier messages") }
                    else if(model.historyEpoch.isNotBlank() && !model.historyIndexing) Text(if(model.historyComplete) "Beginning of conversation" else "No earlier page reported; history may be incomplete.",color = Muted,style = MaterialTheme.typography.labelSmall)
                    if(model.historyIndexing) Text("History is still indexing. Unread totals may be unavailable.",color = Muted,style = MaterialTheme.typography.labelSmall)
                    if(historyReason.isNotBlank() && events.isNotEmpty()) Text(historyReason,color = Muted,style = MaterialTheme.typography.labelSmall)
                } }
                // Freshness lives in the status strip; this row keeps only the session's own badge.
                item(key = "history:status") { if (loadStatus?.initial != true && events.isNotEmpty() && (raw != null || model.demo))
                    SessionDetailBadge(session,raw) }
                if (warning.isNotBlank()) item(key = "history:warning") { Text(warning, color = Amber, style = MaterialTheme.typography.bodySmall) }
                item(key = "history:missing-anchor") {
                    if (unavailableSaved) Text("Your saved reading position is outside the loaded messages. Showing the earliest available messages.",
                        color = Muted, style = MaterialTheme.typography.bodySmall)
                }
                // A fork's inherited rows: each source under its label, then where the fork begins.
                var inheritedSource = ""
                events.forEach { event ->
                    if (event.inheritedFrom != inheritedSource) {
                        val label = if (event.inheritedFrom.isNotBlank()) "Inherited from ${event.inheritedFrom}: the history this fork began from"
                            else "Forked here: this session's own activity continues below"
                        item(key = ConversationViewport.inheritedKey(rowKeys.key(event.id))) {
                            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                HorizontalDivider(Modifier.weight(1f), color = Muted.copy(alpha = .4f))
                                Text(label, Modifier.weight(3f, fill = false), color = Muted, style = MaterialTheme.typography.labelMedium)
                                HorizontalDivider(Modifier.weight(1f), color = Muted.copy(alpha = .4f))
                            }
                        }
                        inheritedSource = event.inheritedFrom
                    }
                    if (event.id == unreadBoundary) item(key = "history:unread") {
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            HorizontalDivider(Modifier.weight(1f), color = Mint.copy(alpha = .4f))
                            Text("Unread messages", color = Mint, style = MaterialTheme.typography.labelMedium)
                            HorizontalDivider(Modifier.weight(1f), color = Mint.copy(alpha = .4f))
                        }
                    }
                    item(key = rowKeys.key(event.id)) {
                    val provider = if (target.agentId.isNotBlank()) raw?.string("provider").orEmpty().ifBlank { session.agent } else session.agent
                    ConversationMessage(target, event, provider, outgoing.find { it.requestId == event.outgoingId },
                        model, onReviewOutgoing)
                    }
                }
                if (events.isEmpty() && !conversationLoading) item(key = "history:empty") {
                    Text(if (raw != null || model.demo) "No messages yet." else
                        "No recent messages are available. Refresh to load activity.", color = Muted)
                }
                item(key = "history:newer") {
                    if(model.hasNewer) Row(Modifier.fillMaxWidth(),horizontalArrangement = Arrangement.Center) {
                        TextButton(onClick = { model.loadNewer(target) },enabled = !model.olderLoading && historyReason.isBlank()) { Text("Load newer messages") }
                        TextButton(onClick = ::requestLatest,enabled = !latestPending) { Text("Latest") }
                    }
                }
                if (queueVisible) item(key = "history:queue") { NativeInputQueue(model,target) }
                if (questions.isNotEmpty()) item(key = "history:questions") {
                    QuestionsPane(model, target, questions, online && model.activityVerified && !model.contextBlocked(target), cardVisible,
                        viewportBounds, cardMaxHeight, questionDisplay, ::updateQuestionDisplay, onReview)
                }
                item(key = "history:tail") { Spacer(Modifier.height(1.dp)) }
            }
            }
            if (loadStatus?.initial == true && !queueVisible) Column(Modifier.align(Alignment.Center).padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(loadStatus.text, Modifier.semantics { if (!obscured) liveRegion = LiveRegionMode.Polite }, color = Muted, style = MaterialTheme.typography.bodyMedium)
            }
            if ((!follow.following || model.hasNewer || latestPending) && (events.isNotEmpty() || questions.isNotEmpty()) &&
                !(questions.isNotEmpty() && cardVisible))
                FilledTonalButton(onClick = ::requestLatest,enabled = !latestPending,
                    modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp).heightIn(min = 48.dp).semantics {
                        contentDescription = "Jump to latest messages"
                        stateDescription = UnreadPresentation.latest(latestPending).description
                    }, shape = RoundedCornerShape(16.dp),
                    colors = ButtonDefaults.filledTonalButtonColors(containerColor = Color(0xFF2D4142),contentColor = Mint,
                        disabledContainerColor = Color(0xFF2D4142),disabledContentColor = Muted)) {
                    if(latestPending) CircularProgressIndicator(Modifier.size(18.dp),color = Mint,strokeWidth = 2.dp)
                    else Icon(Icons.Default.KeyboardArrowDown, null)
                    Text(UnreadPresentation.latest(latestPending).text,
                        Modifier.padding(start = 4.dp),style = MaterialTheme.typography.labelMedium)
                }
            }
            RoundVideoRecordingOverlay(model.speech, Modifier.matchParentSize()) { videoOverlayVisible = it }
        }
        if (archived) {
            ContextFooter(model, session)
            Text("Archived conversation. Read-only.", color = Muted, style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.fillMaxWidth().background(Surface).padding(16.dp))
        }
        else MessageComposer(model, session, online, onReview, onAttach, onReviewOutgoing)
    }
    ExpandedQuestionHost(model, target, questions, online && model.activityVerified && !model.contextBlocked(target),
        questionDisplay, ::updateQuestionDisplay, onReview)
}

/**
 * One settled reading viewport: the visible event rows, the first row and its offset, the private read
 * evidence, and for the shared current acknowledgement the open inspection's values ([ZerusViewModel.currentReadKey])
 * and what the viewport showed of it ([ConversationViewport.currentFlags]).
 */
private data class ReadViewport(val ids:Set<String>,val first:Pair<String,Int>?,val evidence:List<Triple<String,String,Long?>>,
    val current:String,val flags:ConversationViewport.CurrentFlags)

@Composable internal fun ConversationMessage(target: Target, event: Event, agent: String, outgoing: OutgoingMessage?,
    model: ZerusViewModel, onReview: (OutgoingMessage) -> Unit) {
    if (event.role == ActivityNotices.ROLE) return ActivityNotice(target, event, model)
    val own = QuestionReplyPresentation.isOwnRole(event.role)
    val parsedReply by produceState<Pair<String, List<PresentedQuestionReply>?>>(event.text to null, event.text, own) {
        value = event.text to if (own) withContext(Dispatchers.Default) { QuestionReplyPresentation.parse(event.text) } else null
    }
    val replies = parsedReply.takeIf { it.first == event.text }?.second
    val roundVideo = event.voice?.video != null
    val messageColor = if (roundVideo && own) MaterialTheme.colorScheme.primary.copy(alpha = .14f)
        .compositeOver(MaterialTheme.colorScheme.surface) else if (own) Color(0xFF203733) else Surface
    // A fork's inherited row is its source's history: dimmed, and never acting on this session.
    val inherited = event.inheritedFrom.isNotBlank()
    Row(Modifier.fillMaxWidth().alpha(if (inherited) .72f else 1f), horizontalArrangement = if (own) Arrangement.End else Arrangement.Start) {
        Surface(color = if (roundVideo) Color.Transparent else messageColor,
            shape = if (roundVideo) RectangleShape else RoundedCornerShape(17.dp),
            modifier = Modifier.fillMaxWidth(if (own) .92f else 1f).semantics {
                contentDescription = (if (inherited) "Inherited, " else "") + if (own) "You" else agent.ifBlank { "Agent" }
            }) {
            Column(Modifier.padding(horizontal = if (roundVideo) 0.dp else 12.dp,
                vertical = if (roundVideo) 0.dp else 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                if (!own) Text(agent.ifBlank { event.role }, color = Muted, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
                if (replies == null && event.role == "You (answer)") Text("You (answer)", color = Muted, style = MaterialTheme.typography.labelMedium)
                if (event.voice != null) {
                    ProvideTextStyle(conversationTextStyle()) { VoiceMessageContent(model, target, event.voice,
                        at = event.at, videoBubbleColor = messageColor) }
                } else if (replies != null) {
                    Text("You (answer)", color = Muted, style = MaterialTheme.typography.labelMedium)
                    SelectionContainer { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        replies.orEmpty().forEach { reply ->
                            Text("Question", color = Muted, style = MaterialTheme.typography.labelSmall)
                            ProvideTextStyle(conversationTextStyle()) { MarkdownText(reply.question) }
                            Text("Your answer", color = Mint, style = MaterialTheme.typography.labelSmall)
                            Text(reply.answer.ifEmpty { "(empty answer)" }, style = conversationTextStyle())
                        }
                    } }
                } else if (event.text.isNotBlank()) {
                    if(own) SelectionContainer { Text(event.text, style = conversationTextStyle()) }
                    else ProvideTextStyle(conversationTextStyle()) {
                        MarkdownText(event.text, fileScope = target.key) { reference ->
                            model.computerFiles.show(target, reference, model.machines.find {
                                it.connectionId == target.connectionId && it.id == target.computerId
                            }?.name.orEmpty().ifBlank { "computer" })
                        }
                    }
                }
                if (inherited) event.attachments.forEach { Text("Attachment: ${it.name}", color = Muted, style = MaterialTheme.typography.labelSmall) }
                else if (event.voice == null) AttachmentRows(event.attachments, model.selected?.target, model)
                if(event.detailTruncated) Text("Message text shortened",color = Amber,style = MaterialTheme.typography.labelSmall)
                if (event.voice == null && (event.delivery.isNotBlank() || event.at > 0)) Row(Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End, verticalAlignment = Alignment.CenterVertically) {
                    if (event.at > 0) Text(java.text.SimpleDateFormat("HH:mm", java.util.Locale.getDefault())
                        .format(java.util.Date((event.at * 1000).toLong())), color = Muted, style = MaterialTheme.typography.labelSmall)
                    if (event.delivery.isNotBlank()) {
                        Spacer(Modifier.width(8.dp))
                        Text(MessageDeliveryPresentation.label(event.delivery), color = if (MessageDeliveryPresentation.warning(event.delivery)) Amber else Muted,
                            style = MaterialTheme.typography.labelSmall)
                    }
                }
                if (outgoing != null && outgoing.status in listOf("failed", "uncertain")) {
                    if (!outgoing.deliveryDismissed && outgoing.error.isNotBlank()) Text(outgoing.error, color = Amber, style = MaterialTheme.typography.bodySmall)
                    Row {
                        if (outgoing.status == "uncertain") TextButton(onClick = { model.checkOutgoing(outgoing) },
                            enabled = "outgoing:" + outgoing.requestId !in model.receiptFlights) { Text("Check delivery") }
                        if (!outgoing.deliveryDismissed) TextButton(onClick = { onReview(outgoing) },
                            enabled = "dismiss:" + outgoing.requestId !in model.receiptFlights) { Text("Review") }
                    }
                }
            }
        }
    }
}


/** A message the person did not write, framed like desktop's notice cards (ActivityNotices). */
@Composable private fun ActivityNotice(target: Target, event: Event, model: ZerusViewModel) {
    val label = ActivityNotices.label(event.origin, event.sender)
    var expanded by rememberSaveable(event.id) { mutableStateOf(false) }
    val files: (ComputerFileReference) -> Unit = { reference ->
        model.computerFiles.show(target, reference, model.machines.find {
            it.connectionId == target.connectionId && it.id == target.computerId
        }?.name.orEmpty().ifBlank { "computer" })
    }
    Surface(color = Surface, shape = RoundedCornerShape(17.dp), border = BorderStroke(1.dp, Notice.copy(alpha = .45f)),
        modifier = Modifier.fillMaxWidth().semantics { contentDescription = label }) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(label, color = Notice, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
            ProvideTextStyle(conversationTextStyle()) { MarkdownText(event.text, fileScope = target.key, onFileReference = files) }
            if (event.report.isNotBlank()) {
                TextButton(onClick = { expanded = !expanded }) { Text(if (expanded) "Hide report" else "Show report") }
                if (expanded) ProvideTextStyle(conversationTextStyle()) {
                    MarkdownText(event.report, fileScope = target.key, onFileReference = files)
                }
            }
            AttachmentRows(event.attachments, target, model)
            if (event.detailTruncated) Text("Message text shortened", color = Amber, style = MaterialTheme.typography.labelSmall)
            if (event.at > 0) Text(java.text.SimpleDateFormat("HH:mm", java.util.Locale.getDefault())
                .format(java.util.Date((event.at * 1000).toLong())), color = Muted, style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.align(Alignment.End))
        }
    }
}
@Composable private fun MessageComposer(model: ZerusViewModel, session: Session, online: Boolean,
    onReview: (Draft) -> Unit, onAttach: (Target) -> Unit, onReviewOutgoing: (OutgoingMessage) -> Unit) {
    val target = session.target
    val draft = model.draft(target)
    val recording = model.speech.recording
    // Exclude the initial conversation ID promotion from the editor identity.
    val lane = ComposerIdentity.lane(target)
    val editor = remember(lane) { TextFieldState(initialText = draft.text, initialSelection = TextRange(draft.text.length)) }
    var sync by remember(editor) { mutableStateOf(ComposerSync(draft.generation)) }
    val speechBridge = remember(editor, target) {
        SpeechComposerBridge(target, editor, { sync.generation }) { sync = ComposerSync(it) }
    }
    DisposableEffect(speechBridge) {
        model.bindSpeechComposer(speechBridge)
        onDispose { model.unbindSpeechComposer(speechBridge) }
    }
    val currentTarget by rememberUpdatedState(target)
    var focused by remember(lane) { mutableStateOf(false) }
    var restorePickerFocus by remember(lane) { mutableStateOf(false) }
    var pickerStarted by remember(lane) { mutableStateOf(false) }
    var coldDraft by remember(lane) { mutableStateOf<Draft?>(null) }
    var coldClearTarget by remember(lane) { mutableStateOf<Target?>(null) }
    var blockedSendReason by remember(lane) { mutableStateOf<String?>(null) }
    if(coldDraft != null || coldClearTarget != null || blockedSendReason != null) ObscureConversation()
    val focusRequester = remember(lane) { FocusRequester() }
    LaunchedEffect(editor, draft.generation) {
        if (model.draft(currentTarget).generation != draft.generation) return@LaunchedEffect
        val update = ComposerReconciler.external(sync, draft.generation, draft.text)
        sync = update.sync
        update.replacement?.let { text -> editor.edit {
            replace(0, length, text)
            selection = TextRange(text.length)
        } }
    }
    LaunchedEffect(editor) {
        snapshotFlow { editor.text.toString() }.distinctUntilChanged().collect { text ->
            if (text != model.draft(currentTarget).text) model.edit(currentTarget, text)
        }
    }
    LaunchedEffect(model.pickerId, model.attachmentImporting) {
        if (restorePickerFocus && model.pickerDraft != null) pickerStarted = true
        if (restorePickerFocus && pickerStarted && model.pickerDraft == null && !model.attachmentImporting) {
            restorePickerFocus = false; pickerStarted = false
            focusRequester.requestFocus()
        }
    }
    val blockReason = if (model.activityVerified) {
        if(target.agentId.isNotBlank()) model.childSendReason(target) else NativeParser.messageBlockReason(model.activity)
    }
        else "Refresh to verify the session before sending."
    val canSend = (editor.text.isNotBlank() || draft.attachments.isNotEmpty()) && (draft.attachments.isEmpty() || model.attachmentReason(target).isBlank()) && !model.sendingBlocked(target) && !model.contextBlocked(target) &&
        draft.status == "editing" && !model.demo && online && target.run.isNotBlank() && model.storageReady && blockReason.isBlank()
    val previous = model.outgoing.lastOrNull { it.belongsTo(target) && it.blocksSending }
    val sendReason = when {
        recording -> "Stop recording before sending a message. Your typed draft is kept."
        model.demo -> "Pair to send a message."
        !model.storageReady -> "Private storage is not ready. Sending is unavailable."
        draft.status == "submitting" -> "Sending the saved draft. Wait for its result."
        draft.status != "editing" -> "Review the saved draft before sending again."
        draft.attachments.isNotEmpty() && model.attachmentReason(target).isNotBlank() -> model.attachmentReason(target)
        blockReason.isNotBlank() -> blockReason
        !online || target.run.isBlank() -> "Refresh to verify the session before sending."
        previous?.status == "uncertain" -> "Delivery unknown. Check the previous message before sending again."
        previous?.status == "sending" -> "Sending your previous message. You can write the next message."
        model.pickerDraft?.target == target -> "Wait for file selection and importing to finish."
        model.contextBlocked(target) -> "Check the pending session command before sending."
        model.sendingBlocked(target) -> "Preparing your message. You can write the next message."
        else -> ""
    }
    val retainedPreview = !recording && !model.speech.savingRecording && model.speech.draft != null
    Column(Modifier.fillMaxWidth().background(Background).padding(horizontal = 4.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp)) {
        // A retained preview shares the constrained height with the editor instead
        // of receiving only what the expanded editor leaves below the open IME.
        if (model.speech.recording && (!model.speech.touchRecording || model.speech.videoRecorder != null) || model.speech.savingRecording || model.speech.draft != null) {
            Box(Modifier.weight(1f, fill = false).fillMaxWidth().heightIn(max = 280.dp)
                .verticalScroll(rememberScrollState())) { SpeechComposerStatus(model) }
        }
        ContextFooter(model, session) {
            model.edit(target, editor.text.toString())
            model.draft(target)
        }
        if (draft.status != "editing") Row(verticalAlignment = Alignment.CenterVertically) {
            Text(deliveryLabel(draft.status) + ". The original draft is saved.", Modifier.weight(1f), color = Amber, style = MaterialTheme.typography.labelSmall)
            TextButton(onClick = { onReview(draft) }, enabled = draft.status != "submitting" && draft.key !in model.receiptFlights) { Text("Review") }
        }
        if (draft.attachments.isNotEmpty()) Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            draft.attachments.forEach { attachment -> Surface(color = Color(0xFF243136), shape = RoundedCornerShape(12.dp)) {
                Row(Modifier.padding(start = 9.dp, end = 3.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Icon(Icons.Default.AttachFile, null, Modifier.size(16.dp), tint = Muted)
                    Text(attachment.name, Modifier.widthIn(max = 150.dp), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelMedium)
                    IconButton(onClick = { model.removeAttachment(target, attachment.id) },
                    enabled = !recording && model.pickerDraft?.target != target && draft.status == "editing", modifier = Modifier.size(24.dp)) {
                    Icon(Icons.Default.Close, "Remove " + attachment.name, Modifier.size(16.dp))
                    }
                }
            } }
        }
        PendingAttachmentSelection(model, target)
        if (previous?.status == "uncertain") OutgoingRecovery(previous,
            "outgoing:${previous.requestId}" in model.receiptFlights,
            { model.checkOutgoing(previous) }, { onReviewOutgoing(previous) },
            dismissing = "dismiss:${previous.requestId}" in model.receiptFlights)
        CompactMessageInput(state = editor, placeholder = "Message",
            modifier = if (retainedPreview) Modifier.weight(1f, fill = false) else Modifier,
            sharedPreviewSpace = retainedPreview,
            recordingContent = if (recording) ({ SpeechRecordingRow(model.speech) }) else null,
            // Keep an already-open IME session in place during capture. The hidden
            // editor remains locked by the synchronous text-and-selection filter.
            readOnly = draft.status != "editing" || model.demo,
            // Reject IME/paste/drop edits even before the recording overlay recomposes.
            inputTransformation = InputTransformation { if (model.speech.recording) revertAllChanges() },
            focusRequester = focusRequester, onFocusChanged = { focused = it },
            leading = {
                IconButton(onClick = { restorePickerFocus = focused; pickerStarted = false; onAttach(target) },
                    enabled = !recording && !model.demo && model.storageReady && !model.updateInstallPreparing && model.attachmentReason(target).isBlank() && draft.status == "editing" && model.pickerDraft == null && !model.attachmentPreparing && !model.attachmentImporting) {
                    Icon(Icons.Default.AttachFile, "Attach files", Modifier.size(24.dp).rotate(45f), tint = Muted)
                }
            }, action = {
                if (recording || (editor.text.isEmpty() && draft.attachments.isEmpty() && model.pickerDraft?.target != target)) {
                    SpeechComposerButton(model, target) {
                        if (model.draft(target).text != editor.text.toString()) model.edit(target, editor.text.toString())
                    }
                } else ComposerActionButton(onClick = {
                    if (canSend) {
                        model.edit(target, editor.text.toString())
                        val captured = model.draft(target)
                        if (ContextPresentation.isCold(model.activity)) coldDraft = captured
                        else model.sendMessage(target, expectedDraft = captured)
                    } else if (sendReason.isNotBlank()) blockedSendReason = sendReason
                }, enabled = !recording && (canSend || sendReason.isNotBlank()),
                    containerColor = if (canSend) Mint else Color(0xFF2B3B3B), contentColor = if (canSend) Background else Muted,
                    disabledContainerColor = Color(0xFF2B3B3B), disabledContentColor = Muted) {
                    if (!recording && !canSend && sendReason.isNotBlank()) Icon(Icons.Default.Info, "Why sending is unavailable", Modifier.size(22.dp))
                    else Icon(Icons.AutoMirrored.Filled.Send, "Send message", Modifier.size(22.dp))
                }
            })
    }
    blockedSendReason?.let { reason -> ZerusAlertDialog(onDismissRequest = { blockedSendReason = null },
        title = { Text("Sending unavailable") }, text = { Text(reason) },
        confirmButton = { TextButton(onClick = { blockedSendReason = null }) { Text("Close") } }) }
    coldDraft?.let { captured ->
        val current = model.draft(captured.target)
        val sameDraft = current.generation == captured.generation && current.revision == captured.revision &&
            current.text == captured.text && current.attachments == captured.attachments
        val available = sameDraft && model.activityVerified && model.selected?.target == captured.target &&
            !model.demo && model.storageReady && !model.contextBlocked(captured.target)
        val context = remember(model.activity, model.activityVerified) { ContextPresentation.from(model.activity, !model.activityVerified) }
        val compactReason = model.contextActionReason(captured.target, "compact_context").ifBlank { context.compactReason }
        val clearReason = model.contextActionReason(captured.target, "clear_context").ifBlank { context.clearReason }
        ZerusAlertDialog(onDismissRequest = { coldDraft = null }, title = { Text("Continue with a cold cache?") },
            text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("The next message may process the full conversation again, increasing token use or cost. Send with full context, compact it into a summary first, or clear it and keep your draft. Compaction uses tokens and may omit details.")
                if (!sameDraft) Text("The draft changed. Cancel and review the current composition.", color = Amber)
                OutlinedButton(onClick = { model.compactContext(captured.target, continueDraft = true, expectedDraft = captured); coldDraft = null },
                    enabled = available && compactReason.isBlank()) { Text("Compact and continue") }
                if (compactReason.isNotBlank()) Text(compactReason, color = Muted, style = MaterialTheme.typography.bodySmall)
                TextButton(onClick = { coldClearTarget = captured.target; coldDraft = null },
                    enabled = available && clearReason.isBlank()) { Text("Clear context") }
                if (clearReason.isNotBlank()) Text(clearReason, color = Muted, style = MaterialTheme.typography.bodySmall)
            } }, confirmButton = { TextButton(onClick = {
                model.sendMessage(captured.target, expectedDraft = captured, allowColdCache = true); coldDraft = null
            }, enabled = available) { Text("Send with full context") } },
            dismissButton = { TextButton(onClick = { coldDraft = null }) { Text("Cancel") } })
    }
    coldClearTarget?.let { captured -> ClearContextConfirmation(model, captured, onDismiss = { coldClearTarget = null },
        beforeClear = { model.edit(captured, editor.text.toString()) }) }
}

@Composable private fun DraftsScreen(model: ZerusViewModel, onReview: (Draft) -> Unit, onReviewOutgoing: (OutgoingMessage) -> Unit, onDiscard: (Draft) -> Unit) {
    val unsent = model.drafts.filter { (it.text.isNotBlank() || it.attachments.isNotEmpty() || it.answers.isNotBlank()) && it.status !in listOf("submitted", "recorded") }
    val unresolved = model.outgoing.filter { it.status in listOf("sending", "uncertain", "failed") }
    val contextOperations = model.contextOperations.filter { it.status in listOf("sending", "submitted", "uncertain", "failed") }
    val sessionActions = model.sessionActions.filter { it.status in listOf("sending","uncertain","failed") || it.needsProjectReview }
    var reviewContext by remember { mutableStateOf<ContextOperation?>(null) }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Text("Saved drafts", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold) }
        item { Text("Private to this phone. Kept for the exact machine, session, run, and conversation.", color = Muted) }
        if (model.speech.draft != null || model.speech.outgoing.any { it.needsDraftReview }) item { VoiceSavedMessages(model) }
        if(model.actionResultNotice.isNotBlank()) item { Text(model.actionResultNotice,color = Amber,style = MaterialTheme.typography.bodySmall) }
        items(unsent.sortedByDescending { it.updatedAt }, key = { it.key }) { draft ->
            Card(colors = CardDefaults.cardColors(containerColor = Surface)) { Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(draft.target.session, fontWeight = FontWeight.SemiBold)
                Tag(if (draft.questionId.isBlank()) { if (draft.status == "editing") "Draft" else deliveryLabel(draft.status) }
                    else "Answer " + if (draft.status == "editing") "draft" else deliveryLabel(draft.status).lowercase(), if (draft.status == "editing") Mint else Amber)
                if (draft.text.isNotBlank() || draft.answers.isNotBlank()) SelectionContainer { Text(draft.text.ifBlank { "Saved question answer" }, maxLines = 8, overflow = TextOverflow.Ellipsis) }
                AttachmentRows(draft.attachments.map(Attachment::displayed), draft.target, model)
                Text("Run ${draft.target.run.take(12)}", color = Muted, style = MaterialTheme.typography.labelSmall)
                model.confirmedDraftAction(draft)?.let { action ->
                    TextButton(onClick = { model.restoreDraftToConfirmedResult(draft,action) }) { Text("Restore draft to confirmed session") }
                }
                Row {
                    val session = model.sessions.find { it.target == draft.target }
                        ?: model.sessions.find { ContextPolicies.sameSessionRun(it.target, draft.target) }
                    if (session != null) TextButton(onClick = { model.open(session) }) { Text(if (session.target == draft.target) "Open conversation" else "Open current conversation") }
                    if (draft.detachedId.isNotBlank()) TextButton(onClick = { model.restoreDetached(draft) }) { Text("Restore") }
                    if (draft.status != "editing") TextButton(onClick = { onReview(draft) }, enabled = draft.status != "submitting" && draft.key !in model.receiptFlights) { Text("Review") }
                    TextButton(onClick = { onDiscard(draft) }, enabled = draft.status != "submitting" && draft.key !in model.receiptFlights) { Text("Discard") }
                }
            } }
        }
        if (model.pickerDraft != null) item { PendingAttachmentSelection(model) }
        if (unresolved.isNotEmpty()) item { Text("Outgoing messages", style = MaterialTheme.typography.titleMedium) }
        items(unresolved.sortedByDescending { it.createdAt }, key = { "outgoing:${it.requestId}" }) { message ->
            Card(colors = CardDefaults.cardColors(containerColor = Surface)) { Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(message.target.session, fontWeight = FontWeight.SemiBold)
                Tag(deliveryLabel(message.status), if (message.status in listOf("sending", "uncertain", "failed")) Amber else Mint)
                if (message.text.isNotBlank()) SelectionContainer { Text(message.text, maxLines = 8, overflow = TextOverflow.Ellipsis) }
                AttachmentRows(message.attachments.map(Attachment::displayed), message.target, model)
                if (!message.deliveryDismissed && message.error.isNotBlank()) Text(message.error, color = Amber, style = MaterialTheme.typography.bodySmall)
                Row {
                    model.sessions.find { message.belongsTo(it.target) }?.let { session -> TextButton(onClick = { model.open(session) }) { Text("Open") } }
                    if (message.status == "uncertain") TextButton(onClick = { model.checkOutgoing(message) }, enabled = "outgoing:${message.requestId}" !in model.receiptFlights) { Text("Check delivery") }
                    if (!message.deliveryDismissed && message.status in listOf("failed", "uncertain")) TextButton(onClick = { onReviewOutgoing(message) }, enabled = "dismiss:${message.requestId}" !in model.receiptFlights) { Text("Review") }
                }
            } }
        }
        if (contextOperations.isNotEmpty()) item { Text("Context commands", style = MaterialTheme.typography.titleMedium) }
        items(contextOperations.sortedByDescending { it.createdAt }, key = { "context:${it.requestId}" }) { operation ->
            Card(colors = CardDefaults.cardColors(containerColor = Surface)) { Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(operation.target.session, fontWeight = FontWeight.SemiBold)
                Text(contextOperationLabel(operation.operation, operation.status), color = Amber, style = MaterialTheme.typography.labelMedium)
                if (operation.error.isNotBlank()) Text(operation.error, color = Amber, style = MaterialTheme.typography.bodySmall)
                Text("The original command identity is saved. It is never repeated automatically.", color = Muted, style = MaterialTheme.typography.bodySmall)
                Row {
                    model.sessions.find { ContextPolicies.sameSessionRun(it.target, operation.target) }?.let { current ->
                        TextButton(onClick = { model.open(current) }) { Text("Open") }
                    }
                    if (operation.status in listOf("submitted", "uncertain")) TextButton(onClick = { model.checkContextOperation(operation) },
                        enabled = model.connections.any { it.id == operation.target.connectionId } && "context:${operation.requestId}" !in model.receiptFlights) { Text("Check result") }
                    if (operation.status in listOf("failed", "uncertain")) TextButton(onClick = { reviewContext = operation },
                        enabled = "context:${operation.requestId}" !in model.receiptFlights) { Text("Review") }
                }
            } }
        }
        if(sessionActions.isNotEmpty()) item { Text("Native actions",style = MaterialTheme.typography.titleMedium) }
        items(sessionActions.sortedByDescending { it.createdAt },key = { "action:" + it.requestId }) { action -> SessionActionRecoveryCard(model,action) }
        if (unsent.isEmpty() && unresolved.isEmpty() && contextOperations.isEmpty() && sessionActions.isEmpty()) item { Text("Your unsent messages will appear here.", color = Muted, modifier = Modifier.padding(vertical = 32.dp)) }
    }
    reviewContext?.let { operation -> ContextOperationReview(model, operation, onDismiss = { reviewContext = null }) }
}


private fun deliveryLabel(status: String) = MessageDeliveryPresentation.label(status)
@Composable private fun AttachmentRows(attachments: List<DisplayedAttachment>, target: Target?, model: ZerusViewModel) =
    AttachmentList(attachments, target, model.attachments)
