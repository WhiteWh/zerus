// Raw-container desync delivery and a paired model-free Windows executor.
package main

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"
	"sync/atomic"
	"time"

	d "github.com/folbricht/desync"
)

const MiB = int64(1024 * 1024)

var executorSourceIdentity = "unversioned"

type Config struct {
	Source       string `json:"source"`
	SourceOffset int64  `json:"source_offset"`
	Length       int64  `json:"length"`
	Scenario     string `json:"scenario"`
	Average      uint64 `json:"average"`
	StashBytes   int64  `json:"stash_bytes"`
	MemoryBytes  int64  `json:"memory_bytes"`
	FailAfter    int64  `json:"fail_after"`
	CrashAfter   int64  `json:"crash_after"`
	CrashPhase   string `json:"crash_phase,omitempty"`
	CorruptAfter int64  `json:"corrupt_after"`
	RateBytes    int64  `json:"rate_bytes"`
	ProbeChunks  int    `json:"probe_chunks"`
	ServeSeconds int    `json:"serve_seconds,omitempty"`
}

type part struct {
	start, size, source int64
	literal             []byte
	xor                 bool
}

// A view opens the original read-only and never materializes another build.
type view struct {
	file       *os.File
	base, size int64
	parts      []part
	reads      atomic.Int64
}

func makeView(f *os.File, cfg Config, scenario string) (*view, error) {
	v := &view{file: f, base: cfg.SourceOffset}
	add := func(source, length int64, literal []byte, xor bool) {
		if length == 0 {
			return
		}
		v.parts = append(v.parts, part{v.size, length, source, literal, xor})
		v.size += length
	}
	synthetic := func(n int64) []byte {
		b := make([]byte, n)
		for off := 0; off < len(b); {
			h := sha256.Sum256([]byte(fmt.Sprintf("zerus-public-synthetic-%d", off)))
			off += copy(b[off:], h[:])
		}
		return b
	}
	length := cfg.Length
	switch scenario {
	case "same":
		add(0, length, nil, false)
	case "overwrite":
		p, n := length/3, min(int64(65536), length/4)
		add(0, p, nil, false)
		add(0, n, synthetic(n), false)
		add(p+n, length-p-n, nil, false)
	case "insert":
		p, n := length/2, min(4*MiB, length/8)
		add(0, p, nil, false)
		add(0, n, synthetic(n), false)
		add(p, length-p, nil, false)
	case "swap":
		n, a, b := min(4*MiB, length/8), length/4, 3*length/4
		add(0, a, nil, false)
		add(b, n, nil, false)
		add(a+n, b-a-n, nil, false)
		add(a, n, nil, false)
		add(b+n, length-b-n, nil, false)
	case "shrink":
		p, n := length/2, min(4*MiB, length/8)
		add(0, p, nil, false)
		add(p+n, length-p-n, nil, false)
	case "all_changed":
		add(0, length, nil, true)
	default:
		return nil, errors.New("unsupported synthetic scenario")
	}
	return v, nil
}

func (v *view) ReadAt(buf []byte, offset int64) (int, error) {
	if offset < 0 {
		return 0, errors.New("negative offset")
	}
	total := 0
	for total < len(buf) && offset < v.size {
		found := false
		for _, p := range v.parts {
			if offset < p.start || offset >= p.start+p.size {
				continue
			}
			n := int(min(int64(len(buf)-total), p.start+p.size-offset))
			dst := buf[total : total+n]
			local := offset - p.start
			if p.literal != nil {
				copy(dst, p.literal[local:local+int64(n)])
			} else {
				got, err := v.file.ReadAt(dst, v.base+p.source+local)
				v.reads.Add(int64(got))
				if got != n || err != nil {
					return total + got, errors.New("source read failed")
				}
				if p.xor {
					for i := range dst {
						dst[i] ^= 165
					}
				}
			}
			total += n
			offset += int64(n)
			found = true
			break
		}
		if !found {
			return total, errors.New("invalid segment")
		}
	}
	if total < len(buf) {
		return total, io.EOF
	}
	return total, nil
}

