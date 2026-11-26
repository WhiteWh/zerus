// Authenticated durable control requests outlive their controlling connections.
package main

import (
	"encoding/json"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

type ExecutorConfig struct {
	Root            string           `json:"root"`
	Peer            PeerConfig       `json:"peer"`
	DataPeers       []string         `json:"data_peers,omitempty"`
	ServeSeconds    int              `json:"serve_seconds"`
	Memory          int64            `json:"memory_bytes"`
	ScenarioMemory  int64            `json:"scenario_memory_bytes"`
	ScenarioSeconds int              `json:"scenario_seconds"`
	Stash           int64            `json:"stash_bytes"`
	Devices         []ExecutorDevice `json:"devices,omitempty"`
	// Access selects multi-person mode (executor_access.go).
	Access *AccessConfig `json:"access,omitempty"`
}
type ExecutorDevice struct {
	ID      string `json:"device_id"`
	Adapter string `json:"adapter"`
}
type ExecutorRequest struct {
	Protocol    int                        `json:"protocol"`
	Job         string                     `json:"job_id"`
	Slot        string                     `json:"slot"`
	Device      string                     `json:"device_id"`
	Artifact    TreeOffer                  `json:"artifact"`
	Scenario    TreeOffer                  `json:"scenario"`
	Parameters  map[string]json.RawMessage `json:"parameters"`
	Performance *PerformancePolicy         `json:"performance,omitempty"`
	// Project names the shared project in multi-person mode; empty otherwise.
	Project string `json:"project,omitempty"`
}
type ExecutorSubmission struct {
	Protocol int             `json:"protocol"`
	Executor string          `json:"executor"`
	Root     string          `json:"root_identity"`
	Digest   string          `json:"request_sha256"`
	Request  ExecutorRequest `json:"request"`
	Phase    string          `json:"phase"`
	Reason   string          `json:"reason,omitempty"`
	Created  int64           `json:"created"`
	// Owner is the admitting person in multi-person mode; nil otherwise.
	Owner *JobOwner `json:"owner,omitempty"`
}
type ExecutorQueue struct {
	Protocol int      `json:"protocol"`
	Root     string   `json:"root_identity"`
	Jobs     []string `json:"jobs"`
}
type executorService struct {
	cfg                 ExecutorConfig
	identity, root      string
	mu                  sync.Mutex
	control             sync.Mutex
	stopping            bool
	queue               ExecutorQueue
	wake, stop, settled chan struct{}
	access              accessState
	lastPerson          string
	// Test seams for delivery and launch; nil uses the real ones.
	deliverFn func(ExecutorSubmission, TreeOffer, string) (string, string, error)
	launchFn  func(command, path string) error
}

func executorRequestDigest(request ExecutorRequest) string {
	request.Artifact.Endpoint = ""
	request.Artifact.Token = ""
	request.Artifact.Session = ""
	request.Scenario.Endpoint = ""
	request.Scenario.Token = ""
	request.Scenario.Session = ""
	b, _ := json.Marshal(request)
	return sumBytes(b)
}
func (s *executorService) submissionPath(job string) string {
	return filepath.Join(s.cfg.Root, "submissions", job[:2], job+".json")
}
func (s *executorService) readSubmission(job string) (ExecutorSubmission, error) {
	var value ExecutorSubmission
	if !validHex(job, 16) {
		return value, errors.New("invalid job ID")
	}
	err := readSnapshot(s.submissionPath(job), &value)
	if err != nil {
		return value, err
	}
	if value.Protocol != deliveryProtocol || value.Root != s.root || value.Executor != s.identity || value.Request.Job != job || value.Digest != executorRequestDigest(value.Request) {
		return value, errors.New("submission identity changed")
	}
	return value, nil
}
func (s *executorService) saveSubmission(value ExecutorSubmission) error {
	parents, err := managedDirectory(s.cfg.Root, "submissions/"+value.Request.Job[:2])
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	return durableTreeJSON(s.submissionPath(value.Request.Job), value)
}
func (s *executorService) validateRequest(request ExecutorRequest) error {
	if err := validPerformance(request.Performance); err != nil {
		return err
	}
	if request.Protocol != deliveryProtocol || !validHex(request.Job, 16) || !validDevice(request.Slot) || !validDevice(request.Device) || len(request.Parameters) > 32 {
		return errors.New("unsupported executor request")
	}
	if s.shared() != accessName(request.Project) || (!s.shared() && request.Project != "") {
		return errors.New("project does not match the access mode")
	}
	allowedDevice := request.Device == "host"
	for _, device := range s.cfg.Devices {
		if device.ID == request.Device {
			allowedDevice = true
		}
	}
	if !allowedDevice {
		return errors.New("device is not configured on this executor")
	}
	for _, offer := range []TreeOffer{request.Artifact, request.Scenario} {
		if err := validTreeOffer(offer); err != nil {
			return err
		}
		allowed := offer.Peer != "" && offer.Peer == s.cfg.Peer.Fingerprint
		for _, peer := range s.cfg.DataPeers {
			if offer.Peer == peer {
				allowed = true
			}
		}
		if !allowed {
			return errors.New("publication peer is not configured")
		}
	}
	return nil
}
func (s *executorService) submit(request ExecutorRequest, owner *JobOwner) (ExecutorSubmission, error) {
	if err := s.validateRequest(request); err != nil {
		return ExecutorSubmission{}, err
	}
	if s.shared() != (owner != nil) || (owner != nil && owner.Project != request.Project) {
		return ExecutorSubmission{}, errors.New("job owner does not match the access mode")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.stopping {
		return ExecutorSubmission{}, errors.New("executor is draining")
	}
	if prior, err := s.readSubmission(request.Job); err == nil {
		// An existing ID answers only its own owner: another person, or an
		// ownerless receipt in multi-person mode, gets the plain refusal and
		// never the receipt.
		if !sameOwner(prior.Owner, owner) {
			return ExecutorSubmission{}, errors.New("job admission refused")
		}
		if prior.Digest != executorRequestDigest(request) {
			return prior, errors.New("job ID already has different inputs")
		}
		if prior.Phase == "queued" || prior.Phase == "deploying" {
			present := false
			for _, job := range s.queue.Jobs {
				if job == request.Job {
					present = true
					break
				}
			}
			if !present {
				if len(s.queue.Jobs) >= 6 {
					return prior, errors.New("queue receipt reconciliation is waiting for capacity")
				}
				s.queue.Jobs = append(s.queue.Jobs, request.Job)
				if err := durableJSON(filepath.Join(s.cfg.Root, "executor-queue.json"), s.queue); err != nil {
					return prior, err
				}
				select {
				case s.wake <- struct{}{}:
				default:
				}
			}
		}
		return prior, nil
	} else if !os.IsNotExist(err) {
		return prior, err
	}
	if len(s.queue.Jobs) >= 6 {
		return ExecutorSubmission{}, errors.New("executor queue full; collect retained results")
	}
	if owner != nil && s.ownedJobs(owner.Person) >= s.personJobLimit() {
		return ExecutorSubmission{}, errors.New("this person's queue is full; collect retained results")
	}
	value := ExecutorSubmission{deliveryProtocol, s.identity, s.root, executorRequestDigest(request), request, "queued", "", time.Now().UnixMilli(), owner}
	if err := s.saveSubmission(value); err != nil {
		return value, err
	}
	s.queue.Jobs = append(s.queue.Jobs, request.Job)
	if err := durableJSON(filepath.Join(s.cfg.Root, "executor-queue.json"), s.queue); err != nil {
		return value, err
	}
	select {
	case s.wake <- struct{}{}:
	default:
	}
	return value, nil
}
func (s *executorService) phase(value *ExecutorSubmission, phase, reason string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	// A cancelled receipt is final: a worker that read the job before the
	// cancel never moves it on.
	if stored, err := s.readSubmission(value.Request.Job); err == nil && stored.Phase == "cancelled" && phase != "cancelled" {
		*value = stored
		return errors.New("job was cancelled")
	}
	value.Phase = phase
	value.Reason = reason
	return s.saveSubmission(*value)
}
func (s *executorService) worker() {
	defer close(s.settled)
	for {
		select {
		case <-s.stop:
			return
		default:
		}
		s.mu.Lock()
		jobs := append([]string{}, s.queue.Jobs...)
		s.mu.Unlock()
		worked := false
		if s.shared() {
			worked = s.runNextShared(jobs)
			select {
			case <-s.stop:
				return
			default:
			}
			if worked {
				continue
			}
			jobs = nil
		}
		for _, job := range jobs {
			value, err := s.readSubmission(job)
			if err != nil {
				continue
			}
			if value.Phase != "queued" && value.Phase != "deploying" {
				continue
			}
			worked = true
			s.runOne(value)
			select {
			case <-s.stop:
				return
			default:
			}
		}
		if worked {
			continue
		}
		select {
		case <-s.stop:
			return
		case <-s.wake:
		case <-time.After(time.Second):
		}
	}
}
func (s *executorService) delivery(value ExecutorSubmission, offer TreeOffer, kind string) (string, string, error) {
	relative := slotRelative(value, kind)
	base := filepath.Join(s.cfg.Root, filepath.FromSlash(relative))
	parents, err := managedDirectory(s.cfg.Root, relative)
	defer closeFiles(parents)
	if err != nil {
		return "", "", err
	}
	var current TreeInstalled
	previous := ""
	delivery := sumBytes([]byte(value.Request.Job + ":" + kind))[:32]
	if err := readTreeControl(filepath.Join(base, "tree-installed.json"), &current); err == nil {
		if current.Artifact == offer.ID {
			if _, err := os.Lstat(filepath.Join(base, "tree-intent.json")); os.IsNotExist(err) {
				delivery = current.Delivery
			}
		}
		previous = current.Artifact
	} else if !os.IsNotExist(err) {
		return "", "", err
	}
	var pending TreeIntent
	if err := readTreeControl(filepath.Join(base, "tree-intent.json"), &pending); err == nil {
		if pending.Artifact != offer.ID {
			return "", "", errors.New("slot contains another pinned partial delivery")
		}
		delivery = pending.Delivery
		previous = pending.Previous
	} else if !os.IsNotExist(err) {
		return "", "", err
	}
	cfg := TreeConfig{Offer: offer, Delivery: delivery, Previous: previous, Stash: s.cfg.Stash, Memory: s.cfg.Memory, Peer: PeerConfig{Identity: s.cfg.Peer.Identity, Fingerprint: offer.Peer, ReadIdle: s.cfg.Peer.ReadIdle}, ExecutorIdentity: s.cfg.Peer.Identity}
	config := filepath.Join(base, "config.json")
	if err := durableJSON(config, cfg); err != nil {
		return "", "", err
	}
	if err := runTree("tree-receive", config); err != nil {
		return "", "", err
	}
	return base, delivery, nil
}
func (s *executorService) runConfig(value ExecutorSubmission) (ScenarioConfig, error) {
	var cfg ScenarioConfig
	err := strictControl(filepath.Join(s.cfg.Root, "requests", value.Request.Job, "run.json"), &cfg)
	return cfg, err
}
func (s *executorService) runOne(value ExecutorSubmission) {
	if err := s.phase(&value, "deploying", ""); err != nil {
		return
	}
	deliver, launch := s.delivery, runScenario
	if s.deliverFn != nil {
		deliver = s.deliverFn
	}
	if s.launchFn != nil {
		launch = s.launchFn
	}
	// Leases on both slots are reserved, under each slot's lock, before any
	// delivery reads or advances them; they are released only when the
	// job's own receipt is final and its process tree settled.
	defer func() {
		if value.Phase == "held" || finalReleasable[value.Phase] {
			s.releaseJobLeases(value)
		}
	}()
	for _, slot := range []struct {
		kind  string
		offer TreeOffer
	}{{"slots", value.Request.Artifact}, {"bundles", value.Request.Scenario}} {
		lease := slotLease{Generation: slot.offer.ID, Job: value.Request.Job}
		if value.Owner != nil {
			lease.Person = value.Owner.Person
		}
		if err := acquireLease(s.cfg.Root, slotRelative(value, slot.kind), lease); err != nil {
			_ = s.phase(&value, "held", "slot generation is in use by an unresolved job; it resumes after that job settles or is reviewed")
			return
		}
	}
	artifact, artifactDelivery, err := deliver(value, value.Request.Artifact, "slots")
	if err != nil {
		_ = s.phase(&value, "held", "artifact unavailable; refresh the pinned publication to resume")
		return
	}
	if s.refusedDuringDeploy(&value) {
		return
	}
	bundle, bundleDelivery, err := deliver(value, value.Request.Scenario, "bundles")
	if err != nil {
		_ = s.phase(&value, "held", "scenario bundle unavailable; refresh its pinned publication to resume")
		return
	}
	if s.refusedDuringDeploy(&value) {
		return
	}
	parents, err := managedDirectory(s.cfg.Root, "requests/"+value.Request.Job)
	defer closeFiles(parents)
	if err != nil {
		_ = s.phase(&value, "infrastructure_error", "request storage unavailable")
		return
	}
	container, err := s.containerFor(value)
	if err != nil {
		_ = s.phase(&value, "access_refused", "isolation: "+err.Error())
		return
	}
	cfg := ScenarioConfig{Container: container, ExecutorRoot: s.cfg.Root, Identity: s.cfg.Peer.Identity, Job: value.Request.Job, ArtifactSlot: artifact, Artifact: value.Request.Artifact.ID, ArtifactDelivery: artifactDelivery, ScenarioSlot: bundle, Scenario: value.Request.Scenario.ID, ScenarioDelivery: bundleDelivery, Device: value.Request.Device, Parameters: value.Request.Parameters, MaxMemory: s.cfg.ScenarioMemory, MaxSeconds: s.cfg.ScenarioSeconds, Performance: value.Request.Performance}
	path := filepath.Join(s.cfg.Root, "requests", value.Request.Job, "run.json")
	if err := durableJSON(path, cfg); err != nil {
		_ = s.phase(&value, "infrastructure_error", "job configuration persistence failed")
		return
	}
	if s.shared() {
		owned := value
		executionAdmissions.Store(owned.Request.Job, func() error { return s.authorizeExecution(owned) })
		defer executionAdmissions.Delete(owned.Request.Job)
		if container != nil {
			executionContainers.Store(owned.Request.Job, func(spec *containerSpec) error {
				want, err := s.expectedContainer(owned)
				if err != nil {
					return err
				}
				if !sameContainer(spec, want) {
					return accessCode("container-envelope-mismatch")
				}
				return nil
			})
			defer executionContainers.Delete(owned.Request.Job)
		}
	}
	if err := s.phase(&value, "executing", ""); err != nil {
		return
	}
	runErr := launch("scenario-run", path)
	var state ScenarioState
	if err := readTreeControl(filepath.Join(s.cfg.Root, "jobs", cfg.Job, "job-state.json"), &state); err == nil {
		if activeScenarioStatus(state.Status) {
			state.Status = "unknown"
			state.Reason = "execution returned without a final receipt"
			_ = saveScenario(filepath.Join(s.cfg.Root, "jobs", cfg.Job), &state)
		}
		_ = s.phase(&value, state.Status, state.Reason)
	} else if runErr != nil {
		_ = s.phase(&value, "infrastructure_error", "job admission refused or device busy")
	} else {
		_ = s.phase(&value, "unknown", "no final execution receipt")
	}
}
func activeScenarioStatus(status string) bool {
	return status == "admitted" || status == "waiting_for_quiet" || status == "creating" || status == "running" || status == "awaiting_observation" || status == "collecting"
}
func (s *executorService) recoverQueue() error {
	for _, job := range s.queue.Jobs {
		value, err := s.readSubmission(job)
		if err != nil {
			return err
		}
		if value.Phase == "executing" {
			var state ScenarioState
			if err := readTreeControl(filepath.Join(s.cfg.Root, "jobs", job, "job-state.json"), &state); err == nil && !activeScenarioStatus(state.Status) {
				value.Phase = state.Status
				value.Reason = state.Reason
			} else {
				value.Phase = "unknown"
				value.Reason = "executor restarted after admission; never replay this job"
			}
			if err := s.saveSubmission(value); err != nil {
				return err
			}
		}
	}
	return nil
}
func (s *executorService) json(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}
func (s *executorService) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.RawQuery != "" {
		http.Error(w, "unsupported query", 400)
		return
	}
	if r.Method == "GET" && r.URL.Path == "/executor/v1/capabilities" {
		var access map[string]any
		if s.shared() {
			caller, _, ok := s.authenticate(w, r, "capabilities", "")
			if !ok {
				return
			}
			// Only the caller's own identity and the host's limits; never
			// another person, project roster or account.
			access = map[string]any{"mode": "multi-person", "contract": hostAccessContract, "person": caller.Person, "policy_revision": caller.Policy.Revision,
				"isolation_backend": s.cfg.Access.Isolation, "maximum_person_jobs": s.personJobLimit(), "cancel": "settled"}
		}
		capabilities := map[string]any{"protocol": deliveryProtocol, "executor": s.identity, "os": "windows", "backend": deliveryBackend, "model_runtime": false, "features": []string{"artifact-tree-v1", "scenario-v1", "host-performance-v1"}, "performance_scope": "host-cpu-volumes-physical-disks", "performance_availability": "checked-at-admission", "devices": append([]ExecutorDevice{{"host", "host"}}, s.cfg.Devices...), "runtimes": []string{"powershell", "native"}, "maximum_scenario_memory_bytes": s.cfg.ScenarioMemory, "maximum_scenario_seconds": s.cfg.ScenarioSeconds, "maximum_pending_jobs": 6, "maximum_log_bytes": maxScenarioLog, "maximum_evidence_bytes": 64 * MiB}
		if access != nil {
			capabilities["access"] = access
		}
		s.json(w, capabilities)
		return
	}
	if r.Method == "POST" && r.URL.Path == "/executor/v1/jobs" {
		var request ExecutorRequest
		if decodeRequest(w, r, &request) != nil {
			http.Error(w, "request refused", 400)
			return
		}
		var owner *JobOwner
		if s.shared() {
			caller, _, ok := s.authenticate(w, r, "jobs.submit", request.Project)
			if !ok || !s.mutationBound(w, r, caller) {
				return
			}
			// Refused before any job directory, queue receipt or download.
			if err := s.executionContext(caller.Policy, caller.Person); err != nil {
				accessRefused(w, err.Error())
				return
			}
			owner = &JobOwner{caller.Person, caller.Device, request.Project, caller.Policy.Revision}
		}
		value, err := s.submit(request, owner)
		if err != nil {
			http.Error(w, "job admission refused", 409)
			return
		}
		s.json(w, value)
		return
	}
	if r.Method == "GET" && r.URL.Path == "/executor/v1/jobs" {
		var caller *accessCaller
		if s.shared() {
			var ok bool
			if caller, _, ok = s.authenticate(w, r, "jobs.list", ""); !ok {
				return
			}
		}
		s.mu.Lock()
		jobs := append([]string{}, s.queue.Jobs...)
		s.mu.Unlock()
		rows := []ExecutorSubmission{}
		for _, job := range jobs {
			if value, err := s.readSubmission(job); err == nil && (!s.shared() || s.listable(caller, value)) {
				rows = append(rows, value)
			}
		}
		sort.Slice(rows, func(i, j int) bool { return rows[i].Created < rows[j].Created })
		s.json(w, rows)
		return
	}
	const prefix = "/executor/v1/jobs/"
	parts := strings.Split(strings.TrimPrefix(r.URL.Path, prefix), "/")
	if !strings.HasPrefix(r.URL.Path, prefix) || len(parts) < 1 || !validHex(parts[0], 16) {
		http.Error(w, "unknown resource", 404)
		return
	}
	job := parts[0]
	value, err := s.readSubmission(job)
	if err != nil {
		http.Error(w, "unknown job", 404)
		return
	}
	if s.shared() && !s.authorizeJob(w, r, value, parts) {
		return
	}
	if len(parts) == 2 && parts[1] == "release" && r.Method == "POST" {
		s.control.Lock()
		defer s.control.Unlock()
		var release struct {
			Request string `json:"request_id"`
			Digest  string `json:"request_sha256"`
		}
		if decodeRequest(w, r, &release) != nil || !validHex(release.Request, 16) || release.Digest != value.Digest || (value.Phase != "infrastructure_error" && value.Phase != "access_refused") {
			http.Error(w, "release refused", 409)
			return
		}
		// An executing/unknown job is never released by absence inference. This
		// terminal phase is written only after the worker returned, and a native
		// process cannot be created before its core admission receipt exists.
		if _, err := os.Lstat(filepath.Join(s.cfg.Root, "jobs", job, "job-state.json")); !os.IsNotExist(err) {
			http.Error(w, "admission needs result collection or review", 409)
			return
		}
		// A job refused at execution by access never deployed: it has no
		// admission receipt or spool to release.
		var cfg ScenarioConfig
		if value.Phase != "access_refused" {
			if cfg, err = s.runConfig(value); err != nil {
				http.Error(w, "admission receipt unavailable", 409)
				return
			}
		} else {
			parents, err := managedDirectory(s.cfg.Root, "requests/"+job)
			closeFiles(parents)
			if err != nil {
				http.Error(w, "release unavailable", 503)
				return
			}
		}
		path := filepath.Join(s.cfg.Root, "requests", job, "released.json")
		var prior struct {
			Request string `json:"request_id"`
			Digest  string `json:"request_sha256"`
		}
		if _, err := readControl(path, &prior); os.IsNotExist(err) {
			if durableJSON(path, release) != nil {
				http.Error(w, "release unavailable", 503)
				return
			}
		} else if err != nil || prior != release {
			http.Error(w, "release identity changed", 409)
			return
		}
		if value.Phase != "access_refused" {
			if err := releaseScenarioSpool(s.cfg.Root, ScenarioState{Job: job, Digest: scenarioDigest(cfg, s.identity)}); err != nil && !os.IsNotExist(err) {
				http.Error(w, "spool release unavailable", 409)
				return
			}
		}
		s.mu.Lock()
		defer s.mu.Unlock()
		for i, id := range s.queue.Jobs {
			if id == job {
				s.queue.Jobs = append(s.queue.Jobs[:i], s.queue.Jobs[i+1:]...)
				break
			}
		}
		if durableJSON(filepath.Join(s.cfg.Root, "executor-queue.json"), s.queue) != nil {
			http.Error(w, "queue release unavailable", 503)
			return
		}
		s.json(w, map[string]any{"released": true, "job_id": job})
		return
	}
	if len(parts) == 1 && r.Method == "GET" {
		var state ScenarioState
		_ = readSnapshot(filepath.Join(s.cfg.Root, "jobs", job, "job-state.json"), &state)
		s.json(w, map[string]any{"submission": value, "result": state})
		return
	}
	if len(parts) == 2 && parts[1] == "publication" && r.Method == "PUT" {
		var request ExecutorRequest
		if decodeRequest(w, r, &request) != nil {
			http.Error(w, "request refused", 400)
			return
		}
		s.mu.Lock()
		defer s.mu.Unlock()
		current, err := s.readSubmission(job)
		if err != nil || current.Phase != "held" || request.Job != job || executorRequestDigest(request) != current.Digest || s.validateRequest(request) != nil {
			http.Error(w, "publication refresh refused", 409)
			return
		}
		current.Request = request
		current.Phase = "queued"
		current.Reason = ""
		if err := s.saveSubmission(current); err != nil {
			http.Error(w, "publication refresh unavailable", 503)
			return
		}
		select {
		case s.wake <- struct{}{}:
		default:
		}
		s.json(w, current)
		return
	}
	if len(parts) == 2 && (parts[1] == "observation" || parts[1] == "acknowledgement") && r.Method == "POST" {
		s.control.Lock()
		defer s.control.Unlock()
		cfg, err := s.runConfig(value)
		if err != nil {
			http.Error(w, "job is not executing", 409)
			return
		}
		command := "scenario-observe"
		if parts[1] == "observation" {
			var observation Observation
			if decodeRequest(w, r, &observation) != nil {
				http.Error(w, "observation refused", 400)
				return
			}
			cfg.Observation = &observation
		} else {
			var ack ResultAcknowledgement
			if decodeRequest(w, r, &ack) != nil {
				http.Error(w, "acknowledgement refused", 400)
				return
			}
			cfg.Acknowledgement = &ack
			command = "scenario-ack"
		}
		path := filepath.Join(s.cfg.Root, "requests", job, parts[1]+".json")
		if err := durableJSON(path, cfg); err != nil {
			http.Error(w, "control persistence unavailable", 503)
			return
		}
		if err := runScenario(command, path); err != nil {
			http.Error(w, "control request refused", 409)
			return
		}
		if command == "scenario-ack" {
			s.mu.Lock()
			for i, id := range s.queue.Jobs {
				if id == job {
					s.queue.Jobs = append(s.queue.Jobs[:i], s.queue.Jobs[i+1:]...)
					break
				}
			}
			err := durableJSON(filepath.Join(s.cfg.Root, "executor-queue.json"), s.queue)
			s.mu.Unlock()
			if err != nil {
				http.Error(w, "collection receipt unavailable", 503)
				return
			}
		}
		s.json(w, map[string]any{"accepted": true, "job_id": job})
		return
	}
	if len(parts) == 2 && parts[1] == "log" && r.Method == "GET" {
		s.serveLog(w, r, job)
		return
	}
	if len(parts) == 3 && parts[1] == "evidence" && r.Method == "GET" {
		s.serveEvidence(w, r, job, parts[2])
		return
	}
	http.Error(w, "unknown operation", 404)
}
func decodeRequest(w http.ResponseWriter, r *http.Request, value any) error {
	decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxControlBytes))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return errors.New("trailing request data")
	}
	return nil
}
func (s *executorService) serveLog(w http.ResponseWriter, r *http.Request, job string) {
	offset, err := strconv.ParseInt(r.Header.Get("X-Zerus-Offset"), 10, 64)
	if err != nil || offset < 0 {
		http.Error(w, "invalid log offset", 400)
		return
	}
	f, err := openLiveLog(filepath.Join(s.cfg.Root, "jobs", job, "stdout.log"))
	if err != nil {
		http.Error(w, "log unavailable", 404)
		return
	}
	defer f.Close()
	b := make([]byte, maxControlBytes)
	n, err := f.ReadAt(b, offset)
	if err != nil && err != io.EOF {
		http.Error(w, "log unavailable", 409)
		return
	}
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("X-Zerus-Next-Offset", strconv.FormatInt(offset+int64(n), 10))
	_, _ = w.Write(b[:n])
}
func (s *executorService) serveEvidence(w http.ResponseWriter, r *http.Request, job, key string) {
	if !validHex(key, 32) {
		http.Error(w, "invalid evidence key", 400)
		return
	}
	work := filepath.Join(s.cfg.Root, "jobs", job)
	if _, err := os.Lstat(filepath.Join(work, "ack-intent.json")); !os.IsNotExist(err) {
		http.Error(w, "collection is being acknowledged", 409)
		return
	}
	var state ScenarioState
	if err := readSnapshot(filepath.Join(work, "job-state.json"), &state); err != nil || state.Collected {
		http.Error(w, "evidence unavailable", 410)
		return
	}
	var entry *EvidenceFile
	for i := range state.Evidence {
		if sumBytes([]byte(state.Evidence[i].Path)) == key {
			entry = &state.Evidence[i]
			break
		}
	}
	if entry == nil || !portablePath(entry.Path) {
		http.Error(w, "evidence unavailable", 404)
		return
	}
	var owned *GeneratedObject
	for i := range state.Cleanup {
		if state.Cleanup[i].Path == "outputs/"+entry.Path {
			owned = &state.Cleanup[i]
			break
		}
	}
	if owned == nil || owned.Directory || owned.Bytes != entry.Bytes {
		http.Error(w, "evidence ownership unavailable", 409)
		return
	}
	f, err := openSealed(filepath.Join(work, "outputs", filepath.FromSlash(entry.Path)), owned.Identity, false)
	if err != nil {
		http.Error(w, "evidence unavailable", 409)
		return
	}
	defer f.Close()
	info, err := f.Stat()
	_, modified, metaErr := fileMetadata(f)
	if err != nil || metaErr != nil || modified != owned.Modified || info.Size() != entry.Bytes {
		http.Error(w, "evidence changed", 409)
		return
	}
	offset, err := strconv.ParseInt(r.Header.Get("X-Zerus-Offset"), 10, 64)
	if err != nil || offset < 0 || offset > entry.Bytes {
		http.Error(w, "invalid evidence offset", 400)
		return
	}
	n := min(4*MiB, entry.Bytes-offset)
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("Content-Length", strconv.FormatInt(n, 10))
	w.Header().Set("X-Zerus-SHA256", entry.SHA256)
	_, _ = io.CopyN(w, io.NewSectionReader(f, offset, n), n)
}

