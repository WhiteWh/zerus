package app.zerus.mobile

import android.os.SystemClock
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowLeft
import androidx.compose.material.icons.filled.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.input.key.*
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

@Composable internal fun QuestionsPane(model: ZerusViewModel, target: Target, questions: List<Question>, available: Boolean,
    visible: Boolean, viewport: Rect?, maxHeight: Dp, display: QuestionDisplayState,
    onDisplay: ((QuestionDisplayState) -> QuestionDisplayState) -> Unit, onReview: (Draft) -> Unit) {
    val queue = remember(questions, target, display.chosen) { QuestionPolicies.queue(questions, target, display.chosen) }
    @Composable fun InlineCard(question: Question, canAnswer: Boolean) {
        if (!display.matches(question)) NativeQuestionCard(model, target, question, canAnswer, visible, viewport, maxHeight,
            display.page(question), { page -> onDisplay { it.withPage(question, page) } },
            display.offset(question, display.page(question)), { offset -> onDisplay { it.withOffset(question, display.page(question), offset) } },
            false, { onDisplay { it.open(question) } }, {}, onReview)
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        queue.submitted.forEach { question ->
            var show by remember(target.key, question.id, question.hash) { mutableStateOf(false) }
            Text("Answer submitted. Waiting for the agent to record it.", color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { show = !show }) { Text(if (show) "Hide submitted answer" else "Show submitted answer") }
            if (show) InlineCard(question, false)
        }
        queue.selected?.let { question ->
            if (question.optional && queue.optional.size > 1) Row(Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = { onDisplay { it.copy(chosen = QuestionPolicies.key(queue.optional[queue.index - 1])) } }, enabled = queue.index > 0) { Text("Previous") }
                Text("Request ${queue.index + 1}/${queue.optional.size}", style = MaterialTheme.typography.labelMedium)
                TextButton(onClick = { onDisplay { it.copy(chosen = QuestionPolicies.key(queue.optional[queue.index + 1])) } }, enabled = queue.index + 1 < queue.optional.size) { Text("Next") }
            }
            InlineCard(question, available)
        }
    }
}

/** Lives beside the history list so a small viewport or recycled list item cannot dismiss it. */
@Composable internal fun ExpandedQuestionHost(model: ZerusViewModel, target: Target, questions: List<Question>,
    available: Boolean, display: QuestionDisplayState, onDisplay: ((QuestionDisplayState) -> QuestionDisplayState) -> Unit,
    onReview: (Draft) -> Unit) {
    if (!display.expanded) return
    val presence = QuestionPresencePolicies.resolve(display, questions, model.activityVerified, available,
        model.detailBusy, model.questionCheckedAt, SystemClock.elapsedRealtime(),
        complete = model.questionProjectionComplete) ?: return
    val question = display.resolve(questions)
    if (question != null) NativeQuestionCard(model, target, question, available, true, null, 120.dp,
        display.page(question), { page -> onDisplay { it.withPage(question, page) } },
        display.offset(question, display.page(question)), { offset -> onDisplay { it.withOffset(question, display.page(question), offset) } },
        true, {}, { onDisplay { it.close() } }, onReview, presence)
    else QuestionPresenceNotice(presence, onClose = { onDisplay { it.close() } },
        onRefresh = { model.refreshActivity(explicit = true) },
        onReplacement = { replacement -> onDisplay { it.open(replacement) } })
}

/** An obsolete expansion becomes an inline notice: it never obscures the conversation or a delivery review. */
@Composable private fun QuestionPresenceNotice(view: QuestionPresenceView, onClose: () -> Unit, onRefresh: () -> Unit,
    onReplacement: (Question) -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.medium) {
        Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(view.title, style = MaterialTheme.typography.titleSmall)
            Text(view.detail, style = MaterialTheme.typography.bodySmall)
            if (view.age.isNotBlank()) Text(view.age, color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.labelSmall)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (view.refresh) TextButton(onClick = onRefresh) { Text("Refresh") }
                view.replacement?.let { replacement -> TextButton(onClick = { onReplacement(replacement) }) { Text("Open new request") } }
                TextButton(onClick = onClose) { Text("Close") }
            }
        }
    }
}

