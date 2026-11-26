// Durable, model-free execution of an explicit versioned scenario bundle.
package main

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type ScenarioSpec struct {
	Protocol     int      `json:"protocol"`
	Instructions string   `json:"instructions"`
	Runtime      string   `json:"runtime"`
	Entry        string   `json:"entry"`
	Arguments    []string `json:"arguments"`
	Timeout      int      `json:"timeout_seconds"`
	Memory       int64    `json:"memory_bytes"`
	Guided       bool     `json:"guided"`
	Adapter      string   `json:"adapter,omitempty"`
}

type ScenarioConfig struct {
	ExecutorRoot     string                     `json:"executor_root"`
	Identity         string                     `json:"identity"`
	Job              string                     `json:"job_id"`
	ArtifactSlot     string                     `json:"artifact_slot"`
	Artifact         string                     `json:"artifact_id"`
	ArtifactDelivery string                     `json:"artifact_delivery_id"`
	ScenarioSlot     string                     `json:"scenario_slot"`
	Scenario         string                     `json:"scenario_id"`
	ScenarioDelivery string                     `json:"scenario_delivery_id"`
	Device           string                     `json:"device_id"`
	Parameters       map[string]json.RawMessage `json:"parameters"`
	CrashPhase       string                     `json:"crash_phase,omitempty"`
	Observation      *Observation               `json:"observation,omitempty"`
	Acknowledgement  *ResultAcknowledgement     `json:"acknowledgement,omitempty"`
	MaxMemory        int64                      `json:"maximum_memory_bytes,omitempty"`
	MaxSeconds       int                        `json:"maximum_seconds,omitempty"`
	Performance      *PerformancePolicy         `json:"performance,omitempty"`
	// Container is set only by a shared executor; a shared executor never
	// launches a scenario without one.
	Container *containerSpec `json:"container,omitempty"`
}

type EvidenceFile struct {
	Path   string `json:"path"`
	Bytes  int64  `json:"bytes"`
	SHA256 string `json:"sha256"`
}

type ScenarioState struct {
	Protocol     int                `json:"protocol"`
	Job          string             `json:"job_id"`
	Digest       string             `json:"request_sha256"`
	Executor     string             `json:"executor"`
	Artifact     string             `json:"artifact_id"`
	Scenario     string             `json:"scenario_id"`
	Device       string             `json:"device_id"`
	Status       string             `json:"status"`
	Instructions string             `json:"instructions"`
	Reason       string             `json:"reason,omitempty"`
	Supervisor   processReceipt     `json:"supervisor"`
	Process      processReceipt     `json:"process"`
	Revision     uint64             `json:"revision"`
	Started      int64              `json:"started"`
	Finished     int64              `json:"finished,omitempty"`
	Exit         *uint32            `json:"exit_code,omitempty"`
	LogBytes     int64              `json:"log_bytes"`
	LogSHA256    string             `json:"log_sha256,omitempty"`
	LogOmitted   int64              `json:"log_omitted"`
	Evidence     []EvidenceFile     `json:"evidence"`
	Observation  *Observation       `json:"observation,omitempty"`
	WorkRoot     string             `json:"work_root_identity"`
	Cleanup      []GeneratedObject  `json:"cleanup,omitempty"`
	ResultHash   string             `json:"result_sha256,omitempty"`
	Collected    bool               `json:"collected,omitempty"`
	Ack          string             `json:"acknowledgement_id,omitempty"`
	Performance  *PerformanceReport `json:"performance,omitempty"`
	// RevokedWhileRunning records the access refusal observed while the job
	// ran. The job is not killed for it (no new kill authority); new
	// launches and result access are refused by the executor's gates.
	RevokedWhileRunning string `json:"revoked_while_running,omitempty"`
	CancelRequest       string `json:"cancel_request,omitempty"`
}

