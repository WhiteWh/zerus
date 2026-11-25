package main

import (
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

type GeneratedObject struct {
	Path      string `json:"path"`
	Identity  string `json:"identity"`
	Directory bool   `json:"directory"`
	Bytes     int64  `json:"bytes"`
	Modified  int64  `json:"modified"`
}
type ResultAcknowledgement struct {
	Request string `json:"request_id"`
	Result  string `json:"result_sha256"`
}
type acknowledgementIntent struct {
	Protocol int                   `json:"protocol"`
	Job      string                `json:"job_id"`
	Digest   string                `json:"request_sha256"`
	Root     string                `json:"root_identity"`
	Ack      ResultAcknowledgement `json:"acknowledgement"`
	Objects  []GeneratedObject     `json:"objects"`
}

func resultDigest(state ScenarioState) string {
	state.ResultHash = ""
	state.Revision = 0
	state.Collected = false
	state.Ack = ""
	b, _ := json.Marshal(state)
	return sumBytes(b)
}

func generatedObjects(work string) ([]GeneratedObject, error) {
	return generatedObjectsMode(work, false)
}
func generatedObjectsMode(work string, live bool) ([]GeneratedObject, error) {
	queue := []string{"runtime", "outputs"}
	objects := []GeneratedObject{}
	total := int64(0)
	for len(queue) > 0 {
		rel := queue[0]
		queue = queue[1:]
		if len(objects) > 512 {
			return nil, errors.New("generated object count exceeded")
		}
		path := filepath.Join(work, filepath.FromSlash(rel))
		dir, err := openDirectory(path)
		if err != nil {
			return nil, err
		}
		id, err := nativeIdentity(dir)
		if err != nil {
			dir.Close()
			return nil, err
		}
		objects = append(objects, GeneratedObject{Path: rel, Identity: id, Directory: true})
		for {
			entries, err := dir.ReadDir(32)
			if err != nil && err != io.EOF {
				dir.Close()
				return nil, err
			}
			for _, entry := range entries {
				child := rel + "/" + entry.Name()
				if !portablePath(child) || entry.Type()&os.ModeSymlink != 0 {
					dir.Close()
					return nil, errors.New("unsafe generated path")
				}
				if entry.IsDir() {
					queue = append(queue, child)
					if len(queue)+len(objects) > 512 {
						dir.Close()
						return nil, errors.New("generated object count exceeded")
					}
					continue
				}
				if len(objects) >= 512 {
					dir.Close()
					return nil, errors.New("generated object count exceeded")
				}
				open := func(path string) (*os.File, error) {
					if live {
						return statEvidence(path)
					}
					return openSealed(path, "", false)
				}
				f, err := open(filepath.Join(work, filepath.FromSlash(child)))
				if err != nil {
					dir.Close()
					return nil, err
				}
				id, idErr := nativeIdentity(f)
				info, statErr := f.Stat()
				_, modified, metaErr := fileMetadata(f)
				f.Close()
				if idErr != nil || statErr != nil || metaErr != nil {
					dir.Close()
					return nil, errors.New("generated object inspection failed")
				}
				total += info.Size()
				if total > 80*MiB {
					dir.Close()
					return nil, errors.New("generated output byte limit exceeded")
				}
				objects = append(objects, GeneratedObject{child, id, false, info.Size(), modified})
			}
			if err == io.EOF {
				break
			}
		}
		dir.Close()
	}
	controls := []string{"stdout.log", "parameters.json", "observation.json", "observation.lock"}
	if live {
		controls = nil
	}
	for _, name := range controls {
		f, err := openSealed(filepath.Join(work, name), "", false)
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return nil, err
		}
		id, idErr := nativeIdentity(f)
		info, statErr := f.Stat()
		_, modified, metaErr := fileMetadata(f)
		f.Close()
		if idErr != nil || statErr != nil || metaErr != nil {
			return nil, errors.New("generated control inspection failed")
		}
		objects = append(objects, GeneratedObject{name, id, false, info.Size(), modified})
	}
	sort.Slice(objects, func(i, j int) bool { return objects[i].Path < objects[j].Path })
	return objects, nil
}

