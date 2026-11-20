/* The few CPython-internal helpers the _sqlite3 module uses, written against the public API
 * the bridge provides. CPython builds this module with Py_BUILD_CORE_MODULE and its internal
 * headers; here every pycore_*.h header includes this one instead. */
#ifndef NANOCODE_PYCORE_H
#define NANOCODE_PYCORE_H

#include <Python.h>
#include <string.h>

#define _Py_ID(NAME) \
    (__extension__({ \
        static PyObject *nanocode_id; \
        if (nanocode_id == NULL) { \
            nanocode_id = PyUnicode_InternFromString(#NAME); \
        } \
        nanocode_id; \
    }))

static inline PyObject *const *
nanocode_unpack_keywords(PyObject *const *args, Py_ssize_t nargs, PyObject *kwargs,
                         PyObject *kwnames, struct _PyArg_Parser *parser, int minpos,
                         int maxpos, int minkw, int varpos, PyObject **buf)
{
    const char *name = parser->fname != NULL ? parser->fname : "function";
    Py_ssize_t total = 0;
    Py_ssize_t posonly = 0;
    while (parser->keywords[total] != NULL) {
        if (parser->keywords[total][0] == '\0') {
            posonly = total + 1;
        }
        total++;
    }
    if (varpos) {
        PyErr_Format(PyExc_SystemError, "%.200s() takes *args, which this build cannot parse",
                     name);
        return NULL;
    }
    if (nargs > maxpos) {
        if (maxpos == 0) {
            PyErr_Format(PyExc_TypeError, "%.200s() takes no positional arguments", name);
        }
        else {
            PyErr_Format(PyExc_TypeError,
                         "%.200s() takes at most %d positional argument%s (%zd given)", name,
                         maxpos, maxpos == 1 ? "" : "s", nargs);
        }
        return NULL;
    }
    for (Py_ssize_t i = 0; i < total; i++) {
        buf[i] = i < nargs ? args[i] : NULL;
    }
    Py_ssize_t given = 0;
    if (kwnames != NULL) {
        given = PyTuple_Size(kwnames);
    }
    else if (kwargs != NULL) {
        given = PyDict_Size(kwargs);
    }
    Py_ssize_t position = 0;
    for (Py_ssize_t k = 0; k < given; k++) {
        PyObject *key;
        PyObject *value;
        if (kwnames != NULL) {
            key = PyTuple_GetItem(kwnames, k);
            value = args[nargs + k];
        }
        else if (!PyDict_Next(kwargs, &position, &key, &value)) {
            break;
        }
        if (!PyUnicode_Check(key)) {
            PyErr_SetString(PyExc_TypeError, "keywords must be strings");
            return NULL;
        }
        const char *word = PyUnicode_AsUTF8(key);
        if (word == NULL) {
            return NULL;
        }
        Py_ssize_t at = posonly;
        while (at < total && strcmp(word, parser->keywords[at]) != 0) {
            at++;
        }
        if (at == total) {
            PyErr_Format(PyExc_TypeError, "%.200s() got an unexpected keyword argument '%s'",
                         name, word);
            return NULL;
        }
        if (buf[at] != NULL) {
            PyErr_Format(PyExc_TypeError,
                         "argument for %.200s() given by name ('%s') and position (%zd)", name,
                         word, at + 1);
            return NULL;
        }
        buf[at] = value;
    }
    for (Py_ssize_t i = 0; i < minpos; i++) {
        if (buf[i] == NULL) {
            if (i < posonly) {
                PyErr_Format(PyExc_TypeError,
                             "%.200s() takes at least %d positional argument%s (%zd given)",
                             name, minpos, minpos == 1 ? "" : "s", nargs);
            }
            else {
                PyErr_Format(PyExc_TypeError, "%.200s() missing required argument '%s' (pos %zd)",
                             name, parser->keywords[i], i + 1);
            }
            return NULL;
        }
    }
    for (Py_ssize_t i = maxpos; i < maxpos + minkw && i < total; i++) {
        if (buf[i] == NULL) {
            PyErr_Format(PyExc_TypeError,
                         "%.200s() missing required keyword-only argument '%s'", name,
                         parser->keywords[i]);
            return NULL;
        }
    }
    return buf;
}

#define _PyArg_UnpackKeywords(args, nargs, kwargs, kwnames, parser, minpos, maxpos, minkw, \
                              varpos, buf) \
    (((minkw) == 0 && (kwargs) == NULL && (kwnames) == NULL && (minpos) <= (nargs) && \
      (nargs) <= (maxpos) && (args) != NULL) \
         ? (args) \
         : nanocode_unpack_keywords((args), (nargs), (kwargs), (kwnames), (parser), (minpos), \
                                    (maxpos), (minkw), (varpos), (buf)))

