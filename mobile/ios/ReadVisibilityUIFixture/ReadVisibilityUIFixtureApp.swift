import SwiftUI
import ZerusKit

/// zerus-a4mub.31 on-screen proof. Production code under test, unchanged: `readingItem` and
/// `ConversationReadLayout` (mobile/ios/Zerus/Conversation/ConversationReadLayout.swift), which report each list item's
/// placement from `bounds(of: .scrollView)`, and ZerusKit's readable-visibility rule, read state and unread count.
/// Synthetic: the rows, the scroll controls and the dwell glue, which repeats ConversationView's settled 350 ms dwell.
@main struct ReadVisibilityUIFixtureApp: App {
    var body: some Scene { WindowGroup { ReadVisibilityScreen() } }
}

struct ReadVisibilityScreen: View {
    private struct DwellKey: Equatable { let ids: [String]; let settled: Bool }
    private struct Row: Identifiable { let event: Event; let height: CGFloat; var id: String { event.id } }

    @State private var layout = ConversationReadLayout()
    @State private var position = ScrollPosition(idType: String.self)
    @State private var readState: ConversationReadState
    @State private var questionsSeen = false
    @State private var userScrolling = false
    @State private var dwells = 0
    @State private var viewport: CGFloat = 0

    private static let target = Target(computerId: "fixture-computer", session: "claude/fixture/read-visibility", run: "fixture-run",
                                       conversation: "fixture-conversation", connectionId: "fixture-workspace")
    private static let epoch = "fixture-epoch"
    // Content offsets: earlier 0..40, e1 40..740, e2 740..860, newer 860..1460, questions 1460..3060, tail 3060.
    // Each control yields a different shown set, so each starts a new dwell.
    private let earlier: CGFloat = 40, newer: CGFloat = 600, questions: CGFloat = 1600
    private let rows: [Row] = [
        Row(event: Self.message("e1", 1, "First reply, already on screen at the top."), height: 700),
        Row(event: Self.message("e2", 2, "Second reply, just below the fold."), height: 120),
    ]
    private var e2Top: CGFloat { earlier + rows[0].height }
    private var e2Bottom: CGFloat { e2Top + rows[1].height }
    private var questionsTop: CGFloat { e2Bottom + newer }

    init() {
        // The computer's head: two incoming replies, none read yet.
        _readState = State(initialValue: ConversationReadState().head(Self.target, epoch: Self.epoch, latest: 2, total: 2, complete: true))
    }

    private static func message(_ id: String, _ seq: Int64, _ text: String) -> Event {
        Event(id: id, role: "Claude", text: text, time: "", nativeType: "AgentMessage", historyId: "history-" + id,
              historyCursor: "cursor-" + id, historyEpoch: epoch, incomingSeq: seq)
    }

    private var unread: Int {
        ConversationReadPolicies.lowerBound(readState.record(Self.target), rows.map(\.event)) ?? -1
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Unread: \(unread)").font(.title2.bold()).accessibilityIdentifier("unread-count")
            Text("Questions seen: \(questionsSeen ? "yes" : "no")").font(.headline).accessibilityIdentifier("questions-seen")
            Text("Visible: \(layout.visible.joined(separator: " "))").font(.caption2).lineLimit(2).accessibilityIdentifier("visible-keys")
            Text("Dwells: \(dwells)  Viewport: \(Int(viewport.rounded())) pt  Questions pane: \(Int(questions)) pt")
                .font(.caption2).accessibilityIdentifier("dwell-count")
            HStack {
                Button("Top") { scroll(0) }
                Button("Sliver e2") { scroll(e2Top + 20 - viewport) }
                Button("Full e2") { scroll(e2Bottom - viewport) }
            }.font(.caption).buttonStyle(.bordered)
            HStack {
                Button("Sliver questions") { scroll(questionsTop + 20 - viewport) }
                Button("Readable questions") { scroll(questionsTop + 60 - viewport) }
            }.font(.caption).buttonStyle(.bordered)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    Text("Earlier messages").frame(maxWidth: .infinity, minHeight: earlier, maxHeight: earlier)
                        .readingItem("history:earlier", layout)
                    ForEach(rows) { row in
                        Text(row.event.text).padding(12).frame(maxWidth: .infinity, minHeight: row.height, maxHeight: row.height, alignment: .topLeading)
                            .background(Color.blue.opacity(row.id == "e1" ? 0.15 : 0.35))
                            .readingItem("event:" + row.id, layout)
                    }
                    Color.gray.opacity(0.1).frame(height: newer).readingItem("history:newer", layout)
                    Text("Questions pane (taller than twice the viewport)").padding(12)
                        .frame(maxWidth: .infinity, minHeight: questions, maxHeight: questions, alignment: .topLeading)
                        .background(Color.orange.opacity(0.35))
                        .readingItem("history:questions", layout)
                    Color.clear.frame(height: 1).readingItem("history:tail", layout)
                }
                .scrollTargetLayout()
            }
            .scrollPosition($position)
            .onScrollGeometryChange(for: CGFloat.self) { $0.containerSize.height } action: { _, new in viewport = new }
            .onScrollPhaseChange { _, new in userScrolling = new == .interacting || new == .decelerating || new == .tracking }
            .accessibilityIdentifier("conversation-list")
        }
        .padding(.horizontal, 12)
        // As ConversationView: a changed shown set or scroll phase restarts the settled dwell.
        .task(id: DwellKey(ids: layout.visible, settled: !userScrolling)) {
            guard !userScrolling else { return }
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            let visible = layout.visible
            let ids = ConversationReadViewport.eventIds(visible)
            readState = readState.visible(Self.target, rows.map(\.event).filter { ids.contains($0.id) })
            if ConversationReadViewport.flags(visible: visible, newestId: "e2").questions { questionsSeen = true }
            dwells += 1
        }
    }

    private func scroll(_ y: CGFloat) { position.scrollTo(y: max(0, y)) }
}
