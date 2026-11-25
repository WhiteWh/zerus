package main

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
)

func declaredScenarioEntry(root, entry string) bool {
	var installed TreeInstalled
	if readTreeControl(filepath.Join(root, "tree-installed.json"), &installed) != nil {
		return false
	}
	for _, file := range installed.Files {
		if file.Path == entry {
			return true
		}
	}
	return false
}

func scenarioEnvironment(work string, cfg ScenarioConfig) (map[string]string, error) {
	system, err := systemDirectory()
	if err != nil {
		return nil, err
	}
	windowsRoot := filepath.Dir(system)
	runtime := filepath.Join(work, "runtime")
	env := map[string]string{"SystemRoot": windowsRoot, "WINDIR": windowsRoot, "COMSPEC": filepath.Join(system, "cmd.exe"),
		"PATH": system + string(os.PathListSeparator) + filepath.Join(system, "WindowsPowerShell", "v1.0"),
		"TEMP": runtime, "TMP": runtime, "HOME": runtime, "USERPROFILE": runtime, "LOCALAPPDATA": runtime, "APPDATA": runtime,
		"ZERUS_ARTIFACT_ROOT": filepath.Join(cfg.ArtifactSlot, "build"), "ZERUS_SCENARIO_ROOT": filepath.Join(cfg.ScenarioSlot, "build"),
		"ZERUS_RESULT_DIR": filepath.Join(work, "outputs"), "ZERUS_PARAMETERS_FILE": filepath.Join(work, "parameters.json"),
		"ZERUS_OBSERVATION_FILE": filepath.Join(work, "observation.json"), "ZERUS_DEVICE_ID": cfg.Device,
		"GOTELEMETRY": "off", "OTEL_SDK_DISABLED": "true"}
	return env, nil
}

func snapshotScenario(work string, cfg ScenarioConfig, executor string) error {
	var state ScenarioState
	if err := readSnapshot(filepath.Join(work, "job-state.json"), &state); err != nil {
		return err
	}
	if state.Job != cfg.Job || state.Digest != scenarioDigest(cfg, executor) || state.Executor != executor {
		return errors.New("requested job identity changed")
	}
	if activeScenarioStatus(state.Status) {
		exited, err := rootExited(state.Supervisor)
		if err != nil {
			return err
		}
		if exited {
			state.Status = "unknown"
			state.Reason = "supervisor exited without a final receipt; never replay this job"
		}
	}
	return durableTreeJSON(filepath.Join(filepath.Dir(work), cfg.Job+"-snapshot.json"), state)
}

