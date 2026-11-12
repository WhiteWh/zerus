package app.zerus.mobile

import android.widget.Toast
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.CallSplit
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import org.json.JSONObject

/** One long-press action. A non-blank [reason] disables it and is shown as its supporting line. */
data class SessionMenuItem(val id: String, val label: String, val reason: String = "", val hint: String = "") {
    val enabled get() = reason.isBlank()
}
/**
 * Phone-local read facts for one row; [unavailable] is non-blank while read state cannot change.
 * [checking] means the bounded metadata probe can still prepare a fresh head for this row.
 */
data class SessionMenuRead(val unavailable: String = "", val canMark: Boolean = false, val partial: Boolean = false,
    val replies: Boolean = false, val reminder: Boolean = false, val checking: Boolean = false)
/** Exact row facts captured by the view model. [supported] is the gateway and computer operation intersection. */
data class SessionMenuFacts(val target: Target, val row: JSONObject, val state: String, val archived: Boolean, val online: Boolean,
    val supported: Set<String>, val blocked: String = "", val busy: String = "", val needsAttention: Boolean = false,
    val read: SessionMenuRead = SessionMenuRead(), val reference: String? = null, val folder: String = "", val referenceReason: String = "",
    val terminalAccessReason: String? = null, val projectReason: String = "")
/** A native action chosen in Sessions, consumed once by the conversation header it opened. */
data class SessionMenuRequest(val target: Target, val operation: String, val navigation: Long)

/**
 * Sessions long-press menu, mirroring the desktop session context menu where Android has the capability.
 * Native items are presentation gates from the catalog row only: choosing one opens the exact session,
 * whose header confirms it with the existing dialog after a fresh inspection verifies identity and eligibility.
 */
