// Local PDH counters, with English paths and bounded wildcard results. No WMI,
// command process, remote machine path or native agent participates.
package main

import (
	"errors"
	"golang.org/x/sys/windows"
	"path/filepath"
	"unsafe"
)

var pdhDLL = windows.NewLazySystemDLL("pdh.dll")
var pdhOpen = pdhDLL.NewProc("PdhOpenQueryW")
var pdhAdd = pdhDLL.NewProc("PdhAddEnglishCounterW")
var pdhCollect = pdhDLL.NewProc("PdhCollectQueryData")
var pdhValue = pdhDLL.NewProc("PdhGetFormattedCounterValue")
var pdhArray = pdhDLL.NewProc("PdhGetFormattedCounterArrayW")
var pdhClose = pdhDLL.NewProc("PdhCloseQuery")

type pdhFormatted struct {
	Status  uint32
	Padding uint32
	Value   float64
}
type pdhItem struct {
	Name  *uint16
	Value pdhFormatted
}
type pdhSource struct {
	query, cpu, disk uintptr
	volumes          []string
}

func (p *pdhSource) Close() {
	if p.query != 0 {
		pdhClose.Call(p.query)
		p.query = 0
	}
}
func newLoadSource(paths ...string) (loadSource, error) {
	p := &pdhSource{}
	if len(paths) > 3 {
		return nil, errors.New("local volume inventory exceeds bound")
	}
	seen := map[string]bool{}
	for _, path := range paths {
		if err := localPath(path); err != nil {
			return nil, err
		}
		volume := filepath.VolumeName(path) + `\`
		if !seen[volume] {
			p.volumes = append(p.volumes, volume)
			seen[volume] = true
		}
	}
	if code, _, _ := pdhOpen.Call(0, 0, uintptr(unsafe.Pointer(&p.query))); code != 0 {
		return nil, errors.New("local load query unavailable")
	}
	for _, counter := range []struct {
		path   string
		handle *uintptr
	}{{`\Processor Information(_Total)\% Processor Time`, &p.cpu}, {`\PhysicalDisk(*)\% Idle Time`, &p.disk}} {
		name, err := windows.UTF16PtrFromString(counter.path)
		if err != nil {
			p.Close()
			return nil, err
		}
		if code, _, _ := pdhAdd.Call(p.query, uintptr(unsafe.Pointer(name)), 0, uintptr(unsafe.Pointer(counter.handle))); code != 0 {
			p.Close()
			return nil, errors.New("local load counter unavailable")
		}
	}
	if code, _, _ := pdhCollect.Call(p.query); code != 0 {
		p.Close()
		return nil, errors.New("local load baseline unavailable")
	}
	return p, nil
}
func (p *pdhSource) Sample() (LoadSample, error) {
	var sample LoadSample
	if code, _, _ := pdhCollect.Call(p.query); code != 0 {
		return sample, errors.New("local load collection unavailable")
	}
	var value pdhFormatted
	if code, _, _ := pdhValue.Call(p.cpu, 0x200, 0, uintptr(unsafe.Pointer(&value))); code != 0 || value.Status > 1 {
		return sample, errors.New("local CPU counter invalid")
	}
	sample.CPU = value.Value
	var size, count uint32
	code, _, _ := pdhArray.Call(p.disk, 0x200, uintptr(unsafe.Pointer(&size)), uintptr(unsafe.Pointer(&count)), 0)
	if uint32(code) != 0x800007d2 || size == 0 || size > 65536 {
		return sample, errors.New("local disk counters unavailable or exceed bound")
	}
	buffer := make([]byte, size)
	code, _, _ = pdhArray.Call(p.disk, 0x200, uintptr(unsafe.Pointer(&size)), uintptr(unsafe.Pointer(&count)), uintptr(unsafe.Pointer(&buffer[0])))
	if code != 0 || count < 1 || count > 64 || uintptr(count)*unsafe.Sizeof(pdhItem{}) > uintptr(len(buffer)) {
		return sample, errors.New("local disk counter inventory invalid")
	}
	base := uintptr(unsafe.Pointer(&buffer[0]))
	end := base + uintptr(len(buffer))
	physical := 0
	for i := uint32(0); i < count; i++ {
		item := (*pdhItem)(unsafe.Pointer(&buffer[uintptr(i)*unsafe.Sizeof(pdhItem{})]))
		at := uintptr(unsafe.Pointer(item.Name))
		if at < base || at+2 > end || at%2 != 0 {
			return sample, errors.New("local disk counter name invalid")
		}
		units := unsafe.Slice(item.Name, min(int((end-at)/2), 256))
		name := []uint16{}
		terminated := false
		for _, unit := range units {
			if unit == 0 {
				terminated = true
				break
			}
			name = append(name, unit)
		}
		if !terminated {
			return sample, errors.New("local disk counter name exceeds bound")
		}
		if windows.UTF16ToString(name) == "_Total" {
			continue
		}
		if item.Value.Status > 1 || !validLoad(LoadSample{Disk: item.Value.Value}) {
			return sample, errors.New("local disk counter invalid")
		}
		physical++
		sample.DiskBusy = max(sample.DiskBusy, 100-item.Value.Value)
	}
	for _, volume := range p.volumes {
		name, err := windows.UTF16PtrFromString(volume)
		if err != nil {
			return sample, err
		}
		var available, total, free uint64
		if err := windows.GetDiskFreeSpaceEx(name, &available, &total, &free); err != nil || total == 0 || available > total {
			return sample, errors.New("executor volume capacity unavailable")
		}
		sample.Disk = max(sample.Disk, 100*(1-float64(available)/float64(total)))
	}
	if physical == 0 || !validLoad(sample) {
		return sample, errors.New("local physical disk counters absent")
	}
	return sample, nil
}