func submitObservation(work string, cfg ScenarioConfig, executor string) error {
	observation := cfg.Observation
	if observation == nil || !validHex(observation.Request, 16) || observation.Job != cfg.Job || observation.Digest != scenarioDigest(cfg, executor) ||
		(observation.Verdict != "passed" && observation.Verdict != "failed") || len(observation.Notes) > 4096 {
		return errors.New("invalid human observation binding")
	}
	guard, err := lockSlot(filepath.Join(work, "observation.lock"))
	if err != nil {
		return err
	}
	defer guard.Close()
	var existing Observation
	if err := readSnapshot(filepath.Join(work, "observation.json"), &existing); err == nil {
		if existing != *observation {
			return errors.New("observation already recorded with different content")
		}
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	var state ScenarioState
	if err := readSnapshot(filepath.Join(work, "job-state.json"), &state); err != nil {
		return err
	}
	if state.Executor != executor || state.Job != cfg.Job || state.Digest != observation.Digest || state.Status != "awaiting_observation" {
		return errors.New("job is not waiting for this observation")
	}
	exited, err := rootExited(state.Supervisor)
	if err != nil || exited {
		return errors.New("guided execution is not live")
	}
	return durableJSON(filepath.Join(work, "observation.json"), observation)
}

func observeScenario(work string, state *ScenarioState) error {
	if state.Observation != nil {
		return nil
	}
	var observation Observation
	if err := readSnapshot(filepath.Join(work, "observation.json"), &observation); os.IsNotExist(err) {
		return nil
	} else if err != nil {
		return err
	}
	if observation.Job != state.Job || observation.Digest != state.Digest || !validHex(observation.Request, 16) ||
		(observation.Verdict != "passed" && observation.Verdict != "failed") || len(observation.Notes) > 4096 {
		return errors.New("human observation identity changed")
	}
	state.Observation = &observation
	state.Status = "collecting"
	return saveScenario(work, state)
}

func scanEvidence(root string, hash bool) ([]EvidenceFile, error) {
	parents, _, err := holdDirectoryChain(root)
	defer closeFiles(parents)
	if err != nil {
		return nil, err
	}
	result := []EvidenceFile{}
	queue := []string{""}
	directories := 0
	total := int64(0)
	for len(queue) > 0 {
		rel := queue[0]
		queue = queue[1:]
		directories++
		if directories > 128 {
			return nil, errors.New("evidence directory limit exceeded")
		}
		dir, err := openDirectory(filepath.Join(root, filepath.FromSlash(rel)))
		if err != nil {
			return nil, err
		}
		for {
			entries, readErr := dir.ReadDir(32)
			if readErr != nil && readErr != io.EOF {
				dir.Close()
				return nil, readErr
			}
			for _, entry := range entries {
				path := entry.Name()
				if rel != "" {
					path = rel + "/" + path
				}
				if !portablePath(path) || entry.Type()&os.ModeSymlink != 0 {
					dir.Close()
					return nil, errors.New("unsafe evidence path")
				}
				if entry.IsDir() {
					queue = append(queue, path)
					if len(queue)+directories > 128 {
						dir.Close()
						return nil, errors.New("evidence directory limit exceeded")
					}
					continue
				}
				if len(result) >= 128 {
					dir.Close()
					return nil, errors.New("evidence file limit exceeded")
				}
				var f *os.File
				var err error
				if hash {
					f, err = openSealed(filepath.Join(root, filepath.FromSlash(path)), "", false)
				} else {
					f, err = statEvidence(filepath.Join(root, filepath.FromSlash(path)))
				}
				if err != nil {
					dir.Close()
					return nil, err
				}
				info, err := f.Stat()
				if err != nil {
					f.Close()
					dir.Close()
					return nil, err
				}
				total += info.Size()
				if total > 64*MiB {
					f.Close()
					dir.Close()
					return nil, errors.New("evidence byte limit exceeded")
				}
				item := EvidenceFile{Path: path, Bytes: info.Size()}
				if hash {
					h := sha256.New()
					_, err = io.Copy(h, f)
					if err != nil {
						f.Close()
						dir.Close()
						return nil, err
					}
					item.SHA256 = hex.EncodeToString(h.Sum(nil))
				}
				f.Close()
				result = append(result, item)
			}
			if readErr == io.EOF {
				break
			}
		}
		dir.Close()
	}
	sort.Slice(result, func(i, j int) bool { return result[i].Path < result[j].Path })
	return result, nil
}

func collectEvidence(root string) ([]EvidenceFile, error) { return scanEvidence(root, true) }
func evidenceBudget(root string) error                    { _, err := scanEvidence(root, false); return err }

func checkDevice(cfg ScenarioConfig, spec ScenarioSpec, executor string) error {
	adapter := spec.Adapter
	if adapter == "" {
		adapter = "host"
	}
	if cfg.Device == "host" && adapter == "host" {
		return nil
	}
	var device struct {
		Protocol int    `json:"protocol"`
		Executor string `json:"executor"`
		Device   string `json:"device_id"`
		Adapter  string `json:"adapter"`
	}
	if _, err := readControl(filepath.Join(cfg.ExecutorRoot, "devices", sumBytes([]byte(cfg.Device)), "device.json"), &device); err != nil {
		return errors.New("requested device adapter is not configured")
	}
	if device.Protocol != deliveryProtocol || device.Executor != executor || device.Device != cfg.Device || device.Adapter != adapter {
		return errors.New("requested device adapter identity changed")
	}
	return nil
}

type spoolRegistry struct {
	Protocol int               `json:"protocol"`
	Root     string            `json:"root_identity"`
	Retained map[string]string `json:"retained"`
}

func reserveScenarioSpool(root string, state ScenarioState) error {
	s, err := openSlot(root)
	if err != nil {
		return err
	}
	defer s.Close()
	var registry spoolRegistry
	path := filepath.Join(root, "spool-registry.json")
	if _, err := readControl(path, &registry); os.IsNotExist(err) {
		registry = spoolRegistry{deliveryProtocol, s.root, make(map[string]string)}
	} else if err != nil {
		return err
	}
	if registry.Protocol != deliveryProtocol || registry.Root != s.root || registry.Retained == nil {
		return errors.New("spool registry identity changed")
	}
	if digest, ok := registry.Retained[state.Job]; ok {
		if digest != state.Digest {
			return errors.New("spool reservation identity changed")
		}
		return nil
	}
	if len(registry.Retained) >= 6 {
		return errors.New("result spool admission is full; collect and acknowledge retained jobs first")
	}
	registry.Retained[state.Job] = state.Digest
	return durableJSON(path, registry)
}