type Observation struct {
	Request string `json:"request_id"`
	Job     string `json:"job_id"`
	Digest  string `json:"request_sha256"`
	Verdict string `json:"verdict"`
	Notes   string `json:"notes"`
}

type scenarioCustody struct {
	Protocol int            `json:"protocol"`
	Root     string         `json:"root_identity"`
	Executor string         `json:"executor"`
	Job      string         `json:"job_id"`
	Digest   string         `json:"request_sha256"`
	Process  processReceipt `json:"process"`
}

func validDevice(name string) bool {
	if name == "" || len(name) > 64 {
		return false
	}
	for _, r := range name {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '_' || r == '-') {
			return false
		}
	}
	return true
}

func scenarioDigest(cfg ScenarioConfig, executor string) string {
	cfg.CrashPhase = ""
	cfg.Identity = ""
	cfg.Observation = nil
	cfg.Acknowledgement = nil
	b, _ := json.Marshal(struct {
		Executor string         `json:"executor"`
		Config   ScenarioConfig `json:"config"`
	}{executor, cfg})
	return sumBytes(b)
}

func checkScenarioCustody(s *slot, executor string) error {
	path := filepath.Join(s.work, "active-scenario.json")
	var custody scenarioCustody
	if _, err := readControl(path, &custody); os.IsNotExist(err) {
		return nil
	} else if err != nil {
		return err
	}
	if executor == "" || custody.Protocol != deliveryProtocol || custody.Root != s.root || custody.Executor != executor || !validHex(custody.Job, 16) || !validHex(custody.Digest, 32) {
		return errors.New("scenario custody identity needs local review")
	}
	settled, err := namedJobSettled(custody.Process)
	if err != nil || !settled {
		return errors.New("scenario process settlement is unknown or still in use")
	}
	return os.Remove(path)
}

func bindScenario(s *slot, state ScenarioState) error {
	if err := checkScenarioCustody(s, state.Executor); err != nil {
		return err
	}
	return durableJSON(filepath.Join(s.work, "active-scenario.json"), scenarioCustody{deliveryProtocol, s.root, state.Executor, state.Job, state.Digest, state.Process})
}

func saveScenario(work string, state *ScenarioState) error {
	state.Revision++
	return durableTreeJSON(filepath.Join(work, "job-state.json"), state)
}

func readScenarioConfig(path string) (ScenarioConfig, string, error) {
	var cfg ScenarioConfig
	if err := strictControl(path, &cfg); err != nil {
		return cfg, "", err
	}
	if !validHex(cfg.Job, 16) || !validHex(cfg.Artifact, 32) || !validHex(cfg.Scenario, 32) || !validHex(cfg.ArtifactDelivery, 16) || !validHex(cfg.ScenarioDelivery, 16) || !validDevice(cfg.Device) || len(cfg.Parameters) > 32 {
		return cfg, "", errors.New("invalid scenario job identity")
	}
	for _, path := range []string{cfg.ExecutorRoot, cfg.ArtifactSlot, cfg.ScenarioSlot, cfg.Identity} {
		if err := localPath(path); err != nil {
			return cfg, "", err
		}
	}
	_, executor, err := loadIdentity(cfg.Identity)
	return cfg, executor, err
}

func executorProfile(root, identity string) error {
	s, err := openSlot(root)
	if err != nil {
		return err
	}
	defer s.Close()
	path := filepath.Join(root, "executor-owner.json")
	var owner struct {
		Protocol int    `json:"protocol"`
		Root     string `json:"root_identity"`
		Identity string `json:"identity"`
	}
	if _, err := readControl(path, &owner); os.IsNotExist(err) {
		owner.Protocol = deliveryProtocol
		owner.Root = s.root
		owner.Identity = identity
		return durableJSON(path, owner)
	} else if err != nil {
		return err
	}
	if owner.Protocol != deliveryProtocol || owner.Root != s.root || owner.Identity != identity {
		return errors.New("executor profile identity changed")
	}
	return nil
}