// serveExecutor is the single-person service entry. It never starts
// multi-person mode, so a configuration that gains or loses an access section
// cannot move a service between the modes.
func serveExecutor(configPath string) error {
	var cfg ExecutorConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	if cfg.Access != nil {
		return errors.New("multi-person mode starts only through executor-serve-shared")
	}
	return runExecutor(cfg, configPath, nil)
}

// runExecutor serves a parsed configuration; trust is set exactly in
// multi-person mode.
func runExecutor(cfg ExecutorConfig, configPath string, trust policyTrust) error {
	if (trust == nil) != (cfg.Access == nil) {
		return errors.New("executor mode and configuration disagree")
	}
	if cfg.Peer.Identity == "" || (cfg.Access == nil && cfg.Peer.Fingerprint == "") || cfg.Root == "" || cfg.ServeSeconds < 1 || cfg.ServeSeconds > 604800 || cfg.Memory < 128*MiB || cfg.Memory > 4*1024*MiB || cfg.ScenarioMemory < 64*MiB || cfg.ScenarioMemory >= cfg.Memory || cfg.ScenarioSeconds < 1 || cfg.ScenarioSeconds > 86400 || cfg.Stash < 0 || len(cfg.DataPeers) > 64 {
		return errors.New("invalid executor configuration or limits")
	}
	for _, pin := range cfg.DataPeers {
		if !validHex(pin, 32) {
			return errors.New("invalid data peer")
		}
	}
	// Multi-person mode was checked, including its isolation backend and
	// operator, before the executor writes anything.
	if trust != nil {
		sharedExecution.Store(true)
	} else if err := refuseLegacyAfterShared(cfg.Root); err != nil {
		return err
	}
	parents, root, err := holdDirectoryChain(cfg.Root)
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	_, identity, err := loadIdentity(cfg.Peer.Identity)
	if err != nil {
		return err
	}
	if err := executorProfile(cfg.Root, identity); err != nil {
		return err
	}
	serviceLock, err := lockSlot(filepath.Join(cfg.Root, "executor-service.lock"))
	if err != nil {
		return errors.New("executor service already owns this root")
	}
	defer serviceLock.Close()
	if len(cfg.Devices) > 32 {
		return errors.New("device roster exceeds limit")
	}
	seen := map[string]bool{"host": true}
	for _, device := range cfg.Devices {
		if !validDevice(device.ID) || !validDevice(device.Adapter) || seen[device.ID] || device.Adapter == "host" {
			return errors.New("invalid or duplicate device adapter")
		}
		seen[device.ID] = true
		dirs, err := managedDirectory(cfg.Root, "devices/"+sumBytes([]byte(device.ID)))
		if err != nil {
			return err
		}
		err = durableJSON(filepath.Join(cfg.Root, "devices", sumBytes([]byte(device.ID)), "device.json"), map[string]any{"protocol": deliveryProtocol, "executor": identity, "device_id": device.ID, "adapter": device.Adapter})
		closeFiles(dirs)
		if err != nil {
			return err
		}
	}
	limit, err := limitExecutorProcess(cfg.Memory)
	if err != nil {
		return err
	}
	defer limit()
	debug.SetMemoryLimit(cfg.Memory / 2)
	s := &executorService{cfg: cfg, identity: identity, root: root, wake: make(chan struct{}, 1), stop: make(chan struct{}), settled: make(chan struct{}), queue: ExecutorQueue{deliveryProtocol, root, []string{}}}
	if _, err := readControl(filepath.Join(cfg.Root, "executor-queue.json"), &s.queue); err != nil && !os.IsNotExist(err) {
		return err
	}
	if s.queue.Protocol != deliveryProtocol || s.queue.Root != root || len(s.queue.Jobs) > 6 {
		return errors.New("executor queue identity changed")
	}
	if s.shared() {
		s.access.trust = trust
		if _, err := s.currentPolicy(); err != nil {
			return errors.New("access policy unavailable: " + trustDetail(err).Error())
		}
		if err := s.prepareAccessRoot(); err != nil {
			return err
		}
	}
	if err := s.recoverQueue(); err != nil {
		return err
	}
	s.reconcileLeases()
	session, err := newIdentity()
	if err != nil {
		return err
	}
	var listener net.Listener
	var endpoint, pin string
	if s.shared() {
		listener, endpoint, pin, err = s.accessListener()
	} else {
		listener, endpoint, pin, err = dataListener(cfg.Peer)
	}
	if err != nil {
		return err
	}
	defer listener.Close()
	server := &http.Server{Handler: s, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 10 * time.Second, WriteTimeout: 15 * time.Minute, IdleTimeout: 5 * time.Second, MaxHeaderBytes: 4096, ErrorLog: log.New(io.Discard, "", 0)}
	defer server.Close()
	finished := make(chan error, 1)
	go func() { finished <- server.Serve(listener) }()
	go s.worker()
	var stopOnce sync.Once
	stopWorker := func() { stopOnce.Do(func() { close(s.stop) }); <-s.settled }
	defer stopWorker()
	if err := durableJSON(filepath.Join(filepath.Dir(configPath), "executor-offer.json"), map[string]any{"protocol": deliveryProtocol, "endpoint": endpoint, "peer_fingerprint": pin, "executor": identity, "session": session}); err != nil {
		return err
	}
	timer := time.NewTimer(time.Duration(cfg.ServeSeconds) * time.Second)
	defer timer.Stop()
	select {
	case err := <-finished:
		return err
	case <-timer.C:
		s.mu.Lock()
		s.stopping = true
		s.mu.Unlock()
		stopWorker() // Keep observations and result reads reachable while the active job settles.
		_ = server.Close()
		<-finished
		return nil
	}
}