func makeIndex(v *view, average uint64) (d.Index, string, error) {
	h := sha256.New()
	reader := io.TeeReader(io.NewSectionReader(v, 0, v.size), h)
	chunker, err := d.NewChunker(reader, average/4, average, average*4)
	if err != nil {
		return d.Index{}, "", err
	}
	idx := d.Index{Index: d.FormatIndex{
		FeatureFlags: d.CaFormatExcludeFile | d.CaFormatExcludeNoDump,
		ChunkSizeMin: average / 4, ChunkSizeAvg: average, ChunkSizeMax: average * 4,
	}}
	for {
		offset, data, err := chunker.Next()
		if err != nil {
			return idx, "", err
		}
		if len(data) == 0 {
			break
		}
		idx.Chunks = append(idx.Chunks, d.IndexChunk{
			ID: d.Digest.Sum(data), Start: offset, Size: uint64(len(data)),
		})
	}
	return idx, hex.EncodeToString(h.Sum(nil)), nil
}

func writeIndex(path string, idx d.Index) error {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer f.Close()
	if _, err := idx.WriteTo(f); err != nil {
		return err
	}
	return f.Sync()
}
func readIndex(path string) (d.Index, error) {
	f, err := os.Open(path)
	if err != nil {
		return d.Index{}, err
	}
	defer f.Close()
	return d.IndexFromReader(f)
}
func durableJSON(path string, value any) error {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	return durableBytes(path, b)
}
func durableBytes(path string, b []byte) error {
	f, err := openReceiptNext(path + ".next")
	if err != nil {
		return err
	}
	if _, err = f.Write(b); err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	return replaceDurable(path+".next", path)
}
func hashFile(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

type publisher struct {
	view                         *view
	chunks                       map[d.ChunkID]d.IndexChunk
	sourceInfo                   os.FileInfo
	cfg                          Config
	token                        string
	requests, bytes, corruptions atomic.Int64
}

func (p *publisher) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet ||
		subtle.ConstantTimeCompare([]byte(r.Header.Get("Authorization")), []byte(p.token)) != 1 {
		http.Error(w, "refused", http.StatusForbidden)
		return
	}
	raw := strings.TrimPrefix(r.URL.Path, "/chunks/")
	data, err := hex.DecodeString(raw)
	if !strings.HasPrefix(r.URL.Path, "/chunks/") || err != nil || len(data) != 32 {
		http.Error(w, "invalid chunk", http.StatusBadRequest)
		return
	}
	var id d.ChunkID
	copy(id[:], data)
	c, ok := p.chunks[id]
	if !ok {
		http.Error(w, "missing", http.StatusNotFound)
		return
	}
	info, err := p.view.file.Stat()
	if err != nil || !os.SameFile(info, p.sourceInfo) || info.Size() != p.sourceInfo.Size() ||
		!info.ModTime().Equal(p.sourceInfo.ModTime()) {
		http.Error(w, "source changed", http.StatusConflict)
		return
	}
	n := p.requests.Add(1)
	if p.cfg.CrashAfter > 0 && n >= p.cfg.CrashAfter {
		os.Exit(86)
	}
	if p.cfg.FailAfter > 0 && n >= p.cfg.FailAfter {
		http.Error(w, "injected interruption", http.StatusServiceUnavailable)
		return
	}
	buf := make([]byte, c.Size)
	if got, err := p.view.ReadAt(buf, int64(c.Start)); err != nil || uint64(got) != c.Size {
		http.Error(w, "source read failed", http.StatusConflict)
		return
	}
	if d.Digest.Sum(buf) != id {
		http.Error(w, "source content changed", http.StatusConflict)
		return
	}
	if p.cfg.CorruptAfter > 0 && n >= p.cfg.CorruptAfter && len(buf) > 0 {
		buf[0] ^= 255
		p.corruptions.Add(1)
	}
	if p.cfg.RateBytes > 0 {
		timer := time.NewTimer(time.Duration(float64(len(buf)) / float64(p.cfg.RateBytes) * float64(time.Second)))
		defer timer.Stop()
		select {
		case <-timer.C:
		case <-r.Context().Done():
			return
		}
	}
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("Content-Length", fmt.Sprint(len(buf)))
	nn, _ := w.Write(buf)
	p.bytes.Add(int64(nn))
}