func scenarioRuntime(spec ScenarioSpec, bundle string) (string, []string, error) {
	if spec.Protocol != deliveryProtocol || !portablePath(spec.Entry) || len(spec.Arguments) > 64 || spec.Timeout < 1 || spec.Timeout > 86400 || spec.Memory < 64*MiB || spec.Memory > 16*1024*MiB || len(spec.Instructions) > 16384 {
		return "", nil, errors.New("unsupported scenario or resource budget")
	}
	for _, arg := range spec.Arguments {
		if len(arg) > 4096 || strings.ContainsRune(arg, 0) {
			return "", nil, errors.New("scenario argument limit exceeded")
		}
	}
	entry := filepath.Join(bundle, "build", filepath.FromSlash(spec.Entry))
	if spec.Runtime == "powershell" && strings.EqualFold(filepath.Ext(entry), ".ps1") {
		system, err := systemDirectory()
		if err != nil {
			return "", nil, err
		}
		return filepath.Join(system, "WindowsPowerShell", "v1.0", "powershell.exe"), append([]string{"-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", entry}, spec.Arguments...), nil
	}
	if spec.Runtime == "native" && strings.EqualFold(filepath.Ext(entry), ".exe") {
		return entry, append([]string{}, spec.Arguments...), nil
	}
	return "", nil, errors.New("scenario runtime capability is unavailable")
}

