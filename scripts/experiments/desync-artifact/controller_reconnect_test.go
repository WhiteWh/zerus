package main

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
)

// dropConnection closes the TCP connection without a response.
func dropConnection(w http.ResponseWriter) {
	conn, _, err := w.(http.Hijacker).Hijack()
	if err == nil {
		conn.Close()
	}
}

func TestControllerSurvivesTransientExecutorLoss(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if calls.Add(1) <= 3 {
			dropConnection(w)
			return
		}
		http.Error(w, "no such job", http.StatusNotFound)
	}))
	defer server.Close()
	c := executorClient{client: server.Client(), endpoint: server.URL}
	_, status, err := c.snapshotWithin("job", 5*time.Second, 10*time.Millisecond)
	if status != http.StatusNotFound || err == nil || errors.Is(err, errExecutorUnavailable) {
		t.Fatalf("status %d err %v after %d calls: want the executor's own answer after the drops", status, err, calls.Load())
	}
	if calls.Load() != 4 {
		t.Fatalf("calls %d, want 4", calls.Load())
	}
}

func TestControllerGivesUpAfterGrace(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		dropConnection(w)
	}))
	defer server.Close()
	c := executorClient{client: server.Client(), endpoint: server.URL}
	started := time.Now()
	_, _, err := c.snapshotWithin("job", 200*time.Millisecond, 20*time.Millisecond)
	if !errors.Is(err, errExecutorUnavailable) || time.Since(started) < 200*time.Millisecond || calls.Load() < 2 {
		t.Fatalf("err %v after %v and %d calls", err, time.Since(started), calls.Load())
	}
}

func TestControllerDoesNotRetryRefusals(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		http.Error(w, "refused", http.StatusForbidden)
	}))
	defer server.Close()
	c := executorClient{client: server.Client(), endpoint: server.URL}
	if _, status, err := c.snapshotWithin("job", 5*time.Second, 10*time.Millisecond); status != http.StatusForbidden || err == nil || calls.Load() != 1 {
		t.Fatalf("status %d err %v calls %d", status, err, calls.Load())
	}
}
