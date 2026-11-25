"""Bounded loopback TCP fault relay for synthetic transport qualification.

The relay sits between a paired-TLS receiver and its publisher on literal
127.0.0.1 ports. TLS stays end to end: the relay only sees ciphertext, so the
mutual certificate pins are exercised unchanged. It can delay, fragment and
rate-limit each direction, and it can cut or stall the publisher-to-receiver
stream at an exact byte offset inside a chosen response. A response is
identified by turn: on one connection, every new run of publisher bytes that
follows receiver bytes begins a turn, and each connection's first turn (its TLS
handshake flight) is not counted as a response. Go's HTTP/1.1 client sends one
request at a time per connection, so turns line up with HTTP responses. A cut
inside a TLS record may withhold that record's plaintext from the receiver:
the relay controls wire bytes, not decrypted ones.
"""
import random
import socket
import threading
import time

LOOPBACK = "127.0.0.1"


class FaultRelay:
    def __init__(self, target_port, *, seed=0, max_fragment=0, fragment_delay=0.0,
                 first_byte_delay=0.0, rate_bytes=0, cut_response=None, cut_offset=0,
                 stall=False, max_connections=32):
        if not isinstance(target_port, int) or not 0 < target_port < 65536:
            raise ValueError("relay target must be a loopback port")
        self.target = (LOOPBACK, target_port)
        self.random = random.Random(seed)
        self.max_fragment, self.fragment_delay = max_fragment, fragment_delay
        self.first_byte_delay, self.rate_bytes = first_byte_delay, rate_bytes
        self.cut_response, self.cut_offset, self.stall = cut_response, cut_offset, stall
        self.max_connections = max_connections
        self.lock = threading.Lock()
        self.closed = threading.Event()
        self.fired = threading.Event()  # The cut or stall happened.
        self.connections = self.responses = 0
        self.down_bytes = self.up_bytes = 0
        self.refused = 0
        self.sizes = {}  # Response number -> publisher bytes forwarded in it.
        self.sockets, self.threads = [], []
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind((LOOPBACK, 0))
        self.listener.listen(8)
        self.listener.settimeout(0.2)
        self.port = self.listener.getsockname()[1]
        self.endpoint_host = f"{LOOPBACK}:{self.port}"
        self.accepter = threading.Thread(target=self._accept, daemon=True)
        self.accepter.start()

    def _accept(self):
        while not self.closed.is_set():
            try:
                client, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with self.lock:
                # After a cut the link is down: new connections close at once,
                # like a peer outage, until the test replaces the relay.
                if self.fired.is_set() or self.connections >= self.max_connections or self.closed.is_set():
                    self.refused += 1
                    client.close()
                    continue
                self.connections += 1
            try:
                upstream = socket.create_connection(self.target, timeout=3)
            except OSError:
                client.close()
                continue
            for s in (client, upstream):
                s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                s.settimeout(0.2)
            state = {"last": None, "turns": 0, "response": None, "into": 0}
            with self.lock:
                if self.closed.is_set():  # close() may already have snapshotted pumps.
                    client.close()
                    upstream.close()
                    return
                self.sockets += [client, upstream]
                pair = [threading.Thread(target=self._pump, args=(client, upstream, False, state), daemon=True),
                        threading.Thread(target=self._pump, args=(upstream, client, True, state), daemon=True)]
                self.threads += pair
            for t in pair:
                t.start()

    def _pace(self, clock, size):
        # A per-direction schedule: Windows timer waits are coarse, so pay the
        # accumulated delay in waits of at least 20 ms. Every wait ends at once
        # when the relay closes.
        clock["due"] = max(clock["due"], time.monotonic()) + self.fragment_delay + (size / self.rate_bytes if self.rate_bytes else 0)
        ahead = clock["due"] - time.monotonic()
        if ahead >= 0.02:
            self.closed.wait(ahead)

    def _pump(self, source, sink, down, state):
        first = True
        clock = {"due": 0.0}
        try:
            while not self.closed.is_set():
                try:
                    data = source.recv(65536)
                except socket.timeout:
                    continue
                if not data:
                    break
                if first and self.first_byte_delay:
                    self.closed.wait(self.first_byte_delay)
                first = False
                if not down:
                    with self.lock:
                        state["last"] = "up"
                        self.up_bytes += len(data)
                    self._send(sink, data)
                    continue
                while data and not self.closed.is_set():
                    with self.lock:
                        if state["last"] != "down":
                            state["last"] = "down"
                            state["turns"] += 1
                            state["into"] = 0
                            state["response"] = None
                            if state["turns"] > 1:  # Turn 1 is the TLS handshake flight.
                                self.responses += 1
                                state["response"] = self.responses
                        limit = len(data)
                        targeted = (self.cut_response is not None and state["response"] == self.cut_response
                                    and not self.fired.is_set())
                        if targeted:
                            limit = min(limit, max(0, self.cut_offset - state["into"]))
                        if self.max_fragment:
                            limit = min(limit, self.random.randint(1, self.max_fragment))
                    if targeted and limit == 0:
                        self.fired.set()
                        if self.stall:
                            # Hold both sides open and forward nothing until the
                            # test closes the relay or the receiver deadline ends.
                            self.closed.wait()
                        return
                    piece, data = data[:limit], data[limit:]
                    self._pace(clock, len(piece))
                    self._send(sink, piece)
                    with self.lock:
                        state["into"] += len(piece)
                        if state["response"] is not None:
                            self.sizes[state["response"]] = state["into"]
                        self.down_bytes += len(piece)
        except OSError:
            pass
        finally:
            for s in (source, sink):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                s.close()

    def _send(self, sink, data):
        view = memoryview(data)
        while view:
            if self.closed.is_set():
                raise OSError("relay closed")
            try:
                n = sink.send(view)
            except socket.timeout:
                continue
            if n == 0:
                raise OSError("relay stream closed")
            view = view[n:]

    def stats(self):
        with self.lock:
            return {"connections": self.connections, "responses": self.responses, "refused": self.refused,
                    "down_bytes": self.down_bytes, "up_bytes": self.up_bytes, "fired": self.fired.is_set(),
                    "sizes": dict(self.sizes)}

    def close(self):
        self.closed.set()
        try:
            self.listener.close()
        except OSError:
            pass
        deadline = time.monotonic() + 5  # One settlement deadline for all relay threads.
        # Settle the accepter first: after it ends no pump can be registered.
        self.accepter.join(timeout=max(0.0, deadline - time.monotonic()))
        with self.lock:
            sockets, threads = list(self.sockets), list(self.threads)
        for s in sockets:
            try:
                s.close()
            except OSError:
                pass
        for t in threads:
            t.join(timeout=max(0.0, deadline - time.monotonic()))
        if self.accepter.is_alive() or any(t.is_alive() for t in threads):
            raise RuntimeError("synthetic fault relay did not settle")

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def through(offer, relay):
    """Return a copy of a paired offer whose endpoint is the relay; pins unchanged."""
    changed = dict(offer)
    if not changed["endpoint"].startswith("https://127.0.0.1:"):
        raise ValueError("fault relay only fronts paired loopback publications")
    changed["endpoint"] = "https://" + relay.endpoint_host
    return changed