type loopbackStore struct {
	base, token               string
	artifact, session         string
	client                    *http.Client
	known                     map[d.ChunkID]d.IndexChunk
	bytes, requests, verified atomic.Int64
	// retryBudget replaces chunkRetryBudget for a paired peer (chunkRetryWindow).
	retryBudget time.Duration
}

// chunkRetryWindow is how long a paired receiver keeps retrying one chunk:
// four of the peer's read-idle bounds, so the default 30 s bound tolerates a
// publisher host that is busy for two minutes, and a shorter configured bound
// holds the delivery as soon as the person asked for.
func chunkRetryWindow(peer PeerConfig) time.Duration {
	idle, err := readIdleBound(peer.ReadIdle)
	if err != nil {
		return chunkRetryBudget
	}
	return 4 * idle
}

// A chunk whose connection failed or whose body was cut is retried with a
// growing pause for up to chunkRetryBudget, so a publisher host that is busy
// for a minute does not fail a 5 GiB file; refusals, unknown chunks and
// digest mismatches never retry.
var (
	chunkRetryPause  = time.Second
	chunkRetryCap    = 10 * time.Second
	chunkRetryBudget = 2 * time.Minute
)

func (s *loopbackStore) GetChunk(id d.ChunkID) (*d.Chunk, error) {
	started := time.Now()
	pause := chunkRetryPause
	budget := chunkRetryBudget
	if s.retryBudget > 0 {
		budget = s.retryBudget
	}
	for {
		chunk, transient, err := s.getChunkOnce(id)
		if err == nil || !transient || time.Since(started)+pause > budget {
			return chunk, err
		}
		time.Sleep(pause)
		if pause *= 2; pause > chunkRetryCap {
			pause = chunkRetryCap
		}
	}
}

func (s *loopbackStore) getChunkOnce(id d.ChunkID) (*d.Chunk, bool, error) {
	req, err := http.NewRequest(http.MethodGet, s.base+"/chunks/"+hex.EncodeToString(id[:]), nil)
	if err != nil {
		return nil, false, err
	}
	req.Header.Set("Authorization", s.token)
	if s.artifact != "" {
		setPublicationHeaders(req.Header, s.artifact, s.session)
	}
	s.requests.Add(1)
	res, err := s.client.Do(req)
	if err != nil {
		return nil, !idleTimeout(err), errors.New("loopback transport failed")
	}
	defer res.Body.Close()
	if s.artifact != "" && !publicationHeadersMatch(res.Header, s.artifact, s.session) {
		return nil, false, errors.New("publication response changed")
	}
	if res.StatusCode != http.StatusOK {
		return nil, false, errors.New("publisher refused chunk")
	}
	c, ok := s.known[id]
	if !ok {
		return nil, false, errors.New("unexpected chunk")
	}
	b, err := io.ReadAll(io.LimitReader(res.Body, int64(c.Size)+1))
	s.bytes.Add(int64(len(b)))
	if err != nil {
		// The connection broke mid-body: transient. A stalled peer that
		// reached the configured read-idle bound is not retried, so the
		// delivery is held at once and resumes through a publication refresh.
		return nil, !idleTimeout(err), errors.New("invalid chunk length")
	}
	if uint64(len(b)) != c.Size {
		return nil, false, errors.New("invalid chunk length")
	}
	chunk, err := d.NewChunkWithID(id, b, false)
	if err == nil {
		s.verified.Add(int64(len(b)))
	}
	return chunk, false, err
}
// idleTimeout reports a read that reached its inactivity bound.
func idleTimeout(err error) bool {
	var timeout net.Error
	return errors.Is(err, errReadIdle) || (errors.As(err, &timeout) && timeout.Timeout())
}
func (s *loopbackStore) HasChunk(id d.ChunkID) (bool, error) { _, ok := s.known[id]; return ok, nil }
func (s *loopbackStore) String() string                      { return "loopback-raw-container" }
func (s *loopbackStore) Close() error                        { s.client.CloseIdleConnections(); return nil }