@Composable private fun NativeQuestionCard(model: ZerusViewModel, target: Target, question: Question, frameAvailable: Boolean,
    visible: Boolean, viewport: Rect?, maxHeight: Dp, page: Int, onPage: (Int) -> Unit, scrollOffset: Int, onScroll: (Int) -> Unit,
    expanded: Boolean, onExpand: () -> Unit, onCollapse: () -> Unit, onReview: (Draft) -> Unit,
    presence: QuestionPresenceView? = null) {
    val bound = QuestionSendPolicies.bound(question, target)
    val available = frameAvailable && bound
    val draft = model.draft(target, question)
    val native = QuestionPolicies.submitted(question, target)
    val saved = remember(draft.answers, native) {
        if (native != null) native.answers.associate { it.questionId to it } else QuestionPolicies.savedAnswers(draft.answers)
    }
    var options by remember(target.key, question.id, question.hash, native, draft.generation) {
        mutableStateOf(QuestionPolicies.initialOptions(question.prompts, saved))
    }
    var texts by remember(target.key, question.id, question.hash, native, draft.generation) {
        mutableStateOf(QuestionPolicies.initialTexts(question.prompts, saved))
    }
    fun answers() = QuestionPolicies.answers(question.prompts, options, texts)
    fun persist() { model.edit(target, texts.values.filter { it.isNotBlank() }.joinToString("\n"), question, answers().toString()) }
    val editing = bound && draft.status == "editing" && native == null && model.pendingQuestionSend == null
    val formReplyText = QuestionPolicies.formReplyText(question)
    val error = QuestionPolicies.answerError(question.prompts, options, texts, formReplyText)
    val complete = QuestionPolicies.complete(question.prompts, options, texts, formReplyText)
    val submitGate = remember(target.key,question.id,question.hash,draft.generation) { QuestionSubmitGate() }
    var submitPending by remember(submitGate) { mutableStateOf(false) }
    val submitScope = rememberCoroutineScope()
    val sendEnabled = question.canAnswer && editing && complete && available && !submitPending
    fun submitAnswer() {
        val allowed = question.canAnswer && editing && available && !submitPending &&
            model.draft(target,question).status == "editing" && QuestionPolicies.complete(question.prompts,options,texts,formReplyText)
        if(!submitGate.claim(allowed)) return
        submitPending = true
        persist()
        val job = model.send(target,question,answers())
        submitScope.launch { try { job.join() } finally { submitGate.finish();submitPending = false } }
    }
    val bodyScroll = rememberSaveable(target.key, question.id, question.hash, page,
        saver = androidx.compose.foundation.ScrollState.Saver) { androidx.compose.foundation.ScrollState(scrollOffset) }
    LaunchedEffect(bodyScroll) { snapshotFlow { bodyScroll.value }.collect { onScroll(it) } }
    fun checkpoint() { onScroll(bodyScroll.value) }
    @Composable fun Content(inDialog: Boolean, contentLimit: Dp) {
    val focusManager = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    fun navigate(next: Int) {
        checkpoint(); focusManager.clearFocus(); keyboard?.hide(); onPage(next)
    }
    val approvalSupported = QuestionPolicies.approvalSupported(question)
    val permission = QuestionPolicies.openCodePermission(question)
    val choices = QuestionPolicies.nativeChoices(question)
    var alwaysAllow by remember(target.key, question.id, question.hash) { mutableStateOf(AlwaysAllowConfirmation()) }
    var broader by remember(target.key, question.id, question.hash) { mutableStateOf(BroaderChoiceConfirmation()) }
    val window = LocalWindowInfo.current
    val view = LocalView.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var foreground by remember(lifecycle) { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) }
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, _ -> foreground = lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED) }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    var disclosureInWindow by remember(target.key, question.id, question.hash) { mutableStateOf(false) }
    val reviewEligible = (inDialog || visible && LocalConversationObscuration.current?.obscured != true) && disclosureInWindow && foreground && window.isWindowFocused && available && editing && question.canAnswer && (approvalSupported || permission || choices)
    var review by remember(target.key, question.id, question.hash) { mutableStateOf(ApprovalReview()) }
    var reviewNow by remember(target.key, question.id, question.hash) { mutableLongStateOf(SystemClock.elapsedRealtime()) }
    LaunchedEffect(reviewEligible, question.approval) {
        reviewNow = SystemClock.elapsedRealtime(); review = review.observe(reviewEligible, reviewNow)
        if (question.approval && reviewEligible) while (true) {
            delay(100); reviewNow = SystemClock.elapsedRealtime(); review = review.observe(true, reviewNow)
        }
    }
    val remaining = review.secondsRemaining(reviewNow)
    // Every native choice, a rejection included, waits for the review, as on the desktop.
    val reviewed = reviewEligible && remaining == 0
    val step = QuestionSteps.current(question.prompts, page, options, texts, formReplyText)
    val pageIndex = step.page
    fun primaryAction() {
        // BEGIN QUESTION_UI_PROBE entry
        if (QuestionUiProbe.enabled) QuestionUiProbe.entry(question, options, step.nextPage != null, step.canAdvance, editing, submitPending, choices, reviewed, broader.open)
        // END QUESTION_UI_PROBE entry
        if (step.nextPage != null) {
            if (editing && !submitPending && step.canAdvance) {
                persist(); navigate(step.nextPage)
            }
        } else if (!choices) submitAnswer()
        else if (reviewed) {
            // A broader option sends only from its own confirmation.
            val confirmation = broader.request(question, options, true)
            // BEGIN QUESTION_UI_PROBE request
            if (QuestionUiProbe.enabled) QuestionUiProbe.requested(question, options, confirmation)
            // END QUESTION_UI_PROBE request
            if (confirmation.open) broader = confirmation else submitAnswer()
        }
    }
    val primaryEnabled = if (step.nextPage != null) editing && !submitPending && step.canAdvance else sendEnabled && (!choices || reviewed)
    // BEGIN QUESTION_UI_PROBE committed
    if (QuestionUiProbe.enabled) {
        val committedOptions = options
        val committed = QuestionUiProbe.Committed(reviewEligible, remaining, reviewed, primaryEnabled,
            available, editing, submitPending, complete, disclosureInWindow, foreground, window.isWindowFocused,
            step.nextPage != null, step.canAdvance, choices, broader.open, inDialog, visible,
            LocalConversationObscuration.current?.obscured == true, question.canAnswer, approvalSupported || permission || choices,
            review.startedAt)
        SideEffect { QuestionUiProbe.committed(question, committedOptions, committed) }
    }
    // END QUESTION_UI_PROBE committed
    val currentPrompts = question.prompts.getOrNull(pageIndex)?.let(::listOf).orEmpty()
    val otherPrompts = currentPrompts.mapNotNull { prompt -> QuestionPolicies.textField(question, prompt)?.let { prompt to it } }
    val submitVisible = QuestionPolicies.submitVisible(question)
    val lowerActions = question.prompts.size > 1 || question.canSkip || otherPrompts.isEmpty()
    val density = LocalDensity.current
    val readableHeightPx = with(density) { MaterialTheme.typography.bodyLarge.lineHeight.toPx() + 24.dp.toPx() }
    val disclosureVisibility = Modifier.onGloballyPositioned { coordinates ->
        val bounds = coordinates.boundsInWindow()
        val display = android.graphics.Rect(); view.getWindowVisibleDisplayFrame(display)
        val screen = IntArray(2); val inWindow = IntArray(2)
        view.getLocationOnScreen(screen); view.getLocationInWindow(inWindow)
        val dx = screen[0] - inWindow[0]; val dy = screen[1] - inWindow[1]
        val visibleTop = maxOf(bounds.top, display.top - dy.toFloat(), if(inDialog) bounds.top else viewport?.top ?: Float.POSITIVE_INFINITY)
        val visibleBottom = minOf(bounds.bottom, display.bottom - dy.toFloat(), if(inDialog) bounds.bottom else viewport?.bottom ?: Float.NEGATIVE_INFINITY)
        disclosureInWindow = visibleBottom - visibleTop >= readableHeightPx && bounds.width > 0 &&
            bounds.right + dx > display.left && bounds.left + dx < display.right &&
            (inDialog || viewport != null && bounds.overlaps(viewport))
    }
    Card(colors = CardDefaults.cardColors(containerColor = Color(0xFF2B2923)), modifier = Modifier.fillMaxWidth().then(if(inDialog) Modifier.fillMaxHeight() else Modifier)) {

        Column(Modifier.then(if(inDialog) Modifier.fillMaxSize() else Modifier.heightIn(max = contentLimit)).padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(if (question.approval || question.trustRequest) "Needs approval" else if (question.optional) "Optional question" else "Needs input",
                    Modifier.weight(1f), color = Color(0xFFFFCB7D), style = MaterialTheme.typography.labelLarge)
                TextButton(onClick = { checkpoint(); if(inDialog) onCollapse() else onExpand() }) { Text(if(inDialog) "Collapse" else "Expand") }
                if (!inDialog && question.createdAt > 0) Text(java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.SHORT, java.text.DateFormat.SHORT)
                    .format(java.util.Date((question.createdAt * 1000).toLong())), color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.labelSmall)
            }
            // Full disclosure scrolls; the answer field and compact controls stay visible.
            // The column measures them first, so the disclosure gets only the height left.
            Box(Modifier.fillMaxWidth().weight(1f, fill = inDialog).then(disclosureVisibility)) {
            Column(Modifier.fillMaxWidth().then(if(inDialog) Modifier.fillMaxSize() else Modifier).verticalScroll(bodyScroll)
                .padding(bottom = 24.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                currentPrompts.forEach { prompt ->
                    SelectionContainer { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (prompt.header.isNotBlank()) Text(prompt.header, style = MaterialTheme.typography.labelLarge)
                        if(!QuestionPolicies.authoredMarkdown(question)) Text(prompt.text, fontWeight = FontWeight.SemiBold)
                        else ProvideTextStyle(LocalTextStyle.current.copy(fontWeight=FontWeight.SemiBold)) { MarkdownText(prompt.text) }
                        if (prompt.body.isNotBlank()) {
                            if(!QuestionPolicies.authoredMarkdown(question)) Text(prompt.body, fontFamily=FontFamily.Monospace)
                            else MarkdownText(prompt.body)
                        }
                    } }
                    if (prompt.otherDescription.isNotBlank()) {
                        if(!QuestionPolicies.authoredMarkdown(question)) SelectionContainer { Text(prompt.otherDescription, style=MaterialTheme.typography.bodySmall) }
                        else ProvideTextStyle(MaterialTheme.typography.bodySmall) { MarkdownText(prompt.otherDescription) }
                    }
                    prompt.choices.forEach { choice ->
                        if (question.approval && !choices) {
                            SelectionContainer { Text(choice.label, style = MaterialTheme.typography.bodySmall) }
                            if (choice.description.isNotBlank()) SelectionContainer { Text(choice.description, style = MaterialTheme.typography.bodySmall) }
                        } else QuestionOption(if (choice.scope != "broader") choice else choice.copy(description = listOf(choice.description,
                            "Applies beyond this request. Asks for confirmation.").filter { it.isNotBlank() }.joinToString("\n")),
                            prompt.multi, choice.id in options[prompt.id].orEmpty(), QuestionPolicies.inputEnabled(question, editing),markdown=QuestionPolicies.authoredMarkdown(question)) {
                            val selected = options[prompt.id].orEmpty()
                            options = options + (prompt.id to if (prompt.multi) {
                                if (choice.id in selected) selected - choice.id else selected + choice.id
                            } else setOf(choice.id))
                            if (!prompt.multi) texts = texts + (prompt.id to "")
                            persist()
                        }
                    }
                }
            }
            }
            // Reserve a sibling row: Surface also blocks touches behind its bounds.
            if (bodyScroll.canScrollForward) Surface(color = Color(0xFF2B2923), modifier = Modifier.align(Alignment.CenterHorizontally)) {
                Text("Scroll for options ↓", Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                    color = Color(0xFFFFCB7D), style = MaterialTheme.typography.labelSmall)
            }
            otherPrompts.forEachIndexed { index,(prompt,field) ->
                var focused by remember(target.key,question.id,question.hash,prompt.id) { mutableStateOf(false) }
                Row(Modifier.fillMaxWidth(),horizontalArrangement = Arrangement.spacedBy(8.dp),verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(texts[prompt.id].orEmpty(), { typed ->
                    val value = QuestionPolicies.limitText(typed, field.maxLength)
                    texts = texts + (prompt.id to value)
                    if (!prompt.multi && value.isNotBlank()) options = options + (prompt.id to emptySet())
                    persist()
                }, Modifier.weight(1f).onFocusChanged { focused = it.isFocused }.onPreviewKeyEvent { event ->
                    if(!focused || event.key !in setOf(Key.Enter,Key.NumPadEnter)) false
                    else {
                        if(event.type == KeyEventType.KeyDown && event.nativeKeyEvent.repeatCount == 0 &&
                            !event.isShiftPressed && !event.isCtrlPressed && !event.isAltPressed && !event.isMetaPressed) primaryAction()
                        true // Consume key up/repeats too; the field must not generate a second IME action.
                    }
                }, label = { Text(field.label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    supportingText = if (field.maxLength != null) { { Text("${QuestionPolicies.characters(texts[prompt.id].orEmpty())}/${field.maxLength}") } } else null,
                    singleLine = true, readOnly = !QuestionPolicies.inputEnabled(question, editing),
                    keyboardOptions = KeyboardOptions.Default.copy(imeAction = if(step.nextPage != null) ImeAction.Next else ImeAction.Send),
                    keyboardActions = KeyboardActions(onNext = { if(focused) primaryAction() }, onSend = { if(focused) primaryAction() }))
                if(index == otherPrompts.lastIndex && submitVisible) QuestionActionButton(primaryEnabled,step,::primaryAction)
                }
            }
            if (native != null) {
                Text("Answer submitted. Waiting for the agent to record it.", style = MaterialTheme.typography.bodySmall)
                native.answers.filter { it.skip }.forEach { Text("Skipped", style = MaterialTheme.typography.bodySmall) }
            } else if (model.answerStatus(draft) != "editing") {
                val status = model.answerStatus(draft)
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if(status == "submitting") CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                    Text(when(status) { "submitting" -> "Sending answer…"; "submitted" -> "Answer submitted. Waiting for the agent."; "failed" -> "Not sent. Your answer is kept."; else -> "Delivery unknown. Your answer may already have reached the agent." }, color = Color(0xFFFFCB7D), style = MaterialTheme.typography.bodySmall)
                }
                if(status in setOf("uncertain", "failed")) Row {
                    if(draft.requestId.isNotBlank()) TextButton(onClick = { model.checkReceipt(draft) }, enabled = draft.key !in model.receiptFlights) { Text("Check delivery") }
                    TextButton(onClick = { onReview(draft) }, enabled = draft.key !in model.receiptFlights) { Text("Review") }
                }
            }
            if (error.isNotBlank()) Text(error, color = Color(0xFFFFCB7D), style = MaterialTheme.typography.bodySmall)
            val sendNotice = model.questionSendNotice(target, question)
            if (sendNotice.isNotBlank()) Text(sendNotice, color = Color(0xFFFFCB7D), style = MaterialTheme.typography.bodySmall)
            fun submit(option: String) { model.send(target, question, QuestionPolicies.approvalAnswer(question, option)) }
            // Outside the permission branch: a card that stops qualifying also drops an open confirmation.
            AlwaysAllowDialog(alwaysAllow, question, question.canAnswer && editing && available && foreground, { alwaysAllow = it }) { model.send(target, question, it) }
            BroaderChoiceDialog(broader, question, options, question.canAnswer && editing && available && foreground, { broader = it }) { submitAnswer() }
            if (permission) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    QuestionPolicies.openCodePermissionActions(question).forEach { action ->
                        val onClick = { if (action.confirmation) alwaysAllow = alwaysAllow.request(question, reviewed) else submit(action.option) }
                        val enabled = action.enabled(question.canAnswer && editing && available, reviewed)
                        if (action == OpenCodePermissionAction.AllowOnce) Button(onClick = onClick, enabled = enabled) { Text(action.label(remaining)) }
                        else OutlinedButton(onClick = onClick, enabled = enabled) { Text(action.label(remaining)) }
                    }
                }
                Text("Reject rejects every pending request in this session.", style = MaterialTheme.typography.bodySmall)
            } else if (question.approval && !choices) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { submit("deny") }, enabled = approvalSupported && question.canAnswer && editing && available) { Text("Deny") }
                    Button(onClick = { submit("allow") }, enabled = reviewEligible && remaining == 0) { Text(if (remaining > 0) "Approve once ($remaining)" else "Approve once") }
                }
                Text("Approval applies only to this command. Review it before continuing.", style = MaterialTheme.typography.bodySmall)
            } else if(lowerActions) Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                if (question.prompts.size > 1) Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                    IconButton(onClick = { navigate(pageIndex - 1) }, enabled = pageIndex > 0, modifier = Modifier.size(48.dp)) {
                        Icon(Icons.Default.KeyboardArrowLeft, "Previous part")
                    }
                    Text("Question ${pageIndex + 1}/${question.prompts.size}", Modifier.weight(1f), style = MaterialTheme.typography.labelMedium)
                    IconButton(onClick = { navigate(pageIndex + 1) }, enabled = pageIndex + 1 < question.prompts.size, modifier = Modifier.size(48.dp)) {
                        Icon(Icons.Default.KeyboardArrowRight, "Next part")
                    }
                }
                if(question.canSkip || otherPrompts.isEmpty() && submitVisible) Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                Spacer(Modifier.weight(1f))
                if (question.canSkip) TextButton(onClick = { model.send(target, question, JSONArray(question.prompts.map {
                    JSONObject().put("question_id", it.id).put("skip", true).put("selected_option_ids", JSONArray()).put("text", "")
                })) }, enabled = editing && available, contentPadding = PaddingValues(horizontal = 8.dp)) { Text("Skip") }
                if(otherPrompts.isEmpty() && submitVisible) QuestionActionButton(primaryEnabled,step,::primaryAction)
                }
            }
            if (choices && question.canAnswer && available) Text(if (remaining > 0) "Review the request, then choose one of the agent's options ($remaining)."
                else "Choose one of the agent's options.", style = MaterialTheme.typography.bodySmall)
            if (!available) Text("Refresh to verify this request. Your answer is kept.", style = MaterialTheme.typography.bodySmall)
            else if (!question.canAnswer) SelectionContainer { Text(presence?.detail?.takeIf { it.isNotBlank() }
                ?: QuestionPolicies.unavailableNotice(question), style = MaterialTheme.typography.bodySmall) }
            presence?.age?.takeIf { it.isNotBlank() }?.let { Text(it, style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant) }
            if (question.approval && !approvalSupported && !permission && !choices) Text("This approval cannot be answered here. Continue on your machine.", style = MaterialTheme.typography.bodySmall)
        }
    }
    }
    if(expanded) {
        ObscureConversation()
        Dialog(onDismissRequest = { checkpoint(); onCollapse() }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            Surface(Modifier.fillMaxSize().imePadding().padding(8.dp), color = MaterialTheme.colorScheme.background) {
                BoxWithConstraints(Modifier.fillMaxSize()) { Content(true, this.maxHeight) }
            }
        }
    } else Content(false, maxHeight)
}

