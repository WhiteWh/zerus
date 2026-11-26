// The builder-side controller holds raw source handles only until deployment.
// Its durable job identity is also the executor's deduplication key.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	d "github.com/folbricht/desync"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"time"
)

type ControllerConfig struct {
	Endpoint     string                     `json:"endpoint"`
	Peer         PeerConfig                 `json:"peer"`
	Job          string                     `json:"job_id"`
	Slot         string                     `json:"slot"`
	Device       string                     `json:"device_id"`
	ArtifactRoot string                     `json:"artifact_root"`
	ScenarioRoot string                     `json:"scenario_root"`
	Listen       string                     `json:"listen"`
	Average      uint64                     `json:"average"`
	Memory       int64                      `json:"memory_bytes"`
	Seconds      int                        `json:"timeout_seconds"`
	Parameters   map[string]json.RawMessage `json:"parameters"`
	Observation  *Observation               `json:"observation,omitempty"`
	Performance  *PerformancePolicy         `json:"performance,omitempty"`
}
type ControllerState struct {
	Protocol     int                `json:"protocol"`
	Job          string             `json:"job_id"`
	Digest       string             `json:"config_sha256"`
	Root         string             `json:"root_identity"`
	Executor     string             `json:"executor"`
	Device       string             `json:"device_id"`
	Artifact     string             `json:"artifact_id,omitempty"`
	Scenario     string             `json:"scenario_id,omitempty"`
	Phase        string             `json:"phase"`
	Updated      int64              `json:"updated"`
	Ack          string             `json:"acknowledgement_id,omitempty"`
	Collected    bool               `json:"collected,omitempty"`
	Instructions string             `json:"instructions,omitempty"`
	Performance  *PerformanceReport `json:"performance,omitempty"`
}
type ExecutorSnapshot struct {
	Submission ExecutorSubmission `json:"submission"`
	Result     ScenarioState      `json:"result"`
}
type executorClient struct {
	client   *http.Client
	endpoint string
}

func (c executorClient) request(method, path string, value any, headers map[string]string, maximum int64) ([]byte, http.Header, int, error) {
	var body io.Reader
	if value != nil {
		b, err := json.Marshal(value)
		if err != nil {
			return nil, nil, 0, err
		}
		if len(b) > maxControlBytes {
			return nil, nil, 0, errors.New("control request exceeds limit")
		}
		body = bytes.NewReader(b)
	}
	r, err := http.NewRequest(method, c.endpoint+path, body)
	if err != nil {
		return nil, nil, 0, err
	}
	for key, value := range headers {
		r.Header.Set(key, value)
	}
	response, err := c.client.Do(r)
	if err != nil {
		return nil, nil, 0, errors.New("paired executor unavailable")
	}
	defer response.Body.Close()
	b, err := io.ReadAll(io.LimitReader(response.Body, maximum+1))
	if err != nil || int64(len(b)) > maximum {
		return nil, nil, response.StatusCode, errors.New("executor response exceeds limit")
	}
	if response.StatusCode != 200 {
		return nil, response.Header, response.StatusCode, fmt.Errorf("executor refused operation (%d)", response.StatusCode)
	}
	return b, response.Header, 200, nil
}
func (c executorClient) snapshot(job string) (ExecutorSnapshot, int, error) {
	var value ExecutorSnapshot
	b, _, status, err := c.request("GET", "/executor/v1/jobs/"+job, nil, nil, maxTreeBytes)
	if err == nil {
		err = json.Unmarshal(b, &value)
	}
	if err == nil && (value.Submission.Request.Job != job || value.Submission.Executor == "" || value.Submission.Digest != executorRequestDigest(value.Submission.Request)) {
		err = errors.New("executor receipt identity changed")
	}
	return value, status, err
}
func controllerDigest(cfg ControllerConfig) string {
	cfg.Observation = nil
	b, _ := json.Marshal(cfg)
	return sumBytes(b)
}
func saveController(work string, state *ControllerState) error {
	state.Updated = time.Now().UnixMilli()
	return durableJSON(filepath.Join(work, "controller-state.json"), state)
}

type localPublication struct {
	offer    TreeOffer
	release  chan struct{}
	finished chan error
	closed   bool
}

