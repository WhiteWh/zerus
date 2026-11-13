package app.zerus.mobile

import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.NotificationsActive
import androidx.compose.material.icons.filled.QuestionAnswer

import android.widget.Toast
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Checklist
import androidx.compose.material.icons.filled.DoneAll
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Queue
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

@Composable internal fun SessionHeaderTitle(model: ZerusViewModel,session: Session,onDetails: () -> Unit) {
    val target = session.target
    val status = if (model.activity == null && !model.demo) "Checking…" else SelectedSessionPresentation.status(session,model.activity)
    // Freshness is the conversation status strip's (zerus-a4mub.10); the header keeps the session state.
    val freshness = if (model.demo) " / Preview" else ""
    val kind = if (model.activity == null && !model.demo) DesktopBadgeKind.Neutral
        else DesktopSessionPalette.badge(SelectedSessionPresentation.session(session,model.activity),true).kind
    val colors = DesktopSessionPalette.colors(kind,MaterialTheme.colorScheme.background.luminance() < .5f)
    Column(Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable(role = Role.Button,onClickLabel = "Session details",onClick = onDetails),
        verticalArrangement = Arrangement.Center) {
        Text(session.title,maxLines = 1,overflow = TextOverflow.Ellipsis,fontSize = 18.sp,lineHeight = 22.sp)
        val source = ConversationSource.label(session.agent, model.activity ?: session.raw)
        if (source.isNotEmpty()) Text(source, color = MaterialTheme.colorScheme.tertiary, style = MaterialTheme.typography.labelMedium)
        if (session.agent == "external") Text(ConversationSource.QUEUE_WARNING, color = MaterialTheme.colorScheme.onSurfaceVariant,
            style = MaterialTheme.typography.bodySmall)

        Spacer(Modifier.height(4.dp))
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val machineWidth = maxWidth * .4f
            Row(verticalAlignment = Alignment.CenterVertically,horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                MachineLabel(model.machineName(target.connectionId,target.computerId,target.computerId),
                    Modifier.widthIn(max = machineWidth),colorHex = model.machineColor(MachineKey(target.connectionId,target.computerId)))
                Surface(Modifier.weight(1f,fill = false),color = Color(colors.background),shape = RoundedCornerShape(5.dp)) {
                    Text(status + freshness,Modifier.padding(horizontal = 7.dp,vertical = 3.dp),maxLines = 1,
                        overflow = TextOverflow.Ellipsis,style = MaterialTheme.typography.labelSmall,color = Color(colors.foreground))
                }
            }
        }
    }
}

