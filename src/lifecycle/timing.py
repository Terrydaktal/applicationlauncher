"""Opt-in, bounded scalar timing for one lifecycle operation; never restart policy."""
import json
import os
from pathlib import Path
import re
import stat
import time
import fcntl
from functools import wraps

MAX_EVENTS = 256
MAX_BYTES = 128 * 1024
MAX_FILES = 64
MAX_TTL_SECONDS = 300
STAGES = frozenset({
    "capture", "tab_plan", "fallback_select", "confirmation_wait", "revalidate",
    "restart_plan",
    "stage_replacement", "stop_application", "release", "verify", "commit", "recover",
    "capture_terminal",
})


class Disabled:
    state = "disabled"
    fault = None

    def span(self, stage, **fields):
        return self

    def __enter__(self):
        return self

    def __exit__(self, *error):
        return False

    def event(self, stage, **fields):
        pass

    def close(self):
        pass

    def status(self):
        return {"state": self.state, "first_error": self.fault}


DISABLED = Disabled()
current = DISABLED


def observed(stage):
    def decorate(function):
        @wraps(function)
        def measured(*arguments, **keywords):
            with current.span(stage):
                return function(*arguments, **keywords)
        return measured
    return decorate


def private(metadata, directory=False):
    return ((stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode))
            and metadata.st_uid == os.getuid() and not metadata.st_mode & 0o077
            and (directory or metadata.st_nlink == 1))


def from_directory(directory):
    """No clocks, buffers, output files or helpers unless the daemon issued a lease."""
    global current
    current = DISABLED
    directory = Path(directory)
    try:
        descriptor = os.open(directory / "timing-request.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return DISABLED
    except OSError as error:
        result = Disabled()
        result.state = "unavailable"
        result.fault = dict(type=type(error).__name__[:48], errno=error.errno)
        return result
    try:
        with os.fdopen(descriptor, "rb") as stream:
            if not private(os.fstat(stream.fileno())):
                raise ValueError("unsafe lease")
            data = stream.read(4097)
        if len(data) > 4096:
            raise ValueError("oversized lease")
        lease = json.loads(data)
        remaining = lease["deadline_unix_ms"] - time.time_ns() // 1_000_000
        if lease["schema_version"] != 1 or not 0 < remaining <= MAX_TTL_SECONDS * 1000:
            return DISABLED
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,96}", directory.name):
            raise ValueError("invalid operation")
        if not isinstance(lease["build_id"], str) or not re.fullmatch(r"[A-Za-z0-9_.+-]{1,96}", lease["build_id"]):
            raise ValueError("invalid build identity")
        deadline = time.monotonic_ns() + remaining * 1_000_000
        current = Trace(directory, lease["build_id"], deadline)
        return current
    except (OSError, ValueError, TypeError, KeyError) as error:
        result = Disabled()
        result.state = "unavailable"
        result.fault = dict(type=type(error).__name__[:48], errno=getattr(error, "errno", None))
        return result