static inline int
nanocode_check_positional(const char *name, Py_ssize_t nargs, Py_ssize_t min, Py_ssize_t max)
{
    if (nargs < min) {
        PyErr_Format(PyExc_TypeError, "%.200s expected %s%zd argument%s, got %zd",
                     name != NULL ? name : "function", min == max ? "" : "at least ", min,
                     min == 1 ? "" : "s", nargs);
        return 0;
    }
    if (nargs > max) {
        PyErr_Format(PyExc_TypeError, "%.200s expected %s%zd argument%s, got %zd",
                     name != NULL ? name : "function", min == max ? "" : "at most ", max,
                     max == 1 ? "" : "s", nargs);
        return 0;
    }
    return 1;
}

#define _PyArg_CheckPositional(funcname, nargs, min, max) \
    (((min) <= (nargs) && (nargs) <= (max)) || \
     nanocode_check_positional((funcname), (nargs), (min), (max)))

static inline void
_PyArg_BadArgument(const char *fname, const char *displayname, const char *expected,
                   PyObject *arg)
{
    PyErr_Format(PyExc_TypeError, "%.200s() %.200s must be %.50s, not %.50s", fname,
                 displayname, expected, arg == Py_None ? "None" : Py_TYPE(arg)->tp_name);
}

static inline int
nanocode_no_keywords(const char *funcname, PyObject *kwargs)
{
    if (PyDict_Size(kwargs) == 0) {
        return 1;
    }
    PyErr_Format(PyExc_TypeError, "%.200s() takes no keyword arguments", funcname);
    return 0;
}

#define _PyArg_NoKeywords(funcname, kwargs) \
    ((kwargs) == NULL || nanocode_no_keywords((funcname), (kwargs)))

#define _Py_IsInterpreterFinalizing(interp) 0

static inline int
_PyWeakref_IsDead(PyObject *ref)
{
    PyObject *target = NULL;
    int alive = PyWeakref_GetRef(ref, &target);
    if (alive < 0) {
        PyErr_Clear();
        return 1;
    }
    Py_XDECREF(target);
    return alive == 0;
}

#define FT_CLEAR_WEAKREFS(obj, weakref_list) \
    do { \
        if ((weakref_list) != NULL) { \
            PyObject_ClearWeakRefs(obj); \
        } \
    } while (0)

#define _PyErr_ChainExceptions1 nanocode_chain_exceptions

static inline void
nanocode_chain_exceptions(PyObject *exception)
{
    if (exception == NULL) {
        return;
    }
    if (PyErr_Occurred()) {
        PyObject *raised = PyErr_GetRaisedException();
        PyException_SetContext(raised, exception);
        PyErr_SetRaisedException(raised);
    }
    else {
        PyErr_SetRaisedException(exception);
    }
}

static inline PyObject *
_PyErr_FormatFromCause(PyObject *exception, const char *format, ...)
{
    PyObject *cause = PyErr_GetRaisedException();
    va_list vargs;
    va_start(vargs, format);
    PyErr_FormatV(exception, format, vargs);
    va_end(vargs);
    if (cause != NULL) {
        PyObject *raised = PyErr_GetRaisedException();
        PyException_SetCause(raised, Py_NewRef(cause));
        PyException_SetContext(raised, cause);
        PyErr_SetRaisedException(raised);
    }
    return NULL;
}

static inline const char *
_PyUnicode_AsUTF8NoNUL(PyObject *unicode)
{
    Py_ssize_t size;
    const char *text = PyUnicode_AsUTF8AndSize(unicode, &size);
    if (text != NULL && strlen(text) != (size_t)size) {
        PyErr_SetString(PyExc_ValueError, "embedded null character");
        return NULL;
    }
    return text;
}

static inline PyObject *
nanocode_import_attribute(const char *module, const char *attribute)
{
    PyObject *loaded = PyImport_ImportModule(module);
    if (loaded == NULL) {
        return NULL;
    }
    PyObject *value = PyObject_GetAttrString(loaded, attribute);
    Py_DECREF(loaded);
    return value;
}

#define PyImport_ImportModuleAttrString nanocode_import_attribute

/* Public functions the bridge declares but does not provide, built from ones it does. */