func runScenario(command, configPath string) error {
	cfg, executor, err := readScenarioConfig(configPath)
	if err != nil {
		return err
	}
	parents, _, err := holdDirectoryChain(cfg.ExecutorRoot)
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	if err := executorProfile(cfg.ExecutorRoot, executor); err != nil {
		return err
	}
	jobParents, err := managedDirectory(cfg.ExecutorRoot, "jobs/"+cfg.Job)
	defer closeFiles(jobParents)
	if err != nil {
		return err
	}
	work := filepath.Join(cfg.ExecutorRoot, "jobs", cfg.Job)
	if command == "scenario-status" {
		return snapshotScenario(work, cfg, executor)
	}
	if command == "scenario-observe" {
		return submitObservation(work, cfg, executor)
	}
	if command == "scenario-ack" {
		return acknowledgeScenario(work, cfg, executor)
	}
	jobSlot, err := openSlot(work)
	if err != nil {
		return err
	}
	defer jobSlot.Close()
	digest := scenarioDigest(cfg, executor)
	var prior ScenarioState
	if err := readTreeControl(filepath.Join(work, "job-state.json"), &prior); err == nil {
		if prior.Job != cfg.Job || prior.Digest != digest || prior.Executor != executor {
			return errors.New("job ID is already bound to another request")
		}
		if activeScenarioStatus(prior.Status) {
			prior.Status = "unknown"
			prior.Reason = "previous execution ended without a final receipt; never replay this job"
			return saveScenario(work, &prior)
		}
		return nil // Observation/reconciliation never launches the script again.
	} else if !os.IsNotExist(err) {
		return err
	}
	deviceParents, err := managedDirectory(cfg.ExecutorRoot, "devices/"+sumBytes([]byte(cfg.Device)))
	defer closeFiles(deviceParents)
	if err != nil {
		return err
	}
	device, err := openSlot(filepath.Join(cfg.ExecutorRoot, "devices", sumBytes([]byte(cfg.Device))))
	if err != nil {
		return errors.New("device reservation is busy")
	}
	defer device.Close()
	if err := checkScenarioCustody(device, executor); err != nil {
		return err
	}
	artifact, err := openSlot(cfg.ArtifactSlot)
	if err != nil {
		return err
	}
	defer artifact.Close()
	if err := checkScenarioCustody(artifact, executor); err != nil {
		return err
	}
	artifactFiles, err := verifyTree(artifact, cfg.Artifact, cfg.ArtifactDelivery)
	defer closeFiles(artifactFiles)
	if err != nil {
		return err
	}
	if filepath.Clean(cfg.ArtifactSlot) == filepath.Clean(cfg.ScenarioSlot) {
		return errors.New("scenario bundles use a distinct managed slot")
	}
	bundle, err := openSlot(cfg.ScenarioSlot)
	if err != nil {
		return err
	}
	defer bundle.Close()
	if err := checkScenarioCustody(bundle, executor); err != nil {
		return err
	}
	bundleFiles, err := verifyTree(bundle, cfg.Scenario, cfg.ScenarioDelivery)
	defer closeFiles(bundleFiles)
	if err != nil {
		return err
	}
	var spec ScenarioSpec
	if err := strictControl(filepath.Join(cfg.ScenarioSlot, "build", "scenario.json"), &spec); err != nil {
		return err
	}
	if (cfg.MaxMemory > 0 && spec.Memory > cfg.MaxMemory) || (cfg.MaxSeconds > 0 && spec.Timeout > cfg.MaxSeconds) {
		return errors.New("scenario exceeds executor resource capabilities")
	}
	executable, args, err := scenarioRuntime(spec, cfg.ScenarioSlot)
	if err != nil {
		return err
	}
	if err := checkDevice(cfg, spec, executor); err != nil {
		return err
	}
	if !declaredScenarioEntry(cfg.ScenarioSlot, spec.Entry) {
		return errors.New("scenario entry is absent from its verified bundle")
	}
	state := ScenarioState{Protocol: deliveryProtocol, Job: cfg.Job, Digest: digest, Executor: executor, Artifact: cfg.Artifact, Scenario: cfg.Scenario, Device: cfg.Device, Status: "admitted", Started: time.Now().UnixMilli(), Evidence: []EvidenceFile{}}
	state.Instructions = spec.Instructions
	state.WorkRoot = jobSlot.root
	state.Supervisor, err = currentSupervisor()
	if err != nil {
		return err
	}
	native, err := newNativeScenario(spec.Memory)
	if err != nil {
		return err
	}
	defer native.Close()
	state.Process = native.info
	if err := reserveScenarioSpool(cfg.ExecutorRoot, state); err != nil {
		return err
	}
	if err := saveScenario(work, &state); err != nil {
		return err
	}
	for _, slot := range []*slot{artifact, bundle, device} {
		if err := bindScenario(slot, state); err != nil {
			return err
		}
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "scenario_admitted")
	runtimeParents, err := managedDirectory(work, "runtime")
	defer closeFiles(runtimeParents)
	if err != nil {
		return err
	}
	outputParents, err := managedDirectory(work, "outputs")
	defer closeFiles(outputParents)
	if err != nil {
		return err
	}
	if err := durableJSON(filepath.Join(work, "parameters.json"), cfg.Parameters); err != nil {
		return err
	}
	log, err := os.OpenFile(filepath.Join(work, "stdout.log"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer log.Close()
	environment, err := scenarioEnvironment(work, cfg)
	if err != nil {
		return err
	}
	monitor, perfErr := beginPerformance(cfg, &state, spec.Timeout)
	if monitor != nil {
		defer monitor.source.Close()
	}
	if perfErr == nil && monitor != nil {
		state.Status = "waiting_for_quiet"
		perfErr = saveScenario(work, &state)
		crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "scenario_quiet_wait")
		if perfErr == nil {
			perfErr = monitor.gate(func() error { return nil })
		}
	}
	if perfErr != nil {
		return finishPerformanceRefusal(work, cfg, &state, log, perfErr.Error())
	}
	// Multi-person access is checked again at the launch boundary (a
	// revocation during delivery or the quiet wait refuses the launch).
	if err := admitExecution(cfg.Job); err != nil {
		return finishPerformanceRefusal(work, cfg, &state, log, "access: "+err.Error())
	}
	if cancelRequested(cfg.Job) {
		state.Status, state.Reason = "cancelled", "cancelled before launch"
		return saveScenario(work, &state)
	}
	plan, err := containedLaunchPlan(work, cfg, prepareContainedJob)
	if err != nil {
		return finishPerformanceRefusal(work, cfg, &state, log, "isolation: "+err.Error())
	}
	executionStart := time.Now()
	launch := native.Start
	if plan != nil {
		contained := *plan
		launch = func(executable string, args []string, environment map[string]string, cwd string, log *os.File, created func(processReceipt) error) error {
			return native.StartContained(contained, executable, args, environment, cwd, log, created)
		}
	}
	err = launch(executable, args, environment, filepath.Join(work, "runtime"), log, func(info processReceipt) error {
		if !info.Accepted {
			// Called before CreateProcess: the last point to refuse.
			if err := admitExecution(cfg.Job); err != nil {
				return err
			}
		}
		state.Process = info
		state.Status = "creating"
		if info.Accepted {
			state.Status = "running"
		}
		if spec.Guided && info.Accepted {
			state.Status = "awaiting_observation"
		}
		if err := saveScenario(work, &state); err != nil {
			return err
		}
		for _, slot := range []*slot{artifact, bundle, device} {
			// The slot remains held; this replaces our own creating marker only.
			if err := durableJSON(filepath.Join(slot.work, "active-scenario.json"), scenarioCustody{deliveryProtocol, slot.root, executor, state.Job, state.Digest, info}); err != nil {
				return err
			}
		}
		if info.Accepted {
			crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "scenario_created")
		}
		return nil
	})
	if err != nil {
		state.Status = "infrastructure_error"
		state.Reason = "native process creation refused"
		_ = saveScenario(work, &state)
		return err
	}
	crashCheckpoint(Config{CrashPhase: cfg.CrashPhase}, "scenario_running")
	lastCheck := time.Time{}
	code, timedOut, runErr := native.Wait(spec.Timeout, func() error {
		monitor.poll()
		if record, ok := cancelRequests.Load(cfg.Job); ok {
			state.CancelRequest = record.(cancelRecord).RequestID
			return errCancelRequested
		}
		if state.RevokedWhileRunning == "" && sharedExecution.Load() {
			if err := admitExecution(cfg.Job); err != nil {
				state.RevokedWhileRunning = err.Error()
				_ = saveScenario(work, &state)
			}
		}
		if time.Since(lastCheck) < 250*time.Millisecond {
			return nil
		}
		lastCheck = time.Now()
		if err := evidenceBudget(filepath.Join(work, "outputs")); err != nil {
			return err
		}
		if _, err := generatedObjectsMode(work, true); err != nil {
			return err
		}
		if spec.Guided {
			return observeScenario(work, &state)
		}
		return nil
	})
	monitor.finish(time.Since(executionStart))
	if errors.Is(runErr, errCancelRequested) {
		// Cancelled only after the whole tree is verified at zero; otherwise
		// the outcome stays unconfirmed and its slots stay pinned.
		state.Finished = time.Now().UnixMilli()
		if native.TerminateAndSettle() {
			state.Status, state.Reason = "cancelled", "cancelled; the process tree settled"
			_ = log.Close()
			if err := saveScenario(work, &state); err != nil {
				return err
			}
			for _, slot := range []*slot{artifact, bundle, device} {
				if err := checkScenarioCustody(slot, executor); err != nil {
					return err
				}
			}
			return nil
		}
		state.Status, state.Reason = "cancel_unconfirmed", "cancel requested; process tree settlement could not be verified; never replay"
		_ = log.Close()
		return saveScenario(work, &state)
	}
	state.Exit = &code
	state.Finished = time.Now().UnixMilli()
	// An observation is accepted only while the guided process is live
	// (submitObservation), but the process may exit right after it lands and
	// before the next monitor tick reads it: read it once more before settling.
	if spec.Guided && runErr == nil {
		if err := observeScenario(work, &state); err != nil {
			runErr = err
		}
	}
	if native.capture != nil {
		native.capture.mu.Lock()
		state.LogBytes = native.capture.bytes
		state.LogOmitted = native.capture.omitted
		native.capture.mu.Unlock()
	}
	if err := log.Sync(); err != nil && runErr == nil {
		runErr = err
	}
	if err := log.Close(); err != nil && runErr == nil {
		runErr = err
	}
	if runErr == nil {
		state.LogSHA256, runErr = hashFile(filepath.Join(work, "stdout.log"))
	}
	if runErr != nil {
		state.Status = "unknown"
		state.Reason = "process, log or evidence settlement could not be proved"
	} else if timedOut {
		state.Status = "timeout"
		state.Reason = "scenario budget expired"
	} else if code != 0 {
		state.Status = "failed"
	} else if spec.Guided && state.Observation == nil {
		state.Status = "infrastructure_error"
		state.Reason = "guided scenario ended without a human observation"
	} else if state.Observation != nil && state.Observation.Verdict == "failed" {
		state.Status = "failed"
	} else {
		state.Status = "passed"
	}
	if state.Performance != nil && !state.Performance.Valid && runErr == nil {
		state.Status = "infrastructure_error"
		state.Reason = "executor load evidence became unavailable"
	}
	if runErr == nil {
		for _, input := range []struct {
			s            *slot
			id, delivery string
		}{{artifact, cfg.Artifact, cfg.ArtifactDelivery}, {bundle, cfg.Scenario, cfg.ScenarioDelivery}} {
			held, err := verifyTree(input.s, input.id, input.delivery)
			closeFiles(held)
			if err != nil {
				state.Status = "infrastructure_error"
				state.Reason = "scenario input tree changed during execution"
			}
		}
		files, err := collectEvidence(filepath.Join(work, "outputs"))
		if err != nil {
			state.Status = "infrastructure_error"
			state.Reason = "evidence exceeded declared limits or contained unsafe paths"
		} else {
			state.Evidence = files
		}
		objects, err := generatedObjects(work)
		if err != nil {
			state.Status = "infrastructure_error"
			state.Reason = "generated output ownership or budget could not be established"
		} else {
			state.Cleanup = objects
		}
		state.ResultHash = resultDigest(state)
	}
	if err := saveScenario(work, &state); err != nil {
		return err
	}
	if runErr == nil {
		for _, slot := range []*slot{artifact, bundle, device} {
			if err := checkScenarioCustody(slot, executor); err != nil {
				return err
			}
		}
	}
	return runErr
}

