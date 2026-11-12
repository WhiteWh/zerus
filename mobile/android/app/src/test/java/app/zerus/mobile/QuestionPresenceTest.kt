package app.zerus.mobile

import org.junit.Assert.*
import org.junit.Test

class QuestionPresenceTest {
    private val target = Target("computer", "session", "run", "conversation", "workspace")
    private val prompt = Prompt("q", "Question", listOf(Choice("first", "First", "")), false, true, false)
    private fun question(id: String = "request", hash: String = "hash-$id", canAnswer: Boolean = true,
        optional: Boolean = false, reason: String = "", at: Double = 1.0) =
        Question(id, hash, listOf(prompt), canAnswer, true, optional, "", createdAt = at,
            source = "codex_async", unavailableReason = reason, run = target.run, conversation = target.conversation)
    private val now = 1_000_000_000L
    private fun resolve(display: QuestionDisplayState, questions: List<Question>, verified: Boolean = true,
        available: Boolean = true, checking: Boolean = false, checkedAt: Long = now - 5_000,
        complete: Boolean = true) =
        QuestionPresencePolicies.resolve(display, questions, verified, available, checking, checkedAt, now, complete)!!

    @Test fun anAgedOptionalRequestStaysPendingAndUnanswerableWhileTheAgentWorks() {
        // An optional request can legitimately stay pending while the agent works:
        // it is Waiting, never Answerable, and shows a clear reason and age.
        val view = resolve(QuestionDisplayState(target.key).open(question(optional = true, canAnswer = false, reason = "")), listOf(question(optional = true, canAnswer = false)))
        assertEquals(QuestionPresence.Waiting, view.presence)
        assertEquals("Waiting for the agent", view.title)
        assertEquals("The agent is still working. This optional request stays open until it asks again.", view.detail)
        assertEquals("Checked 5s ago", view.age)
        assertTrue(view.modal)
        // A raw veto reason never reaches the person; it maps to a clear state.
        val veto = resolve(QuestionDisplayState(target.key).open(question(canAnswer = false, reason = "Open Terminal to answer this question.")),
            listOf(question(canAnswer = false, reason = "Open Terminal to answer this question.")))
        assertEquals(QuestionPresence.Waiting, veto.presence)
        assertEquals("Answer this request on your computer.", veto.detail)
    }

    @Test fun aRefreshRemovalWhileExpandedIsAConfirmedClosure() {
        val request = question()
        val display = QuestionDisplayState(target.key).open(request)
        val closed = resolve(display, emptyList())
        assertEquals(QuestionPresence.Closed, closed.presence)
        assertEquals("Request closed", closed.title)
        assertEquals("Checked 5s ago", closed.age)
        assertFalse(closed.modal)
        // The exact expansion is reconciled by a verified absence, not by age or work state.
        assertNotEquals(QuestionPresence.Answerable, closed.presence)
        assertNull(closed.replacement)
    }

    @Test fun aReplacementKeepsTheSameIdentityWithANewHash() {
        val request = question()
        val replacement = question(hash = "hash-2")
        val view = resolve(QuestionDisplayState(target.key).open(request), listOf(replacement))
        assertEquals(QuestionPresence.Replaced, view.presence)
        assertEquals("Request replaced", view.title)
        assertSame(replacement, view.replacement)
        assertFalse(view.modal)
    }

    @Test fun rotationAndRestoreResolveTheSameExactExpansion() {
        val request = question()
        val display = QuestionDisplayState(target.key).open(request)
        val restored = QuestionDisplayState.decode(display.encode())!!
        assertEquals(display, restored)
        assertEquals(QuestionPresence.Answerable, resolve(restored, listOf(request)).presence)
        // A saved answer's draft is not touched by presence resolution: it is a pure projection.
        assertEquals(display, restored)
    }

    @Test fun anObsoleteExpansionNeverObscuresAnUnrelatedDeliveryReview() {
        // The obsolete states render inline (modal=false), so an unknown outbox
        // Review remains reachable; its unknown/unavailable semantics stay separate.
        val display = QuestionDisplayState(target.key).open(question())
        for (view in listOf(resolve(display, emptyList()), resolve(display, emptyList(), verified = false, checking = true),
            resolve(display, emptyList(), verified = false, checking = false))) {
            assertFalse(view.modal)
        }
        assertEquals(QuestionPresence.Unavailable, resolve(display, emptyList(), verified = false, checking = false).presence)
        assertTrue(resolve(display, emptyList(), verified = false, checking = false).refresh)
        assertEquals(QuestionPresence.Checking, resolve(display, emptyList(), verified = false, checking = true).presence)
        // Neither state is the unknown delivery state: they only describe the question source.
        assertTrue(resolve(display, emptyList(), verified = false, checking = false).detail.contains("verified conversation"))
    }