func acknowledgeScenario(work string, cfg ScenarioConfig, executor string) error {
	ack := cfg.Acknowledgement
	if ack == nil || !validHex(ack.Request, 16) || !validHex(ack.Result, 32) {
		return errors.New("result acknowledgement identity required")
	}
	s, err := openSlot(work)
	if err != nil {
		return err
	}
	defer s.Close()
	var state ScenarioState
	if err := readTreeControl(filepath.Join(work, "job-state.json"), &state); err != nil {
		return err
	}
	if state.Job != cfg.Job || state.Digest != scenarioDigest(cfg, executor) || state.Executor != executor || state.WorkRoot != s.root || state.ResultHash != ack.Result {
		return errors.New("result acknowledgement binding changed")
	}
	if state.Collected {
		if state.Ack != ack.Request {
			return errors.New("result already acknowledged by another receipt")
		}
		if err := releaseScenarioSpool(cfg.ExecutorRoot, state); err != nil {
			return err
		}
		// The tombstone can have committed before queue release or intent
		// removal. Only discard an exactly bound metadata-only ACK intent.
		var pending acknowledgementIntent
		path := filepath.Join(work, "ack-intent.json")
		if err := readTreeControl(path, &pending); os.IsNotExist(err) {
			return nil
		} else if err != nil {
			return err
		}
		if pending.Protocol != deliveryProtocol || pending.Job != state.Job || pending.Digest != state.Digest || pending.Root != s.root || pending.Ack != *ack {
			return errors.New("collected acknowledgement intent changed")
		}
		return os.Remove(path)
	}
	if state.Status != "passed" && state.Status != "failed" && state.Status != "timeout" && state.Status != "infrastructure_error" && state.Status != "not_run" {
		return errors.New("uncertain or active jobs are not automatically collected")
	}
	if settled, err := namedJobSettled(state.Process); err != nil || !settled {
		return errors.New("scenario processes are not settled")
	}
	intentPath := filepath.Join(work, "ack-intent.json")
	var intent acknowledgementIntent
	if err := readTreeControl(intentPath, &intent); os.IsNotExist(err) {
		actual, err := generatedObjects(work)
		if err != nil {
			return err
		}
		if len(actual) != len(state.Cleanup) {
			return errors.New("changed generated objects are preserved for review")
		}
		for i, object := range actual {
			if object != state.Cleanup[i] {
				return errors.New("changed generated object is preserved for review")
			}
		}
		intent = acknowledgementIntent{deliveryProtocol, state.Job, state.Digest, s.root, *ack, actual}
		if err := durableTreeJSON(intentPath, intent); err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	if intent.Protocol != deliveryProtocol || intent.Job != state.Job || intent.Digest != state.Digest || intent.Root != s.root || intent.Ack != *ack {
		return errors.New("pending acknowledgement changed")
	}
	sort.Slice(intent.Objects, func(i, j int) bool { return len(intent.Objects[i].Path) > len(intent.Objects[j].Path) })
	for _, object := range intent.Objects {
		if !portablePath(object.Path) || (!strings.HasPrefix(object.Path, "runtime/") && !strings.HasPrefix(object.Path, "outputs/") && object.Path != "runtime" && object.Path != "outputs" && object.Path != "stdout.log" && object.Path != "parameters.json" && object.Path != "observation.json" && object.Path != "observation.lock") {
			return errors.New("acknowledgement escaped its generated output scope")
		}
		path := filepath.Join(work, filepath.FromSlash(object.Path))
		parents, _, err := holdDirectoryChain(filepath.Dir(path))
		if os.IsNotExist(err) {
			closeFiles(parents)
			continue
		}
		if err != nil {
			closeFiles(parents)
			return err
		}
		err = removeGenerated(path, object)
		closeFiles(parents)
		if err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	state.Collected = true
	state.Ack = ack.Request
	state.Evidence = nil
	state.Cleanup = nil
	if err := saveScenario(work, &state); err != nil {
		return err
	}
	if err := releaseScenarioSpool(cfg.ExecutorRoot, state); err != nil {
		return err
	}
	return os.Remove(intentPath)
}

func releaseScenarioSpool(root string, state ScenarioState) error {
	s, err := openSlot(root)
	if err != nil {
		return err
	}
	defer s.Close()
	var registry spoolRegistry
	path := filepath.Join(root, "spool-registry.json")
	if _, err := readControl(path, &registry); err != nil {
		return err
	}
	if registry.Protocol != deliveryProtocol || registry.Root != s.root {
		return errors.New("spool registry identity changed")
	}
	if digest, ok := registry.Retained[state.Job]; ok {
		if digest != state.Digest {
			return errors.New("spool acknowledgement identity changed")
		}
		delete(registry.Retained, state.Job)
		return durableJSON(path, registry)
	}
	return nil
}
