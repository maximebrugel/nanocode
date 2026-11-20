import _host
import errno as _errno

F_OK, R_OK, W_OK, X_OK = 0, 4, 2, 1
O_RDONLY, O_WRONLY, O_RDWR, O_ACCMODE = 0, 1, 2, 3
O_CREAT, O_EXCL, O_NOCTTY, O_TRUNC, O_APPEND, O_NONBLOCK = 0o100, 0o200, 0o400, 0o1000, 0o2000, 0o4000
O_DIRECTORY, O_NOFOLLOW, O_CLOEXEC = 0o200000, 0o400000, 0o2000000
SEEK_SET, SEEK_CUR, SEEK_END = 0, 1, 2
EX_OK = 0
error = OSError
_have_functions = []
environ = dict(_host.env())

for _name, _code in (('EPERM', 1), ('ENOENT', 2), ('EIO', 5), ('EBADF', 9), ('EAGAIN', 11), ('ENOMEM', 12), ('EACCES', 13), ('EEXIST', 17), ('EXDEV', 18), ('ENOTDIR', 20), ('EISDIR', 21), ('EINVAL', 22), ('ENOTTY', 25), ('EFBIG', 27), ('ENOSPC', 28), ('ESPIPE', 29), ('EROFS', 30), ('EPIPE', 32), ('ENOSYS', 38), ('ENOTEMPTY', 39), ('ELOOP', 40), ('ENOTSUP', 95)):
    if not hasattr(_errno, _name):
        setattr(_errno, _name, _code)
        _errno.errorcode[_code] = _name
_TEXT = {
    _errno.EPERM: 'Operation not permitted', _errno.ENOENT: 'No such file or directory', _errno.EIO: 'Input/output error',
    _errno.EBADF: 'Bad file descriptor', _errno.EACCES: 'Permission denied', _errno.EEXIST: 'File exists',
    _errno.ENOTDIR: 'Not a directory', _errno.EISDIR: 'Is a directory', _errno.EINVAL: 'Invalid argument',
    _errno.ENOTTY: 'Inappropriate ioctl for device', _errno.EFBIG: 'File too large', _errno.ENOSPC: 'No space left on device',
    _errno.ESPIPE: 'Illegal seek', _errno.EROFS: 'Read-only file system', _errno.EPIPE: 'Broken pipe',
    _errno.ENOSYS: 'Function not implemented', _errno.ENOTEMPTY: 'Directory not empty', _errno.ENOTSUP: 'Operation not supported',
}
_KINDS = {
    _errno.ENOENT: FileNotFoundError, _errno.EEXIST: FileExistsError, _errno.ENOTDIR: NotADirectoryError,
    _errno.EISDIR: IsADirectoryError, _errno.EACCES: PermissionError, _errno.EPERM: PermissionError,
}


def strerror(code):
    return _TEXT.get(code, 'Unknown error %d' % code)


def _fail(code, path=None):
    kind = _KINDS.get(code, OSError)
    if path is None:
        return kind(code, strerror(code))
    return kind(code, strerror(code), path)


def _rename_function(call, fn):
    call.__name__ = fn.__name__
    call.__qualname__ = fn.__qualname__
    call.__doc__ = fn.__doc__
    return call