func startPublication(work, source string, cfg ControllerConfig) (*localPublication, error) {
	if err := os.Mkdir(work, 0700); err != nil && !os.IsExist(err) {
		return nil, err
	}
	p := &localPublication{release: make(chan struct{}), finished: make(chan error, 1)}
	ready := make(chan TreeOffer, 1)
	value := TreeConfig{SourceRoot: source, Average: cfg.Average, ServeSeconds: cfg.Seconds, Memory: cfg.Memory / 2, Peer: PeerConfig{Identity: cfg.Peer.Identity, Fingerprint: cfg.Peer.Fingerprint, Listen: cfg.Listen}}
	path := filepath.Join(work, "publish.json")
	if err := durableJSON(path, value); err != nil {
		return nil, err
	}
	go func() {
		p.finished <- publishTreeUntil(path, value, func(offer TreeOffer) error { ready <- offer; return nil }, p.release)
	}()
	select {
	case p.offer = <-ready:
		return p, nil
	case err := <-p.finished:
		if err == nil {
			err = errors.New("publication ended before readiness")
		}
		return nil, err
	}
}
func (p *localPublication) close() {
	if p != nil && !p.closed {
		p.closed = true
		close(p.release)
		<-p.finished
	}
}

func controllerPhase(snapshot ExecutorSnapshot) string {
	if snapshot.Submission.Phase == "executing" && activeScenarioStatus(snapshot.Result.Status) {
		return snapshot.Result.Status
	}
	return snapshot.Submission.Phase
}
func deployed(phase string) bool { return phase != "queued" && phase != "deploying" && phase != "held" }
func terminal(phase string) bool {
	return deployed(phase) && phase != "executing" && !activeScenarioStatus(phase)
}