func startPublisher(v *view, idx d.Index, cfg Config, info os.FileInfo) (*publisher, *loopbackStore, func(), error) {
	// Jobs cannot choose an address, proxy, redirect destination or external interface.
	listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		return nil, nil, nil, err
	}
	secret := make([]byte, 32)
	if _, err := rand.Read(secret); err != nil {
		listener.Close()
		return nil, nil, nil, err
	}
	p := &publisher{view: v, sourceInfo: info, cfg: cfg, token: hex.EncodeToString(secret),
		chunks: make(map[d.ChunkID]d.IndexChunk)}
	for _, c := range idx.Chunks {
		p.chunks[c.ID] = c
	}
	server := &http.Server{Handler: p, ReadHeaderTimeout: 5 * time.Second, MaxHeaderBytes: 4096}
	go server.Serve(listener)
	address := listener.Addr().String()
	transport := &http.Transport{Proxy: nil, DisableCompression: true, MaxConnsPerHost: 2,
		DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
			if addr != address {
				return nil, errors.New("non-loopback connection refused")
			}
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "tcp4", address)
		}}
	store := &loopbackStore{base: "http://" + address, token: p.token, known: p.chunks,
		client: &http.Client{Transport: transport, Timeout: 30 * time.Second,
			CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return errors.New("redirect refused") }}}
	return p, store, func() { store.Close(); server.Close() }, nil
}

