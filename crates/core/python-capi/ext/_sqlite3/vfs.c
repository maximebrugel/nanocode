/* SQLite's operating-system layer for nanocode: database files are the Python program's own
 * files, opened through io.open and os, so they live where its other files do (the workspace),
 * never in the host's file system. SQLite is built with SQLITE_OS_OTHER, so this file supplies
 * sqlite3_os_init. One thread runs every program that uses it, so locking is a no-op. */
#include <Python.h>
#include <string.h>

#include "sqlite3.h"

typedef struct {
    sqlite3_file base;
    PyObject *file;
} nanocode_file;

static PyObject *
nanocode_module(const char *name)
{
    return PyImport_ImportModule(name);
}

/* Calls module.function(*arguments) given as a format; returns a new reference or NULL. */
static PyObject *
nanocode_call(const char *module, const char *function, const char *format, ...)
{
    PyObject *loaded = nanocode_module(module);
    if (loaded == NULL) {
        return NULL;
    }
    PyObject *callable = PyObject_GetAttrString(loaded, function);
    Py_DECREF(loaded);
    if (callable == NULL) {
        return NULL;
    }
    va_list arguments;
    va_start(arguments, format);
    PyObject *tuple = Py_VaBuildValue(format, arguments);
    va_end(arguments);
    if (tuple == NULL) {
        Py_DECREF(callable);
        return NULL;
    }
    if (!PyTuple_Check(tuple)) {
        PyObject *single = PyTuple_Pack(1, tuple);
        Py_DECREF(tuple);
        tuple = single;
        if (tuple == NULL) {
            Py_DECREF(callable);
            return NULL;
        }
    }
    PyObject *result = PyObject_CallObject(callable, tuple);
    Py_DECREF(tuple);
    Py_DECREF(callable);
    return result;
}

static int
nanocode_failed(int code)
{
    PyErr_Clear();
    return code;
}

static int
nanocode_seek(nanocode_file *file, sqlite3_int64 offset)
{
    PyObject *done = PyObject_CallMethod(file->file, "seek", "L", (long long)offset);
    if (done == NULL) {
        return 0;
    }
    Py_DECREF(done);
    return 1;
}

static int
nanocode_close(sqlite3_file *base)
{
    nanocode_file *file = (nanocode_file *)base;
    if (file->file == NULL) {
        return SQLITE_OK;
    }
    PyObject *done = PyObject_CallMethod(file->file, "close", NULL);
    Py_CLEAR(file->file);
    if (done == NULL) {
        return nanocode_failed(SQLITE_IOERR_CLOSE);
    }
    Py_DECREF(done);
    return SQLITE_OK;
}

static int
nanocode_read(sqlite3_file *base, void *buffer, int amount, sqlite3_int64 offset)
{
    nanocode_file *file = (nanocode_file *)base;
    if (!nanocode_seek(file, offset)) {
        return nanocode_failed(SQLITE_IOERR_READ);
    }
    PyObject *data = PyObject_CallMethod(file->file, "read", "i", amount);
    if (data == NULL) {
        return nanocode_failed(SQLITE_IOERR_READ);
    }
    char *bytes;
    Py_ssize_t length;
    if (PyBytes_AsStringAndSize(data, &bytes, &length) < 0) {
        Py_DECREF(data);
        return nanocode_failed(SQLITE_IOERR_READ);
    }
    if (length > amount) {
        length = amount;
    }
    memcpy(buffer, bytes, (size_t)length);
    Py_DECREF(data);
    if (length < amount) {
        memset((char *)buffer + length, 0, (size_t)(amount - length));
        return SQLITE_IOERR_SHORT_READ;
    }
    return SQLITE_OK;
}

static int
nanocode_write(sqlite3_file *base, const void *buffer, int amount, sqlite3_int64 offset)
{
    nanocode_file *file = (nanocode_file *)base;
    if (!nanocode_seek(file, offset)) {
        return nanocode_failed(SQLITE_IOERR_WRITE);
    }
    PyObject *done = PyObject_CallMethod(file->file, "write", "y#", (const char *)buffer,
                                         (Py_ssize_t)amount);
    if (done == NULL) {
        return nanocode_failed(SQLITE_IOERR_WRITE);
    }
    Py_DECREF(done);
    return SQLITE_OK;
}