func runController(command, configPath string) error {
	d.Digest = d.SHA256{}
	var cfg ControllerConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	if err := validPerformance(cfg.Performance); err != nil {
		return err
	}
	if !validHex(cfg.Job, 16) || !validDevice(cfg.Slot) || !validDevice(cfg.Device) || cfg.Memory < 256*MiB || cfg.Memory > 4*1024*MiB || cfg.Seconds < 1 || cfg.Seconds > 604800 || cfg.Average < 64*1024 || cfg.Average > 4*1024*1024 || len(cfg.Parameters) > 32 {
		return errors.New("invalid controller configuration")
	}
	if err := validateEndpoint(cfg.Endpoint, cfg.Peer.Fingerprint); err != nil {
		return err
	}
	if cfg.Peer.Fingerprint == "" {
		return errors.New("controller requires paired HTTPS")
	}
	client, err := dataClient(cfg.Endpoint, cfg.Peer.Fingerprint, cfg.Peer)
	if err != nil {
		return err
	}
	defer client.CloseIdleConnections()
	c := executorClient{client, cfg.Endpoint}
	_, builder, err := loadIdentity(cfg.Peer.Identity)
	if err != nil {
		return err
	}
	work := filepath.Dir(configPath)
	parents, root, err := holdDirectoryChain(work)
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	if command == "controller-run" || command == "controller-deploy" || command == "controller-collect" {
		s, err := openSlot(work)
		if err != nil {
			return err
		}
		defer s.Close()
	}
	state := ControllerState{Protocol: deliveryProtocol, Job: cfg.Job, Digest: controllerDigest(cfg), Root: root, Executor: cfg.Peer.Fingerprint, Device: cfg.Device, Phase: "new"}
	var prior ControllerState
	if _, err := readControl(filepath.Join(work, "controller-state.json"), &prior); err == nil {
		if prior.Protocol != deliveryProtocol || prior.Job != state.Job || prior.Digest != state.Digest || prior.Root != root || prior.Executor != state.Executor {
			return errors.New("controller job configuration changed")
		}
		state = prior
	} else if !os.IsNotExist(err) {
		return err
	}
	mutating := command == "controller-run" || command == "controller-deploy" || command == "controller-collect"
	if mutating {
		if err := saveController(work, &state); err != nil {
			return err
		}
	}
	if command == "controller-capabilities" {
		b, _, _, err := c.request("GET", "/executor/v1/capabilities", nil, nil, maxControlBytes)
		if err != nil {
			return err
		}
		return durableJSON(filepath.Join(work, "capabilities.json"), json.RawMessage(b))
	}
	snapshot, status, err := c.snapshot(cfg.Job)
	if err == nil {
		expected := ExecutorRequest{deliveryProtocol, cfg.Job, cfg.Slot, cfg.Device, snapshot.Submission.Request.Artifact, snapshot.Submission.Request.Scenario, cfg.Parameters, cfg.Performance, ""}
		if snapshot.Submission.Digest != executorRequestDigest(expected) || snapshot.Submission.Request.Artifact.Peer != builder || snapshot.Submission.Request.Scenario.Peer != builder {
			return errors.New("remote request parameters or publication peer differ")
		}
		if snapshot.Submission.Executor != state.Executor || snapshot.Submission.Request.Device != cfg.Device || snapshot.Submission.Request.Slot != cfg.Slot {
			return errors.New("remote job scope differs")
		}
		if state.Artifact != "" && (snapshot.Submission.Request.Artifact.ID != state.Artifact || snapshot.Submission.Request.Scenario.ID != state.Scenario) {
			return errors.New("remote content identity changed")
		}
		// A remote receipt without our durable publication identity is not adopted.
		if state.Artifact == "" {
			return errors.New("local publication receipt is missing; review remote job")
		}
		state.Phase = controllerPhase(snapshot)
		state.Instructions = snapshot.Result.Instructions
		state.Performance = snapshot.Result.Performance
		if mutating {
			if err := saveController(work, &state); err != nil {
				return err
			}
		}
	}
	if command == "controller-observe" {
		if err != nil {
			return err
		}
		if cfg.Observation == nil {
			return errors.New("explicit human observation required")
		}
		_, _, _, err = c.request("POST", "/executor/v1/jobs/"+cfg.Job+"/observation", cfg.Observation, nil, maxControlBytes)
		return err
	}
	if command == "controller-status" {
		if err != nil {
			return err
		}
		return durableTreeJSON(filepath.Join(work, "controller-status.json"), snapshot)
	}
	if command == "controller-collect" {
		if err != nil {
			return err
		}
		return c.collect(work, cfg, &state, snapshot)
	}
	if command != "controller-run" && command != "controller-deploy" {
		return errors.New("unknown controller command")
	}
	if err != nil && status != 404 {
		return err
	}
	var artifact, bundle *localPublication
	defer func() { artifact.close(); bundle.close() }()
	if status == 404 || state.Phase == "held" {
		if cfg.Performance != nil {
			if cfg.Device != "host" {
				return errors.New("host performance admission cannot qualify a console device")
			}
			b, _, _, err := c.request("GET", "/executor/v1/capabilities", nil, nil, maxControlBytes)
			if err != nil {
				return err
			}
			var capabilities struct {
				Features []string `json:"features"`
			}
			if json.Unmarshal(b, &capabilities) != nil || len(capabilities.Features) > 32 {
				return errors.New("executor capabilities unavailable")
			}
			supported := false
			for _, feature := range capabilities.Features {
				if feature == "host-performance-v1" {
					supported = true
				}
			}
			if !supported {
				return errors.New("paired executor does not support host performance admission")
			}
		}
		for _, source := range []string{cfg.ArtifactRoot, cfg.ScenarioRoot} {
			if err := localPath(source); err != nil {
				return err
			}
		}
		state.Phase = "publishing"
		if err := saveController(work, &state); err != nil {
			return err
		}
		artifact, err = startPublication(filepath.Join(work, "artifact-publication"), cfg.ArtifactRoot, cfg)
		if err != nil {
			return err
		}
		bundle, err = startPublication(filepath.Join(work, "scenario-publication"), cfg.ScenarioRoot, cfg)
		if err != nil {
			return err
		}
		if state.Artifact != "" && (state.Artifact != artifact.offer.ID || state.Scenario != bundle.offer.ID) {
			return errors.New("source changed since this job was pinned; use a new job ID")
		}
		state.Artifact = artifact.offer.ID
		state.Scenario = bundle.offer.ID
		if err := saveController(work, &state); err != nil {
			return err
		} // Before the potentially unknown POST.
		request := ExecutorRequest{deliveryProtocol, cfg.Job, cfg.Slot, cfg.Device, artifact.offer, bundle.offer, cfg.Parameters, cfg.Performance, ""}
		method, path := "POST", "/executor/v1/jobs"
		if status != 404 {
			method = "PUT"
			path += "/" + cfg.Job + "/publication"
		}
		if _, _, _, err := c.request(method, path, request, nil, maxControlBytes); err != nil {
			return err
		}
	}
	deadline := time.Now().Add(time.Duration(cfg.Seconds) * time.Second)
	for {
		snapshot, _, err = c.snapshot(cfg.Job)
		if err != nil {
			return err
		}
		if snapshot.Submission.Executor != state.Executor || snapshot.Submission.Request.Artifact.ID != state.Artifact || snapshot.Submission.Request.Scenario.ID != state.Scenario {
			return errors.New("executor content receipt differs")
		}
		state.Phase = controllerPhase(snapshot)
		state.Instructions = snapshot.Result.Instructions
		state.Performance = snapshot.Result.Performance
		if err := saveController(work, &state); err != nil {
			return err
		}
		if deployed(state.Phase) {
			artifact.close()
			if artifact != nil {
				if err := retirePublicationIndexes(filepath.Join(work, "artifact-publication")); err != nil {
					return err
				}
			}
			artifact = nil
			bundle.close()
			if bundle != nil {
				if err := retirePublicationIndexes(filepath.Join(work, "scenario-publication")); err != nil {
					return err
				}
			}
			bundle = nil
			if command == "controller-deploy" {
				return nil
			}
		}
		if terminal(state.Phase) {
			return c.collect(work, cfg, &state, snapshot)
		}
		if state.Phase == "held" {
			return errors.New("delivery held; reconnect with the same pinned job")
		}
		if time.Now().After(deadline) {
			return errors.New("controller wait expired; remote job continues; reconnect with the same ID")
		}
		time.Sleep(250 * time.Millisecond)
	}
}