object SessionActionsMenu {
    fun items(facts: SessionMenuFacts): List<List<SessionMenuItem>> {
        val archived = facts.archived || facts.target.archiveId.isNotBlank()
        val running = !archived && facts.state == "running"
        fun native(operation: String, label: String) = SessionMenuItem(operation,label,operationReason(facts,operation))
        return listOf(
            if (archived) emptyList() else listOf(SessionMenuItem("terminal","Open terminal",terminalReason(facts))),
            if (archived || facts.target.agentId.isNotBlank()) emptyList() else listOf(readItem(facts)),
            listOf(SessionMenuItem(SessionNotificationPreferences.operation,
                SessionNotificationPreferences.label(SessionNotificationPreferences.parse(facts.row,facts.target)),notificationReason(facts),"Applies to this conversation on all devices.")),
            // The person's per-child question route switch (zerus-sa9qn.33): live Claude children only.
            if(SessionQuestionRouting.applies(facts.target,facts.row)) listOf(SessionMenuItem(SessionQuestionRouting.operation,
                SessionQuestionRouting.label(facts.row),questionRoutingReason(facts),SessionQuestionRouting.hint)) else emptyList(),
            if(archived || facts.target.agentId.isNotBlank()) emptyList() else listOf(SessionMenuItem(SessionProjectProtocol.assign,"Move to project…",projectReason(facts),"Change project grouping; keep the working folder.")),
            buildList {
                add(if (archived) native("restore","Restore session…") else if (running) native("pause","Pause session…") else native("resume","Resume session…"))
                add(native("rename","Rename…"))
                add(native("fork","Fork session…"))
                if (!archived && facts.state in setOf("paused","stopped")) add(native("archive","Move to archive…"))
            },
            listOf(SessionMenuItem("copy_name","Copy session name",if (facts.target.session.isBlank()) "The session name is not reported." else ""),
                SessionMenuItem("copy_reference","Copy session reference",
                    if (facts.reference == null) facts.referenceReason.ifBlank { "A reference needs the computer's native identity and the session's identity." } else ""),
                SessionMenuItem("copy_folder","Copy folder path",if (facts.folder.isBlank()) "The session folder is not reported." else "")),
            listOf(if (archived) native("forget","Forget archived version…") else if (running) native("terminate","Terminate session…")
                else native("forget","Forget saved session…"))
        ).filter { it.isNotEmpty() }
    }
    fun projectReason(facts:SessionMenuFacts):String = when {
        facts.blocked.isNotBlank() -> facts.blocked
        facts.archived || facts.target.archiveId.isNotBlank() || facts.target.agentId.isNotBlank() -> "Project grouping belongs to the parent session."
        !facts.online -> "The machine is offline."
        !facts.supported.containsAll(SessionProjectProtocol.operations) -> "This gateway or computer does not support project grouping changes."
        facts.projectReason.isNotBlank() -> facts.projectReason
        facts.busy.isNotBlank() -> facts.busy
        else -> ""
    }
    /** Mark as read reuses Read all's evidence rules; otherwise the row offers a phone-local reminder. */
    fun readItem(facts: SessionMenuFacts): SessionMenuItem {
        val read = facts.read
        if (!facts.needsAttention) return SessionMenuItem("review_later","Review later",read.unavailable,"Keep a reminder on this phone to review this session later.")
        val label = if (read.canMark && read.partial) "Mark loaded replies read" else "Mark as read"
        return when {
            read.unavailable.isNotBlank() -> SessionMenuItem("mark_read",label,read.unavailable)
            read.canMark -> SessionMenuItem("mark_read",label,hint = if (read.partial) "Only replies loaded on this phone are marked read." else "")
            read.reminder -> SessionMenuItem("clear_reminder","Clear review reminder",
                hint = if (read.replies) "Replies stay unread until this phone has verified history or loaded replies." else "")
            read.replies -> SessionMenuItem("mark_read",label,if (read.checking) "Waiting for verified history from its machine. Open the session if its replies are not loaded on this phone."
                else "Needs verified history or loaded replies on this phone. Open the session to read them.")
            else -> SessionMenuItem("mark_read",label,"Questions, approvals and errors clear when you open the session. Mark as read covers replies only.")
        }
    }
    fun nativeTerminalReason(target:Target,row:JSONObject?):String = TerminalAvailability.nativeReason(target,row)
    fun terminalReason(facts: SessionMenuFacts): String = when {
        facts.blocked.isNotBlank() -> facts.blocked
        facts.target.agentId.isNotBlank() || facts.archived || facts.target.archiveId.isNotBlank() -> "Terminal is available only for the live parent session."
        !facts.online -> "The machine is offline."
        facts.terminalAccessReason != null -> facts.terminalAccessReason
        "terminal_snapshot" !in facts.supported -> "This native adapter does not provide remote Terminal."
        nativeTerminalReason(facts.target,facts.row).isNotBlank() -> nativeTerminalReason(facts.target,facts.row)
        else -> ""
    }
    fun operationReason(facts: SessionMenuFacts, operation: String): String {
        val target = facts.target
        val archived = facts.archived || target.archiveId.isNotBlank()
        return when {
            operation !in SessionActionPolicies.lifecycle -> "Unsupported session action."
            facts.blocked.isNotBlank() -> facts.blocked
            target.agentId.isNotBlank() -> "Session controls belong to the parent conversation."
            !facts.online -> "The machine is offline."
            operation !in facts.supported -> "This gateway or computer does not support this action."
            archived && operation !in setOf("restore","forget","rename","fork") -> "This archived conversation is read-only."
            !archived && operation == "restore" -> "Select an archived version first."
            facts.busy.isNotBlank() -> facts.busy
            else -> SessionControlPresentation.reason(target,operation,facts.row)
        }
    }
    fun questionRoutingReason(facts: SessionMenuFacts): String = when {
        facts.blocked.isNotBlank() -> facts.blocked
        !SessionQuestionRouting.applies(facts.target,facts.row) || facts.archived -> SessionQuestionRouting.notChild
        !facts.online -> "The machine is offline."
        SessionQuestionRouting.operation !in facts.supported -> SessionQuestionRouting.unavailable
        facts.busy.isNotBlank() -> facts.busy
        else -> SessionQuestionRouting.reason(facts.target,facts.row)
    }
    fun notificationReason(facts: SessionMenuFacts): String = when {
        facts.blocked.isNotBlank() -> facts.blocked
        facts.target.agentId.isNotBlank() || facts.archived || facts.target.archiveId.isNotBlank() -> "Notification controls belong to the live parent conversation."
        !facts.online -> "The machine is offline."
        SessionNotificationPreferences.operation !in facts.supported -> SessionNotificationPreferences.unavailable
        facts.busy.isNotBlank() -> facts.busy
        else -> SessionNotificationPreferences.reason(facts.target,facts.row)
    }
}

