"""Hard bounds for owned local qualification children, with a startup event gate."""
import ctypes
from ctypes import wintypes
import uuid


class BasicLimits(ctypes.Structure):
    _fields_ = [("process_time", ctypes.c_longlong), ("job_time", ctypes.c_longlong),
                ("flags", wintypes.DWORD), ("minimum_working", ctypes.c_size_t),
                ("maximum_working", ctypes.c_size_t), ("active_processes", wintypes.DWORD),
                ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD),
                ("scheduling", wintypes.DWORD)]


class IOCounters(ctypes.Structure):
    _fields_ = [(name, ctypes.c_ulonglong) for name in
                ("read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")]


class ExtendedLimits(ctypes.Structure):
    _fields_ = [("basic", BasicLimits), ("io", IOCounters),
                ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                ("peak_process_memory", ctypes.c_size_t), ("peak_job_memory", ctypes.c_size_t)]


kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
kernel.CreateJobObjectW.restype = wintypes.HANDLE
kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
kernel.SetInformationJobObject.restype = wintypes.BOOL
kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
kernel.AssignProcessToJobObject.restype = wintypes.BOOL
kernel.CreateEventW.argtypes = [ctypes.c_void_p, wintypes.BOOL, wintypes.BOOL, wintypes.LPCWSTR]
kernel.CreateEventW.restype = wintypes.HANDLE
kernel.SetEvent.argtypes = [wintypes.HANDLE]
kernel.SetEvent.restype = wintypes.BOOL
kernel.CloseHandle.argtypes = [wintypes.HANDLE]


class ResourceGate:
    def __init__(self, memory_bytes):
        if memory_bytes < 32 * 1024 * 1024:
            raise ValueError("qualification process memory limit is too small")
        self.job = self.event = None
        self.name = "Local\\ZerusDesyncQualification-" + uuid.uuid4().hex
        try:
            self.job = kernel.CreateJobObjectW(None, None)
            if not self.job:
                raise RuntimeError("cannot create qualification job")
            limits = ExtendedLimits()
            # Process and job committed-memory caps, kill owned children when
            # the job closes, suppress Windows unhandled-exception dialogs.
            limits.basic.flags = 0x100 | 0x200 | 0x2000 | 0x400
            limits.process_memory = limits.job_memory = memory_bytes
            if not kernel.SetInformationJobObject(self.job, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
                raise RuntimeError("cannot install hard qualification memory limit")
            self.event = kernel.CreateEventW(None, True, False, self.name)
            if not self.event or ctypes.get_last_error() == 183:
                raise RuntimeError("cannot create unique qualification startup gate")
        except BaseException:
            self.close()
            raise

    def admit(self, process):
        if not kernel.AssignProcessToJobObject(self.job, wintypes.HANDLE(int(process._handle))):
            raise RuntimeError("cannot enforce qualification job membership")
        if not kernel.SetEvent(self.event):
            raise RuntimeError("cannot open qualification startup gate")

    def close(self):
        for name in ("event", "job"):
            handle = getattr(self, name, None)
            if handle:
                kernel.CloseHandle(handle)
                setattr(self, name, None)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