func retirePublicationIndexes(work string) error {
	s, err := openSlot(work)
	if err != nil {
		return err
	}
	defer s.Close()
	inventory, err := previousIndexes(s)
	if err != nil {
		return err
	}
	return finishIndexes(s, inventory, map[string]string{})
}

func (c executorClient) collect(work string, cfg ControllerConfig, state *ControllerState, snapshot ExecutorSnapshot) error {
	phase := controllerPhase(snapshot)
	if phase == "infrastructure_error" && snapshot.Result.Job == "" {
		if err := durableTreeJSON(filepath.Join(work, "controller-result.json"), snapshot); err != nil {
			return err
		}
		if state.Ack == "" {
			var err error
			state.Ack, err = newIdentity()
			if err != nil {
				return err
			}
			if err := saveController(work, state); err != nil {
				return err
			}
		}
		_, _, _, err := c.request("POST", "/executor/v1/jobs/"+cfg.Job+"/release", map[string]string{"request_id": state.Ack, "request_sha256": snapshot.Submission.Digest}, nil, maxControlBytes)
		if err != nil {
			return err
		}
		state.Collected = true
		if err := saveController(work, state); err != nil {
			return err
		}
		return errors.New("scenario admission refused before execution")
	}
	if !terminal(phase) || snapshot.Result.Job != cfg.Job || snapshot.Result.Executor != state.Executor || !validHex(snapshot.Result.ResultHash, 32) {
		return errors.New("no settled result available for collection")
	}
	result := snapshot.Result
	if result.Collected { // Never claim locally lost evidence was successfully collected.
		var saved ExecutorSnapshot
		if err := readTreeControl(filepath.Join(work, "controller-result.json"), &saved); err != nil || saved.Result.ResultHash != result.ResultHash {
			return errors.New("remote result was collected without this local receipt")
		}
		if err := verifyCollected(work, saved.Result); err != nil {
			return err
		}
		state.Collected = true
		if err := saveController(work, state); err != nil {
			return err
		}
		if phase != "passed" {
			return fmt.Errorf("scenario result: %s", phase)
		}
		return nil
	}
	parents, err := managedDirectory(work, "evidence")
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	if len(result.Evidence) > 128 || resultDigest(result) != result.ResultHash {
		return errors.New("result identity or evidence inventory differs")
	}
	total := int64(0)
	for _, entry := range result.Evidence {
		if !portablePath(entry.Path) || entry.Bytes < 0 || !validHex(entry.SHA256, 32) {
			return errors.New("unsafe evidence inventory")
		}
		total += entry.Bytes
		if total > 64*MiB {
			return errors.New("evidence budget exceeded")
		}
		// Local names are path digests; the signed result retains original names.
		key := sumBytes([]byte(entry.Path))
		path := filepath.Join(work, "evidence", key)
		if hash, err := hashFile(path); err == nil {
			if hash == entry.SHA256 {
				continue
			}
			return errors.New("local evidence changed; preserved for review")
		} else if !os.IsNotExist(err) {
			return err
		}
		f, err := openReceiptNext(path + ".next")
		if err != nil {
			return err
		}
		hash := sha256.New()
		offset := int64(0)
		for offset < entry.Bytes {
			b, headers, _, err := c.request("GET", "/executor/v1/jobs/"+cfg.Job+"/evidence/"+key, nil, map[string]string{"X-Zerus-Offset": strconv.FormatInt(offset, 10)}, 4*MiB)
			if err != nil || len(b) == 0 || int64(len(b)) > entry.Bytes-offset || headers.Get("X-Zerus-SHA256") != entry.SHA256 {
				f.Close()
				return errors.New("evidence transfer incomplete")
			}
			if _, err := f.Write(b); err != nil {
				f.Close()
				return err
			}
			hash.Write(b)
			offset += int64(len(b))
		}
		if hex.EncodeToString(hash.Sum(nil)) != entry.SHA256 {
			f.Close()
			return errors.New("evidence hash mismatch")
		}
		err = f.Sync()
		closeErr := f.Close()
		if err != nil {
			return err
		}
		if closeErr != nil {
			return closeErr
		}
		if err := replaceDurable(path+".next", path); err != nil {
			return err
		}
	}
	log, err := openReceiptNext(filepath.Join(work, "stdout.log.next"))
	if err != nil {
		return err
	}
	offset := int64(0)
	logHash := sha256.New()
	for offset < result.LogBytes {
		b, headers, _, err := c.request("GET", "/executor/v1/jobs/"+cfg.Job+"/log", nil, map[string]string{"X-Zerus-Offset": strconv.FormatInt(offset, 10)}, maxControlBytes)
		next, parseErr := strconv.ParseInt(headers.Get("X-Zerus-Next-Offset"), 10, 64)
		if err != nil || parseErr != nil || len(b) == 0 || next != offset+int64(len(b)) || next > result.LogBytes || next > maxScenarioLog {
			log.Close()
			return errors.New("log transfer incomplete")
		}
		if _, err := log.Write(b); err != nil {
			log.Close()
			return err
		}
		logHash.Write(b)
		offset = next
	}
	if hex.EncodeToString(logHash.Sum(nil)) != result.LogSHA256 {
		log.Close()
		return errors.New("log hash mismatch")
	}
	err = log.Sync()
	closeErr := log.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	if err := replaceDurable(filepath.Join(work, "stdout.log.next"), filepath.Join(work, "stdout.log")); err != nil {
		return err
	}
	if err := durableTreeJSON(filepath.Join(work, "controller-result.json"), snapshot); err != nil {
		return err
	}
	if state.Ack == "" {
		state.Ack, err = newIdentity()
		if err != nil {
			return err
		}
		if err := saveController(work, state); err != nil {
			return err
		}
	}
	if _, _, _, err := c.request("POST", "/executor/v1/jobs/"+cfg.Job+"/acknowledgement", ResultAcknowledgement{state.Ack, result.ResultHash}, nil, maxControlBytes); err != nil {
		return err
	}
	state.Phase = phase
	state.Collected = true
	if err := saveController(work, state); err != nil {
		return err
	}
	if phase != "passed" {
		return fmt.Errorf("scenario result: %s", phase)
	}
	return nil
}

func verifyCollected(work string, result ScenarioState) error {
	if len(result.Evidence) > 128 || result.ResultHash != resultDigest(result) {
		return errors.New("local result receipt changed")
	}
	for _, entry := range result.Evidence {
		if !portablePath(entry.Path) {
			return errors.New("unsafe evidence name")
		}
		path := filepath.Join(work, "evidence", sumBytes([]byte(entry.Path)))
		hash, err := hashFile(path)
		if err != nil || hash != entry.SHA256 {
			return errors.New("local collected evidence changed")
		}
	}
	hash, err := hashFile(filepath.Join(work, "stdout.log"))
	if err != nil || hash != result.LogSHA256 {
		return errors.New("local collected log changed")
	}
	return nil
}
