import Foundation
import Synchronization

/// One ordered writer, ported from Android `OrderedStatePersistence`. UI reducers
/// apply immediately; ordinary edits are written after a short debounce, and
/// only acknowledged `durable` reducers release transport.
@MainActor
public final class OrderedStatePersistence<S: Sendable> {
    public private(set) var state: S
    /// Called after every state publication.
    public var onChange: ((S) -> Void)?
    /// Called with nil after a successful write, or with the failure.
    public var onResult: ((Error?) -> Void)?

    private let normalize: @Sendable (S) -> S
    private var acceptedSequence: Int64 = 0
    private let queue = CommandQueue<S>()
    private var loop: Task<Void, Never>?

    public init(initial: S, debounce: Duration = .milliseconds(200), maxDelay: Duration = .milliseconds(800),
                normalize: @escaping @Sendable (S) -> S = { $0 },
                normalizePublished: (@Sendable (S) -> S)? = nil,
                write: @escaping @Sendable (S) throws -> Void) {
        state = initial
        self.normalize = normalizePublished ?? normalize
        let queue = self.queue
        let owner = WeakOwner<S>()
        loop = Task.detached(priority: .utility) {
            await Self.run(initial: initial, queue: queue, debounce: debounce, maxDelay: maxDelay, normalize: normalize, write: write, owner: owner)
        }
        owner.value = self
    }

    /// Applies a reducer now and schedules a debounced write.
    @discardableResult
    public func edit(_ reduce: @escaping @Sendable (S) throws -> S) rethrows -> S {
        let next = normalize(try reduce(state))
        acceptedSequence += 1
        queue.push(.edit(acceptedSequence, reduce))
        publish(next)
        return next
    }

    /// Writes the reducer's result before returning. `acknowledge` merges the
    /// written value into newer UI state; by default the reducer is reapplied.
    public func durable(_ reduce: @escaping @Sendable (S) throws -> S,
                        acknowledge: (@Sendable (S, S) -> S)? = nil) async throws -> S {
        let merge: @Sendable (S, S) -> S = acknowledge ?? { current, written in (try? reduce(current)) ?? written }
        return try await withCheckedThrowingContinuation { continuation in
            acceptedSequence += 1
            queue.push(.durable(acceptedSequence, reduce, merge, continuation))
        }
    }

    public func flush() async throws -> S {
        try await withCheckedThrowingContinuation { continuation in
            acceptedSequence += 1
            queue.push(.flush(acceptedSequence, continuation))
        }
    }

    public func flushAsync() {
        acceptedSequence += 1
        queue.push(.flush(acceptedSequence, nil))
    }

    /// Stops the writer after pending commands. Used when a store is discarded.
    public func close() {
        queue.push(.close)
    }

    private func publish(_ value: S) {
        state = value
        onChange?(value)
    }

    fileprivate func caughtUp(_ sequence: Int64, _ value: S) {
        if sequence == acceptedSequence { publish(value) }
    }

    fileprivate func acknowledged(_ value: S, merge: @Sendable (S, S) -> S) {
        publish(normalize(merge(state, value)))
    }

    fileprivate func report(_ error: Error?) { onResult?(error) }