    @Test fun unavailableAndUnknownStayIndependent() {
        val display = QuestionDisplayState(target.key).open(question())
        val unavailable = resolve(display, emptyList(), verified = false, checking = false)
        assertEquals(QuestionPresence.Unavailable, unavailable.presence)
        assertEquals("Request unavailable", unavailable.title)
        assertEquals("This request is not in the verified conversation. Refresh to check again. Your saved answer is kept.", unavailable.detail)
        assertEquals("Checked 5s ago", unavailable.age)
        // A cached or partial projection never proves closure, and it never enables an answer.
        assertNotEquals(QuestionPresence.Closed, resolve(display, emptyList(), verified = false, checking = true).presence)
    }

    @Test fun anAnswerIsNeverEnabledFromCacheAgeWorkingOrAPartialScanAlone() {
        val request = question()
        val display = QuestionDisplayState(target.key).open(request)
        // Present but the frame is not answerable: Waiting (the card shows refresh, Send stays disabled).
        assertEquals(QuestionPresence.Waiting, resolve(display, listOf(request), available = false).presence)
        // An unverified projection says nothing about absence: Checking or Unavailable, never Closed.
        for (checking in listOf(true, false)) {
            val view = resolve(display, emptyList(), verified = false, checking = checking)
            assertNotEquals(QuestionPresence.Answerable, view.presence)
            assertNotEquals(QuestionPresence.Closed, view.presence)
        }
        assertEquals(QuestionPresence.Answerable, resolve(display, listOf(request)).presence)
    }

    @Test fun anIncompleteVerifiedProjectionIsCheckingNeverClosedOrReplaced() {
        // zerus-a4mub.27: while the native question index is still being
        // built (pending_questions_complete=false) a verified projection
        // proves nothing about absence, so it can never close or replace.
        val display = QuestionDisplayState(target.key).open(question())
        for (questions in listOf(emptyList<Question>(), listOf(question(hash = "hash-2")))) {
            val view = resolve(display, questions, verified = true, complete = false)
            assertEquals(QuestionPresence.Checking, view.presence)
            assertNotEquals(QuestionPresence.Closed, view.presence)
            assertNull(view.replacement)
            assertTrue(view.refresh)
            assertTrue(view.detail.contains("indexing"))
            assertEquals("Checked 5s ago", view.age)
        }
        // A verified and complete projection still closes or replaces, and a
        // missing field (an older CLI) keeps that behaviour.
        assertEquals(QuestionPresence.Closed, resolve(display, emptyList(), verified = true, complete = true).presence)
        assertEquals(QuestionPresence.Closed, resolve(display, emptyList(), verified = true).presence)
        assertEquals(QuestionPresence.Replaced, resolve(display, listOf(question(hash = "hash-2")), verified = true, complete = true).presence)
        // Unverified stays Checking or Unavailable whatever completeness says.
        for (complete in listOf(true, false)) {
            val view = resolve(display, emptyList(), verified = false, complete = complete)
            assertTrue(view.presence in setOf(QuestionPresence.Checking, QuestionPresence.Unavailable))
        }
    }

    @Test fun ageAndReasonsAreBound() {
        assertEquals("Not checked yet", QuestionPresencePolicies.age(0, now))
        assertEquals("Checked just now", QuestionPresencePolicies.age(now - 400, now))
        assertEquals("Checked 59s ago", QuestionPresencePolicies.age(now - 59_000, now))
        assertEquals("Checked 2m ago", QuestionPresencePolicies.age(now - 120_000, now))
        assertEquals("Checked 3h ago", QuestionPresencePolicies.age(now - 3 * 3_600_000, now))
        assertEquals("Checked 2d ago", QuestionPresencePolicies.age(now - 2 * 86_400_000, now))
        // A future timestamp never claims time travel.
        assertEquals("Checked just now", QuestionPresencePolicies.age(now + 5_000, now))
        assertEquals("Finish the draft in the composer on your computer.", QuestionPresencePolicies.reason("a draft is open", false))
        assertEquals("Close the open dialog on your computer.", QuestionPresencePolicies.reason("dialog open", false))
        assertEquals("The agent is not showing an answerable prompt yet.", QuestionPresencePolicies.reason("unrecognized prompt", false))
        assertEquals("The agent is still working. Answers stay disabled until it asks again.", QuestionPresencePolicies.reason("the agent is working", false))
        assertEquals("Complete this request on your computer.", QuestionPresencePolicies.reason("some new native text", false))
        assertEquals("The agent has not opened answers for this request yet.", QuestionPresencePolicies.reason("", false))
    }
}