/** Tap opens the session; long press (with haptic feedback) or the accessibility long-press action opens its actions. */
internal fun Modifier.sessionCardGestures(shape: Shape, onOpen: () -> Unit, onActions: () -> Unit): Modifier =
    clip(shape).combinedClickable(onClickLabel = "Open session", role = Role.Button, onLongClickLabel = "Session actions",
        onLongClick = onActions, hapticFeedbackEnabled = true, onClick = onOpen)

@OptIn(ExperimentalMaterial3Api::class)
@Composable internal fun SessionActionsSheet(model: ZerusViewModel, captured: Session, onDismiss: () -> Unit) {
    val target = captured.target
    // Actions always use the current catalog row for this exact identity.
    val session = model.sessions.find { it.target == target }
    val context = LocalContext.current
    val clipboard = LocalClipboardManager.current
    // The shared probe scheduler bounds these requests; an already-fresh head is not fetched again.
    LaunchedEffect(target) { while (true) { model.prepareReadEvidence(target); delay(2_000) } }
    val sections = session?.let { model.sessionMenu(it) }.orEmpty()
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().heightIn(max = 640.dp).verticalScroll(rememberScrollState()).padding(bottom = 16.dp)) {
            Text(SessionFilters.label(session ?: captured), Modifier.padding(horizontal = 20.dp), style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val machine = model.machines.find { it.connectionId == target.connectionId && it.id == target.computerId }?.name ?: target.computerId
            Text("${target.session} on $machine", Modifier.padding(horizontal = 20.dp, vertical = 4.dp), style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
            if (session == null) Text("This session is no longer available. Open its actions again.", Modifier.padding(20.dp))
            sections.forEachIndexed { index, items ->
                if (index > 0) HorizontalDivider(Modifier.padding(vertical = 4.dp))
                items.forEach { item -> SessionMenuRow(item) {
                    val current = session ?: return@SessionMenuRow
                    onDismiss()
                    when (item.id) {
                        "mark_read" -> {
                            val result = model.markAllRead(listOf(target))
                            Toast.makeText(context,ConversationLoadPresentation.readAllOutcome(result.marked,result.skipped,result.partial),Toast.LENGTH_SHORT).show()
                        }
                        "clear_reminder" -> model.clearReviewLater(target)
                        "review_later" -> model.reviewLater(target,model.captureReadAttention(target))
                        "copy_name" -> clipboard.setText(AnnotatedString(target.session))
                        "copy_reference" -> SessionReference.resolve(target,model.machines).uri?.let { clipboard.setText(AnnotatedString(it)) }
                        "copy_folder" -> current.raw.string("cwd").takeIf { it.isNotBlank() }?.let { clipboard.setText(AnnotatedString(it)) }
                        else -> model.openWithSessionAction(current,item.id)
                    }
                } }
            }
        }
    }
}

private fun menuIcon(id: String): ImageVector = when (id) {
    SessionNotificationPreferences.operation -> Icons.Default.NotificationsOff
    SessionQuestionRouting.operation -> Icons.Default.QuestionAnswer
    "project_assign" -> Icons.Default.Folder
    "terminal" -> Icons.Default.Terminal
    "mark_read" -> Icons.Default.DoneAll
    "clear_reminder" -> Icons.Default.NotificationsOff
    "review_later" -> Icons.Default.MarkEmailUnread
    "pause" -> Icons.Default.Pause
    "resume" -> Icons.Default.PlayArrow
    "restore" -> Icons.Default.Restore
    "rename" -> Icons.Default.Edit
    "fork" -> Icons.AutoMirrored.Filled.CallSplit
    "archive" -> Icons.Default.Archive
    "copy_name" -> Icons.Default.ContentCopy
    "copy_reference" -> Icons.Default.Link
    "copy_folder" -> Icons.Default.Folder
    "terminate" -> Icons.Default.StopCircle
    else -> Icons.Default.DeleteOutline
}

@Composable private fun SessionMenuRow(item: SessionMenuItem, onClick: () -> Unit) {
    val supporting = item.reason.ifBlank { item.hint }
    val tint = if (item.enabled) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface.copy(alpha = .38f)
    ListItem(headlineContent = { Text(item.label) },
        leadingContent = { Icon(menuIcon(item.id), null, tint = tint) },
        supportingContent = if (supporting.isBlank()) null else { { Text(supporting, style = MaterialTheme.typography.bodySmall) } },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent,
            headlineColor = if (item.enabled) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurface.copy(alpha = .38f)),
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable(enabled = item.enabled, role = Role.Button, onClick = onClick))
}