    private nonisolated static func run(initial: S, queue: CommandQueue<S>, debounce: Duration, maxDelay: Duration,
                                        normalize: @Sendable (S) -> S, write: @Sendable (S) throws -> Void, owner box: WeakOwner<S>) async {
        var owner: OrderedStatePersistence<S>? { box.value }
        var current = initial
        var processedSequence: Int64 = 0
        var dirty = false
        var firstDirty = ContinuousClock.now
        var deadline = ContinuousClock.now
        let clock = ContinuousClock()

        func persist(_ value: S) async throws {
            try write(value)
            await owner?.report(nil)
        }

        while true {
            let command: Command<S>?
            if dirty {
                command = await queue.next(until: deadline)
            } else {
                command = await queue.next(until: nil)
            }
            guard let command else {
                do {
                    let next = normalize(current)
                    try await persist(next)
                    current = next
                    await owner?.caughtUp(processedSequence, next)
                } catch { await owner?.report(error) }
                dirty = false
                continue
            }
            switch command {
            case .close:
                if dirty { try? write(normalize(current)) }
                return
            case .edit(let sequence, let reduce):
                do {
                    current = normalize(try reduce(current))
                    processedSequence = sequence
                    let value = current
                    await owner?.caughtUp(sequence, value)
                    let now = clock.now
                    if !dirty { firstDirty = now }
                    dirty = true
                    deadline = min(now + debounce, firstDirty + maxDelay)
                } catch {
                    await owner?.report(error)
                }
            case .durable(let sequence, let reduce, let merge, let reply):
                let next: S
                do { next = normalize(try reduce(current)) } catch {
                    // A failed precondition changes nothing and is the caller's error to show.
                    reply.resume(throwing: error)
                    continue
                }
                do {
                    try write(next)
                    current = next
                    processedSequence = sequence
                    dirty = false
                    await owner?.report(nil)
                    await owner?.acknowledged(next, merge: merge)
                    reply.resume(returning: next)
                } catch {
                    await owner?.report(error)
                    reply.resume(throwing: error)
                }
            case .flush(let sequence, let reply):
                do {
                    let next = normalize(current)
                    try await persist(next)
                    current = next
                    processedSequence = sequence
                    dirty = false
                    let value = current
                    await owner?.caughtUp(sequence, value)
                    reply?.resume(returning: value)
                } catch {
                    dirty = false
                    await owner?.report(error)
                    reply?.resume(throwing: error)
                }
            }
        }
    }
}

/// The writer must not keep its owner alive.
final class WeakOwner<S: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private weak var stored: OrderedStatePersistence<S>?
    var value: OrderedStatePersistence<S>? {
        get { lock.withLock { stored } }
        set { lock.withLock { stored = newValue } }
    }
}

enum Command<S: Sendable>: Sendable {
    case edit(Int64, @Sendable (S) throws -> S)
    case durable(Int64, @Sendable (S) throws -> S, @Sendable (S, S) -> S, CheckedContinuation<S, Error>)
    case flush(Int64, CheckedContinuation<S, Error>?)
    case close
}

/// A FIFO with a single consumer that can wait for work or a deadline.
final class CommandQueue<S: Sendable>: Sendable {
    private struct State: ~Copyable {
        var items: [Command<S>] = []
        var waiter: CheckedContinuation<Void, Never>?
    }
    private let state = Mutex(State())

    func push(_ command: Command<S>) {
        let waiter = state.withLock { state -> CheckedContinuation<Void, Never>? in
            state.items.append(command)
            defer { state.waiter = nil }
            return state.waiter
        }
        waiter?.resume()
    }

    private func pop() -> Command<S>? {
        state.withLock { state in state.items.isEmpty ? nil : state.items.removeFirst() }
    }

    /// The next command, or nil once `deadline` passes with nothing queued.
    func next(until deadline: ContinuousClock.Instant?) async -> Command<S>? {
        while true {
            if let command = pop() { return command }
            if let deadline, ContinuousClock.now >= deadline { return nil }
            let timer = deadline.map { deadline in
                Task { [weak self] in
                    try? await Task.sleep(until: deadline, clock: .continuous)
                    self?.wake()
                }
            }
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let resumeNow = state.withLock { state -> Bool in
                    if !state.items.isEmpty { return true }
                    state.waiter = continuation
                    return false
                }
                if resumeNow { continuation.resume() }
            }
            timer?.cancel()
        }
    }

    private func wake() {
        let waiter = state.withLock { state -> CheckedContinuation<Void, Never>? in
            defer { state.waiter = nil }
            return state.waiter
        }
        waiter?.resume()
    }
}
