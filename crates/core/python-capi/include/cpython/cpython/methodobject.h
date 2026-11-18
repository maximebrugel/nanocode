#ifndef Py_CPYTHON_METHODOBJECT_H
#  error "this header file must not be included directly"
#endif

// PyCFunctionObject structure

typedef struct {
    PyObject_HEAD
    PyMethodDef *m_ml; /* Description of the C function to call */
    PyObject    *m_self; /* Passed as 'self' arg to the C func, can be NULL */
    PyObject    *m_module; /* The __module__ attribute, can be anything */
    PyObject    *m_weakreflist; /* List of weak references */
    vectorcallfunc vectorcall;
} PyCFunctionObject;

#define _PyCFunctionObject_CAST(func) \
    (assert(PyCFunction_Check(func)), \
     _Py_CAST(PyCFunctionObject*, (func)))


// PyCMethodObject structure

typedef struct {
    PyCFunctionObject func;
    PyTypeObject *mm_class; /* Class that defines this method */
} PyCMethodObject;

#define _PyCMethodObject_CAST(func) \
    (assert(PyCMethod_Check(func)), \
     _Py_CAST(PyCMethodObject*, (func)))

PyAPI_DATA(PyTypeObject) PyCMethod_Type;

#define PyCMethod_CheckExact(op) Py_IS_TYPE((op), &PyCMethod_Type)
#define PyCMethod_Check(op) PyObject_TypeCheck((op), &PyCMethod_Type)


/* nanocode: builtin functions are RustPython objects, not PyCFunctionObject
   structs, so these read the method definition and self the runtime keeps
   for each function PyCMethod_New made, as the PyCFunction_Get* calls do.
   A function made in Python (no method definition) gives NULL and 0. */
static inline PyCFunction PyCFunction_GET_FUNCTION(PyObject *func) {
    return PyCFunction_GetFunction(func);
}
#define PyCFunction_GET_FUNCTION(func) PyCFunction_GET_FUNCTION(_PyObject_CAST(func))

static inline PyObject* PyCFunction_GET_SELF(PyObject *func_obj) {
    if (PyCFunction_GetFlags(func_obj) & METH_STATIC) {
        return _Py_NULL;
    }
    return PyCFunction_GetSelf(func_obj);
}
#define PyCFunction_GET_SELF(func) PyCFunction_GET_SELF(_PyObject_CAST(func))

static inline int PyCFunction_GET_FLAGS(PyObject *func) {
    return PyCFunction_GetFlags(func);
}
#define PyCFunction_GET_FLAGS(func) PyCFunction_GET_FLAGS(_PyObject_CAST(func))

/* The defining class is not kept for builtin functions here. */
static inline PyTypeObject* PyCFunction_GET_CLASS(PyObject *func_obj) {
    (void)func_obj;
    return _Py_NULL;
}
#define PyCFunction_GET_CLASS(func) PyCFunction_GET_CLASS(_PyObject_CAST(func))