// containedLaunchPlan decides how a scenario launches: no container in
// single-person mode; in a shared executor, only the re-derived container of
// its run configuration with every grant applied and read back. A shared
// executor never launches without one.
func containedLaunchPlan(work string, cfg ScenarioConfig, prepare func(containedLayout, containerPlan) error) (*containerPlan, error) {
	if cfg.Container == nil {
		if sharedExecution.Load() {
			return nil, accessCode("container-missing")
		}
		return nil, nil
	}
	// The run configuration lives in the executor root, which no contained
	// principal can read or write; still, the container is accepted only if
	// it is exactly what the current policy gives this job's owner, context
	// and project (verifyLaunchEnvelope), not merely self-consistent.
	if err := verifyLaunchEnvelope(cfg.Job, cfg.Container); err != nil {
		return nil, err
	}
	plan, err := planOf(cfg.Container)
	if err != nil {
		return nil, err
	}
	layout := containedLayout{Root: cfg.ExecutorRoot, Parent: work, Runtime: filepath.Join(work, "runtime"), Outputs: filepath.Join(work, "outputs"),
		Generation: filepath.Join(cfg.ArtifactSlot, "build"), Bundle: filepath.Join(cfg.ScenarioSlot, "build")}
	if err := prepare(layout, plan); err != nil {
		return nil, err
	}
	return &plan, nil
}
