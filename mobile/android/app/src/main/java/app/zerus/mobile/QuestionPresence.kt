package app.zerus.mobile

/**
 * The expanded question's verified presence. It describes evidence only and
 * never grants answer authority: a card is answerable solely when the
 * question itself says so and the verified frame is available.
 */
internal enum class QuestionPresence { Answerable, Waiting, Checking, Closed, Replaced, Unavailable }

/** A clear, non-enabling presentation for the expanded question. */
internal data class QuestionPresenceView(val presence: QuestionPresence, val title: String, val detail: String,
    val age: String, val modal: Boolean, val refresh: Boolean = false, val replacement: Question? = null)

internal object QuestionPresencePolicies {
    /** Resolves the expanded identity against the current projection. `null` when nothing is expanded. */
    fun resolve(display: QuestionDisplayState, questions: List<Question>, verified: Boolean, available: Boolean,
        checking: Boolean, checkedAtMillis: Long, nowMillis: Long, complete: Boolean = true): QuestionPresenceView? {
        if (!display.expanded) return null
        val age = age(checkedAtMillis, nowMillis)
        val exact = display.resolve(questions)
        if (exact != null) {
            val answerable = available && exact.canAnswer
            return QuestionPresenceView(if (answerable) QuestionPresence.Answerable else QuestionPresence.Waiting,
                if (answerable) "" else "Waiting for the agent",
                if (answerable) "" else reason(exact.unavailableReason, exact.optional),
                age, modal = true)
        }
        // Only a verified and complete projection proves that this exact
        // request is gone; an incomplete one is still being indexed.
        if (verified && complete) {
            val replacement = questions.firstOrNull { it.id == display.expandedId }
            return if (replacement != null) {
                QuestionPresenceView(QuestionPresence.Replaced, "Request replaced",
                    "The agent replaced this request with a newer one. Your saved answer is kept.", age,
                    modal = false, replacement = replacement)
            } else {
                QuestionPresenceView(QuestionPresence.Closed, "Request closed",
                    "The agent closed this request. Your answer is no longer needed.", age, modal = false)
            }
        }
        if (verified) {
            return QuestionPresenceView(QuestionPresence.Checking, "Checking the conversation",
                "The agent is still indexing this conversation's questions. Refresh to check again. Your saved answer is kept.",
                age, modal = false, refresh = true)
        }
        if (checking) {
            return QuestionPresenceView(QuestionPresence.Checking, "Checking the conversation",
                "Verifying the pending requests. Your saved answer is kept.", age, modal = false)
        }
        return QuestionPresenceView(QuestionPresence.Unavailable, "Request unavailable",
            "This request is not in the verified conversation. Refresh to check again. Your saved answer is kept.",
            age, modal = false, refresh = true)
    }

    /** How old the last verified question projection is; `0` means it was never checked. */
    fun age(checkedAtMillis: Long, nowMillis: Long): String {
        if (checkedAtMillis <= 0) return "Not checked yet"
        val elapsed = (nowMillis - checkedAtMillis).coerceAtLeast(0)
        return "Checked " + when {
            elapsed < 1_000 -> "just now"
            elapsed < 60_000 -> "${elapsed / 1_000}s ago"
            elapsed < 3_600_000 -> "${elapsed / 60_000}m ago"
            elapsed < 86_400_000 -> "${elapsed / 3_600_000}h ago"
            else -> "${elapsed / 86_400_000}d ago"
        }
    }

    /** Maps the native `input::checked_terminal` veto to a clear reason. Raw reasons never reach the person. */
    fun reason(raw: String, optional: Boolean): String {
        val text = raw.trim()
        if (text.isEmpty()) return if (optional)
            "The agent is still working. This optional request stays open until it asks again."
        else "The agent has not opened answers for this request yet."
        return when {
            text.contains("terminal", ignoreCase = true) -> "Answer this request on your computer."
            text.contains("draft", ignoreCase = true) -> "Finish the draft in the composer on your computer."
            text.contains("dialog", ignoreCase = true) -> "Close the open dialog on your computer."
            text.contains("recogni", ignoreCase = true) || text.contains("prompt", ignoreCase = true) ->
                "The agent is not showing an answerable prompt yet."
            text.contains("working", ignoreCase = true) || text.contains("busy", ignoreCase = true) ->
                "The agent is still working. Answers stay disabled until it asks again."
            else -> "Complete this request on your computer."
        }
    }
}
