package main

import (
	"bytes"
	"encoding/binary"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"

	d "github.com/folbricht/desync"
)

func TestSeparatedTransportRejectsAlternateDestinationsBeforeDial(t *testing.T) {
	for _, endpoint := range []string{"https://127.0.0.1:123", "http://192.0.2.1:123", "http://localhost:123",
		"http://[::1]:123", "http://127.0.0.2:123", "http://127.0.0.1:00080", "http://127.0.0.1:0",
		"http://127.0.0.1:65536", "http://user@127.0.0.1:80", "http://127.0.0.1:80/",
		"http://127.0.0.1:80?", "http://127.0.0.1:80#fragment"} {
		if client, err := fixedLoopbackClient(endpoint); err == nil {
			client.CloseIdleConnections()
			t.Fatal("alternate destination admitted")
		}
	}
}

func TestSeparatedTransportRefusesRedirectWithoutFollowingIt(t *testing.T) {
	var contacted atomic.Int64
	sink := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { contacted.Add(1) }))
	defer sink.Close()
	redirect := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { http.Redirect(w, r, sink.URL, http.StatusFound) }))
	defer redirect.Close()
	client, err := fixedLoopbackClient(redirect.URL)
	if err != nil {
		t.Fatal(err)
	}
	defer client.CloseIdleConnections()
	res, err := client.Get(redirect.URL)
	if res != nil {
		res.Body.Close()
	}
	if err == nil || contacted.Load() != 0 {
		t.Fatal("redirect was followed")
	}
}

func TestSeparatedChunksRequirePinnedIdentityAndExactDigest(t *testing.T) {
	for _, mode := range []string{"correct", "wrong_session", "wrong_artifact", "corrupt", "oversized"} {
		t.Run(mode, func(t *testing.T) {
			d.Digest = d.SHA256{}
			body := []byte("synthetic binary fragment")
			id := d.Digest.Sum(body)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				setPublicationHeaders(w.Header(), "pinned", "session")
				switch mode {
				case "wrong_session":
					w.Header().Set("X-Zerus-Publication", "stale")
				case "wrong_artifact":
					w.Header().Set("X-Zerus-Artifact", "other")
				case "corrupt":
					body = bytes.Repeat([]byte{1}, len(body))
				case "oversized":
					body = append(body, 1)
				}
				_, _ = w.Write(body)
			}))
			defer server.Close()
			client, err := fixedLoopbackClient(server.URL)
			if err != nil {
				t.Fatal(err)
			}
			defer client.CloseIdleConnections()
			store := &loopbackStore{base: server.URL, client: client, artifact: "pinned", session: "session",
				known: map[d.ChunkID]d.IndexChunk{id: {ID: id, Size: uint64(len(body))}}}
			_, err = store.GetChunk(id)
			if (err == nil) != (mode == "correct") {
				t.Fatal("unexpected chunk admission")
			}
		})
	}
}

func TestReceiverCannotAcceptSourceFields(t *testing.T) {
	work := t.TempDir()
	p := filepath.Join(work, "config.json")
	if err := os.WriteFile(p, []byte(`{"source":"reserved-synthetic-path"}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := run("receive", p); err == nil {
		t.Fatal("receiver accepted a source field")
	}
	if _, err := os.Stat(filepath.Join(work, "receiver.bin")); !os.IsNotExist(err) {
		t.Fatal("invalid control created a receiver")
	}
}

func TestWireIndexPreflightRejectsArchiveAllocationHeaders(t *testing.T) {
	p := filepath.Join(t.TempDir(), "malformed.caibx")
	b := make([]byte, 144)
	binary.LittleEndian.PutUint64(b[0:8], ^uint64(0)-1)
	binary.LittleEndian.PutUint64(b[8:16], d.CaFormatUser)
	if err := os.WriteFile(p, b, 0600); err != nil {
		t.Fatal(err)
	}
	f, err := os.Open(p)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	offer := Offer{IndexLen: int64(len(b)), Artifact: Artifact{Index: sumBytes(b)}}
	if _, err := checkedIndex(f, offer); err == nil {
		t.Fatal("allocation-bearing element reached the archive decoder")
	}
}
