"""Finite custody policy shared by the native gateway fixture and pure controls."""
import re


class CustodyUnknown(RuntimeError):
    pass


def termination_adapter(kernel):
    """Adapt the two-argument Windows API to the custody callback contract."""
    return lambda handle: kernel.TerminateProcess(handle, 1)


def guard_root_cleanup(case, temporary, may_remove, *, cleanup_callback=None):
    """Prevent unittest and TemporaryDirectory GC from bypassing custody."""
    original = temporary.cleanup if cleanup_callback is None else cleanup_callback

    def cleanup(*args, **kwargs):
        if not may_remove():
            raise CustodyUnknown(f"connector custody unknown; private fixture retained at {temporary.name}")
        original(*args, **kwargs)
    for index, (callback, args, kwargs) in enumerate(case._cleanups):
        if callback == original:
            case._cleanups[index] = (cleanup, args, kwargs)
            temporary._finalizer.detach()
            return
    raise RuntimeError("private root cleanup registration was not found")


class ConnectorCustody:
    def __init__(self, revision, read_control, pin, wait, terminate, close, stop):
        self.revision = revision
        self.read_control, self.pin = read_control, pin
        self.wait, self.terminate, self.close, self.stop = wait, terminate, close, stop
        self.attempted = False
        self.handle = None
        self.control = None
        self.exited = False
        self.identity_unknown = False
        self.preexisting = False

    def begin(self):
        if self.attempted:
            raise CustodyUnknown("startup requires a fresh private control identity")
        # This reservation precedes the launcher call, including timeout/failure.
        self.attempted = True
        try:
            if self.read_control() is not None:
                raise CustodyUnknown("startup requires a fresh private control identity")
        except Exception:
            self.preexisting = True
            raise

    def recover(self):
        if not self.attempted or self.preexisting:
            raise CustodyUnknown("startup custody was not reserved")
        control = self.read_control()
        if not isinstance(control, dict) or control.get("revision") != self.revision:
            raise CustodyUnknown("private admission revision is unknown")
        generation, pid = control.get("generation"), control.get("pid")
        if (not isinstance(generation, str) or re.fullmatch(r"[0-9a-f]{32}", generation) is None
                or type(pid) is not int or pid <= 0
                or self.control is not None and self.control != control):
            raise CustodyUnknown("private generation identity is unknown or changed")
        if self.handle is not None:
            return
        handle = self.pin(control)  # Must prove the unique copied image, no PID kill.
        if not handle:
            raise CustodyUnknown("owned process handle could not be pinned")
        if self.read_control() != control:
            self.close(handle)
            raise CustodyUnknown("private control changed while pinning")
        self.control, self.handle = dict(control), handle

    def cleanup(self):
        if self.identity_unknown:
            raise CustodyUnknown("private control identity became unknown; retain the root")
        if not self.attempted or self.exited:
            return False
        if self.handle is None:
            self.recover()  # Uncertain launch still owns cleanup; absence is unknown.
        stop_error = None
        try:
            if self.read_control() != self.control:
                self.identity_unknown = True
                raise CustodyUnknown("private control changed before stop")
            self.stop(self.control)
        except Exception as error:
            stop_error = error
        forced = False
        if self.wait(self.handle, 50000) != 0:
            forced = True
            terminated = self.terminate(self.handle)
            final = self.wait(self.handle, 5000)
            if final != 0:
                raise CustodyUnknown(f"owned process exit unconfirmed (terminate={bool(terminated)}, wait={final})")
        # Only a signalled process handle permits closing custody/deleting roots.
        self.exited = True
        self.close(self.handle)
        self.handle = None
        try:
            if self.read_control() != self.control:
                self.identity_unknown = True
        except Exception:
            self.identity_unknown = True
        if self.identity_unknown:
            raise CustodyUnknown("owned handle exited but private control changed; retain the root")
        if stop_error:
            raise RuntimeError("connector stop was not confirmed; owned handle exit was confirmed") from stop_error
        return forced

    @property
    def may_remove_root(self):
        return not self.attempted or self.exited and not self.identity_unknown
