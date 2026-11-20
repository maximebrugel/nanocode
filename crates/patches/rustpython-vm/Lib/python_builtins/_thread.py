"""Cooperative _thread for interpreters built without OS threads.

There is one OS thread, so Python threads take turns.  A started thread does
not run at once: it waits in a queue until the running thread blocks (on a
held lock, a Condition, an Event or Queue, or join()) or the interpreter shuts
down; then queued threads run, one at a time, each to its end or until the
thing it waits for is free.  A thread cannot be suspended halfway, so a thread
that blocks while nothing else can run is stuck:

* a timed wait sleeps out its timeout and fails, as CPython's would;
* an untimed wait in a started thread ends that thread quietly (it raises
  _ThreadParked, a SystemExit, which threading's excepthook ignores), much as
  CPython leaves such a thread blocked forever;
* an untimed wait in the main thread raises RuntimeError instead of hanging.

Locks are real: a held lock cannot be acquired again, and RLock tracks its
owner by thread ident.
"""

__all__ = [
    "error",
    "start_new_thread",
    "exit",
    "get_ident",
    "allocate_lock",
    "interrupt_main",
    "LockType",
    "RLock",
    "_count",
    "start_joinable_thread",
    "daemon_threads_allowed",
    "_shutdown",
    "_make_thread_handle",
    "_ThreadHandle",
    "_get_main_thread_ident",
    "_is_main_interpreter",
    "_local",
    "_yield",
]

TIMEOUT_MAX = 2**31

# Marks this module as the cooperative implementation (threading checks it).
_COOPERATIVE = True

_MAIN_THREAD_IDENT = -1
_current_ident = _MAIN_THREAD_IDENT
_current_daemon = False
_next_ident = 1
# Started threads that have not run yet, oldest first: [handle, function, daemon].
_pending = []

error = RuntimeError


class _ThreadParked(SystemExit):
    """Ends a thread that would block forever while no other thread can run."""


def _new_ident():
    global _next_ident
    ident = _next_ident
    _next_ident += 1
    return ident


def _run(handle, function, daemon):
    global _current_ident, _current_daemon
    previous = (_current_ident, _current_daemon)
    _current_ident = handle._ident
    _current_daemon = daemon
    handle._running = True
    try:
        function()
    except SystemExit:
        pass
    except:
        import traceback

        traceback.print_exc()
    finally:
        _current_ident, _current_daemon = previous
        handle._running = False
        handle._set_done()


def _run_next():
    """Run the oldest queued thread; False when none is queued."""
    if not _pending:
        return False
    handle, function, daemon = _pending.pop(0)
    _run(handle, function, daemon)
    return True


def _yield():
    """Run every queued thread: the running one is about to block outside
    _thread (in select(), say), where a thread it waits for could not run.
    Returns whether any thread ran."""
    ran = False
    while _run_next():
        ran = True
    return ran


def _wait_for(ready, timeout):
    """Block until ready() is true: let queued threads run first, then sleep out
    a timeout (timeout < 0 means none).  Returns ready()."""
    if ready():
        return True
    if timeout == 0:
        return False
    while _run_next():
        if ready():
            return True
    if timeout > 0:
        import time

        time.sleep(timeout)
        return ready()
    if _current_ident == _MAIN_THREAD_IDENT:
        raise RuntimeError(
            "deadlock: the main thread would block forever, and no other thread "
            "can run (threads run cooperatively in this interpreter)"
        )
    raise _ThreadParked()


def _check_timeout(blocking, timeout):
    if timeout is None:
        timeout = -1
    if not blocking and timeout != -1:
        raise ValueError("can't specify a timeout for a non-blocking call")
    if timeout < 0 and timeout != -1:
        raise ValueError("timeout value must be a non-negative number")
    if timeout > TIMEOUT_MAX:
        raise OverflowError("timeout value is too large")
    return timeout if blocking else 0


def start_new_thread(function, args, kwargs={}):
    """Queue function(*args, **kwargs) to run as a new thread; return its ident."""
    if type(args) != type(tuple()):
        raise TypeError("2nd arg must be a tuple")
    if type(kwargs) != type(dict()):
        raise TypeError("3rd arg must be a dict")
    if not callable(function):
        raise TypeError("first arg must be callable")

    def bootstrap():
        global _interrupt
        try:
            function(*args, **kwargs)
        except SystemExit:
            pass
        except BaseException as e:
            import sys

            hook = getattr(sys, "unraisablehook", None)
            import traceback

            sys.stderr.write("Unhandled exception in thread started by %r\n" % (function,))
            traceback.print_exc()

    handle = _ThreadHandle()
    handle._ident = _new_ident()
    _pending.append([handle, bootstrap, True])
    return handle._ident


def start_joinable_thread(function, handle=None, daemon=True):
    """Queue function to run as a new thread; return its handle."""
    if handle is None:
        handle = _ThreadHandle()
    if handle._started:
        raise RuntimeError("thread already started")
    handle._started = True
    handle._ident = _new_ident()
    _pending.append([handle, function, daemon])
    return handle


def daemon_threads_allowed():
    return True


def _shutdown():
    """Run every queued non-daemon thread (threads they start included)."""
    while True:
        for i, entry in enumerate(_pending):
            if not entry[2]:
                del _pending[i]
                _run(*entry)
                break
        else:
            return


def _make_thread_handle(ident):
    handle = _ThreadHandle()
    handle._ident = ident
    handle._started = True
    return handle


