// Experimental single-file delivery contract. All receipts stay local.
package main

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
)

const deliveryProtocol = 1
const deliveryBackend = "desync-717d63b1756554abc470671ea591ad6d25e6059d"
const maxControlBytes = 64 * 1024

type Artifact struct {
	Protocol int    `json:"protocol"`
	Backend  string `json:"backend"`
	SHA256   string `json:"sha256"`
	Index    string `json:"index_sha256"`
	Bytes    int64  `json:"bytes"`
	Average  uint64 `json:"average"`
}

type Manifest struct {
	Protocol    int      `json:"protocol"`
	Artifact    Artifact `json:"artifact"`
	ArtifactID  string   `json:"artifact_id"`
	DeliveryID  string   `json:"delivery_id"`
	Executor    string   `json:"executor"`
	Destination string   `json:"destination_id"`
	Root        string   `json:"root_identity"`
	Receiver    string   `json:"receiver_identity"`
	Source      string   `json:"source_identity"`
	SourceSize  int64    `json:"source_size"`
	SourceMTime int64    `json:"source_mtime"`
	Expected    string   `json:"expected_sha256"`
	TargetBytes int64    `json:"target_bytes"`
	Chunks      int      `json:"chunks"`
	IndexTime   float64  `json:"index_seconds"`
	Config      Config   `json:"config"`
	TreePath    string   `json:"tree_path,omitempty"`
}

type Receipt struct {
	Protocol    int    `json:"protocol"`
	Manifest    string `json:"manifest_sha256"`
	ArtifactID  string `json:"artifact_id"`
	DeliveryID  string `json:"delivery_id"`
	Destination string `json:"destination_id"`
	Root        string `json:"root_identity"`
	Receiver    string `json:"receiver_identity"`
	Sequence    uint64 `json:"sequence"`
	State       string `json:"state"`
	Expected    string `json:"expected_sha256"`
}

// Slot custody serializes cooperating deliveries and future test admission.
// Native object handles, rather than a stale PID/lease timeout, own the lock.
type slot struct {
	work, root string
	guard      *os.File
	parents    []*os.File
}

func (s *slot) Close() {
	if s.guard != nil {
		s.guard.Close()
	}
	for i := len(s.parents) - 1; i >= 0; i-- {
		s.parents[i].Close()
	}
}

func openSlot(work string) (*slot, error) {
	s := &slot{work: work}
	var err error
	s.parents, s.root, err = holdDirectoryChain(work)
	if err == nil {
		s.guard, err = lockSlot(filepath.Join(work, "slot.lock"))
	}
	if err != nil {
		s.Close()
		return nil, errors.New("deployment slot is unavailable")
	}
	return s, nil
}

func readControl(path string, value any) ([]byte, error) {
	f, err := openSealed(path, "", false)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, maxControlBytes+1))
	if err != nil || len(b) > maxControlBytes {
		return nil, errors.New("control record exceeds limit")
	}
	if err := json.Unmarshal(b, value); err != nil {
		return nil, errors.New("invalid control record")
	}
	return b, nil
}

func sumBytes(b []byte) string {
	h := sha256.Sum256(b)
	return hex.EncodeToString(h[:])
}

func artifactID(a Artifact) string {
	b, _ := json.Marshal(a)
	return sumBytes(b)
}

func newIdentity() (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return hex.EncodeToString(b), nil
}

func validHex(s string, n int) bool {
	b, err := hex.DecodeString(s)
	return err == nil && len(b) == n && hex.EncodeToString(b) == s
}

func (s *slot) manifest() (Manifest, string, error) {
	var m Manifest
	b, err := readControl(filepath.Join(s.work, "manifest.json"), &m)
	if err != nil {
		return m, "", err
	}
	if m.Protocol != deliveryProtocol || m.Artifact.Protocol != deliveryProtocol ||
		m.Artifact.Backend != deliveryBackend || m.Executor != "windows-local-qualification" ||
		!validHex(m.DeliveryID, 16) || !validHex(m.Destination, 16) ||
		!validHex(m.Expected, 32) || !validHex(m.Artifact.Index, 32) ||
		m.Root != s.root || m.Receiver == "" || m.Source == "" ||
		m.ArtifactID != artifactID(m.Artifact) || m.Artifact.SHA256 != m.Expected ||
		m.Artifact.Bytes != m.TargetBytes || m.Artifact.Average != m.Config.Average ||
		m.TargetBytes < 0 || m.Chunks < 0 || (m.TargetBytes == 0) != (m.Chunks == 0) ||
		m.Config.Average < 4096 || m.Config.Average > 4*uint64(MiB) {
		return m, "", errors.New("unsupported or mismatched delivery manifest")
	}
	return m, sumBytes(b), nil
}

