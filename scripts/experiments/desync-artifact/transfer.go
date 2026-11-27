// Separate-process qualification transport. It can never dial outside loopback.
package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"runtime/debug"
	"strconv"
	"strings"
	"sync"
	"time"

	d "github.com/folbricht/desync"
)

func newRemoteStore(offer Offer, client *http.Client, idx d.Index, peer PeerConfig) *loopbackStore {
	store := &loopbackStore{base: offerBase(offer) + "/v1", token: offer.Token, artifact: offer.ID, session: offer.Session, client: client, known: make(map[d.ChunkID]d.IndexChunk, len(idx.Chunks)),
		retryBudget: chunkRetryWindow(peer)}
	for _, chunk := range idx.Chunks {
		store.known[chunk.ID] = chunk
	}
	return store
}

const wireProtocol = 1
const maxWireIndex = 32 * MiB
const maxWireChunks = 500000

// This descriptor carries no publisher paths or native source identities.
// It is a local bearer capability and must not be logged or committed.
type Offer struct {
	Protocol int      `json:"protocol"`
	Artifact Artifact `json:"artifact"`
	ID       string   `json:"artifact_id"`
	Chunks   int      `json:"chunks"`
	IndexLen int64    `json:"index_bytes"`
	Endpoint string   `json:"endpoint"`
	Token    string   `json:"token"`
	Session  string   `json:"session"`
	Resource string   `json:"resource,omitempty"`
	Peer     string   `json:"peer_fingerprint,omitempty"`
}

type ReceiveConfig struct {
	Offer         Offer      `json:"offer"`
	Delivery      string     `json:"delivery_id"`
	AdoptReceiver string     `json:"adopt_receiver_identity,omitempty"`
	Stash         int64      `json:"stash_bytes"`
	Memory        int64      `json:"memory_bytes"`
	CrashPhase    string     `json:"crash_phase,omitempty"`
	Previous      string     `json:"previous_artifact_id,omitempty"`
	TreePath      string     `json:"tree_path,omitempty"`
	Peer          PeerConfig `json:"peer,omitempty"`
}

func strictControl(path string, value any) error {
	var raw json.RawMessage
	b, err := readControl(path, &raw)
	if err != nil {
		return err
	}
	return strictControlBytes(b, value)
}

// strictControlBytes decodes control bytes already read, refusing unknown
// fields.
func strictControlBytes(b []byte, value any) error {
	decoder := json.NewDecoder(bytes.NewReader(b))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return errors.New("unsupported control fields")
	}
	return nil
}

func setPublicationHeaders(h http.Header, artifact, session string) {
	h.Set("X-Zerus-Artifact", artifact)
	h.Set("X-Zerus-Publication", session)
}

func publicationHeadersMatch(h http.Header, artifact, session string) bool {
	return h.Get("X-Zerus-Artifact") == artifact && h.Get("X-Zerus-Publication") == session
}

func validOffer(o Offer) error {
	a := o.Artifact
	if (o.Resource != "" && !validHex(o.Resource, 32)) || o.Protocol != wireProtocol || a.Protocol != deliveryProtocol || a.Backend != deliveryBackend ||
		a.Average < 4096 || a.Average > 4*uint64(MiB) || a.Bytes < 0 || (a.Bytes == 0) != (o.Chunks == 0) ||
		!validHex(a.SHA256, 32) || !validHex(a.Index, 32) || o.ID != artifactID(a) ||
		!validHex(o.Token, 32) || !validHex(o.Session, 16) ||
		o.Chunks < 0 || o.Chunks > maxWireChunks || o.IndexLen != 104+40*int64(o.Chunks) || o.IndexLen > maxWireIndex {
		return errors.New("unsupported publication descriptor")
	}
	return validateEndpoint(o.Endpoint, o.Peer)
}