def open_trace(root, name):
    lock = os.open(root / ".retention.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        if not private(os.fstat(lock)):
            raise ValueError("unsafe timing allocation lock")
        # Serialize allocation, not event writes; contention disables this trace.
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        entries = []
        for index, entry in enumerate(root.iterdir()):
            if index >= MAX_FILES * 2:
                raise ValueError("timing retention overflow")
            if entry.suffix == ".jsonl":
                entries.append(entry)
        for entry in sorted(entries, key=lambda path: path.lstat().st_mtime_ns):
            if len(entries) < MAX_FILES:
                break
            descriptor = os.open(entry, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            try:
                if not private(os.fstat(descriptor)):
                    raise ValueError("unsafe timing history")
                try:
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    continue
                entry.unlink()
                entries.remove(entry)
            finally:
                os.close(descriptor)
        if len(entries) >= MAX_FILES:
            raise ValueError("all timing slots are active")
        descriptor = os.open(root / (name + ".jsonl"), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            os.close(descriptor)
            raise
        return descriptor
    finally:
        os.close(lock)


class Trace:
    def __init__(self, directory, build_id, deadline_ns):
        root = directory.parent / "timings"
        root.mkdir(mode=0o700, exist_ok=True)
        if not private(root.lstat(), directory=True):
            raise ValueError("unsafe timing directory")
        self.path = root / (directory.name + ".jsonl")
        self.descriptor = open_trace(root, directory.name)
        self.started = time.monotonic_ns()
        self.deadline = deadline_ns
        self.sequence = self.span_id = self.bytes_written = self.dropped = 0
        self.stack = []
        self.state = "active"
        self.fault = None
        self.identity = dict(schema_version=1, operation_id=directory.name, pid=os.getpid(), build_id=str(build_id)[:96])
        self._write(dict(event="started", unix_ms=time.time_ns() // 1_000_000, monotonic_ns=self.started))

    def _write(self, fields):
        if self.descriptor is None:
            return
        try:
            row = dict(self.identity, sequence=self.sequence, **fields)
            data = (json.dumps(row, separators=(",", ":")) + "\n").encode()
            if len(data) > 1024 or self.bytes_written + len(data) > MAX_BYTES or self.sequence >= MAX_EVENTS:
                self.dropped += 1
                return
            if os.write(self.descriptor, data) != len(data):
                raise OSError("short trace write")
            self.bytes_written += len(data)
            self.sequence += 1
        except (OSError, ValueError, TypeError) as error:
            self.state = "unavailable"
            if self.fault is None:
                self.fault = dict(type=type(error).__name__[:48], errno=getattr(error, "errno", None))

    def _allowed(self):
        if self.state != "active":
            return False
        if time.monotonic_ns() >= self.deadline:
            self.state = "expired"
            self._write(dict(event="expired", dropped=self.dropped))
            return False
        if self.sequence >= MAX_EVENTS - 1 or self.bytes_written >= MAX_BYTES - 1024:
            self.dropped += 1
            self.state = "truncated"
            self._write(dict(event="truncated", dropped=self.dropped))
            return False
        return True

    def span(self, stage, **fields):
        if stage not in STAGES or not self._allowed() or len(self.stack) >= 8:
            return DISABLED
        return Span(self, stage, fields)

    def event(self, stage, **fields):
        if stage in STAGES and self._allowed():
            self._write(dict(event="observation", stage=stage, monotonic_ns=time.monotonic_ns(), **safe_fields(fields)))

    def status(self):
        return dict(state=self.state, file=str(self.path), events=self.sequence,
                    bytes=self.bytes_written, dropped=self.dropped, first_error=self.fault,
                    current_stage=self.stack[-1][1] if self.stack else None)

    def close(self):
        if self.descriptor is not None:
            if self.state != "unavailable":
                self._write(dict(event="finished", state=self.state, dropped=self.dropped))
            try:
                os.close(self.descriptor)
            except OSError:
                self.state = "unavailable"
            self.descriptor = None
            if self.state == "active":
                self.state = "complete"


def safe_fields(fields):
    result = {}
    for key in ("bytes", "cache_hit", "attempt"):
        value = fields.get(key)
        if isinstance(value, (int, bool)) and 0 <= value <= 2**63 - 1:
            result[key] = value
    for key, allowed in {
        "role": {"application", "return_shell", "terminal"},
        "method": {"reflink", "copy"},
        "route": {"native", "compatibility"},
    }.items():
        if isinstance(fields.get(key), str) and fields[key] in allowed:
            result[key] = fields[key]
    return result


class Span:
    def __init__(self, trace, stage, fields):
        self.trace, self.stage, self.fields = trace, stage, safe_fields(fields)

    def __enter__(self):
        trace = self.trace
        trace.span_id += 1
        self.identity = trace.span_id
        self.parent = trace.stack[-1][0] if trace.stack else None
        self.wall, self.cpu = time.monotonic_ns(), time.process_time_ns()
        trace.stack.append((self.identity, self.stage))
        trace._write(dict(event="begin", span_id=self.identity, parent_span_id=self.parent,
                          stage=self.stage, monotonic_ns=self.wall, **self.fields))
        return self

    def __exit__(self, error_type, error, traceback):
        trace = self.trace
        wall, cpu = time.monotonic_ns(), time.process_time_ns()
        if trace._allowed():
            trace._write(dict(event="end", span_id=self.identity, parent_span_id=self.parent,
                              stage=self.stage, monotonic_ns=wall, duration_ns=max(0, wall - self.wall),
                              cpu_ns=max(0, cpu - self.cpu), outcome="error" if error_type else "ok",
                              error_type=error_type.__name__[:48] if error_type else None,
                              errno=error.errno if isinstance(error, OSError) else None, **self.fields))
        trace.stack.pop()
        return False
