package main

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	d "github.com/folbricht/desync"
)

func chunkServer(t *testing.T, data []byte, failures int32, status int) (*loopbackStore, d.ChunkID, *atomic.Int32) {
	t.Helper()
	id := d.Digest.Sum(data)
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := calls.Add(1)
		if status != 0 {
			http.Error(w, "refused", status)
			return
		}
		if n <= failures {
			// Promise the full chunk, send half, then drop the connection.
			w.Header().Set("Content-Length", fmt.Sprint(len(data)))
			w.Write(data[:len(data)/2])
			w.(http.Flusher).Flush()
			conn, _, _ := w.(http.Hijacker).Hijack()
			conn.Close()
			return
		}
		w.Header().Set("Content-Length", fmt.Sprint(len(data)))
		w.Write(data)
	}))
	t.Cleanup(server.Close)
	store := &loopbackStore{base: server.URL, client: server.Client(),
		known: map[d.ChunkID]d.IndexChunk{id: {ID: id, Size: uint64(len(data))}}}
	return store, id, &calls
}

func withFastChunkRetry(t *testing.T) {
	previous := chunkRetryPause
	chunkRetryPause = time.Millisecond
	t.Cleanup(func() { chunkRetryPause = previous })
}

func TestChunkRetriesATruncatedBody(t *testing.T) {
	withFastChunkRetry(t)
	data := make([]byte, 256*1024)
	for i := range data {
		data[i] = byte(i * 7)
	}
	store, id, calls := chunkServer(t, data, 2, 0)
	chunk, err := store.GetChunk(id)
	if err != nil || chunk == nil || calls.Load() != 3 {
		t.Fatalf("err %v calls %d", err, calls.Load())
	}
}

func TestChunkGivesUpAfterBoundedAttempts(t *testing.T) {
	withFastChunkRetry(t)
	data := make([]byte, 64*1024)
	store, id, calls := chunkServer(t, data, 100, 0)
	if _, err := store.GetChunk(id); err == nil || calls.Load() != chunkAttempts {
		t.Fatalf("err %v calls %d", err, calls.Load())
	}
}

func TestChunkRefusalIsNotRetried(t *testing.T) {
	withFastChunkRetry(t)
	store, id, calls := chunkServer(t, []byte("x"), 0, http.StatusForbidden)
	if _, err := store.GetChunk(id); err == nil || calls.Load() != 1 {
		t.Fatalf("err %v calls %d", err, calls.Load())
	}
}