// Validate literal host/port before any dial. DNS, proxies, redirects, alternate
// interfaces, userinfo, query strings and externally supplied URL paths are out.
func fixedLoopbackClient(endpoint string) (*http.Client, error) {
	u, err := url.Parse(endpoint)
	if err != nil || u.Scheme != "http" || u.User != nil || u.Path != "" ||
		u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || u.Opaque != "" {
		return nil, errors.New("invalid loopback endpoint")
	}
	host, port, err := net.SplitHostPort(u.Host)
	n, portErr := strconv.Atoi(port)
	if err != nil || portErr != nil || host != "127.0.0.1" || n < 1 || n > 65535 || strconv.Itoa(n) != port {
		return nil, errors.New("non-loopback endpoint refused")
	}
	address := u.Host
	transport := &http.Transport{Proxy: nil, DisableCompression: true, MaxConnsPerHost: 2,
		MaxIdleConns: 2, MaxIdleConnsPerHost: 2, IdleConnTimeout: 5 * time.Second,
		MaxResponseHeaderBytes: 4096,
		DialContext: func(ctx context.Context, _, addr string) (net.Conn, error) {
			if addr != address {
				return nil, errors.New("alternate endpoint refused")
			}
			return (&net.Dialer{Timeout: 3 * time.Second}).DialContext(ctx, "tcp4", address)
		}}
	return &http.Client{Transport: transport, Timeout: 30 * time.Second,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return errors.New("redirect refused") }}, nil
}

type publicationHandler struct {
	offer    Offer
	chunks   *publisher
	index    *os.File
	admitted chan struct{}
	mu       sync.Mutex
	closing  bool
	active   sync.WaitGroup
}

func (p *publicationHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet || r.URL.RawQuery != "" ||
		subtle.ConstantTimeCompare([]byte(r.Header.Get("Authorization")), []byte(p.offer.Token)) != 1 ||
		!publicationHeadersMatch(r.Header, p.offer.ID, p.offer.Session) {
		http.Error(w, "refused", http.StatusForbidden)
		return
	}
	p.mu.Lock()
	if p.closing {
		p.mu.Unlock()
		http.Error(w, "closed", http.StatusServiceUnavailable)
		return
	}
	p.active.Add(1)
	p.mu.Unlock()
	defer p.active.Done()
	select {
	case p.admitted <- struct{}{}:
		defer func() { <-p.admitted }()
	default:
		http.Error(w, "busy", http.StatusTooManyRequests)
		return
	}
	setPublicationHeaders(w.Header(), p.offer.ID, p.offer.Session)
	if r.URL.Path == "/v1/index" {
		w.Header().Set("Content-Type", "application/octet-stream")
		w.Header().Set("Content-Length", strconv.FormatInt(p.offer.IndexLen, 10))
		_, _ = io.Copy(w, io.NewSectionReader(p.index, 0, p.offer.IndexLen))
		return
	}
	if !strings.HasPrefix(r.URL.Path, "/v1/chunks/") {
		http.Error(w, "unknown resource", http.StatusNotFound)
		return
	}
	clone := r.Clone(r.Context())
	copyURL := *r.URL
	copyURL.Path = strings.TrimPrefix(copyURL.Path, "/v1")
	clone.URL = &copyURL
	p.chunks.ServeHTTP(w, clone)
}

func runSeparated(command, configPath string) error {
	if command == "serve" {
		return servePublication(configPath)
	}
	var cfg ReceiveConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	if !validHex(cfg.Delivery, 16) || cfg.Stash < 0 || cfg.Memory < 0 {
		return errors.New("invalid receiver bounds")
	}
	if cfg.Memory > 0 {
		debug.SetMemoryLimit(cfg.Memory)
	}
	s, err := openSlot(filepath.Dir(configPath))
	if err != nil {
		return err
	}
	defer s.Close()
	if command == "receive-status" {
		m, digest, err := s.manifest()
		if err != nil {
			return err
		}
		if m.DeliveryID != cfg.Delivery || m.ArtifactID != cfg.Offer.ID || m.Source != "publication:"+cfg.Offer.ID {
			return errors.New("requested delivery changed")
		}
		f, err := s.ready(m, digest)
		if err == nil {
			f.Close()
		}
		return err // No connection or token is needed to verify already ready data.
	}
	if err := validOffer(cfg.Offer); err != nil {
		return err
	}
	d.Digest = d.SHA256{}
	client, err := dataClient(cfg.Offer.Endpoint, cfg.Offer.Peer, cfg.Peer)
	if err != nil {
		return err
	}
	defer client.CloseIdleConnections()
	indexDownloadBytes := int64(0)
	if _, err := os.Lstat(filepath.Join(s.work, "target.caibx")); os.IsNotExist(err) {
		indexDownloadBytes = cfg.Offer.IndexLen
	}
	m, digest, err := prepareRemoteSlot(s, cfg, client)
	if err != nil {
		return err
	}
	index, err := openSealed(filepath.Join(s.work, "target.caibx"), "", false)
	if err != nil {
		return err
	}
	defer index.Close()
	idx, err := checkedIndex(index, cfg.Offer)
	if err != nil {
		return err
	}
	store := &loopbackStore{base: offerBase(cfg.Offer) + "/v1", token: cfg.Offer.Token,
		artifact: cfg.Offer.ID, session: cfg.Offer.Session, client: client,
		known: make(map[d.ChunkID]d.IndexChunk, len(idx.Chunks)), retryBudget: chunkRetryWindow(cfg.Peer)}
	for _, chunk := range idx.Chunks {
		store.known[chunk.ID] = chunk
	}
	return reconstruct(s, m, digest, Config{Average: m.Artifact.Average, StashBytes: cfg.Stash,
		CrashPhase: cfg.CrashPhase}, idx, store, func() error { return nil }, func() map[string]any {
		return withScope(transportScope(cfg.Offer.Endpoint, cfg.Offer.Peer), map[string]any{"separate_processes": true, "downloaded_payload_bytes": store.bytes.Load(),
			"chunk_requests": store.requests.Load(), "verified_payload_bytes": store.verified.Load(),
			"downloaded_index_bytes": indexDownloadBytes,
			"source_access_required": false})
	})
}