func receiptFor(m Manifest, digest string) Receipt {
	return Receipt{Protocol: deliveryProtocol, Manifest: digest, ArtifactID: m.ArtifactID,
		DeliveryID: m.DeliveryID, Destination: m.Destination, Root: m.Root,
		Receiver: m.Receiver, Expected: m.Expected}
}

func (s *slot) receipt(m Manifest, digest string) (Receipt, error) {
	var r Receipt
	if _, err := readControl(filepath.Join(s.work, "state.json"), &r); err != nil {
		return r, err
	}
	want := receiptFor(m, digest)
	want.Sequence, want.State = r.Sequence, r.State
	if r != want || r.Sequence == 0 ||
		(r.State != "prepared" && r.State != "applying" && r.State != "verifying" && r.State != "ready") {
		return r, errors.New("receipt identity or lifecycle mismatch")
	}
	return r, nil
}

func (s *slot) transition(r *Receipt, next string) error {
	allowed := (r.Sequence == 0 && r.State == "" && next == "prepared") ||
		(next == "applying" && r.Sequence > 0 &&
			(r.State == "prepared" || r.State == "applying" || r.State == "verifying" || r.State == "ready")) ||
		(r.State == "applying" && next == "verifying") ||
		(r.State == "verifying" && next == "ready")
	if !allowed || r.Sequence == ^uint64(0) {
		return errors.New("invalid delivery transition")
	}
	n := *r
	n.Sequence++
	n.State = next
	if err := durableJSON(filepath.Join(s.work, "state.json"), n); err != nil {
		return err
	}
	*r = n
	return nil
}

func privateFailure(configPath string, failure error) {
	if err := localPath(configPath); err != nil {
		return
	}
	s, err := openSlot(filepath.Dir(configPath))
	if err != nil {
		return
	}
	defer s.Close()
	message := failure.Error()
	if len(message) > 8192 {
		message = "operation refused; diagnostic exceeds limit"
	}
	// No diagnostic path is created through a network drive or redirected root.
	_ = durableJSON(filepath.Join(s.work, "error.json"), map[string]string{"error": message})
}

func samePublication(m Manifest, cfg Config, src *os.File, v *view) error {
	id, err := nativeIdentity(src)
	info, statErr := src.Stat()
	if err != nil || statErr != nil || m.Source != id || m.SourceSize != info.Size() ||
		m.SourceMTime != info.ModTime().UnixNano() || m.Config.Source != cfg.Source ||
		m.Config.SourceOffset != cfg.SourceOffset || m.Config.Length != cfg.Length ||
		m.Config.Scenario != cfg.Scenario || m.Config.Average != cfg.Average {
		return errors.New("publication changed")
	}
	// Metadata alone cannot detect same-size edits with a restored timestamp.
	h := sha256.New()
	if _, err := io.Copy(h, io.NewSectionReader(v, 0, v.size)); err != nil ||
		hex.EncodeToString(h.Sum(nil)) != m.Expected {
		return errors.New("published bytes changed")
	}
	return nil
}

// Readiness is a verified capability held under custody, never just a JSON bit.
// Callers must retain both this handle and the slot lock through any future run.
func (s *slot) ready(m Manifest, digest string) (*os.File, error) {
	if _, err := os.Lstat(filepath.Join(s.work, "generation-intent.json")); !os.IsNotExist(err) {
		return nil, errors.New("generation transition is pending")
	}
	r, err := s.receipt(m, digest)
	if err != nil || r.State != "ready" {
		return nil, errors.New("delivery is not ready")
	}
	target, err := receiverPath(s, m)
	if err != nil {
		return nil, err
	}
	f, err := openSealed(target, m.Receiver, false)
	if err != nil {
		return nil, errors.New("ready receiver is unavailable")
	}
	if err := verifyHandle(f, m); err != nil {
		f.Close()
		// If invalidation cannot be persisted, this admission still fails closed.
		if e := s.transition(&r, "applying"); e != nil {
			return nil, e
		}
		return nil, err
	}
	return f, nil
}

func verifyHandle(f *os.File, m Manifest) error {
	info, err := f.Stat()
	if err != nil || info.Size() != m.TargetBytes {
		return errors.New("receiver length mismatch")
	}
	h := sha256.New()
	if _, err := io.Copy(h, io.NewSectionReader(f, 0, info.Size())); err != nil ||
		hex.EncodeToString(h.Sum(nil)) != m.Expected {
		return errors.New("receiver content mismatch")
	}
	return nil
}
