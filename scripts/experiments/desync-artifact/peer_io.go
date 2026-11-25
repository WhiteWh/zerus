// Read-inactivity bound for paired data connections. The paired client's
// overall 15-minute budget bounds a whole exchange; this bounds a silent peer:
// a Read that receives nothing for the configured interval fails, and every
// Read that returns data starts a new interval. A slow transfer with continued
// progress may therefore last far longer than the interval. A peer that
// trickles bytes below the interval stays bounded only by the overall budget.
// A failure here is a transport failure for the caller's existing repair path;
// it never retries a request, a delivery or a scenario.
package main

import (
	"errors"
	"net"
	"sync"
	"time"
)

const defaultReadIdle = 30 * time.Second
const maxReadIdleSeconds = 300

// readIdleBound maps PeerConfig.ReadIdle: 0 (omitted) selects the 30 s
// default; explicit values must be 1..300 seconds.
func readIdleBound(seconds int) (time.Duration, error) {
	if seconds == 0 {
		return defaultReadIdle, nil
	}
	if seconds < 1 || seconds > maxReadIdleSeconds {
		return 0, errors.New("read_idle_seconds must be between 1 and 300")
	}
	return time.Duration(seconds) * time.Second, nil
}

// errReadIdle reports a timeout so net/http and crypto/tls treat it as one.
type readIdleError struct{}

func (readIdleError) Error() string {
	return "paired peer sent no data within the read inactivity bound"
}
func (readIdleError) Timeout() bool   { return true }
func (readIdleError) Temporary() bool { return true }

var errReadIdle net.Error = readIdleError{}

// idleConn applies the inactivity bound to each Read and keeps any explicit
// read deadline a caller sets: the earlier of the two always applies.
type idleConn struct {
	net.Conn
	idle     time.Duration
	mu       sync.Mutex
	explicit time.Time // Caller's read deadline; zero means none.
	reading  time.Time // Inactivity deadline of the Read in progress; zero when idle.
}

func newIdleConn(conn net.Conn, idle time.Duration) *idleConn {
	return &idleConn{Conn: conn, idle: idle}
}

func earlier(a, b time.Time) time.Time {
	if a.IsZero() || (!b.IsZero() && b.Before(a)) {
		return b
	}
	return a
}

func (c *idleConn) Read(b []byte) (int, error) {
	c.mu.Lock()
	c.reading = time.Now().Add(c.idle)
	bound := c.reading
	effective := earlier(c.reading, c.explicit)
	err := c.Conn.SetReadDeadline(effective)
	c.mu.Unlock()
	if err != nil {
		return 0, err
	}
	n, err := c.Conn.Read(b)
	c.mu.Lock()
	c.reading = time.Time{}
	explicit := c.explicit
	c.mu.Unlock()
	var timeout net.Error
	if n == 0 && err != nil && errors.As(err, &timeout) && timeout.Timeout() &&
		(explicit.IsZero() || bound.Before(explicit)) && !time.Now().Before(bound) {
		return 0, errReadIdle
	}
	return n, err
}

func (c *idleConn) SetReadDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.explicit = t
	if c.reading.IsZero() {
		return c.Conn.SetReadDeadline(t)
	}
	return c.Conn.SetReadDeadline(earlier(c.reading, t))
}

func (c *idleConn) SetDeadline(t time.Time) error {
	if err := c.SetReadDeadline(t); err != nil {
		return err
	}
	return c.Conn.SetWriteDeadline(t)
}