func servePublication(configPath string) error {
	var cfg Config
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	if cfg.Length < MiB || cfg.SourceOffset < 0 || cfg.Average < 4096 || cfg.Average > 4*uint64(MiB) ||
		cfg.StashBytes < 0 || cfg.ServeSeconds < 1 || cfg.ServeSeconds > 300 {
		return errors.New("invalid publisher bounds")
	}
	if cfg.MemoryBytes > 0 {
		debug.SetMemoryLimit(cfg.MemoryBytes)
	}
	s, err := openSlot(filepath.Dir(configPath))
	if err != nil {
		return err
	}
	defer s.Close()
	parents, _, err := holdDirectoryChain(filepath.Dir(cfg.Source))
	defer func() {
		for _, f := range parents {
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
	if err != nil || cfg.SourceOffset > info.Size() || cfg.Length > info.Size()-cfg.SourceOffset {
		return errors.New("invalid source range")
	}
	d.Digest = d.SHA256{}
	v, err := makeView(src, cfg, cfg.Scenario)
	if err != nil {
		return err
	}
	idx, hash, err := makeIndex(v, cfg.Average)
	if err != nil {
		return err
	}
	if len(idx.Chunks) > maxWireChunks {
		return errors.New("publication exceeds qualification index budget")
	}
	indexPath := filepath.Join(s.work, "publication.caibx")
	f, err := openReceiptNext(indexPath + ".next")
	if err != nil {
		return err
	}
	_, writeErr := idx.WriteTo(f)
	syncErr, closeErr := f.Sync(), f.Close()
	if writeErr != nil || syncErr != nil || closeErr != nil {
		return errors.New("publication index write failed")
	}
	if err := replaceDurable(indexPath+".next", indexPath); err != nil {
		return err
	}
	index, err := openSealed(indexPath, "", false)
	if err != nil {
		return err
	}
	defer index.Close()
	indexInfo, err := index.Stat()
	if err != nil {
		return err
	}
	indexHash, err := hashFile(indexPath)
	if err != nil {
		return err
	}
	token, err := newIdentity()
	if err != nil {
		return err
	}
	token2, err := newIdentity()
	if err != nil {
		return err
	}
	session, err := newIdentity()
	if err != nil {
		return err
	}
	listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		return err
	}
	defer listener.Close()
	a := Artifact{deliveryProtocol, deliveryBackend, hash, indexHash, idx.Length(), cfg.Average}
	offer := Offer{Protocol: wireProtocol, Artifact: a, ID: artifactID(a), Chunks: len(idx.Chunks), IndexLen: indexInfo.Size(),
		Endpoint: "http://" + listener.Addr().String(), Token: token + token2, Session: session}
	if err := validOffer(offer); err != nil {
		return err
	}
	p := &publisher{view: v, chunks: make(map[d.ChunkID]d.IndexChunk, len(idx.Chunks)), sourceInfo: info,
		cfg: cfg, token: offer.Token}
	for _, chunk := range idx.Chunks {
		p.chunks[chunk.ID] = chunk
	}
	handler := &publicationHandler{offer: offer, chunks: p, index: index, admitted: make(chan struct{}, 2)}
	server := &http.Server{Handler: handler, ReadHeaderTimeout: 5 * time.Second, IdleTimeout: 5 * time.Second,
		ReadTimeout: 5 * time.Second, WriteTimeout: 30 * time.Second, MaxHeaderBytes: 4096}
	stop := func() {
		handler.mu.Lock()
		handler.closing = true
		handler.mu.Unlock()
		_ = server.Close()
		handler.active.Wait()
	}
	defer stop()
	finished := make(chan error, 1)
	go func() { finished <- server.Serve(listener) }()
	if err := durableJSON(filepath.Join(s.work, "offer.json"), offer); err != nil {
		return err
	}
	timer := time.NewTimer(time.Duration(cfg.ServeSeconds) * time.Second)
	defer timer.Stop()
	select {
	case err := <-finished:
		return err
	case <-timer.C:
		stop() // Settle admitted requests before the source lease ends.
		<-finished
		return durableJSON(filepath.Join(s.work, "server-result.json"), map[string]any{
			"stopped": true, "publisher_requests": p.requests.Load(), "publisher_payload_bytes": p.bytes.Load()})
	}
}

func checkedIndex(f *os.File, offer Offer) (d.Index, error) {
	info, err := f.Stat()
	if err != nil || info.Size() != offer.IndexLen {
		return d.Index{}, errors.New("index length mismatch")
	}
	h := sha256.New()
	if _, err := io.Copy(h, io.NewSectionReader(f, 0, info.Size())); err != nil || hex.EncodeToString(h.Sum(nil)) != offer.Artifact.Index {
		return d.Index{}, errors.New("index digest mismatch")
	}
	// Reject allocation-bearing archive elements before desync's general decoder
	// sees them. This wire revision only admits the pinned raw .caibx shape.
	var headers [64]byte
	if _, err := f.ReadAt(headers[:], 0); err != nil ||
		binary.LittleEndian.Uint64(headers[0:8]) != 48 ||
		binary.LittleEndian.Uint64(headers[8:16]) != d.CaFormatIndex ||
		binary.LittleEndian.Uint64(headers[48:56]) != ^uint64(0) ||
		binary.LittleEndian.Uint64(headers[56:64]) != d.CaFormatTable {
		return d.Index{}, errors.New("unsupported wire index framing")
	}
	idx, err := d.IndexFromReader(io.NewSectionReader(f, 0, info.Size()))
	if err != nil {
		return idx, errors.New("invalid publication index")
	}
	a := offer.Artifact
	if len(idx.Chunks) != offer.Chunks || idx.Length() != a.Bytes || idx.Index.ChunkSizeAvg != a.Average ||
		idx.Index.ChunkSizeMin != a.Average/4 || idx.Index.ChunkSizeMax != a.Average*4 ||
		idx.Index.FeatureFlags != d.CaFormatExcludeFile|d.CaFormatExcludeNoDump {
		return idx, errors.New("index bounds mismatch")
	}
	var offset uint64
	for _, c := range idx.Chunks {
		if c.Start != offset || c.Size == 0 || c.Size > a.Average*4 || c.Size > uint64(a.Bytes)-offset {
			return idx, errors.New("invalid chunk range")
		}
		offset += c.Size
	}
	if offset != uint64(a.Bytes) {
		return idx, errors.New("index coverage mismatch")
	}
	return idx, nil
}

func downloadIndex(s *slot, offer Offer, client *http.Client) error {
	return downloadIndexAs(s, offer, client, "target.caibx")
}

func downloadIndexAs(s *slot, offer Offer, client *http.Client, name string) error {
	path := filepath.Join(s.work, name)
	if f, err := openSealed(path, "", false); err == nil {
		defer f.Close()
		_, err := checkedIndex(f, offer)
		return err // Never silently replace an existing, mismatched pinned index.
	} else if !os.IsNotExist(err) {
		return err
	}
	req, _ := http.NewRequest(http.MethodGet, offerBase(offer)+"/v1/index", nil)
	req.Header.Set("Authorization", offer.Token)
	setPublicationHeaders(req.Header, offer.ID, offer.Session)
	res, err := client.Do(req)
	if err != nil {
		return errors.New("publication index unavailable")
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK || res.ContentLength != offer.IndexLen ||
		!publicationHeadersMatch(res.Header, offer.ID, offer.Session) {
		return errors.New("publication index response refused")
	}
	f, err := openReceiptNext(path + ".next")
	if err != nil {
		return err
	}
	h := sha256.New()
	n, copyErr := io.Copy(io.MultiWriter(f, h), io.LimitReader(res.Body, offer.IndexLen+1))
	syncErr, closeErr := f.Sync(), f.Close()
	if copyErr != nil || syncErr != nil || closeErr != nil || n != offer.IndexLen || hex.EncodeToString(h.Sum(nil)) != offer.Artifact.Index {
		return errors.New("publication index transfer failed")
	}
	return replaceDurable(path+".next", path)
}

func prepareRemoteSlot(s *slot, cfg ReceiveConfig, client *http.Client) (Manifest, string, error) {
	if _, err := os.Lstat(filepath.Join(s.work, "generation-intent.json")); err == nil {
		if err := finishGeneration(s, cfg); err != nil {
			return Manifest{}, "", err
		}
	} else if !os.IsNotExist(err) {
		return Manifest{}, "", err
	}
	m, digest, err := s.manifest()
	if err == nil {
		if m.DeliveryID != cfg.Delivery || m.ArtifactID != cfg.Offer.ID || m.Source != "publication:"+cfg.Offer.ID || m.Chunks != cfg.Offer.Chunks {
			if err := advanceGeneration(s, cfg, m, digest, client); err != nil {
				return m, digest, err
			}
			m, digest, err = s.manifest()
			if err != nil {
				return m, digest, err
			}
		}
		if _, err := s.receipt(m, digest); os.IsNotExist(err) {
			target, e := receiverPath(s, m)
			if e != nil {
				return m, digest, e
			}
			custody, e := holdReceiver(target, m.Receiver)
			if e != nil {
				return m, digest, e
			}
			defer custody.Close()
			if e := downloadIndex(s, cfg.Offer, client); e != nil {
				return m, digest, e
			}
			r := receiptFor(m, digest)
			if e := s.transition(&r, "prepared"); e != nil {
				return m, digest, e
			}
		} else if err != nil {
			return m, digest, err
		}
		if err := downloadIndex(s, cfg.Offer, client); err != nil {
			return m, digest, err
		}
		return m, digest, nil
	}
	if !os.IsNotExist(err) {
		return m, "", err
	}
	if cfg.Previous != "" {
		return m, "", errors.New("previous generation is absent")
	}
	if err := downloadIndex(s, cfg.Offer, client); err != nil {
		return m, "", err
	}
	target, err := receiverPath(s, Manifest{TreePath: cfg.TreePath})
	if err != nil {
		return m, "", err
	}
	var out *os.File
	if cfg.AdoptReceiver == "" {
		out, err = reserveReceiver(s, cfg, target)
	} else {
		out, err = openSealed(target, cfg.AdoptReceiver, true)
	}
	if err != nil {
		return m, "", fmt.Errorf("receiver creation or explicit adoption refused: %w", err)
	}
	defer out.Close()
	identity, err := nativeIdentity(out)
	if err != nil {
		return m, "", err
	}
	if err := out.Sync(); err != nil {
		return m, "", err
	}
	destination, err := newIdentity()
	if err != nil {
		return m, "", err
	}
	a := cfg.Offer.Artifact
	m = Manifest{Protocol: deliveryProtocol, Artifact: a, ArtifactID: cfg.Offer.ID,
		DeliveryID: cfg.Delivery, Executor: "windows-local-qualification", Destination: destination,
		Root: s.root, Receiver: identity, Source: "publication:" + cfg.Offer.ID,
		Expected: a.SHA256, TargetBytes: a.Bytes, Chunks: cfg.Offer.Chunks,
		Config: Config{Average: a.Average}, TreePath: cfg.TreePath}
	if err := durableJSON(filepath.Join(s.work, "manifest.json"), m); err != nil {
		return m, "", err
	}
	_, digest, err = s.manifest()
	if err != nil {
		return m, "", err
	}
	r := receiptFor(m, digest)
	if err := s.transition(&r, "prepared"); err != nil {
		return m, "", err
	}
	return m, digest, nil
}
