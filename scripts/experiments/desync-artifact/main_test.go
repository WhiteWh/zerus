package main

import (
	"encoding/json"
	"math/rand"
	"net/http"
	"os"
	"path/filepath"
	"testing"
)

func fixture(t *testing.T, scenario string) (string, Config) {
	t.Helper()
	base := t.TempDir()
	source := filepath.Join(base, "synthetic-input.bin")
	data := make([]byte, 8*MiB)
	rng := rand.New(rand.NewSource(7))
	if _, err := rng.Read(data); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(source, data, 0600); err != nil {
		t.Fatal(err)
	}
	work := filepath.Join(base, "receiver")
	if err := os.Mkdir(work, 0700); err != nil {
		t.Fatal(err)
	}
	config := Config{Source: source, Length: int64(len(data)), Scenario: scenario,
		Average: 64 * 1024, StashBytes: 2 * MiB, MemoryBytes: 128 * MiB}
	path := filepath.Join(work, "config.json")
	writeConfig(t, path, config)
	return path, config
}

func TestMetadataProbeNeverMaterializesPayload(t *testing.T) {
	for _, scenario := range []string{"same", "rotate", "reverse", "full_changed"} {
		t.Run(scenario, func(t *testing.T) {
			work := t.TempDir()
			config := filepath.Join(work, "config.json")
			writeConfig(t, config, Config{ProbeChunks: 128, Average: 65536, Scenario: scenario,
				StashBytes: MiB, MemoryBytes: 128 * MiB})
			if err := run("metadata", config); err != nil {
				t.Fatal(err)
			}
			var result map[string]any
			b, err := os.ReadFile(filepath.Join(work, "metadata-result.json"))
			if err != nil || json.Unmarshal(b, &result) != nil || result["ok"] != true ||
				result["synthetic_only"] != true || result["last_stage"] != "dependency_graph" {
				t.Fatal("metadata planning did not finish")
			}
			entries, err := os.ReadDir(work)
			if err != nil {
				t.Fatal(err)
			}
			for _, entry := range entries {
				if entry.Name() != "config.json" && entry.Name() != "metadata-result.json" && entry.Name() != "metadata-progress.json" {
					t.Fatal("metadata probe created a payload or unexpected file")
				}
			}
		})
	}
}