func run(command, configPath string) error {
	if err := localPath(configPath); err != nil {
		return err
	}
	configParents, _, err := holdDirectoryChain(filepath.Dir(configPath))
	defer func() {
		for _, f := range configParents {
			f.Close()
		}
	}()
	if err != nil {
		return err
	}
	if command == "serve" || command == "receive" || command == "receive-status" {
		return runSeparated(command, configPath)
	}
	if command == "tree-serve" || command == "tree-receive" || command == "tree-status" {
		return runTree(command, configPath)
	}
	if command == "identity-init" {
		return initializeIdentity(configPath)
	}
	if command == "executor-serve" {
		return serveExecutor(configPath)
	}
	if command == "executor-serve-shared" {
		return serveSharedExecutor(configPath)
	}
	if command == "appcontainer-qualify" {
		return runAppContainerQualification(configPath)
	}
	if command == "appcontainer-host-probe" {
		// Operator diagnostic: the host-compatibility cases activation runs on
		// a Windows 11 host below the qualified kernel, here on demand.
		var cfg struct {
			Root string `json:"root"`
		}
		if err := strictControl(configPath, &cfg); err != nil {
			return err
		}
		if err := runHostProbe(cfg.Root); err != nil {
			return errors.New("host probe failed: " + err.Error())
		}
		fmt.Println("host probe passed on " + osVersion())
		return nil
	}
	switch command {
	case "access-request-device":
		return runRequestDevice(configPath)
	case "access-admit-device":
		return runAdmitDevice(configPath)
	case "access-revoke":
		return runRevoke(configPath)
	case "access-show":
		return runShow(configPath)
	case "appcontainer-capability":
		return runPrintJobsCapability(configPath)
	}
	if strings.HasPrefix(command, "controller-") {
		return runController(command, configPath)
	}
	if command == "scenario-run" || command == "scenario-status" || command == "scenario-observe" || command == "scenario-ack" {
		return runScenario(command, configPath)
	}
	var cfg Config
	if _, err := readControl(configPath, &cfg); err != nil {
		return err
	}
	if command == "metadata" {
		if cfg.MemoryBytes > 0 {
			debug.SetMemoryLimit(cfg.MemoryBytes)
		}
		result, err := d.ZerusProbePlan(cfg.ProbeChunks, cfg.Average, cfg.Scenario, cfg.StashBytes,
			func(progress map[string]any) error {
				return durableJSON(filepath.Join(filepath.Dir(configPath), "metadata-progress.json"), progress)
			})
		if err != nil {
			return err
		}
		return durableJSON(filepath.Join(filepath.Dir(configPath), "metadata-result.json"), result)
	}
	if command != "prepare" && command != "apply" && command != "status" {
		return errors.New("unsupported command")
	}
	work, err := filepath.Abs(filepath.Dir(configPath))
	if err != nil {
		return err
	}
	slot, err := openSlot(work)
	if err != nil {
		return err
	}
	defer slot.Close()
	if command == "status" {
		m, digest, err := slot.manifest()
		if err != nil {
			return err
		}
		ready, err := slot.ready(m, digest)
		if err == nil {
			ready.Close()
		}
		return err
	}
	if cfg.Length < MiB || cfg.SourceOffset < 0 || cfg.Average < 4096 ||
		cfg.Average > 4*uint64(MiB) || cfg.StashBytes < 0 {
		return errors.New("invalid test bounds")
	}
	// This Windows qualification helper refuses UNC and relative source paths.
	volume := filepath.VolumeName(cfg.Source)
	if runtime.GOOS == "windows" && (!filepath.IsAbs(cfg.Source) || len(volume) != 2 || volume[1] != ':') {
		return errors.New("source must be an absolute local drive path")
	}
	if cfg.MemoryBytes > 0 {
		debug.SetMemoryLimit(cfg.MemoryBytes)
	}
	d.Digest = d.SHA256{}
	target := filepath.Join(work, "receiver.bin")
	sourceParents, _, err := holdDirectoryChain(filepath.Dir(cfg.Source))
	defer func() {
		for _, f := range sourceParents {
			f.Close()
		}
	}()
	if err != nil {
		return err
	}
	src, err := openSealed(cfg.Source, "", false)
	if err != nil {
		return err
	}
	defer src.Close()
	info, err := src.Stat()
	if err != nil || !info.Mode().IsRegular() || cfg.SourceOffset > info.Size() ||
		cfg.Length > info.Size()-cfg.SourceOffset {
		return errors.New("invalid source range")
	}
	if ti, err := os.Stat(target); err == nil && os.SameFile(info, ti) {
		return errors.New("source and receiver alias")
	}
	newView, err := makeView(src, cfg, cfg.Scenario)
	if err != nil {
		return err
	}
	if command == "prepare" {
		if _, err := os.Lstat(filepath.Join(work, "manifest.json")); err == nil {
			m, digest, err := slot.manifest()
			if err != nil {
				return err
			}
			if err := samePublication(m, cfg, src, newView); err != nil {
				return err
			}
			custody, err := holdReceiver(target, m.Receiver)
			if err != nil {
				return err
			}
			defer custody.Close()
			if _, err := slot.receipt(m, digest); os.IsNotExist(err) {
				// Crash between complete publication and the first receipt. No apply
				// is admitted without a receipt; recover only this initial gap.
				hash, e := hashFile(filepath.Join(work, "target.caibx"))
				if e != nil || hash != m.Artifact.Index {
					return errors.New("publication index changed")
				}
				r := receiptFor(m, digest)
				return slot.transition(&r, "prepared")
			} else {
				return err // Reconcile the same request; never reseed an existing slot.
			}
		} else if !os.IsNotExist(err) {
			return err
		}
		baseline, err := makeView(src, cfg, "same")
		if err != nil {
			return err
		}
		out, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			return err
		}
		_, copyErr := io.CopyBuffer(out, io.NewSectionReader(baseline, 0, baseline.size), make([]byte, 4*MiB))
		targetIdentity, identityErr := nativeIdentity(out)
		syncErr := out.Sync()
		closeErr := out.Close()
		if copyErr != nil || identityErr != nil || syncErr != nil || closeErr != nil {
			return errors.New("seed preparation failed")
		}
		start := time.Now()
		idx, sha, err := makeIndex(newView, cfg.Average)
		if err != nil {
			return err
		}
		if err := writeIndex(filepath.Join(work, "target.caibx"), idx); err != nil {
			return err
		}
		indexHash, err := hashFile(filepath.Join(work, "target.caibx"))
		if err != nil {
			return err
		}
		sourceID, err := nativeIdentity(src)
		if err != nil {
			return err
		}
		deliveryID, err := newIdentity()
		if err != nil {
			return err
		}
		destinationID, err := newIdentity()
		if err != nil {
			return err
		}
		a := Artifact{deliveryProtocol, deliveryBackend, sha, indexHash, idx.Length(), cfg.Average}
		m := Manifest{Protocol: deliveryProtocol, Artifact: a, ArtifactID: artifactID(a),
			DeliveryID: deliveryID, Destination: destinationID, Executor: "windows-local-qualification",
			Root: slot.root, Receiver: targetIdentity, Source: sourceID,
			SourceSize: info.Size(), SourceMTime: info.ModTime().UnixNano(),
			Expected: sha, TargetBytes: idx.Length(), Chunks: len(idx.Chunks),
			IndexTime: time.Since(start).Seconds(), Config: cfg}
		if err := durableJSON(filepath.Join(work, "manifest.json"), m); err != nil {
			return err
		}
		_, digest, err := slot.manifest()
		if err != nil {
			return err
		}
		r := receiptFor(m, digest)
		return slot.transition(&r, "prepared")
	}
	manifest, digest, err := slot.manifest()
	if err != nil {
		return err
	}
	_, err = slot.receipt(manifest, digest)
	if err != nil {
		return err
	}
	if err := samePublication(manifest, cfg, src, newView); err != nil {
		return err
	}
	indexFile, err := openSealed(filepath.Join(work, "target.caibx"), "", false)
	if err != nil {
		return err
	}
	defer indexFile.Close()
	indexHash, err := hashFile(filepath.Join(work, "target.caibx"))
	if err != nil || indexHash != manifest.Artifact.Index {
		return errors.New("publication index changed")
	}
	idx, err := readIndex(filepath.Join(work, "target.caibx"))
	if err != nil {
		return err
	}
	if idx.Length() != manifest.TargetBytes || len(idx.Chunks) != manifest.Chunks {
		return errors.New("publication index shape changed")
	}
	publisher, store, closeAll, err := startPublisher(newView, idx, cfg, info)
	if err != nil {
		return err
	}
	defer closeAll()
	return reconstruct(slot, manifest, digest, cfg, idx, store, func() error {
		after, err := src.Stat()
		if err != nil || !os.SameFile(info, after) || info.Size() != after.Size() || !info.ModTime().Equal(after.ModTime()) {
			return errors.New("source changed during apply")
		}
		return nil
	}, func() map[string]any {
		return map[string]any{
			"publisher_requests": publisher.requests.Load(), "publisher_payload_bytes": publisher.bytes.Load(),
			"publisher_source_read_bytes": newView.reads.Load(),
		}
	})
}