@Composable internal fun SessionHeaderTools(model: ZerusViewModel, session: Session,onDetails: (() -> Unit)? = null,onJump: (ConversationMessageJump) -> Unit) {
    val target = session.target
    val settings = remember(model.activity) { SessionDetailsPresentation.settings(model.activity) }
    val provider = if (target.agentId.isNotBlank()) model.activity?.string("provider").orEmpty().ifBlank { session.agent } else session.agent
    var details by remember(target.key) { mutableStateOf(false) }
    var settingsOpen by remember(target.key) { mutableStateOf(false) }
    var menu by remember(target.key) { mutableStateOf(false) }
    var request by remember(target.key) { mutableStateOf<String?>(null) }
    var terminal by remember(target.key) { mutableStateOf(false) }
    var projectPicker by remember(target.key) { mutableStateOf(false) }
    var search by remember(target.key) { mutableStateOf(false) }
    var readEvidence by remember(target.key) { mutableStateOf<ReadAttentionEvidence?>(null) }
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    val canReadAll = target.archiveId.isBlank() && !model.demo && model.canReadAll(target)
    val sessionReference = remember(target,model.machines) { SessionReference.resolve(target,model.machines) }
    val notificationPreference = SessionNotificationPreferences.parse(model.activity ?: session.raw,target)
    fun openTerminal() { model.prepareTerminal(target);terminal = true }
    fun closeTerminal() { model.cancelTerminalAvailability();terminal = false }
    // A Sessions long-press action opened this session; confirm it here against fresh native details.
    LaunchedEffect(model.sessionMenuRequest,target.key) {
        model.takeSessionMenuRequest(target)?.let { operation -> when(operation) { "terminal" -> openTerminal();SessionProjectProtocol.assign -> projectPicker=true;else -> request=operation } }
    }
    // Tasks belong to catalogued projects; the badge counts the tasks In review.
    val taskProject = remember(target.key, model.projects) { model.taskProject(session) }
    LaunchedEffect(taskProject) { taskProject?.let(model::refreshReviewCount) }
    val tasksOpen = model.tasksKey != null
    // Tasks opens over this conversation and closes with it.
    DisposableEffect(target.key) { onDispose { model.closeTasks() } }
    if(details || settingsOpen || menu || request != null || terminal || search || projectPicker || tasksOpen) ObscureConversation()
    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        TextButton(onClick = { if (target.agentId.isBlank()) settingsOpen = true else if (onDetails != null) onDetails() else details = true },
            modifier = Modifier.weight(1f),contentPadding = PaddingValues(start = 8.dp,end = 4.dp)) {
            BoxWithConstraints(Modifier.fillMaxWidth()) {
                val providerWidth = maxWidth * .45f
                Row(verticalAlignment = Alignment.CenterVertically,horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    ProviderLabel(provider,Modifier.widthIn(max = providerWidth))
                    Text(listOf(settings.model.ifBlank { "Model not reported" }, settings.effort).filter { it.isNotBlank() }.joinToString(" / "),
                        Modifier.weight(1f),maxLines = 1,overflow = TextOverflow.Ellipsis,style = MaterialTheme.typography.labelMedium)
                }
            }
        }
        IconButton(onClick = {
            val result = model.markAllRead(listOf(target))
            Toast.makeText(context,ConversationLoadPresentation.readAllOutcome(result.marked,result.skipped,result.partial),Toast.LENGTH_SHORT).show()
        },enabled = canReadAll,modifier = Modifier.semantics {
            contentDescription = "Read all"
            stateDescription = if (canReadAll) "Read all replies in this conversation on this phone"
                else if (model.demo) "Read all unavailable in preview"
                else if (target.archiveId.isNotBlank()) "Read all unavailable for archived conversations"
                else if (!model.readAllPending(target)) "Read all unavailable. Nothing in this conversation is unread on this phone"
                else "Read all unavailable. This conversation needs verified history or loaded replies on this phone"
        }) { Icon(Icons.Default.DoneAll,null,Modifier.size(20.dp)) }
        if(notificationPreference?.muted == true) Icon(Icons.Default.NotificationsOff,"Notifications muted on all devices",Modifier.size(18.dp))
        IconButton(onClick = { openTerminal() }) { Icon(Icons.Default.Terminal, "Terminal",Modifier.size(20.dp)) }
        if (taskProject != null && target.agentId.isBlank() && target.archiveId.isBlank() && !model.demo) {
            val review = model.reviewCounts[taskProject] ?: 0
            IconButton(onClick = { model.openTasks(session) }, modifier = Modifier.semantics {
                contentDescription = "Tasks"; if (review > 0) stateDescription = "$review In review"
            }) {
                BadgedBox(badge = { if (review > 0) Badge { Text(if (review > 99) "99+" else "$review") } }) {
                    Icon(Icons.Default.Checklist, null, Modifier.size(20.dp))
                }
            }
        }
        Box {
            IconButton(onClick = { readEvidence = if(target.agentId.isBlank()) model.captureReadAttention(target) else null;menu = true }) { Icon(Icons.Default.MoreVert, "Session actions",Modifier.size(20.dp)) }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false },modifier = Modifier.heightIn(max = 480.dp)) {
                DropdownMenuItem(text = { Text("Search loaded messages") },leadingIcon = { Icon(Icons.Default.Search,null) },
                    onClick = { menu = false;search = true })
                val notificationReason = model.actionReason(target,SessionNotificationPreferences.operation)
                DropdownMenuItem(text = { Column {
                    Text(SessionNotificationPreferences.label(notificationPreference))
                    if(notificationReason.isNotBlank()) Text(notificationReason,style=MaterialTheme.typography.bodySmall)
                } },
                    leadingIcon = { Icon(if(notificationPreference?.muted == true) Icons.Default.NotificationsActive else Icons.Default.NotificationsOff,null) },
                    enabled = notificationReason.isBlank(),
                    onClick = { menu = false;request = SessionNotificationPreferences.operation })
                // The person's per-child question route switch (zerus-sa9qn.33).
                val routingRaw = model.activity ?: session.raw
                if(SessionQuestionRouting.applies(target,routingRaw)) {
                    val routingReason = model.actionReason(target,SessionQuestionRouting.operation)
                    DropdownMenuItem(text = { Column {
                        Text(SessionQuestionRouting.label(routingRaw))
                        if(routingReason.isNotBlank()) Text(routingReason,style=MaterialTheme.typography.bodySmall)
                    } },
                        leadingIcon = { Icon(Icons.Default.QuestionAnswer,null) },
                        enabled = routingReason.isBlank(),
                        onClick = { menu = false;request = SessionQuestionRouting.operation })
                }
                readEvidence?.let { evidence ->
                    val emptyReminder = model.reviewLater(target) && !evidence.authoritative && evidence.loadedIds.isEmpty()
                    DropdownMenuItem(text = { Text(if(emptyReminder) "Clear review reminder" else if(evidence.authoritative) "Mark read on this phone" else "Mark loaded replies read") },
                        onClick = { menu = false;model.markRead(target,evidence) },enabled = model.readPositionReady && (evidence.authoritative || evidence.loadedIds.isNotEmpty() || emptyReminder))
                    DropdownMenuItem(text = { Text("Review later on this phone") },onClick = { menu = false;model.reviewLater(target,evidence) },enabled = model.readPositionReady)
                    HorizontalDivider()
                }
                DropdownMenuItem(text = { Text("Copy session name") },onClick = { menu = false;clipboard.setText(AnnotatedString(target.session)) })
                DropdownMenuItem(text = { Column {
                    Text("Copy session reference")
                    if (sessionReference.reason.isNotBlank()) Text(sessionReference.reason,style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                } },enabled = sessionReference.uri != null,
                    onClick = { menu = false;sessionReference.uri?.let { clipboard.setText(AnnotatedString(it)) } })
                val folder = model.activity?.string("cwd").orEmpty()
                if(folder.isNotBlank()) DropdownMenuItem(text = { Text("Copy folder") },onClick = { menu = false;clipboard.setText(AnnotatedString(folder)) })
                if(target.agentId.isBlank() && target.archiveId.isBlank()) {
                    val reason=model.projectAssignmentReason(target)
                    DropdownMenuItem(text={ Column { Text("Move to project…");if(reason.isNotBlank()) Text(reason,style=MaterialTheme.typography.bodySmall) } },
                        leadingIcon={ Icon(Icons.Default.Folder,null) },enabled=reason.isBlank(),onClick={ menu=false;projectPicker=true })
                }
                if (target.agentId.isBlank()) {
                    HorizontalDivider()
                    val operations = if (target.archiveId.isNotBlank()) listOf("restore","rename","fork","forget")
                        else listOf("pause","resume","archive","rename","fork","terminate","forget")
                    operations.forEach { operation ->
                        val reason = model.actionReason(target,operation).ifBlank { SessionControlPresentation.reason(target,operation,model.activity) }
                        DropdownMenuItem(text = { Column {
                            Text(actionLabel(operation,target))
                            if (reason.isNotBlank()) Text(reason, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        } }, onClick = {
                            menu = false
                            if (operation in listOf("rename","fork","terminate","forget")) request = operation
                            else { model.flushDrafts(); model.sessionAction(target,operation) }
                        }, enabled = reason.isBlank())
                    }
                }
            }
        }
    }
    model.pendingActionResult(target)?.let { action ->
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp),verticalAlignment = Alignment.CenterVertically) {
            Text(model.actionResultNotice.ifBlank { "The machine confirmed this action. Its destination is available for verification." },Modifier.weight(1f),style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { model.openSessionActionResult(action) }) { Text("Open confirmed result") }
        }
    }
    NativeRecoveryCard(model,target)
    if(projectPicker) SessionProjectDialog(model.sessionProjects,target) { projectPicker=false }
    if(search) ConversationSearchDialog(model,session,onJump) { search = false }
    if(terminal) {
        val reason = model.terminalOpenReason(target)
        if(reason.isBlank()) TerminalDialog(model,session) { closeTerminal() }
        else ZerusAlertDialog(onDismissRequest = { closeTerminal() },
            title = { Text(if(model.terminalAvailabilityChecking(target)) "Checking Terminal availability" else "Terminal unavailable") },text = { Text(reason) },
            confirmButton = { if(!model.terminalAvailabilityChecking(target)) TextButton(onClick = { model.prepareTerminal(target) }) { Text("Refresh") } },
            dismissButton = { TextButton(onClick = { closeTerminal() }) { Text("Close") } })
    }
    if (details) SessionDetailsSheet(model,session) { details = false }
    if (tasksOpen) TasksScreen(model)
    if (settingsOpen) NativeSettingsDialog(model,session) { settingsOpen = false }
    request?.let { operation -> SessionActionDialog(model,session,operation) { request = null } }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable internal fun SessionDetailsSheet(model: ZerusViewModel, session: Session, onDismiss: () -> Unit) {
    val target = session.target
    val raw = model.activity
    val verified = model.activityVerified
    val currentAgents = SessionFilters.currentActivity(session, SessionFilters.online(session, model.machines))
    // Polls replace the activity record; keep showing this target's previous view
    // while the next one is parsed so the sheet never collapses to a placeholder.
    var parsed by remember(target.key) { mutableStateOf<SessionDetailsView?>(null) }
    LaunchedEffect(raw,target,verified) { parsed = withContext(Dispatchers.Default) { SessionDetailsPresentation.parse(raw,target,verified) } }
    val details = parsed
    var tab by remember(target.key) { mutableIntStateOf(0) }
    var stop by remember(target.key) { mutableStateOf<Pair<DetailProcess,String>?>(null) }
    var showOutput by remember(target.key) { mutableStateOf<DetailProcess?>(null) }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = TasksColors.Sheet, dragHandle = { BottomSheetDefaults.DragHandle(color = TasksColors.Handle) }) {
        // A fixed height keeps the sheet still when tabs or polled content change size.
        Column(Modifier.fillMaxWidth().fillMaxHeight(.88f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(Modifier.padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("Session details", Modifier.weight(1f), style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                if (details?.recorded == true) Text("Last known / Read-only", style = MaterialTheme.typography.labelMedium)
            }
            PrimaryScrollableTabRow(selectedTabIndex = tab, edgePadding = 0.dp) {
                listOf("Overview","Plan","Agents","Processes").forEachIndexed { index,label -> Tab(tab == index,{ tab = index },text = { Text(label, maxLines = 1, softWrap = false) }) }
            }
            Column(Modifier.fillMaxWidth().weight(1f).verticalScroll(key(tab) { rememberScrollState() }).padding(horizontal = 20.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (details == null) Text("Reading native details…")
                else when(tab) {
                    0 -> {
                        DetailFields(details.fields, copyIdentities = true)
                        HorizontalDivider()
                        DetailFields(details.goal)
                        Text(details.goalNotice, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        NativeActionRows(model,target)
                    }
                    1 -> {
                        if (details.tasks.isEmpty()) Text("No plan reported yet. It appears when the agent updates its plan.")
                        details.tasks.forEach { list ->
                            Text(list.label, fontWeight = FontWeight.SemiBold)
                            Text("${list.tasks.count { it.status == "completed" }} of ${list.tasks.size} completed" + if (list.recorded) " / Last recorded" else "",
                                style = MaterialTheme.typography.labelMedium)
                            if (list.tasks.isEmpty()) Text("The agent cleared this list.")
                            list.tasks.forEach { task -> SelectionContainer { Column {
                                Text(task.title)
                                Text(SessionDetailsPresentation.taskStatus(task.status) + task.owner.takeIf { it.isNotBlank() }?.let { " / $it" }.orEmpty(),
                                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            } } }
                            if (list.truncated) Text("Showing the first 100 tasks.", style = MaterialTheme.typography.bodySmall)
                            HorizontalDivider()
                        }
                    }
                    2 -> {
                        if (details.agentNotice.isNotBlank()) Text(details.agentNotice, style = MaterialTheme.typography.bodySmall)
                        if (details.agents.isEmpty()) Text("No subagent activity reported.")
                        details.agents.forEach { agent ->
                            Text(agent.label, fontWeight = FontWeight.SemiBold)
                            Text(SessionDetailsPresentation.agentStatus(agent.state, agent.recorded || !currentAgents),
                                style = MaterialTheme.typography.labelMedium)
                            if (agent.group && agent.active != null && agent.total != null) Text("${SessionDetailsPresentation.agentActive(agent, currentAgents)} active / ${agent.total} total", style = MaterialTheme.typography.bodySmall)
                            SelectionContainer { Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                if (agent.task.isNotBlank()) Text(agent.task)
                                if (agent.preview.isNotBlank()) Text(agent.preview, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            } }
                            if (!agent.group) {
                                val reason = model.agentInspectReason(target,agent.id)
                                TextButton(onClick = { onDismiss(); model.inspectAgent(target,agent.id) }, enabled = reason.isBlank()) { Text("Open agent activity") }
                                if(reason.isNotBlank()) Text(reason,style = MaterialTheme.typography.bodySmall)
                            }
                            HorizontalDivider()
                        }
                    }
                    3 -> {
                        if (details.processes.isEmpty()) Text("No native processes reported.")
                        if (!details.processComplete) Text("Process inventory is incomplete. Missing entries are not proof that a process stopped.", style = MaterialTheme.typography.bodySmall)
                        details.processNotes.forEach { note -> SelectionContainer { Text(note, style = MaterialTheme.typography.bodySmall) } }
                        details.processes.forEach { process ->
                            SelectionContainer { Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text(process.command.ifBlank { process.description.ifBlank { "Native process" } }, fontWeight = FontWeight.SemiBold)
                                if (process.description.isNotBlank() && process.command.isNotBlank()) Text(process.description)
                                Text(process.status + if (process.stale) " / Last recorded" else "", style = MaterialTheme.typography.labelMedium)
                                if (process.directory.isNotBlank()) Text(process.directory, style = MaterialTheme.typography.bodySmall)
                            } }
                            val outputReason = model.processOutputReason(target,process.id)
                            val args = JSONObject().put("process_id",process.id).also { if(details.generation.isNotBlank()) it.put("generation",details.generation) }
                            val reason = model.actionReason(target,"process_stop").ifBlank {
                                if(!process.stop || process.stale) "This recorded process cannot be stopped here."
                                else SessionActionPolicies.inspectReason(target,"process_stop",args,raw ?: JSONObject())
                            }
                            Row {
                                TextButton(onClick = { showOutput = process; model.processOutput(target,process.id,details.generation.takeIf { it.isNotBlank() }) }, enabled = outputReason.isBlank()) { Text("Output") }
                                TextButton(onClick = { stop = process to details.generation }, enabled = reason.isBlank()) { Text("Stop process") }
                            }
                            if(outputReason.isNotBlank()) Text(outputReason,style = MaterialTheme.typography.bodySmall)
                            if(reason.isNotBlank()) Text(reason,style = MaterialTheme.typography.bodySmall)
                            HorizontalDivider()
                        }
                    }
                }
                Spacer(Modifier.height(20.dp))
            }
        }
    }
    stop?.let { (process,generation) -> ProcessStopDialog(model,target,process,generation) { stop = null } }
    showOutput?.let { process -> ProcessOutputDialog(model,target,process) { showOutput = null } }
}

@Composable private fun DetailFields(fields: List<ContextDetail>, copyIdentities: Boolean = false) {
    val clipboard = LocalClipboardManager.current
    SelectionContainer { Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        fields.forEach { field -> Column {
            Text(field.label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (copyIdentities && field.label in listOf("Session ID", "Run ID", "Agent ID", "Archive ID")) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(field.value, Modifier.weight(1f))
                    IconButton(onClick = { clipboard.setText(AnnotatedString(field.value)) }) {
                        Icon(Icons.Default.ContentCopy, "Copy ${field.label.removeSuffix(" ID").lowercase()} ID")
                    }
                }
            } else Text(field.value)
        } }
    } }
}

@Composable private fun NativeActionRows(model: ZerusViewModel, target: Target) {
    model.sessionActions.filter { it.target == target && (it.status in listOf("sending","uncertain","failed","scheduled") || it.status == "completed" && it.resultTarget != null && it.operation in listOf("fork","restore","rename","resume")) }.forEach { action ->
        Text(actionLabel(action.operation) + " / " + sessionActionStatus(action), style = MaterialTheme.typography.labelMedium)
        if(action.status == "completed" && action.resultTarget != null) TextButton(onClick = { model.openSessionActionResult(action) }) { Text("Open confirmed result") }
        if (action.error.isNotBlank()) Text(action.error, style = MaterialTheme.typography.bodySmall)
        if(action.status == "uncertain") Row {
            TextButton(onClick = { model.checkSessionAction(action) }) { Text("Check original receipt") }
            var review by remember(action.requestId) { mutableStateOf(false) }
            TextButton(onClick = { review = true }) { Text("Review") }
            if(review) ZerusAlertDialog(onDismissRequest = { review = false }, title = { Text("Review ${actionLabel(action.operation).lowercase()}?") },
                text = { Text("This action may already have changed the machine. Marking it reviewed does not undo or repeat it. Check the original receipt and native state first.") },
                confirmButton = { TextButton(onClick = { model.reviewSessionAction(action); review = false }) { Text("Mark reviewed") } },
                dismissButton = { TextButton(onClick = { review = false }) { Text("Cancel") } })
        }
    }
}

@Composable internal fun SessionActionRecoveryCard(model: ZerusViewModel, action: SessionAction) {
    var review by remember(action.requestId) { mutableStateOf(false) }
    Card { Column(Modifier.fillMaxWidth().padding(16.dp),verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(sessionActionTitle(action),fontWeight = FontWeight.SemiBold)
        Text(if(action.needsProjectReview) "Session created / Project assignment needs review" else actionLabel(action.operation) + " / " + sessionActionStatus(action),style = MaterialTheme.typography.labelMedium)
        if(action.error.isNotBlank()) Text(action.error,style = MaterialTheme.typography.bodySmall)
        Text("The original request is saved. It is never repeated automatically.",style = MaterialTheme.typography.bodySmall)
        Row {
            if(action.needsProjectReview) {
                TextButton(onClick={ model.openSessionActionResult(action) },enabled=model.connections.any { it.id==action.target.connectionId }) { Text("Open created session") }
                TextButton(onClick={ review=true }) { Text("Mark reviewed") }
            }
            model.sessions.find { it.target == action.target }?.let { session -> TextButton(onClick = { model.open(session) }) { Text("Open") } }
            if(action.status == "uncertain") {
                TextButton(onClick = { model.checkSessionAction(action) },enabled = model.connections.any { it.id == action.target.connectionId }) { Text("Check original receipt") }
                TextButton(onClick = { review = true }) { Text("Review") }
            }
        }
    } }
    if(review) ZerusAlertDialog(onDismissRequest = { review = false },title = { Text("Review native action?") },
        text = { Text(if(action.needsProjectReview) "${action.resultTarget?.session}\nThe session was created. Review its project placement before dismissing this notice. Marking reviewed never creates another session."
            else "${sessionActionTitle(action)}\nThis action may already have changed the machine. Check its original receipt and native state first. Marking it reviewed does not undo or repeat it.") },
        confirmButton = { TextButton(onClick = { model.reviewSessionAction(action); review = false }) { Text("Mark reviewed") } },
        dismissButton = { TextButton(onClick = { review = false }) { Text("Cancel") } })
}

/** Exact-identity confirmation; its action stays disabled until a fresh inspection of this session permits it. */
@Composable internal fun SessionActionDialog(model: ZerusViewModel, session: Session, operation: String, onDismiss: () -> Unit) {
    val target = session.target
    var tag by remember(target,operation) { mutableStateOf(if(operation == "rename" && target.session.split('/').size == 3) target.session.substringAfterLast('/') else "") }
    var notificationPreference by remember(target,operation) { mutableStateOf<SessionNotificationPreference?>(null) }
    LaunchedEffect(model.activity,model.activityVerified,target,operation) {
        if(notificationPreference == null && model.activityVerified) notificationPreference = SessionNotificationPreferences.parse(model.activity,target)
    }
    var questionRouting by remember(target,operation) { mutableStateOf<QuestionRouting?>(null) }
    var routingRaw by remember(target,operation) { mutableStateOf<JSONObject?>(null) }
    LaunchedEffect(model.activity,model.activityVerified,target,operation) {
        if(questionRouting == null && model.activityVerified) { questionRouting = SessionQuestionRouting.parse(model.activity);routingRaw = model.activity }
    }
    val label = if(operation == SessionNotificationPreferences.operation) SessionNotificationPreferences.label(notificationPreference)
        else if(operation == SessionQuestionRouting.operation) SessionQuestionRouting.label(routingRaw ?: session.raw) else actionLabel(operation,target)
    val computer = remember(target) { model.machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.name ?: "selected machine" }
    val args = when(operation) {
        SessionNotificationPreferences.operation -> JSONObject().put("muted",notificationPreference?.muted != true).put("expected_revision",notificationPreference?.revision ?: -1)
        SessionQuestionRouting.operation -> SessionQuestionRouting.toggle(questionRouting)
        "rename" -> JSONObject().put("new_name",target.session.split('/').take(2).joinToString("/") + "/" + tag)
        "fork" -> JSONObject().put("tag",tag)
        else -> JSONObject()
    }
    val reason = model.actionReason(target,operation).ifBlank { SessionControlPresentation.reason(target,operation,model.activity) }.ifBlank {
        runCatching { SessionActionPolicies.arguments(operation,args); SessionActionPolicies.inspectReason(target,operation,args,model.activity ?: JSONObject()) }.getOrElse { it.message ?: "Enter a valid name." }
    }
    ZerusAlertDialog(onDismissRequest = onDismiss, title = { Text(if(operation == SessionNotificationPreferences.operation || operation == SessionQuestionRouting.operation) label + " / " + session.title else label + " " + session.title + "?") },
        text = { Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("${target.session} on $computer")
            when(operation) {
                SessionNotificationPreferences.operation -> Text("Applies to this conversation on all devices. Unread replies and pending requests stay unchanged. Unmuting does not replay earlier notifications.")
                SessionQuestionRouting.operation -> Text("Applies to this child session on all devices. A blocking question is sent once to its parent session, which can answer it or ask you. Turning this off never cancels or resends a question already sent.")
                "pause" -> Text("Pause keeps the exact native conversation for Resume. Open network connections, child processes and unsent terminal input are not kept.")
                "resume" -> Text("Start the agent again with its saved native conversation.")
                "archive" -> Text("Move this saved session to Archive. Your private drafts stay on this phone.")
                "restore" -> Text("Start the agent again with this archived conversation. The archive is removed only after the agent confirms that conversation.")
                "terminate" -> Text("The agent and its running processes will stop. Native conversation history is kept where supported. Your private drafts stay on this phone.")
                "forget" -> Text(if(target.archiveId.isNotBlank()) "Remove this selected archived version from Zerus on this machine. Your private drafts stay on this phone."
                    else "Stop this session and its running processes, then remove its Zerus record without keeping a new Zerus archive. Native provider history may remain. Your private drafts stay on this phone.")
                "rename" -> OutlinedTextField(tag,{ tag = it },label = { Text("Session tag") },singleLine = true)
                "fork" -> { Text("Create a separate native conversation. Your current draft is kept without sending."); OutlinedTextField(tag,{ tag = it },label = { Text("New session tag") },singleLine = true) }
            }
            if(reason.isNotBlank()) Text(reason, style = MaterialTheme.typography.bodySmall)
        } },
        confirmButton = { TextButton(onClick = { model.flushDrafts(); model.sessionAction(target,operation,args); onDismiss() }, enabled = reason.isBlank()) { Text(label) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
}

@Composable private fun ProcessStopDialog(model: ZerusViewModel, target: Target, process: DetailProcess, generation: String, onDismiss: () -> Unit) {
    val computer = remember(target) { model.machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.name ?: "selected machine" }
    val args = JSONObject().put("process_id",process.id).also { if(generation.isNotBlank()) it.put("generation",generation) }
    val reason = model.actionReason(target,"process_stop").ifBlank { SessionActionPolicies.inspectReason(target,"process_stop",args,model.activity ?: JSONObject()) }
    ZerusAlertDialog(onDismissRequest = onDismiss,title = { Text("Stop this process?") }, text = { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("${target.session} on $computer")
        SelectionContainer { Text(process.command.ifBlank { process.description }) }
        Text("Only the selected native process is targeted. The stop must be confirmed by the machine.")
        if(reason.isNotBlank()) Text(reason,style = MaterialTheme.typography.bodySmall)
    } },confirmButton = { TextButton(onClick = { model.processStop(target,process.id,generation.takeIf { it.isNotBlank() }); onDismiss() },enabled = reason.isBlank()) { Text("Stop process") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
}

@Composable private fun ProcessOutputDialog(model: ZerusViewModel, target: Target, process: DetailProcess, onDismiss: () -> Unit) {
    val result = model.processOutputResult.takeIf { value -> value != null && model.processOutputId == process.id && value.string("name") == target.session &&
        value.string("run_id") == target.run && value.string("conversation_id") == target.conversation && value.string("archive_id") == target.archiveId }
    ZerusAlertDialog(onDismissRequest = onDismiss,title = { Text("Process output") },text = { Column(Modifier.heightIn(max = 420.dp).verticalScroll(rememberScrollState())) {
        if(model.processOutputLoading) Text("Reading output…")
        if(model.processOutputError.isNotBlank()) Text(model.processOutputError)
        result?.let { SelectionContainer { Text(it.string("output").ifEmpty { "No output reported." }) }
            if(it.optBoolean("output_truncated") || it.optBoolean("truncated")) Text("Output is truncated.",style = MaterialTheme.typography.bodySmall) }
    } },confirmButton = { TextButton(onClick = onDismiss) { Text("Close") } })
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun NativeSettingsDialog(model: ZerusViewModel, session: Session, onDismiss: () -> Unit) {
    val target = session.target
    val snapshot = remember(target) { SessionDetailsPresentation.settings(model.activity) }
    val current = remember(model.activity) { SessionDetailsPresentation.settings(model.activity) }
    var selectedModel by remember(target) { mutableStateOf(snapshot.pendingModel.ifBlank { snapshot.model }) }
    var selectedEffort by remember(target) { mutableStateOf(if(snapshot.pendingId.isNotBlank()) snapshot.pendingEffort else snapshot.effort) }
    val choice = snapshot.models.find { it.id == selectedModel }
    val nativeEfforts = model.nativeEffortsSupported(target,model.activity)
    val efforts = SessionDetailsPresentation.effortChoices(snapshot,selectedModel,nativeEfforts)
    val applyingPending = snapshot.pendingId.isNotBlank() && selectedModel == snapshot.pendingModel && selectedEffort == snapshot.pendingEffort
    val reason = model.actionReason(target,"settings").ifBlank {
        when {
            current.signature != snapshot.signature -> "Native settings changed. Close and reopen this dialog to review them."
            !snapshot.supported -> snapshot.reason.ifBlank { "Native settings are unavailable for this session." }
            choice == null -> "Choose a model from the native catalog."
            selectedEffort.isNotBlank() && selectedEffort !in efforts -> "Choose a supported effort for this model."
            else -> ""
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss,sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        NativeSettingsSheetContent(snapshot,target.session,selectedModel,selectedEffort,reason,
            onModelChange = { id ->
                selectedModel = id
                if(selectedEffort !in SessionDetailsPresentation.effortChoices(snapshot,id,nativeEfforts)) selectedEffort = ""
            },onEffortChange = { selectedEffort = it },
            onApply = { model.updateSettings(target,selectedModel,selectedEffort,if(applyingPending) snapshot.pendingId else "");onDismiss() },
            onDismiss = onDismiss,nativeEfforts = nativeEfforts)
    }
}

internal fun actionLabel(operation: String,target: Target? = null) = when(operation) {
    SessionNotificationPreferences.operation -> "Shared notification settings"
    "pause" -> "Pause"; "resume" -> "Resume"; "archive" -> "Archive"; "rename" -> "Rename"; "fork" -> "Fork"
    "terminate" -> "Terminate"; "restore" -> "Restore"; "forget" -> if(target?.archiveId?.isNotBlank() == true) "Forget archived version" else "Forget session"; "send_now" -> "Send queued message"
    "project_assign" -> "Move to project"; "directory_create" -> "Create folder"; "set_mobile_delivery" -> "Phone notification timing"; "settings" -> "Change settings"; "process_stop" -> "Stop process"; "recovery_action" -> "Recovery action"; else -> "Native action"
}
/** Computer-scoped actions such as set_mobile_delivery have no session, so the operation names the card. */
internal fun sessionActionTitle(action: SessionAction) =
    if(action.needsProjectReview) action.resultTarget?.session.orEmpty() else action.target.session.ifBlank { actionLabel(action.operation) }
internal fun sessionActionStatus(action: SessionAction) = when(action.status) {
    "sending" -> "Sending"; "uncertain" -> "Outcome unknown"; "failed" -> "Not applied"; "scheduled" -> "Pending on machine"
    "applied" -> "Applied"; "completed" -> if(action.operation == "process_stop") "Stop requested" else "Completed"
    "reviewed" -> "Reviewed"; else -> "Status not reported"
}

@Composable internal fun NativeInputQueue(model: ZerusViewModel, target: Target) {
    val queue = model.activity?.optJSONObject("input_queue") ?: return
    val id = queue.string("id")
    if (id.isBlank() || target.agentId.isNotBlank() || target.archiveId.isNotBlank()) return
    val reason = model.actionReason(target,"send_now").ifBlank {
        SessionActionPolicies.inspectReason(target,"send_now",JSONObject().put("queue_id",id),model.activity ?: JSONObject())
    }
    var expanded by remember(target.key, id) { mutableStateOf(false) }
    Card { Column(Modifier.fillMaxWidth()) {
        Surface(onClick = { expanded = !expanded }, color = MaterialTheme.colorScheme.surfaceContainer,
            modifier = Modifier.fillMaxWidth().semantics {
                contentDescription = "Waiting for agent, 1 queued message, ${if (expanded) "collapse" else "expand"} queued message"
            }) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Icon(Icons.Default.Queue, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    Column(Modifier.weight(1f)) {
                        Text("Waiting for agent", style = MaterialTheme.typography.labelLarge)
                        Text("1 queued message", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    Icon(if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore, null)
                }
                if (!expanded) Text(queue.string("text"), maxLines = 2, overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.bodyMedium)
            }
        }
        if (expanded) Column(Modifier.padding(start = 12.dp, end = 12.dp, bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            SelectionContainer { Text(queue.string("text"), Modifier.heightIn(max = 180.dp).verticalScroll(rememberScrollState())) }
            if (queue.string("hint").isNotBlank()) Text(queue.string("hint"), style = MaterialTheme.typography.bodySmall)
            if (reason.isNotBlank()) Text(reason, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onClick = { model.sendNow(target,id) }, enabled = reason.isBlank()) { Text("Send now") }
        }
    } }
}