func TestReplacedReceiverIsRefused(t *testing.T) {
	path, _ := fixture(t, "insert")
	if err := run("prepare", path); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(filepath.Dir(path), "receiver.bin")
	discarded := filepath.Join(filepath.Dir(path), "old-owned-receiver.bin")
	if err := os.Rename(target, discarded); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(target, []byte("unrelated local data"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := run("apply", path); err == nil {
		t.Fatal("replaced receiver was accepted")
	}
	data, err := os.ReadFile(target)
	if err != nil || string(data) != "unrelated local data" {
		t.Fatal("replacement data was modified")
	}
}

func TestLoopbackTransportRefusesOtherDestinations(t *testing.T) {
	path, cfg := fixture(t, "same")
	f, err := os.Open(cfg.Source)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	v, err := makeView(f, cfg, "same")
	if err != nil {
		t.Fatal(err)
	}
	idx, _, err := makeIndex(v, cfg.Average)
	if err != nil {
		t.Fatal(err)
	}
	info, err := f.Stat()
	if err != nil {
		t.Fatal(err)
	}
	_, store, closeAll, err := startPublisher(v, idx, cfg, info)
	if err != nil {
		t.Fatal(err)
	}
	defer closeAll()
	transport := store.client.Transport.(*http.Transport)
	if transport.Proxy != nil {
		t.Fatal("proxy configuration was enabled")
	}
	// The transport rejects this reserved address before any socket is opened.
	req, _ := http.NewRequest(http.MethodGet, "http://192.0.2.1/chunks/refused", nil)
	if _, err := store.client.Do(req); err == nil {
		t.Fatal("non-loopback destination was accepted")
	}
	req, _ = http.NewRequest(http.MethodGet, store.base+"/chunks/refused", nil)
	res, err := store.client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != http.StatusForbidden {
		t.Fatal("unauthenticated request was accepted")
	}
	if _, err := store.GetChunk(idx.Chunks[0].ID); err != nil {
		t.Fatal(err)
	}
	_ = path
}

func writeConfig(t *testing.T, path string, cfg Config) {
	t.Helper()
	b, err := json.Marshal(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, b, 0600); err != nil {
		t.Fatal(err)
	}
}

func assertReady(t *testing.T, config string) map[string]any {
	t.Helper()
	work := filepath.Dir(config)
	var result map[string]any
	b, err := os.ReadFile(filepath.Join(work, "result.json"))
	if err != nil || json.Unmarshal(b, &result) != nil {
		t.Fatal("missing result")
	}
	if result["ok"] != true || result["exact_hash_match"] != true {
		t.Fatal("output did not verify")
	}
	var state map[string]any
	b, err = os.ReadFile(filepath.Join(work, "state.json"))
	if err != nil || json.Unmarshal(b, &state) != nil || state["state"] != "ready" {
		t.Fatal("slot is not ready")
	}
	entries, err := os.ReadDir(work)
	if err != nil {
		t.Fatal(err)
	}
	allowed := map[string]bool{"config.json": true, "manifest.json": true, "receiver.bin": true,
		"target.caibx": true, "result.json": true, "state.json": true, "slot.lock": true,
		receiverIndexFile: true, receiverIndexMeta: true}
	for _, entry := range entries {
		if !allowed[entry.Name()] {
			t.Fatalf("unexpected staging or payload file in synthetic fixture: %s", entry.Name())
		}
	}
	return result
}

func TestInPlaceScenarios(t *testing.T) {
	for _, scenario := range []string{"same", "overwrite", "insert", "swap", "shrink", "all_changed"} {
		t.Run(scenario, func(t *testing.T) {
			path, _ := fixture(t, scenario)
			if err := run("prepare", path); err != nil {
				t.Fatal(err)
			}
			if err := run("apply", path); err != nil {
				t.Fatal(err)
			}
			result := assertReady(t, path)
			if scenario == "same" && result["publisher_payload_bytes"].(float64) != 0 {
				t.Fatal("unchanged content was downloaded")
			}
		})
	}
}

func TestZeroStashStillReconstructs(t *testing.T) {
	path, cfg := fixture(t, "swap")
	cfg.StashBytes = 0
	writeConfig(t, path, cfg)
	if err := run("prepare", path); err != nil {
		t.Fatal(err)
	}
	if err := run("apply", path); err != nil {
		t.Fatal(err)
	}
	assertReady(t, path)
}

func TestInterruptionAndCorruptionRepair(t *testing.T) {
	for _, kind := range []string{"interruption", "corruption"} {
		t.Run(kind, func(t *testing.T) {
			path, cfg := fixture(t, "all_changed")
			if err := run("prepare", path); err != nil {
				t.Fatal(err)
			}
			if kind == "interruption" {
				cfg.FailAfter = 2
			} else {
				cfg.CorruptAfter = 1
			}
			writeConfig(t, path, cfg)
			if err := run("apply", path); err == nil {
				t.Fatal("injected failure was accepted")
			}
			var state map[string]any
			b, _ := os.ReadFile(filepath.Join(filepath.Dir(path), "state.json"))
			if json.Unmarshal(b, &state) != nil || state["state"] == "ready" {
				t.Fatal("failed update became ready")
			}
			cfg.FailAfter, cfg.CorruptAfter = 0, 0
			writeConfig(t, path, cfg)
			if err := run("apply", path); err != nil {
				t.Fatal(err)
			}
			assertReady(t, path)
		})
	}
}

func TestPublicationChangeAndAliasAreRefused(t *testing.T) {
	path, cfg := fixture(t, "insert")
	if err := run("prepare", path); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(cfg.Source, os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		t.Fatal(err)
	}
	f.Write([]byte{1})
	f.Close()
	if err := run("apply", path); err == nil {
		t.Fatal("changed source publication accepted")
	}
	cfg.Source = filepath.Join(filepath.Dir(path), "receiver.bin")
	cfg.Length = 8 * MiB
	writeConfig(t, path, cfg)
	before, err := hashFile(cfg.Source)
	if err != nil {
		t.Fatal(err)
	}
	if err := run("prepare", path); err == nil {
		t.Fatal("source alias accepted")
	}
	after, err := hashFile(cfg.Source)
	if err != nil || before != after {
		t.Fatal("source alias was modified")
	}
}
