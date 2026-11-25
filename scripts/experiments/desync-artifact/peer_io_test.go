package main

import (
	"errors"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestReadIdleBoundDefaultsAndRange(t *testing.T) {
	for seconds, want := range map[int]time.Duration{0: 30 * time.Second, 1: time.Second, 300: 300 * time.Second} {
		if got, err := readIdleBound(seconds); err != nil || got != want {
			t.Fatalf("read_idle_seconds %d: %v %v", seconds, got, err)
		}
	}
	for _, seconds := range []int{-1, 301, 1 << 30} {
		if _, err := readIdleBound(seconds); err == nil {
			t.Fatalf("read_idle_seconds %d admitted", seconds)
		}
	}
}

func loopbackPair(t *testing.T) (net.Conn, net.Conn) {
	t.Helper()
	listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	accepted := make(chan net.Conn, 1)
	go func() {
		c, _ := listener.Accept()
		accepted <- c
	}()
	client, err := net.Dial("tcp4", listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	server := <-accepted
	if server == nil {
		t.Fatal("synthetic loopback accept failed")
	}
	t.Cleanup(func() { client.Close(); server.Close() })
	return client, server
}

func TestIdleConnFailsSilentPeerWithinBound(t *testing.T) {
	client, _ := loopbackPair(t)
	conn := newIdleConn(client, 150*time.Millisecond)
	start := time.Now()
	_, err := conn.Read(make([]byte, 16))
	elapsed := time.Since(start)
	var timeout net.Error
	if !errors.Is(err, errReadIdle) || !errors.As(err, &timeout) || !timeout.Timeout() {
		t.Fatalf("silent peer: %v", err)
	}
	if elapsed < 150*time.Millisecond || elapsed > 3*time.Second {
		t.Fatalf("inactivity bound ended after %v", elapsed)
	}
}

func TestIdleConnProgressOutlastsBound(t *testing.T) {
	client, server := loopbackPair(t)
	conn := newIdleConn(client, 200*time.Millisecond)
	go func() {
		for i := 0; i < 12; i++ { // About 0.9 s of trickle, far beyond one interval.
			time.Sleep(75 * time.Millisecond)
			if _, err := server.Write([]byte{byte(i)}); err != nil {
				return
			}
		}
		server.Close()
	}()
	start := time.Now()
	data, err := io.ReadAll(conn)
	if err != nil || len(data) != 12 {
		t.Fatalf("progressing peer failed after %d bytes: %v", len(data), err)
	}
	if time.Since(start) < 400*time.Millisecond {
		t.Fatal("synthetic trickle did not outlast the inactivity interval")
	}
}

func TestIdleConnKeepsEarlierExplicitDeadline(t *testing.T) {
	client, _ := loopbackPair(t)
	conn := newIdleConn(client, 10*time.Second)
	if err := conn.SetReadDeadline(time.Now().Add(100 * time.Millisecond)); err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	_, err := conn.Read(make([]byte, 1))
	if !errors.Is(err, os.ErrDeadlineExceeded) || errors.Is(err, errReadIdle) || time.Since(start) > 3*time.Second {
		t.Fatalf("explicit earlier deadline was not kept: %v", err)
	}
	// A later explicit deadline does not extend the inactivity bound.
	conn = newIdleConn(client, 150*time.Millisecond)
	if err := conn.SetDeadline(time.Now().Add(10 * time.Second)); err != nil {
		t.Fatal(err)
	}
	start = time.Now()
	if _, err := conn.Read(make([]byte, 1)); !errors.Is(err, errReadIdle) || time.Since(start) > 3*time.Second {
		t.Fatalf("later explicit deadline extended inactivity: %v", err)
	}
}

func TestIdleConnExplicitDeadlineInterruptsBlockedRead(t *testing.T) {
	client, _ := loopbackPair(t)
	conn := newIdleConn(client, 10*time.Second)
	done := make(chan error, 1)
	go func() {
		_, err := conn.Read(make([]byte, 1))
		done <- err
	}()
	time.Sleep(100 * time.Millisecond)
	if err := conn.SetReadDeadline(time.Now()); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		if !errors.Is(err, os.ErrDeadlineExceeded) {
			t.Fatalf("interrupted read: %v", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("explicit deadline did not interrupt the blocked read")
	}
}

func syntheticIdentity(t *testing.T, root, name string) (string, string) {
	t.Helper()
	dir := filepath.Join(root, name)
	if err := os.Mkdir(dir, 0700); err != nil {
		t.Fatal(err)
	}
	config := filepath.Join(dir, "init.json")
	if err := os.WriteFile(config, []byte("{}"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := initializeIdentity(config); err != nil {
		t.Fatal(err)
	}
	_, pin, err := loadIdentity(filepath.Join(dir, "identity"))
	if err != nil {
		t.Fatal(err)
	}
	return filepath.Join(dir, "identity"), pin
}

func TestPairedClientBoundsStalledBodyButKeepsSlowProgress(t *testing.T) {
	root := t.TempDir()
	serverIdentity, serverPin := syntheticIdentity(t, root, "server")
	clientIdentity, clientPin := syntheticIdentity(t, root, "client")
	listener, endpoint, pin, err := dataListener(PeerConfig{Identity: serverIdentity, Fingerprint: clientPin, Listen: "127.0.0.1:0"})
	if err != nil || pin != serverPin {
		t.Fatal("synthetic paired listener", err)
	}
	release := make(chan struct{})
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Length", "8")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("syn"))
		w.(http.Flusher).Flush()
		if r.URL.Path == "/stall" {
			<-release // Declared length, partial body, then silence.
			return
		}
		for _, b := range []byte("theti") { // Progress every 400 ms for 2 s.
			time.Sleep(400 * time.Millisecond)
			w.Write([]byte{b})
			w.(http.Flusher).Flush()
		}
	})}
	go server.Serve(listener)
	defer func() { close(release); server.Close() }()
	client, err := dataClient(endpoint, serverPin, PeerConfig{Identity: clientIdentity, Fingerprint: serverPin, ReadIdle: 1})
	if err != nil {
		t.Fatal(err)
	}
	defer client.CloseIdleConnections()

	start := time.Now()
	res, err := client.Get(endpoint + "/slow")
	if err != nil {
		t.Fatal(err)
	}
	body, err := io.ReadAll(res.Body)
	res.Body.Close()
	if err != nil || string(body) != "syntheti" || time.Since(start) < 1500*time.Millisecond {
		t.Fatalf("slow progress beyond the interval failed: %q %v after %v", body, err, time.Since(start))
	}

	start = time.Now()
	res, err = client.Get(endpoint + "/stall")
	if err != nil {
		t.Fatal(err)
	}
	_, err = io.ReadAll(res.Body)
	res.Body.Close()
	elapsed := time.Since(start)
	if err == nil || elapsed < time.Second || elapsed > 5*time.Second {
		t.Fatalf("stalled body ended with %v after %v", err, elapsed)
	}
}

func TestPeerReadIdleConfigurationIsChecked(t *testing.T) {
	pin := strings.Repeat("ab", 32)
	for _, cfg := range []PeerConfig{
		{Identity: "unused", Fingerprint: pin, ReadIdle: -1},
		{Identity: "unused", Fingerprint: pin, ReadIdle: 301},
	} {
		if _, err := dataClient("https://127.0.0.1:9", pin, cfg); err == nil || !strings.Contains(err.Error(), "read_idle_seconds") {
			t.Fatalf("invalid paired read_idle_seconds admitted: %v", err)
		}
		if _, _, _, err := dataListener(cfg); err == nil || !strings.Contains(err.Error(), "read_idle_seconds") {
			t.Fatalf("invalid listener read_idle_seconds admitted: %v", err)
		}
	}
	if _, err := dataClient("http://127.0.0.1:9", "", PeerConfig{ReadIdle: 5}); err == nil {
		t.Fatal("plain loopback client accepted read_idle_seconds")
	}
	if _, _, _, err := dataListener(PeerConfig{ReadIdle: 5}); err == nil {
		t.Fatal("plain loopback listener accepted read_idle_seconds")
	}
	if client, err := dataClient("http://127.0.0.1:9", "", PeerConfig{}); err != nil {
		t.Fatal("plain loopback behavior changed", err)
	} else {
		client.CloseIdleConnections()
	}
}
