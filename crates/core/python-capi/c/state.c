#include "Python.h"
#include "structmember.h"
#include <stdlib.h>

static PyThreadState thread_state = {.gilstate_counter = 1};

static PyThreadState *current(void) {
    thread_state.interp = PyInterpreterState_Get();
    return &thread_state;
}

PyThreadState *PyThreadState_Get(void) {
    return current();
}

PyThreadState *PyThreadState_GetUnchecked(void) {
    return current();
}

PyThreadState *PyGILState_GetThisThreadState(void) {
    return current();
}

PyThreadState *PyThreadState_New(PyInterpreterState *interpreter) {
    (void)interpreter;
    return current();
}

void PyThreadState_Clear(PyThreadState *state) {
    (void)state;
}

void PyThreadState_DeleteCurrent(void) {
}

void PyEval_AcquireThread(PyThreadState *state) {
    (void)state;
}

void PyEval_ReleaseThread(PyThreadState *state) {
    (void)state;
}

PyFrameObject *PyThreadState_GetFrame(PyThreadState *state) {
    (void)state;
    return NULL;
}

PyFrameObject *PyFrame_GetBack(PyFrameObject *frame) {
    (void)frame;
    return NULL;
}

PyCodeObject *PyFrame_GetCode(PyFrameObject *frame) {
    (void)frame;
    PyErr_SetString(PyExc_SystemError, "frame objects are not exposed to the C API");
    return NULL;
}

int PyFrame_GetLineNumber(PyFrameObject *frame) {
    (void)frame;
    return -1;
}

/* Thread-specific storage: the interpreter runs on one thread, so each key
   is an index into one table of values. The key field is pthread_key_t
   (32 bits) on some hosts, too narrow for a pointer, so it holds the index
   plus one rather than an address. */
static void **tss_values;
static size_t tss_count;

int PyThread_tss_create(Py_tss_t *key) {
    if (key->_is_initialized) {
        return 0;
    }
    void **values = realloc(tss_values, (tss_count + 1) * sizeof(void *));
    if (values == NULL) {
        return -1;
    }
    tss_values = values;
    tss_values[tss_count] = NULL;
    tss_count += 1;
    key->_key = tss_count;
    key->_is_initialized = 1;
    return 0;
}

void PyThread_tss_delete(Py_tss_t *key) {
    if (!key->_is_initialized) {
        return;
    }
    tss_values[key->_key - 1] = NULL;
    key->_key = 0;
    key->_is_initialized = 0;
}

void *PyThread_tss_get(Py_tss_t *key) {
    if (!key->_is_initialized) {
        return NULL;
    }
    return tss_values[key->_key - 1];
}

int PyThread_tss_set(Py_tss_t *key, void *value) {
    if (!key->_is_initialized) {
        return -1;
    }
    tss_values[key->_key - 1] = value;
    return 0;
}

Py_tss_t *PyThread_tss_alloc(void) {
    return calloc(1, sizeof(Py_tss_t));
}

void PyThread_tss_free(Py_tss_t *key) {
    PyThread_tss_delete(key);
    free(key);
}

int PyThread_tss_is_created(Py_tss_t *key) {
    return key->_is_initialized;
}

PyObject *PyInterpreterState_GetDict(PyInterpreterState *interpreter) {
    (void)interpreter;
    static PyObject *dict;
    if (dict == NULL) {
        dict = PyDict_New();
    }
    return dict;
}

const char *Py_GetVersion(void) {
    return PY_VERSION " (nanocode)";
}

PyObject **_PyObject_GetDictPtr(PyObject *object) {
    Py_ssize_t offset = Py_TYPE(object)->tp_dictoffset;
    if (offset <= 0) {
        return NULL;
    }
    return (PyObject **)((char *)object + offset);
}

int PyObject_VisitManagedDict(PyObject *object, visitproc visit, void *argument) {
    PyObject **dict = _PyObject_GetDictPtr(object);
    if (dict != NULL && *dict != NULL) {
        return visit(*dict, argument);
    }
    return 0;
}

void PyObject_ClearManagedDict(PyObject *object) {
    PyObject **dict = _PyObject_GetDictPtr(object);
    if (dict != NULL) {
        Py_CLEAR(*dict);
    }
}

static void instancemethod_dealloc(PyObject *self) {
    Py_XDECREF(((PyInstanceMethodObject *)self)->func);
    Py_TYPE(self)->tp_free(self);
}

static PyObject *instancemethod_call(PyObject *self, PyObject *args, PyObject *kwargs) {
    return PyObject_Call(((PyInstanceMethodObject *)self)->func, args, kwargs);
}

static PyObject *instancemethod_get(PyObject *self, PyObject *object, PyObject *type) {
    (void)type;
    PyObject *func = ((PyInstanceMethodObject *)self)->func;
    if (object == NULL || object == Py_None) {
        return Py_NewRef(func);
    }
    return PyMethod_New(func, object);
}

static PyObject *instancemethod_doc(PyObject *self, void *closure) {
    (void)closure;
    return PyObject_GetAttrString(((PyInstanceMethodObject *)self)->func, "__doc__");
}