func crashCheckpoint(cfg Config, phase string) {
	if cfg.CrashPhase == phase {
		os.Exit(86)
	}
}

func main() {
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		_ = json.NewEncoder(os.Stdout).Encode(map[string]any{"product": "hgs-executor", "protocol": deliveryProtocol, "backend": deliveryBackend, "os": runtime.GOOS, "arch": runtime.GOARCH, "source_sha256": executorSourceIdentity, "model_runtime": false})
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "--help" {
		fmt.Println("hgs-executor COMMAND CONFIG\nCommands: identity-init, executor-serve, executor-serve-shared, controller-run, controller-deploy, controller-status, controller-capabilities, controller-observe, controller-collect, tree-serve, tree-receive, tree-status. Access administration: access-request-device, access-admit-device (also rotation), access-revoke, access-show. Disposable-runner qualification: appcontainer-qualify. Host compatibility below the qualified kernel: appcontainer-host-probe. Local qualification also supports prepare, apply, status, metadata, serve, receive and receive-status.")
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "contained-probe" {
		os.Exit(containedProbeMain())
	}
	if err := qualificationGate(); err != nil {
		fmt.Fprintln(os.Stderr, "qualification resource gate refused")
		os.Exit(2)
	}
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: hgs-executor COMMAND CONFIG (see --help)")
		os.Exit(2)
	}
	if err := run(os.Args[1], os.Args[2]); err != nil {
		privateFailure(os.Args[2], err)
		// No input paths, chunk hashes, tokens or detailed native errors reach stdout.
		fmt.Fprintln(os.Stderr, "executor operation failed; inspect private local state")
		os.Exit(1)
	}
	fmt.Println("executor operation completed")
}