def _get_main_thread_ident():
    return _MAIN_THREAD_IDENT


def _is_main_interpreter():
    return True


def exit():
    raise SystemExit


def get_ident():
    return _current_ident


def allocate_lock():
    return LockType()


def stack_size(size=None):
    if size is not None:
        raise error("setting thread stack size not supported")
    return 0


def _set_sentinel():
    return LockType()


def _count():
    return len(_pending)


class LockType(object):
    """A non-reentrant lock: acquiring it while held blocks (see the module doc)."""

    def __init__(self):
        self.locked_status = False

    def acquire(self, blocking=True, timeout=-1):
        timeout = _check_timeout(blocking, timeout)
        if not _wait_for(self._free, timeout):
            return False
        self.locked_status = True
        return True

    def _free(self):
        return not self.locked_status

    def __enter__(self):
        return self.acquire()

    def __exit__(self, typ, val, tb):
        self.release()

    def release(self):
        if not self.locked_status:
            raise error("release unlocked lock")
        self.locked_status = False

    def locked(self):
        return self.locked_status

    def _at_fork_reinit(self):
        self.locked_status = False

    def __repr__(self):
        return "<%s %s.%s object at %s>" % (
            "locked" if self.locked_status else "unlocked",
            self.__class__.__module__,
            self.__class__.__qualname__,
            hex(id(self)),
        )


class _ThreadHandle:
    def __init__(self):
        self._ident = _MAIN_THREAD_IDENT
        self._done = False
        self._started = False
        self._running = False

    @property
    def ident(self):
        return self._ident

    def _set_done(self):
        self._done = True

    def is_done(self):
        return self._done

    def join(self, timeout=None):
        if self._ident == _current_ident and not self._done:
            raise RuntimeError("Cannot join current thread")
        for i, entry in enumerate(_pending):
            if entry[0] is self:
                del _pending[i]
                _run(*entry)
                return
        if timeout is None:
            timeout = -1
        _wait_for(self.is_done, timeout)

    def __repr__(self):
        return f"<_ThreadHandle ident={self._ident}>"


_interrupt = False


def interrupt_main(signum=2):
    if _current_ident == _MAIN_THREAD_IDENT:
        raise KeyboardInterrupt
    global _interrupt
    _interrupt = True


class RLock:
    """A reentrant lock owned by the thread that holds it."""

    def __init__(self):
        self._owner = None
        self._count = 0

    def acquire(self, blocking=True, timeout=-1):
        timeout = _check_timeout(blocking, timeout)
        me = get_ident()
        if self._owner == me:
            self._count += 1
            return True
        if not _wait_for(self._free, timeout):
            return False
        self._owner = me
        self._count = 1
        return True

    def _free(self):
        return self._owner is None

    def __enter__(self):
        return self.acquire()

    def __exit__(self, typ, val, tb):
        self.release()

    def release(self):
        if self._owner != get_ident():
            raise RuntimeError("cannot release un-acquired lock")
        self._count -= 1
        if not self._count:
            self._owner = None

    def locked(self):
        return self._owner is not None

    def _is_owned(self):
        return self._owner == get_ident()

    def _recursion_count(self):
        return self._count if self._owner == get_ident() else 0

    def _release_save(self):
        if self._owner != get_ident():
            raise RuntimeError("cannot release un-acquired lock")
        state = (self._count, self._owner)
        self._count = 0
        self._owner = None
        return state

    def _acquire_restore(self, state):
        _wait_for(self._free, -1)
        self._count, self._owner = state

    def _at_fork_reinit(self):
        self._owner = None
        self._count = 0

    def __repr__(self):
        return "<%s %s.%s object owner=%s count=%s at %s>" % (
            "locked" if self._owner is not None else "unlocked",
            self.__class__.__module__,
            self.__class__.__qualname__,
            self._owner if self._owner is not None else 0,
            self._count,
            hex(id(self)),
        )


class _local:
    """Thread-local storage: each thread ident sees its own attributes."""

    def __new__(cls, *args, **kw):
        if (args or kw) and cls.__init__ is object.__init__:
            raise TypeError("Initialization arguments are not supported")
        self = object.__new__(cls)
        object.__setattr__(self, "_local__args", (args, kw))
        object.__setattr__(self, "_local__dicts", {get_ident(): object.__getattribute__(self, "__dict__")})
        return self

    def _local__switch(self):
        dicts = object.__getattribute__(self, "_local__dicts")
        me = get_ident()
        d = dicts.get(me)
        current = object.__getattribute__(self, "__dict__")
        if d is None:
            d = {
                "_local__args": current["_local__args"],
                "_local__dicts": dicts,
            }
            dicts[me] = d
            object.__setattr__(self, "__dict__", d)
            args, kw = d["_local__args"]
            cls = type(self)
            if cls.__init__ is not object.__init__:
                cls.__init__(self, *args, **kw)
        elif d is not current:
            object.__setattr__(self, "__dict__", d)

    def __getattribute__(self, name):
        _local._local__switch(self)
        return object.__getattribute__(self, name)

    def __setattr__(self, name, value):
        if name == "__dict__":
            raise AttributeError("%r object attribute '__dict__' is read-only" % type(self).__name__)
        _local._local__switch(self)
        return object.__setattr__(self, name, value)

    def __delattr__(self, name):
        if name == "__dict__":
            raise AttributeError("%r object attribute '__dict__' is read-only" % type(self).__name__)
        _local._local__switch(self)
        return object.__delattr__(self, name)