static PyObject *instancemethod_name(PyObject *self, void *closure) {
    (void)closure;
    return PyObject_GetAttrString(((PyInstanceMethodObject *)self)->func, "__name__");
}

static PyMemberDef instancemethod_members[] = {
    {"__func__", Py_T_OBJECT_EX, offsetof(PyInstanceMethodObject, func), Py_READONLY, NULL},
    {NULL, 0, 0, 0, NULL},
};

static PyGetSetDef instancemethod_getset[] = {
    {"__doc__", instancemethod_doc, NULL, NULL, NULL},
    {"__name__", instancemethod_name, NULL, NULL, NULL},
    {NULL, NULL, NULL, NULL, NULL},
};

PyTypeObject PyInstanceMethod_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "instancemethod",
    .tp_basicsize = sizeof(PyInstanceMethodObject),
    .tp_dealloc = instancemethod_dealloc,
    .tp_call = instancemethod_call,
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_members = instancemethod_members,
    .tp_getset = instancemethod_getset,
    .tp_descr_get = instancemethod_get,
};

PyObject *PyInstanceMethod_New(PyObject *func) {
    PyInstanceMethodObject *method = PyObject_New(PyInstanceMethodObject, &PyInstanceMethod_Type);
    if (method == NULL) {
        return NULL;
    }
    method->func = Py_NewRef(func);
    return (PyObject *)method;
}

PyObject *PyInstanceMethod_Function(PyObject *method) {
    return ((PyInstanceMethodObject *)method)->func;
}

static PyObject *builtin_method(PyTypeObject *type, const char *name) {
    return PyObject_GetAttrString((PyObject *)type, name);
}

static PyObject *type_getattro(PyObject *object, PyObject *name) {
    PyObject *method = builtin_method(&PyType_Type, "__getattribute__");
    if (method == NULL) {
        return NULL;
    }
    PyObject *result = PyObject_CallFunctionObjArgs(method, object, name, NULL);
    Py_DECREF(method);
    return result;
}

static int type_setattro(PyObject *object, PyObject *name, PyObject *value) {
    PyObject *method = builtin_method(&PyType_Type, value == NULL ? "__delattr__" : "__setattr__");
    if (method == NULL) {
        return -1;
    }
    PyObject *result = value == NULL ? PyObject_CallFunctionObjArgs(method, object, name, NULL)
                                     : PyObject_CallFunctionObjArgs(method, object, name, value, NULL);
    Py_DECREF(method);
    if (result == NULL) {
        return -1;
    }
    Py_DECREF(result);
    return 0;
}

static PyObject *type_call(PyObject *type, PyObject *args, PyObject *kwargs) {
    PyObject *method = builtin_method(&PyType_Type, "__call__");
    if (method == NULL) {
        return NULL;
    }
    Py_ssize_t count = PyTuple_Size(args);
    PyObject *full = PyTuple_New(count + 1);
    if (full == NULL) {
        Py_DECREF(method);
        return NULL;
    }
    PyTuple_SetItem(full, 0, Py_NewRef(type));
    for (Py_ssize_t index = 0; index < count; index++) {
        PyTuple_SetItem(full, index + 1, Py_NewRef(PyTuple_GetItem(args, index)));
    }
    PyObject *result = PyObject_Call(method, full, kwargs);
    Py_DECREF(full);
    Py_DECREF(method);
    return result;
}

static void type_dealloc(PyObject *type) {
    (void)type;
}

static PyObject *property_get(PyObject *self, PyObject *object, PyObject *type) {
    PyObject *method = builtin_method(&PyProperty_Type, "__get__");
    if (method == NULL) {
        return NULL;
    }
    PyObject *result = PyObject_CallFunctionObjArgs(method, self, object == NULL ? Py_None : object,
                                                    type == NULL ? Py_None : type, NULL);
    Py_DECREF(method);
    return result;
}

static int property_set(PyObject *self, PyObject *object, PyObject *value) {
    PyObject *method = builtin_method(&PyProperty_Type, value == NULL ? "__delete__" : "__set__");
    if (method == NULL) {
        return -1;
    }
    PyObject *result = value == NULL ? PyObject_CallFunctionObjArgs(method, self, object, NULL)
                                     : PyObject_CallFunctionObjArgs(method, self, object, value, NULL);
    Py_DECREF(method);
    if (result == NULL) {
        return -1;
    }
    Py_DECREF(result);
    return 0;
}

void nanocode_builtin_slots(void) {
    PyType_Type.tp_basicsize = sizeof(PyHeapTypeObject);
    PyType_Type.tp_alloc = PyType_GenericAlloc;
    PyType_Type.tp_getattro = type_getattro;
    PyType_Type.tp_setattro = type_setattro;
    PyType_Type.tp_call = type_call;
    PyType_Type.tp_dealloc = type_dealloc;
    PyProperty_Type.tp_descr_get = property_get;
    PyProperty_Type.tp_descr_set = property_set;
}