static int
nanocode_truncate(sqlite3_file *base, sqlite3_int64 size)
{
    nanocode_file *file = (nanocode_file *)base;
    PyObject *done = PyObject_CallMethod(file->file, "truncate", "L", (long long)size);
    if (done == NULL) {
        return nanocode_failed(SQLITE_IOERR_TRUNCATE);
    }
    Py_DECREF(done);
    return SQLITE_OK;
}

static int
nanocode_sync(sqlite3_file *base, int flags)
{
    (void)flags;
    nanocode_file *file = (nanocode_file *)base;
    PyObject *done = PyObject_CallMethod(file->file, "flush", NULL);
    if (done == NULL) {
        return nanocode_failed(SQLITE_IOERR_FSYNC);
    }
    Py_DECREF(done);
    return SQLITE_OK;
}

static int
nanocode_file_size(sqlite3_file *base, sqlite3_int64 *size)
{
    nanocode_file *file = (nanocode_file *)base;
    PyObject *end = PyObject_CallMethod(file->file, "seek", "ii", 0, 2);
    if (end == NULL) {
        return nanocode_failed(SQLITE_IOERR_FSTAT);
    }
    *size = PyLong_AsLongLong(end);
    Py_DECREF(end);
    if (*size < 0 && PyErr_Occurred()) {
        return nanocode_failed(SQLITE_IOERR_FSTAT);
    }
    return SQLITE_OK;
}

static int
nanocode_lock(sqlite3_file *base, int level)
{
    (void)base;
    (void)level;
    return SQLITE_OK;
}

static int
nanocode_reserved(sqlite3_file *base, int *reserved)
{
    (void)base;
    *reserved = 0;
    return SQLITE_OK;
}

static int
nanocode_control(sqlite3_file *base, int operation, void *argument)
{
    (void)base;
    (void)operation;
    (void)argument;
    return SQLITE_NOTFOUND;
}

static int
nanocode_sector_size(sqlite3_file *base)
{
    (void)base;
    return 4096;
}

static int
nanocode_characteristics(sqlite3_file *base)
{
    (void)base;
    return 0;
}

static const sqlite3_io_methods nanocode_methods = {
    1,
    nanocode_close,
    nanocode_read,
    nanocode_write,
    nanocode_truncate,
    nanocode_sync,
    nanocode_file_size,
    nanocode_lock,
    nanocode_lock,
    nanocode_reserved,
    nanocode_control,
    nanocode_sector_size,
    nanocode_characteristics,
};

static int
nanocode_exists(const char *path)
{
    PyObject *exists = nanocode_call("os.path", "exists", "(s)", path);
    if (exists == NULL) {
        PyErr_Clear();
        return 0;
    }
    int found = PyObject_IsTrue(exists);
    Py_DECREF(exists);
    return found > 0;
}

static int
nanocode_open(sqlite3_vfs *vfs, sqlite3_filename name, sqlite3_file *base, int flags,
              int *out_flags)
{
    (void)vfs;
    nanocode_file *file = (nanocode_file *)base;
    file->base.pMethods = NULL;
    file->file = NULL;
    if (name == NULL) {
        file->file = nanocode_call("io", "BytesIO", "()");
    }
    else {
        int exists = nanocode_exists(name);
        if (exists && (flags & SQLITE_OPEN_EXCLUSIVE)) {
            return SQLITE_CANTOPEN;
        }
        if (!exists && !(flags & SQLITE_OPEN_CREATE)) {
            return SQLITE_CANTOPEN;
        }
        const char *mode = (flags & SQLITE_OPEN_READONLY) ? "rb" : exists ? "r+b" : "w+b";
        file->file = nanocode_call("io", "open", "(ss)", name, mode);
    }
    if (file->file == NULL) {
        return nanocode_failed(SQLITE_CANTOPEN);
    }
    if (out_flags != NULL) {
        *out_flags = flags;
    }
    file->base.pMethods = &nanocode_methods;
    return SQLITE_OK;
}

static int
nanocode_delete(sqlite3_vfs *vfs, const char *name, int sync)
{
    (void)vfs;
    (void)sync;
    PyObject *done = nanocode_call("os", "remove", "(s)", name);
    if (done == NULL) {
        if (PyErr_ExceptionMatches(PyExc_FileNotFoundError)) {
            return nanocode_failed(SQLITE_IOERR_DELETE_NOENT);
        }
        return nanocode_failed(SQLITE_IOERR_DELETE);
    }
    Py_DECREF(done);
    return SQLITE_OK;
}

