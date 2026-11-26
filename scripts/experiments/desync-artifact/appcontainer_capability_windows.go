package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

// runPrintJobsCapability prints the installation's jobs capability SID for
// the "root" named in CONFIG (an executor or qualification configuration)
// and the one operator command that lets it list the volume root's folder
// names. It reads nothing else and changes nothing.
func runPrintJobsCapability(configPath string) error {
	data, err := os.ReadFile(configPath)
	if err != nil {
		return err
	}
	var cfg struct {
		Root string `json:"root"`
	}
	if err := json.Unmarshal(data, &cfg); err != nil || cfg.Root == "" {
		return errors.New("configuration names no root")
	}
	parents, root, err := holdDirectoryChain(cfg.Root)
	closeFiles(parents)
	if err != nil {
		return err
	}
	name := jobsCapabilityName(sumBytes([]byte(root)))
	sid, err := capabilitySID(name)
	if err != nil {
		return err
	}
	fmt.Println(sid.String())
	fmt.Fprintf(os.Stderr, "operator, once per volume: icacls %s /grant \"*%s:(RD,X,RA)\"\n", filepath.VolumeName(cfg.Root)+`\`, sid)
	return nil
}
