import SwiftUI
import ZerusKit

/// Where each listed conversation item sits in the visible scroll region, for reading (zerus-a4mub.31). Placements
/// change with every scrolled frame and are not observed. Only the keys the shared readable-visibility rule shows
/// (`ConversationReadViewport.visible`) are, and they change only when a decision flips, which restarts the dwell.
@MainActor @Observable
final class ConversationReadLayout {
    struct Placement: Equatable {
        var offset: Double
        var size: Double
        var viewport: Double
    }

    private(set) var visible: [String] = []
    @ObservationIgnored private var placements: [String: Placement] = [:]
    @ObservationIgnored private var viewport: Double = 0

    func place(_ key: String, _ placement: Placement?) {
        placements[key] = placement
        if let placement { viewport = placement.viewport }
        let items = placements.map { ConversationReadViewport.Item(key: $0.key, offset: $0.value.offset, size: $0.value.size) }
        let next = ConversationReadViewport.visible(items, start: 0, end: viewport)
        if next != visible { visible = next }
    }
}

extension View {
    /// This conversation item with its scroll identity, reporting its placement in the visible scroll region.
    func readingItem(_ key: String, _ layout: ConversationReadLayout) -> some View {
        onGeometryChange(for: ConversationReadLayout.Placement?.self) { proxy in
            // The scroll view's visible bounds in this item's coordinates: the item's top is at -minY.
            proxy.bounds(of: .scrollView).map { .init(offset: -$0.minY, size: proxy.size.height, viewport: $0.height) }
        } action: { placement in
            layout.place(key, placement)
        }
        .onDisappear { layout.place(key, nil) }
        .id(key)
    }
}