static int
nanocode_access(sqlite3_vfs *vfs, const char *name, int flags, int *result)
{
    (void)vfs;
    if (flags == SQLITE_ACCESS_EXISTS) {
        *result = nanocode_exists(name);
        return SQLITE_OK;
    }
    PyObject *os = nanocode_module("os");
    if (os == NULL) {
        return nanocode_failed(SQLITE_IOERR_ACCESS);
    }
    PyObject *mode = PyObject_GetAttrString(os, flags == SQLITE_ACCESS_READWRITE ? "W_OK" : "R_OK");
    Py_DECREF(os);
    if (mode == NULL) {
        return nanocode_failed(SQLITE_IOERR_ACCESS);
    }
    PyObject *allowed = nanocode_call("os", "access", "(sO)", name, mode);
    Py_DECREF(mode);
    if (allowed == NULL) {
        return nanocode_failed(SQLITE_IOERR_ACCESS);
    }
    *result = PyObject_IsTrue(allowed) > 0;
    Py_DECREF(allowed);
    return SQLITE_OK;
}

static int
nanocode_full_pathname(sqlite3_vfs *vfs, const char *name, int size, char *out)
{
    (void)vfs;
    PyObject *path = nanocode_call("os.path", "abspath", "(s)", name);
    if (path == NULL) {
        return nanocode_failed(SQLITE_CANTOPEN);
    }
    Py_ssize_t length;
    const char *text = PyUnicode_AsUTF8AndSize(path, &length);
    if (text == NULL || length + 1 > size) {
        Py_DECREF(path);
        return nanocode_failed(SQLITE_CANTOPEN);
    }
    memcpy(out, text, (size_t)length + 1);
    Py_DECREF(path);
    return SQLITE_OK;
}

static int
nanocode_randomness(sqlite3_vfs *vfs, int size, char *out)
{
    (void)vfs;
    memset(out, 0, (size_t)size);
    PyObject *data = nanocode_call("os", "urandom", "(i)", size);
    if (data == NULL) {
        PyErr_Clear();
        return size;
    }
    char *bytes;
    Py_ssize_t length;
    if (PyBytes_AsStringAndSize(data, &bytes, &length) == 0) {
        memcpy(out, bytes, (size_t)(length < size ? length : size));
    }
    else {
        PyErr_Clear();
    }
    Py_DECREF(data);
    return size;
}

static int
nanocode_sleep(sqlite3_vfs *vfs, int microseconds)
{
    (void)vfs;
    return microseconds;
}

static int
nanocode_current_time_int64(sqlite3_vfs *vfs, sqlite3_int64 *now)
{
    (void)vfs;
    PyObject *seconds = nanocode_call("time", "time", "()");
    if (seconds == NULL) {
        return nanocode_failed(SQLITE_ERROR);
    }
    double value = PyFloat_AsDouble(seconds);
    Py_DECREF(seconds);
    /* Julian day number in milliseconds, as SQLite counts it. */
    *now = (sqlite3_int64)(value * 1000.0) + (sqlite3_int64)210866760000000LL;
    return SQLITE_OK;
}

static int
nanocode_current_time(sqlite3_vfs *vfs, double *now)
{
    sqlite3_int64 milliseconds = 0;
    int code = nanocode_current_time_int64(vfs, &milliseconds);
    *now = (double)milliseconds / 86400000.0;
    return code;
}

static int
nanocode_last_error(sqlite3_vfs *vfs, int size, char *out)
{
    (void)vfs;
    (void)size;
    (void)out;
    return 0;
}

static sqlite3_vfs nanocode_vfs = {
    2,
    sizeof(nanocode_file),
    1024,
    NULL,
    "nanocode",
    NULL,
    nanocode_open,
    nanocode_delete,
    nanocode_access,
    nanocode_full_pathname,
    NULL,
    NULL,
    NULL,
    NULL,
    nanocode_randomness,
    nanocode_sleep,
    nanocode_current_time,
    nanocode_last_error,
    nanocode_current_time_int64,
    NULL,
    NULL,
    NULL,
};

int
sqlite3_os_init(void)
{
    return sqlite3_vfs_register(&nanocode_vfs, 1);
}

int
sqlite3_os_end(void)
{
    return SQLITE_OK;
}