@Composable private fun QuestionActionButton(enabled: Boolean,step: QuestionStep,onClick: () -> Unit) {
    val label = if(step.reviewing) "Review answers" else if(step.nextPage != null) "Next" else if(step.count > 1) "Send all" else "Send"
    val description = if(step.reviewing) "Review unanswered questions" else if(step.nextPage != null) "Next question" else if(step.count > 1) "Send all" else "Send answer"
    Button(onClick = onClick,enabled = enabled,modifier = Modifier.heightIn(min = 48.dp).semantics { contentDescription = description },
        contentPadding = PaddingValues(horizontal = 12.dp),
        colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF67E8CB),contentColor = Color(0xFF101517),
            disabledContainerColor = Color(0xFF2B3B3B),disabledContentColor = Color(0xFF9AACB2))) {
        Text(label)
        Spacer(Modifier.width(6.dp))
        Icon(if(step.nextPage != null) Icons.Default.KeyboardArrowRight else Icons.AutoMirrored.Filled.Send,null,Modifier.size(20.dp))
    }
}

@Composable private fun QuestionOption(choice: Choice, multi: Boolean, selected: Boolean, enabled: Boolean, markdown:Boolean, onClick: () -> Unit) {
    val select = if (multi) Modifier.toggleable(selected, enabled, Role.Checkbox) { onClick() }
        else Modifier.selectable(selected, enabled, Role.RadioButton, onClick)
    Surface(color = MaterialTheme.colorScheme.surface, shape = MaterialTheme.shapes.small,
        modifier = Modifier.fillMaxWidth().semantics(mergeDescendants = true) {}.then(select)) {
        Row(Modifier.padding(10.dp), verticalAlignment = Alignment.CenterVertically) {
            if (multi) Checkbox(selected, null) else RadioButton(selected, null)
            Column(Modifier.padding(start = 8.dp)) {
                Text(choice.label)
                if (choice.description.isNotBlank()) {
                    if(markdown) ProvideTextStyle(MaterialTheme.typography.bodySmall) { MarkdownText(choice.description) }
                    else Text(choice.description,style=MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

/**
 * A broader native option grants more than this request, so it asks first and shows the agent's own description
 * of that grant. It closes without sending when the card changed, another option is chosen, the card can no longer
 * be answered here, or the app left the foreground.
 */
@Composable private fun BroaderChoiceDialog(confirmation: BroaderChoiceConfirmation, question: Question,
    options: Map<String, Set<String>>, eligible: Boolean, onChange: (BroaderChoiceConfirmation) -> Unit, send: () -> Unit) {
    val choice = confirmation.showing(question, options, eligible)
    LaunchedEffect(confirmation, choice) { if (confirmation.open && choice == null) onChange(confirmation.cancel()) }
    // BEGIN QUESTION_UI_PROBE showing
    if (QuestionUiProbe.enabled) SideEffect { QuestionUiProbe.showing(question, options, confirmation, eligible, choice) }
    // END QUESTION_UI_PROBE showing
    if (choice == null) return
    ObscureConversation()
    ZerusAlertDialog(onDismissRequest = { onChange(confirmation.cancel()) },
        title = { Text("Confirm this option?") },
        text = { Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("\u201c${choice.label}\u201d changes the agent's permissions beyond this request.")
            if (choice.description.isNotBlank()) SelectionContainer { Text(choice.description) }
        } },
        confirmButton = { TextButton(onClick = { onChange(confirmation.confirm(question, options, eligible, send)) }) { Text("Confirm") } },
        dismissButton = { TextButton(onClick = { onChange(confirmation.cancel()) }) { Text("Cancel") } })
}

/**
 * Always allow reaches beyond this request, so it asks first and names what it saves. The dialog closes without
 * sending when the card changed, stopped offering Always allow, can no longer be answered here, or the app left
 * the foreground.
 */
@Composable private fun AlwaysAllowDialog(confirmation: AlwaysAllowConfirmation, question: Question, eligible: Boolean,
    onChange: (AlwaysAllowConfirmation) -> Unit, send: (JSONArray) -> Unit) {
    val showing = confirmation.showing(question, eligible)
    LaunchedEffect(confirmation, showing) { if (confirmation.open && !showing) onChange(confirmation.cancel()) }
    if (!showing) return
    ObscureConversation()
    val saved = QuestionPolicies.alwaysAllowDescription(question)
    ZerusAlertDialog(onDismissRequest = { onChange(confirmation.cancel()) },
        title = { Text("Always allow?") },
        text = { Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("OpenCode will allow matching requests without asking again.")
            if (saved.isNotBlank()) Text(saved)
        } },
        confirmButton = { TextButton(onClick = { onChange(confirmation.confirm(question, eligible, send)) }) { Text("Always allow") } },
        dismissButton = { TextButton(onClick = { onChange(confirmation.cancel()) }) { Text("Cancel") } })
}