def _named(fn):
    # The host reports the absolute path it resolved; CPython's errors name
    # the path as the caller gave it.
    def call(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except OSError as error:
            path = args[0] if args else kwargs.get('path')
            if error.errno is None or error.filename is None or path is None or isinstance(path, int):
                raise
            if error.errno == _errno.ENOENT and _under_file(path):
                raise _fail(_errno.ENOTDIR, fspath(path)) from None
            raise type(error)(error.errno, error.strerror, fspath(path)) from None
    return _rename_function(call, fn)


def _under_file(path):
    # Whether a directory the path passes through is a file, as ENOTDIR reports.
    try:
        target = _abs(path)
    except (OSError, TypeError, ValueError):
        return False
    parent = target.rsplit(b'/', 1)[0]
    while parent:
        try:
            kind = _host.stat(parent)[0]
        except OSError:
            parent = parent.rsplit(b'/', 1)[0]
            continue
        return kind != 1
    return False


def _named2(fn):
    # Two-path calls name both paths, as CPython's "src -> dst" errors do.
    def call(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except OSError as error:
            src = args[0] if args else kwargs.get('src')
            dst = args[1] if len(args) > 1 else kwargs.get('dst')
            if error.errno is None or src is None or dst is None:
                raise
            raise type(error)(error.errno, error.strerror, fspath(src), None, fspath(dst)) from None
    return _rename_function(call, fn)


class stat_result(tuple):
    n_sequence_fields = 10
    n_fields = 10
    n_unnamed_fields = 0

    def __new__(cls, fields, mtime_ns=0):
        self = tuple.__new__(cls, fields)
        self._ns = mtime_ns if len(fields) == 7 else fields[8] * 1000000000
        return self

    st_mode = property(lambda self: self[0])
    st_ino = property(lambda self: self[1])
    st_dev = property(lambda self: self[2])
    st_nlink = property(lambda self: self[3])
    st_uid = property(lambda self: self[4])
    st_gid = property(lambda self: self[5])
    st_size = property(lambda self: self[6])
    st_atime = property(lambda self: self._ns / 1e9)
    st_mtime = property(lambda self: self._ns / 1e9)
    st_ctime = property(lambda self: self._ns / 1e9)
    st_atime_ns = property(lambda self: self._ns)
    st_mtime_ns = property(lambda self: self._ns)
    st_ctime_ns = property(lambda self: self._ns)
    st_blksize = property(lambda self: 4096)
    st_blocks = property(lambda self: (self[6] + 511) // 512)
    st_rdev = property(lambda self: 0)

    def __repr__(self):
        return 'os.stat_result(st_mode=%d, st_ino=%d, st_dev=%d, st_nlink=%d, st_uid=%d, st_gid=%d, st_size=%d, st_atime=%d, st_mtime=%d, st_ctime=%d)' % tuple(self)


def _stat_result(mode, path, size, mtime_ns, follow=True):
    seconds = mtime_ns // 1000000000
    ino, nlink = (hash(path) & 0x7FFFFFFFFFFFFFFF) or 1, 1
    if mode & 0o170000 == 0o100000 and isinstance(path, bytes):
        # Hard-linked names share one inode and count each other.
        found = _host.links(path, follow)
        if found is not None:
            ino, nlink = (hash((b'link', found[0])) & 0x7FFFFFFFFFFFFFFF) or 1, found[1]
    self = stat_result((mode, ino, 1, nlink, _uid, _uid, size, seconds, seconds, seconds))
    self._ns = mtime_ns
    return self


class terminal_size(tuple):
    columns = property(lambda self: self[0])
    lines = property(lambda self: self[1])


class uname_result(tuple):
    sysname = property(lambda self: self[0])
    nodename = property(lambda self: self[1])
    release = property(lambda self: self[2])
    version = property(lambda self: self[3])
    machine = property(lambda self: self[4])


_cwd = _host.cwd()
_uid = 1000
_umask = 0o022


def fspath(path):
    if isinstance(path, (str, bytes)):
        return path
    method = getattr(type(path), '__fspath__', None)
    if method is None:
        raise TypeError('expected str, bytes or os.PathLike object, not ' + type(path).__name__)
    path = method(path)
    if isinstance(path, (str, bytes)):
        return path
    raise TypeError('expected __fspath__() to return str or bytes, not ' + type(path).__name__)


def _abs(path):
    path = fspath(path)
    if isinstance(path, str):
        path = path.encode('utf-8', 'surrogateescape')
    if not path:
        raise _fail(_errno.ENOENT, '')
    if path[0] != 47:
        path = _cwd + b'/' + path
    parts = []
    for part in path.split(b'/'):
        if part == b'..':
            if parts:
                parts.pop()
        elif part and part != b'.':
            parts.append(part)
    return b'/' + b'/'.join(parts)


def _mode(kind, mode):
    return (0o100000 if kind == 0 else 0o040000 if kind == 1 else 0o120000) | mode


_BUILTINS = frozenset(
    b'. : alias bg break builtin cd command continue declare dirs disown enable eval '
    b'exec exit export fc fg getopts hash history jobs let local logout popd pushd '
    b'read readonly return set shift shopt source suspend trap type typeset ulimit '
    b'umask unalias unset wait'.split()
)
_programs = {}


def _program(path):
    # The workspace has no /usr/bin: a program the shell runs stands at each
    # directory of PATH, as an executable file, for code that looks for it
    # there (shutil.which, os.access) before running it.
    head, _, name = path.rpartition(b'/')
    if not name or name in _BUILTINS or not _host.spawns():
        return False
    search = environ.get(b'PATH', b'')
    if (head or b'/') not in [dir.rstrip(b'/') or b'/' for dir in search.split(b':') if dir]:
        return False
    found = _programs.get(name)
    if found is None:
        try:
            status, _, _ = _host.spawn([b'command', b'-v', name], [b'PATH', search], _cwd, b'', False)
        except OSError:
            status = 1
        found = _programs[name] = status == 0
    return found


def _stat(path):
    try:
        return _host.stat(path)
    except FileNotFoundError:
        if _program(path):
            return (0, 0, 0o755, 0)
        raise


@_named
def stat(path, *, dir_fd=None, follow_symlinks=True):
    if isinstance(path, int):
        return fstat(path)
    if not follow_symlinks:
        return lstat(path)
    path = _abs(path)
    kind, size, mode, mtime_ns = _stat(path)
    return _stat_result(_mode(kind, mode), path, size, mtime_ns)


@_named
def lstat(path, *, dir_fd=None):
    if isinstance(path, int):
        return fstat(path)
    path = _abs(path)
    try:
        kind, size, mode, mtime_ns = _host.lstat(path)
    except FileNotFoundError:
        return stat(path)
    return _stat_result(_mode(kind, mode), path, size, mtime_ns, False)


def access(path, mode, *, dir_fd=None, effective_ids=False, follow_symlinks=True):
    try:
        _stat(_abs(path))
    except OSError:
        return False
    return True


@_named
def listdir(path='.'):
    names = _host.list(_abs(path))
    if isinstance(fspath(path), bytes):
        return names
    return [name.decode('utf-8', 'surrogateescape') for name in names]


class DirEntry:
    __slots__ = ('name', 'path', '_abs', '_stat')

    def __init__(self, name, path, abs):
        self.name = name
        self.path = path
        self._abs = abs
        self._stat = None

    def stat(self, *, follow_symlinks=True):
        if not follow_symlinks:
            return lstat(self._abs)
        if self._stat is None:
            kind, size, mode, mtime_ns = _host.stat(self._abs)
            self._stat = _stat_result(_mode(kind, mode), self._abs, size, mtime_ns)
        return self._stat

    def _is(self, kind, follow_symlinks):
        try:
            return self.stat(follow_symlinks=follow_symlinks).st_mode & 0o170000 == kind
        except OSError:
            return False

    def is_dir(self, *, follow_symlinks=True):
        return self._is(0o040000, follow_symlinks)

    def is_file(self, *, follow_symlinks=True):
        return self._is(0o100000, follow_symlinks)

    def is_symlink(self):
        try:
            return self.stat(follow_symlinks=False).st_mode & 0o170000 == 0o120000
        except OSError:
            return False

    def is_junction(self):
        return False

    def inode(self):
        return self.stat().st_ino

    def __fspath__(self):
        return self.path

    def __repr__(self):
        return '<DirEntry %r>' % (self.name,)


class _Scan:
    def __init__(self, entries):
        self._entries = iter(entries)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._entries)

    def close(self):
        self._entries = iter(())

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


@_named
def scandir(path='.'):
    given = fspath(path)
    base = _abs(given)
    names = _host.list(base)
    if isinstance(given, bytes):
        prefix = given if given.endswith(b'/') else given + b'/'
        return _Scan([DirEntry(name, prefix + name, base + b'/' + name) for name in names])
    prefix = given if given.endswith('/') else given + '/'
    return _Scan([DirEntry(name.decode('utf-8', 'surrogateescape'), prefix + name.decode('utf-8', 'surrogateescape'), base + b'/' + name) for name in names])


@_named
def mkdir(path, mode=0o777, *, dir_fd=None):
    _host.mkdir(_abs(path), mode & ~_umask & 0o777)


@_named
def rmdir(path, *, dir_fd=None):
    name = fspath(path)
    tail = name.rstrip(b'/' if isinstance(name, bytes) else '/').rsplit(b'/' if isinstance(name, bytes) else '/', 1)[-1]
    if tail in ('.', b'.'):
        raise _fail(_errno.EINVAL, path)
    _host.remove(_abs(path), True)


@_named
def unlink(path, *, dir_fd=None):
    target = _abs(path)
    _host.remove(target, False)
    for file in _files.values():
        if file.path == target and not file.gone:
            _forget(file)


def _forget(file):
    # An open file outlives its name, as on POSIX: it keeps its bytes in
    # memory from here on and never writes them back.
    if file.data is None:
        file.data = bytearray()
    elif not isinstance(file.data, bytearray):
        file.data = bytearray(file.data)
    if file.base >= 0:
        file.base = -1
        file.pos = len(file.data)
    file.gone = True


@_named
def remove(path, *, dir_fd=None):
    return unlink(path, dir_fd=dir_fd)


@_named2
def rename(src, dst, *, src_dir_fd=None, dst_dir_fd=None):
    source, target = _abs(src), _abs(dst)
    if target.startswith(source + b'/'):
        # A directory cannot move into itself.
        raise _fail(_errno.EINVAL, src)
    _host.rename(source, target)


@_named2
def replace(src, dst, *, src_dir_fd=None, dst_dir_fd=None):
    return rename(src, dst)


@_named
def chmod(path, mode, *, dir_fd=None, follow_symlinks=True):
    _host.chmod(_abs(path), mode)


def _utime_ns(value):
    if isinstance(value, int):
        return value * 1000000000
    if isinstance(value, float):
        whole = value // 1
        return int(whole) * 1000000000 + int((value - whole) * 1e9)
    raise TypeError("utime: 'times' must be either a tuple of two ints or None")


@_named
def utime(path, times=None, *, ns=None, dir_fd=None, follow_symlinks=True):
    if times is not None and ns is not None:
        raise ValueError("utime: you may specify either 'times' or 'ns' but not both")
    if times is not None:
        if not isinstance(times, tuple) or len(times) != 2:
            raise TypeError("utime: 'times' must be either a tuple of two ints or None")
        mtime_ns = _utime_ns(times[1])
        _utime_ns(times[0])
    elif ns is not None:
        if not isinstance(ns, tuple) or len(ns) != 2 or not all(isinstance(part, int) for part in ns):
            raise TypeError("utime: 'ns' must be a tuple of two ints")
        mtime_ns = ns[1]
    else:
        import time
        mtime_ns = time.time_ns()
    _host.utime(_abs(path), mtime_ns)


@_named
def chown(path, uid, gid, *, dir_fd=None, follow_symlinks=True):
    _host.stat(_abs(path))


@_named
def readlink(path, *, dir_fd=None):
    given = fspath(path)
    target = _abs(path)
    if _host.lstat(target)[0] != 2:
        raise _fail(_errno.EINVAL, path)
    found = _host.readlink(target)
    if isinstance(given, bytes):
        return found
    return found.decode('utf-8', 'surrogateescape')


@_named2
def symlink(src, dst, target_is_directory=False, *, dir_fd=None):
    src = fspath(src)
    if isinstance(src, str):
        src = src.encode('utf-8', 'surrogateescape')
    try:
        _host.symlink(src, _abs(dst))
    except OSError as error:
        if error.errno == _errno.ENOTSUP:
            raise _fail(_errno.EPERM, dst) from None
        raise


@_named2
def link(src, dst, *, src_dir_fd=None, dst_dir_fd=None, follow_symlinks=True):
    # Like CPython on Linux, which calls link(2): a symlink source is linked itself.
    source = _abs(src)
    kind = _host.lstat(source)[0]
    if kind == 1:
        raise _fail(_errno.EPERM, dst)
    try:
        if kind == 2:
            _host.symlink(_host.readlink(source), _abs(dst))
        else:
            _host.link(source, _abs(dst))
    except OSError as error:
        if error.errno == _errno.ENOTSUP:
            raise _fail(_errno.EPERM, dst) from None
        raise


def getcwd():
    return _cwd.decode('utf-8', 'surrogateescape')


def getcwdb():
    return _cwd


@_named
def chdir(path):
    global _cwd
    target = _abs(path)
    if _host.stat(target)[0] != 1:
        raise _fail(_errno.ENOTDIR, path)
    _cwd = target


class _File:
    __slots__ = ('path', 'flags', 'data', 'pos', 'base', 'dir', 'size', 'gone')


_RANGED = 1 << 20


_files = {}
_next = 3
_run = 0


def _file(fd):
    try:
        return _files[fd]
    except KeyError:
        raise _fail(_errno.EBADF) from None


@_named
def open(path, flags, mode=0o777, *, dir_fd=None):
    global _next
    target = _abs(path)
    access = flags & O_ACCMODE
    file = _File()
    file.path = target
    file.flags = flags
    file.pos = 0
    file.base = -1
    file.dir = False
    file.gone = False
    if target == b'/dev/null':
        file.path = None
        file.data = bytearray()
    else:
        try:
            kind, size, _, _ = _host.stat(target)
        except FileNotFoundError:
            if not flags & O_CREAT:
                raise _fail(_errno.ENOTDIR if _under_file(path) else _errno.ENOENT, path) from None
            _host.write(target, b'', 2)
            file.data = bytearray()
        else:
            if flags & O_CREAT and flags & O_EXCL:
                raise _fail(_errno.EEXIST, path)
            if kind == 1:
                if access != O_RDONLY:
                    raise _fail(_errno.EISDIR, path)
                file.dir = True
                file.data = bytearray()
            elif flags & O_DIRECTORY:
                raise _fail(_errno.ENOTDIR, path)
            elif flags & O_TRUNC and access != O_RDONLY:
                _host.write(target, b'', 0)
                file.data = bytearray()
            elif flags & O_APPEND and access == O_WRONLY:
                file.base = size
                file.data = bytearray()
            else:
                if access == O_RDONLY and size >= _RANGED and _host.ranged():
                    file.data = None
                    file.size = size
                else:
                    data = _host.read(target)
                    file.data = data if access == O_RDONLY else bytearray(data)
    fd = _next
    _next += 1
    _files[fd] = file
    return fd


def _store(file):
    if file.gone:
        return
    if len(file.data) > _host.file_limit():
        raise _fail(_errno.EFBIG)
    _host.write(file.path, file.data, 0)


def _reset():
    global _cwd, _umask, _run
    _files.clear()
    _programs.clear()
    _run += 1
    _cwd = _host.cwd()
    _umask = 0o022
    environ.clear()
    environ.update(_host.env())


def close(fd):
    _file(fd)
    del _files[fd]


def dup(fd):
    global _next
    if fd < 3:
        return fd
    file = _file(fd)
    copy = _next
    _next += 1
    _files[copy] = file
    return copy


def fsync(fd):
    if fd > 2:
        _file(fd)


fdatasync = fsync


def read(fd, size):
    if fd == 0:
        return _host.input(size)
    file = _file(fd)
    if file.flags & O_ACCMODE == O_WRONLY:
        raise _fail(_errno.EBADF)
    if file.data is None:
        data = _host.read_at(file.path, file.pos, size)
    else:
        data = bytes(file.data[file.pos:file.pos + size])
    file.pos += len(data)
    return data


def _readall(fd):
    if fd == 0:
        return _host.input(-1)
    file = _file(fd)
    if file.data is None:
        data = _host.read_at(file.path, file.pos, max(file.size - file.pos, 0))
    else:
        data = bytes(file.data[file.pos:])
    file.pos += len(data)
    return data


def write(fd, data):
    if fd == 1 or fd == 2:
        return _host.output(fd, data)
    file = _file(fd)
    if file.flags & O_ACCMODE == O_RDONLY:
        raise _fail(_errno.EBADF)
    size = len(data)
    if file.path is None:
        return size
    if file.base >= 0:
        _host.write(file.path, data, 1)
        file.base += size
        return size
    end = len(file.data)
    if file.flags & O_APPEND:
        file.pos = end
    if file.pos == end and not file.gone:
        if end + size > _host.file_limit():
            raise _fail(_errno.EFBIG)
        _host.write(file.path, data, 1)
        file.data += data
    else:
        gap = file.pos - end
        if gap > 0:
            file.data += bytes(gap)
        file.data[file.pos:file.pos + size] = data
        _store(file)
    file.pos += size
    return size


def lseek(fd, pos, how):
    file = _file(fd)
    if file.base >= 0:
        return file.base
    if how == 1:
        pos += file.pos
    elif how == 2:
        pos += file.size if file.data is None else len(file.data)
    elif how != 0:
        raise _fail(_errno.EINVAL)
    if pos < 0:
        raise _fail(_errno.EINVAL)
    file.pos = pos
    return pos


def ftruncate(fd, length):
    file = _file(fd)
    if file.base >= 0 or file.flags & O_ACCMODE == O_RDONLY:
        raise _fail(_errno.EINVAL)
    if length < len(file.data):
        del file.data[length:]
    else:
        file.data += bytes(length - len(file.data))
    _store(file)


def truncate(path, length):
    if isinstance(path, int):
        return ftruncate(path, length)
    fd = open(path, O_RDWR)
    try:
        ftruncate(fd, length)
    finally:
        close(fd)


def fstat(fd):
    if fd < 3:
        return _stat_result(0o020620, b'', 0, 0)
    file = _file(fd)
    if file.path is None:
        return _stat_result(0o020666, b'', 0, 0)
    if file.gone:
        return _stat_result(0o100600, file.path, len(file.data), 0)
    kind, size, mode, mtime_ns = _host.stat(file.path)
    return _stat_result(_mode(kind, mode), file.path, size, mtime_ns)


def isatty(fd):
    return False


def device_encoding(fd):
    return None


def get_terminal_size(fd=1):
    raise _fail(_errno.ENOTTY)


def get_inheritable(fd):
    return False


def set_inheritable(fd, inheritable):
    pass


def get_blocking(fd):
    return True


def urandom(size):
    return _host.random(size)


def getpid():
    return 1


def getppid():
    return 0


def getuid():
    return _uid


geteuid = getgid = getegid = getuid


def getlogin():
    return environ.get(b'USER', b'user').decode()


def umask(mask):
    global _umask
    mask, _umask = _umask, mask & 0o777
    return mask


def cpu_count():
    return 1


def uname():
    return uname_result(('Linux', 'localhost', '', '', 'wasm32'))


def putenv(name, value):
    pass


def unsetenv(name):
    pass


def times():
    return (0.0, 0.0, 0.0, 0.0, 0.0)


def waitstatus_to_exitcode(status):
    status = status.__index__()
    if status & 0x7f == 0:
        return (status >> 8) & 0xff
    if 0 < status & 0x7f < 0x7f:
        return -(status & 0x7f)
    raise ValueError('invalid wait status: %d' % status)


def _exit(status):
    # Ends without flushing stdio or running atexit, as CPython does: what sys.stdout
    # still holds is dropped.
    import sys
    for stream in (sys.stdout, sys.stderr, sys.__stdout__, sys.__stderr__):
        raw = getattr(getattr(stream, 'buffer', None), 'raw', None)
        if raw is not None and hasattr(raw, '_run'):
            raw._run = None
    atexit = sys.modules.get('atexit')
    if atexit is not None:
        atexit._clear()
    # Nothing catches it: no except, finally or __exit__ runs after it, as in CPython.
    _host.exit(status)


def system(command):
    if not _host.spawns():
        raise OSError(_errno.ENOSYS, 'this Python has no processes to run a command in')
    import subprocess
    return subprocess.call(command, shell=True) << 8


def popen(command, mode='r', buffering=-1):
    raise OSError(_errno.ENOSYS, 'this Python has no processes to run a command in')