static inline PyObject *
nanocode_bytearray(const char *bytes, Py_ssize_t size)
{
    if (bytes == NULL) {
        return PyObject_CallFunction((PyObject *)&PyByteArray_Type, "n", size);
    }
    return PyObject_CallFunction((PyObject *)&PyByteArray_Type, "y#", bytes, size);
}

#define PyByteArray_FromStringAndSize nanocode_bytearray

static inline void
nanocode_format_unraisable(const char *format, ...)
{
    (void)format;
    PyErr_WriteUnraisable(NULL);
}

#define PyErr_FormatUnraisable nanocode_format_unraisable

static inline int
nanocode_resource_warning(PyObject *source, Py_ssize_t stack_level, const char *format, ...)
{
    (void)source;
    va_list arguments;
    va_start(arguments, format);
    PyObject *message = PyUnicode_FromFormatV(format, arguments);
    va_end(arguments);
    if (message == NULL) {
        return -1;
    }
    const char *text = PyUnicode_AsUTF8(message);
    int warned = text == NULL ? -1 : PyErr_WarnEx(PyExc_ResourceWarning, text, stack_level);
    Py_DECREF(message);
    return warned;
}

#define PyErr_ResourceWarning nanocode_resource_warning

static inline int
nanocode_mapping_optional_item(PyObject *mapping, const char *key, PyObject **result)
{
    *result = PyMapping_GetItemString(mapping, key);
    if (*result != NULL) {
        return 1;
    }
    if (PyErr_ExceptionMatches(PyExc_KeyError)) {
        PyErr_Clear();
        return 0;
    }
    return -1;
}

#define PyMapping_GetOptionalItemString nanocode_mapping_optional_item

static inline int
nanocode_strnicmp(const char *left, const char *right, Py_ssize_t size)
{
    for (Py_ssize_t i = 0; i < size; i++) {
        int a = (unsigned char)left[i];
        int b = (unsigned char)right[i];
        a = a >= 'A' && a <= 'Z' ? a + ('a' - 'A') : a;
        b = b >= 'A' && b <= 'Z' ? b + ('a' - 'A') : b;
        if (a != b || a == 0) {
            return a - b;
        }
    }
    return 0;
}

#define PyOS_mystrnicmp nanocode_strnicmp

/* Runs tp_finalize from tp_dealloc as CPython does: the object is resurrected while it runs,
 * so references it takes and drops meanwhile cannot free it a second time. Returns -1 when the
 * finalizer kept it alive, and the dealloc must stop. */
static inline int
nanocode_finalize_from_dealloc(PyObject *self)
{
    destructor finalize = Py_TYPE(self)->tp_finalize;
    if (finalize == NULL) {
        return 0;
    }
    Py_SET_REFCNT(self, 1);
    PyObject *raised = PyErr_GetRaisedException();
    finalize(self);
    PyErr_SetRaisedException(raised);
    Py_ssize_t left = Py_REFCNT(self) - 1;
    Py_SET_REFCNT(self, left);
    return left > 0 ? -1 : 0;
}

#define PyObject_CallFinalizerFromDealloc nanocode_finalize_from_dealloc

#define PySys_Audit(...) 0

static inline PyObject *
nanocode_decode(const char *bytes, Py_ssize_t size, const char *encoding, const char *errors)
{
    PyObject *data = PyBytes_FromStringAndSize(bytes, size);
    if (data == NULL) {
        return NULL;
    }
    PyObject *text = PyObject_CallMethod(data, "decode", "ss", encoding != NULL ? encoding : "utf-8",
                                         errors != NULL ? errors : "strict");
    Py_DECREF(data);
    return text;
}

#define PyUnicode_Decode nanocode_decode

static inline int
nanocode_fs_converter(PyObject *path, void *result)
{
    PyObject **out = (PyObject **)result;
    if (path == NULL) {
        Py_CLEAR(*out);
        return 1;
    }
    PyObject *name = nanocode_import_attribute("os", "fsencode");
    if (name == NULL) {
        return 0;
    }
    PyObject *encoded = PyObject_CallOneArg(name, path);
    Py_DECREF(name);
    if (encoded == NULL) {
        return 0;
    }
    *out = encoded;
    return Py_CLEANUP_SUPPORTED;
}

#define PyUnicode_FSConverter nanocode_fs_converter

#undef Py_TOLOWER
#define Py_TOLOWER(c) \
    ((unsigned char)(c) >= 'A' && (unsigned char)(c) <= 'Z' ? (unsigned char)(c) + ('a' - 'A') \
                                                            : (unsigned char)(c))

#endif
