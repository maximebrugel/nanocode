use std::{
    ffi::{c_char, c_int, c_void},
    mem::transmute,
    ptr::null_mut,
};

use rustpython_vm::{
    AsObject, Py, PyObject, PyObjectRef, PyPayload, PyRef, PyResult, VirtualMachine,
    builtins::{PyStr, PyTuple, PyType, PyTypeRef},
    function::{Callee, FuncArgs, ItemDoc, PyMethodDef, PyMethodFlags},
    types::{PyTypeFlags, PyTypeSlots},
};

use crate::{
    abi::*,
    bridge::{self, CObject, arg, borrowed, check, enter, finish, incref, new_ref, text, xdecref},
    cast::{call1, call2, call3, call4},
    heap,
};

pub fn builtin(
    name: &'static str,
    doc: Option<&'static str>,
    flags: PyMethodFlags,
    f: impl Fn(&VirtualMachine, FuncArgs) -> PyResult + 'static,
) -> &'static PyMethodDef {
    let call = move |vm: &VirtualMachine, args: FuncArgs, _: Callee| f(vm, args);
    Box::leak(Box::new(PyMethodDef::new_dyn(
        name,
        Box::leak(Box::new(call)),
        flags,
        ItemDoc {
            text: doc,
            ..ItemDoc::NONE
        },
    )))
}

pub fn function(
    vm: &VirtualMachine,
    name: &'static str,
    f: impl Fn(&VirtualMachine, FuncArgs) -> PyResult + 'static,
) -> PyObjectRef {
    builtin(name, None, PyMethodFlags::empty(), f)
        .build_function(&vm.ctx)
        .into()
}

pub fn module_function(
    vm: &VirtualMachine,
    module: &PyObject,
    name: &'static str,
    f: impl Fn(&VirtualMachine, FuncArgs) -> PyResult + 'static,
) -> PyResult<PyObjectRef> {
    let owner = module.get_attr("__name__", vm)?.str(vm)?;
    Ok(builtin(name, None, PyMethodFlags::empty(), f)
        .to_function()
        .with_module(vm.ctx.intern_str(owner.as_wtf8()))
        .with_module_object(module.to_owned())
        .into_ref(&vm.ctx)
        .into())
}

type Call2 = unsafe extern "C" fn(*mut Object, *mut Object) -> *mut Object;
type Call3 = unsafe extern "C" fn(*mut Object, *mut Object, *mut Object) -> *mut Object;
type Fast = unsafe extern "C" fn(*mut Object, *const *mut Object, isize) -> *mut Object;
type FastKeywords =
    unsafe extern "C" fn(*mut Object, *const *mut Object, isize, *mut Object) -> *mut Object;
type Defining = unsafe extern "C" fn(
    *mut Object,
    *mut Type,
    *const *mut Object,
    isize,
    *mut Object,
) -> *mut Object;

fn vector(vm: &VirtualMachine, args: &FuncArgs) -> (Vec<*mut Object>, *mut Object) {
    let mut items: Vec<*mut Object> = args.args.iter().map(|item| borrowed(vm, item)).collect();
    if args.kwargs.is_empty() {
        return (items, null_mut());
    }
    let mut names = Vec::with_capacity(args.kwargs.len());
    for (key, value) in &args.kwargs {
        items.push(borrowed(vm, value));
        names.push(vm.ctx.new_str(key.clone()).into());
    }
    (items, borrowed(vm, vm.ctx.new_tuple(names).as_object()))
}

pub fn invoke(
    vm: &VirtualMachine,
    def: *const MethodDef,
    class: *mut Type,
    zelf: &dyn Fn(&VirtualMachine) -> *mut Object,
    args: FuncArgs,
) -> PyResult {
    let (name, flags, meth) =
        unsafe { (text((*def).name).unwrap_or("?"), (*def).flags, (*def).meth) };
    let Some(meth) = meth else {
        return Err(vm.new_system_error(format!("{name}() has no C function")));
    };
    let positional = args.args.len();
    let keywords = args.kwargs.len();
    let calls =
        flags & (METH_VARARGS | METH_KEYWORDS | METH_NOARGS | METH_O | METH_FASTCALL | METH_METHOD);
    if keywords > 0 && calls & METH_KEYWORDS == 0 {
        return Err(vm.new_type_error(format!("{name}() takes no keyword arguments")));
    }
    match calls {
        METH_NOARGS if positional != 0 => {
            return Err(
                vm.new_type_error(format!("{name}() takes no arguments ({positional} given)"))
            );
        }
        METH_O if positional != 1 => {
            return Err(vm.new_type_error(format!(
                "{name}() takes exactly one argument ({positional} given)"
            )));
        }
        _ => {}
    }
    enter(vm, || {
        let zelf = zelf(vm);
        let result = unsafe {
            match calls {
                METH_NOARGS => call2(
                    transmute::<unsafe extern "C" fn(), Call2>(meth),
                    zelf,
                    null_mut(),
                ),
                METH_O => call2(
                    transmute::<unsafe extern "C" fn(), Call2>(meth),
                    zelf,
                    borrowed(vm, &args.args[0]),
                ),
                METH_VARARGS => {
                    let tuple = vm.ctx.new_tuple(args.args.clone());
                    call2(
                        transmute::<unsafe extern "C" fn(), Call2>(meth),
                        zelf,
                        borrowed(vm, tuple.as_object()),
                    )
                }
                c if c == METH_VARARGS | METH_KEYWORDS => {
                    let tuple = vm.ctx.new_tuple(args.args.clone());
                    let kwargs = if keywords == 0 {
                        null_mut()
                    } else {
                        let dict = vm.ctx.new_dict();
                        for (key, value) in &args.kwargs {
                            dict.set_item(
                                vm.ctx.new_str(key.clone()).as_object(),
                                value.clone(),
                                vm,
                            )?;
                        }
                        borrowed(vm, dict.as_object())
                    };
                    call3(
                        transmute::<unsafe extern "C" fn(), Call3>(meth),
                        zelf,
                        borrowed(vm, tuple.as_object()),
                        kwargs,
                    )
                }
                METH_FASTCALL => {
                    let items: Vec<*mut Object> =
                        args.args.iter().map(|item| borrowed(vm, item)).collect();
                    call3(
                        transmute::<unsafe extern "C" fn(), Fast>(meth),
                        zelf,
                        items.as_ptr(),
                        positional as isize,
                    )
                }
                c if c == METH_FASTCALL | METH_KEYWORDS => {
                    let (items, names) = vector(vm, &args);
                    call4(
                        transmute::<unsafe extern "C" fn(), FastKeywords>(meth),
                        zelf,
                        items.as_ptr(),
                        positional as isize,
                        names,
                    )
                }
                c if c == METH_METHOD | METH_FASTCALL | METH_KEYWORDS => {
                    if class.is_null() {
                        return Err(vm.new_system_error(format!(
                            "{name}() uses METH_METHOD outside a class"
                        )));
                    }
                    let (items, names) = vector(vm, &args);
                    transmute::<unsafe extern "C" fn(), Defining>(meth)(
                        zelf,
                        class,
                        items.as_ptr(),
                        positional as isize,
                        names,
                    )
                }
                other => return Err(vm.new_system_error(format!(
                    "{name}() uses calling convention {other:#x}, which the C API does not run yet"
                ))),
            }
        };
        finish(vm, result)
    })
}

fn pointer_of(vm: &VirtualMachine, object: &PyObject) -> PyResult<*mut Object> {
    if let Some(wrapper) = object.downcast_ref::<CObject>() {
        return Ok(wrapper.pointer());
    }
    Ok(bridge::borrowed(vm, object))
}

fn first(vm: &VirtualMachine, args: &mut FuncArgs) -> PyResult<PyObjectRef> {
    if args.args.is_empty() {
        return Err(vm.new_type_error("the method needs an instance".to_owned()));
    }
    Ok(args.args.remove(0))
}

fn method(
    vm: &VirtualMachine,
    class: &'static Py<PyType>,
    name: &'static str,
    f: impl Fn(&VirtualMachine, PyObjectRef, FuncArgs) -> PyResult + 'static,
) -> PyObjectRef {
    builtin(name, None, PyMethodFlags::METHOD, move |vm, mut args| {
        let zelf = first(vm, &mut args)?;
        f(vm, zelf, args)
    })
    .build_method(&vm.ctx, class)
    .into()
}

fn member(
    vm: &VirtualMachine,
    class: &'static Py<PyType>,
    def: &MemberDef,
) -> PyResult<PyObjectRef> {
    let name = text(def.name).unwrap_or("?");
    let kind = def.kind;
    let offset = def.offset as usize;
    let get: Getter = Box::new(move |vm, zelf| {
        let at = unsafe { pointer_of(vm, &zelf)?.cast::<u8>().add(offset) };
        read_member(vm, kind, at, name)
    });
    let set: Option<Setter> = (def.flags & READONLY == 0).then(|| {
        Box::new(
            move |vm: &VirtualMachine, zelf: PyObjectRef, value: PyObjectRef| {
                let at = unsafe { pointer_of(vm, &zelf)?.cast::<u8>().add(offset) };
                write_member(vm, kind, at, &value)
            },
        ) as Setter
    });
    Ok(descriptor(vm, class, name, Some(get), set, text(def.doc)))
}

type Getter = Box<dyn Fn(&VirtualMachine, PyObjectRef) -> PyResult + Send + Sync>;
type Setter = Box<dyn Fn(&VirtualMachine, PyObjectRef, PyObjectRef) -> PyResult<()> + Send + Sync>;

fn descriptor(
    vm: &VirtualMachine,
    class: &'static Py<PyType>,
    name: &str,
    get: Option<Getter>,
    set: Option<Setter>,
    doc: Option<&'static str>,
) -> PyObjectRef {
    let mut descriptor = rustpython_vm::builtins::PyGetSet::new(name, class, &vm.ctx);
    if let Some(get) = get {
        descriptor =
            descriptor.with_get(move |zelf: PyObjectRef, vm: &VirtualMachine| get(vm, zelf));
    }
    if let Some(set) = set {
        descriptor = descriptor.with_set(
            move |zelf: PyObjectRef,
                  value: rustpython_vm::function::PySetterValue,
                  vm: &VirtualMachine| match value {
                rustpython_vm::function::PySetterValue::Assign(value) => set(vm, zelf, value),
                rustpython_vm::function::PySetterValue::Delete => {
                    Err(vm.new_type_error("can't delete attribute".to_owned()))
                }
            },
        );
    }
    if let Some(doc) = doc {
        descriptor = descriptor.with_doc(rustpython_vm::function::ItemDoc::static_text(doc));
    }
    PyRef::new_ref(descriptor, vm.ctx.types.getset_type.to_owned(), None).into()
}

fn integer<T: Copy>(at: *mut u8) -> T {
    unsafe { at.cast::<T>().read_unaligned() }
}

fn read_member(vm: &VirtualMachine, kind: c_int, at: *mut u8, name: &str) -> PyResult {
    Ok(match kind {
        T_OBJECT | T_OBJECT_EX => {
            let pointer = integer::<*mut Object>(at);
            if pointer.is_null() {
                if kind == T_OBJECT_EX {
                    return Err(vm.new_attribute_error(format!("'{name}' is not set")));
                }
                return Ok(vm.ctx.none());
            }
            enter(vm, || unsafe { bridge::to_rust(vm, pointer) })?
        }
        T_PYSSIZET => vm.ctx.new_int(integer::<isize>(at)).into(),
        T_INT => vm.ctx.new_int(integer::<c_int>(at)).into(),
        T_UINT => vm.ctx.new_int(integer::<u32>(at)).into(),
        T_LONG => vm.ctx.new_int(integer::<std::ffi::c_long>(at)).into(),
        T_ULONG => vm.ctx.new_int(integer::<std::ffi::c_ulong>(at)).into(),
        T_LONGLONG => vm.ctx.new_int(integer::<i64>(at)).into(),
        T_ULONGLONG => vm.ctx.new_int(integer::<u64>(at)).into(),
        T_SHORT => vm.ctx.new_int(integer::<i16>(at)).into(),
        T_USHORT => vm.ctx.new_int(integer::<u16>(at)).into(),
        T_BYTE => vm.ctx.new_int(integer::<i8>(at)).into(),
        T_UBYTE => vm.ctx.new_int(integer::<u8>(at)).into(),
        T_BOOL => vm.ctx.new_bool(integer::<u8>(at) != 0).into(),
        T_CHAR => vm
            .ctx
            .new_str(char::from(integer::<u8>(at)).to_string())
            .into(),
        T_DOUBLE => vm.ctx.new_float(integer::<f64>(at)).into(),
        T_FLOAT => vm.ctx.new_float(f64::from(integer::<f32>(at))).into(),
        T_STRING => text(integer::<*const c_char>(at))
            .map_or_else(|| vm.ctx.none(), |value| vm.ctx.new_str(value).into()),
        other => {
            return Err(vm.new_system_error(format!(
                "member '{name}' has type {other}, which the C API does not read yet"
            )));
        }
    })
}

fn write_member(
    vm: &VirtualMachine,
    kind: c_int,
    at: *mut u8,
    value: &PyObjectRef,
) -> PyResult<()> {
    fn store<T>(at: *mut u8, value: T) {
        unsafe { at.cast::<T>().write_unaligned(value) };
    }
    let index = |vm: &VirtualMachine| value.try_index(vm);
    match kind {
        T_OBJECT | T_OBJECT_EX => enter(vm, || {
            let new = new_ref(vm, value);
            let old = integer::<*mut Object>(at);
            store(at, new);
            unsafe { xdecref(old) };
        }),
        T_PYSSIZET => store(at, index(vm)?.try_to_primitive::<isize>(vm)?),
        T_INT => store(at, index(vm)?.try_to_primitive::<c_int>(vm)?),
        T_UINT => store(at, index(vm)?.try_to_primitive::<u32>(vm)?),
        T_LONG => store(at, index(vm)?.try_to_primitive::<std::ffi::c_long>(vm)?),
        T_ULONG => store(at, index(vm)?.try_to_primitive::<std::ffi::c_ulong>(vm)?),
        T_LONGLONG => store(at, index(vm)?.try_to_primitive::<i64>(vm)?),
        T_ULONGLONG => store(at, index(vm)?.try_to_primitive::<u64>(vm)?),
        T_SHORT => store(at, index(vm)?.try_to_primitive::<i16>(vm)?),
        T_USHORT => store(at, index(vm)?.try_to_primitive::<u16>(vm)?),
        T_BYTE => store(at, index(vm)?.try_to_primitive::<i8>(vm)?),
        T_UBYTE => store(at, index(vm)?.try_to_primitive::<u8>(vm)?),
        T_BOOL => store(at, u8::from(value.clone().is_true(vm)?)),
        T_DOUBLE => store(at, value.try_float(vm)?.to_f64()),
        T_FLOAT => store(at, value.try_float(vm)?.to_f64() as f32),
        other => {
            return Err(
                vm.new_type_error(format!("member type {other} cannot be set from Python yet"))
            );
        }
    }
    Ok(())
}

fn getset(
    vm: &VirtualMachine,
    class: &'static Py<PyType>,
    def: &GetSetDef,
) -> PyResult<PyObjectRef> {
    let name = text(def.name).unwrap_or("?");
    let closure = def.closure as usize;
    let get = def.get.map(|get| {
        Box::new(move |vm: &VirtualMachine, zelf: PyObjectRef| {
            enter(vm, || {
                let pointer = borrowed(vm, &zelf);
                finish(vm, unsafe { call2(get, pointer, closure as *mut c_void) })
            })
        }) as Getter
    });
    let set = def.set.map(|set| {
        Box::new(
            move |vm: &VirtualMachine, zelf: PyObjectRef, value: PyObjectRef| {
                enter(vm, || {
                    let result = unsafe {
                        call3(
                            set,
                            borrowed(vm, &zelf),
                            borrowed(vm, &value),
                            closure as *mut c_void,
                        )
                    };
                    check(vm, result as isize).map(drop)
                })
            },
        ) as Setter
    });
    Ok(descriptor(vm, class, name, get, set, text(def.doc)))
}

fn unary(
    vm: &VirtualMachine,
    zelf: &PyObjectRef,
    f: unsafe extern "C" fn(*mut Object) -> *mut Object,
) -> PyResult {
    enter(vm, || finish(vm, unsafe { call1(f, borrowed(vm, zelf)) }))
}

fn binary(
    vm: &VirtualMachine,
    zelf: &PyObjectRef,
    other: &PyObjectRef,
    f: unsafe extern "C" fn(*mut Object, *mut Object) -> *mut Object,
) -> PyResult {
    enter(vm, || {
        finish(vm, unsafe {
            call2(f, borrowed(vm, zelf), borrowed(vm, other))
        })
    })
}

fn one(vm: &VirtualMachine, args: &FuncArgs) -> PyResult<PyObjectRef> {
    args.args
        .first()
        .cloned()
        .ok_or_else(|| vm.new_type_error("expected one argument".to_owned()))
}

fn keywords(vm: &VirtualMachine, args: &FuncArgs) -> PyResult<*mut Object> {
    if args.kwargs.is_empty() {
        return Ok(null_mut());
    }
    let dict = vm.ctx.new_dict();
    for (key, value) in &args.kwargs {
        dict.set_item(vm.ctx.new_str(key.clone()).as_object(), value.clone(), vm)?;
    }
    Ok(borrowed(vm, dict.as_object()))
}

fn power(
    vm: &VirtualMachine,
    base: &PyObjectRef,
    exponent: &PyObjectRef,
    modulo: Option<&PyObjectRef>,
    f: unsafe extern "C" fn(*mut Object, *mut Object, *mut Object) -> *mut Object,
) -> PyResult {
    enter(vm, || {
        let modulo = modulo.map_or_else(bridge::none, |modulo| borrowed(vm, modulo));
        finish(vm, unsafe {
            call3(f, borrowed(vm, base), borrowed(vm, exponent), modulo)
        })
    })
}

fn numbers(
    vm: &VirtualMachine,
    class: &'static Py<PyType>,
    number: &NumberMethods,
) -> Vec<(&'static str, PyObjectRef)> {
    let mut out: Vec<(&'static str, PyObjectRef)> = Vec::new();
    for (name, reflected, f) in [
        ("__add__", "__radd__", number.add),
        ("__sub__", "__rsub__", number.subtract),
        ("__mul__", "__rmul__", number.multiply),
        ("__mod__", "__rmod__", number.remainder),
        ("__divmod__", "__rdivmod__", number.divmod),
        ("__lshift__", "__rlshift__", number.lshift),
        ("__rshift__", "__rrshift__", number.rshift),
        ("__and__", "__rand__", number.and),
        ("__xor__", "__rxor__", number.xor),
        ("__or__", "__ror__", number.or),
        ("__floordiv__", "__rfloordiv__", number.floor_divide),
        ("__truediv__", "__rtruediv__", number.true_divide),
        ("__matmul__", "__rmatmul__", number.matrix_multiply),
    ] {
        let Some(f) = f else { continue };
        out.push((
            name,
            method(vm, class, name, move |vm, zelf, args| {
                binary(vm, &zelf, &one(vm, &args)?, f)
            }),
        ));
        out.push((
            reflected,
            method(vm, class, reflected, move |vm, zelf, args| {
                binary(vm, &one(vm, &args)?, &zelf, f)
            }),
        ));
    }
    for (name, f) in [
        ("__iadd__", number.inplace_add),
        ("__isub__", number.inplace_subtract),
        ("__imul__", number.inplace_multiply),
        ("__imod__", number.inplace_remainder),
        ("__ilshift__", number.inplace_lshift),
        ("__irshift__", number.inplace_rshift),
        ("__iand__", number.inplace_and),
        ("__ixor__", number.inplace_xor),
        ("__ior__", number.inplace_or),
        ("__ifloordiv__", number.inplace_floor_divide),
        ("__itruediv__", number.inplace_true_divide),
        ("__imatmul__", number.inplace_matrix_multiply),
    ] {
        let Some(f) = f else { continue };
        out.push((
            name,
            method(vm, class, name, move |vm, zelf, args| {
                binary(vm, &zelf, &one(vm, &args)?, f)
            }),
        ));
    }
    for (name, f) in [
        ("__neg__", number.negative),
        ("__pos__", number.positive),
        ("__abs__", number.absolute),
        ("__invert__", number.invert),
        ("__int__", number.int),
        ("__float__", number.float),
        ("__index__", number.index),
    ] {
        let Some(f) = f else { continue };
        out.push((
            name,
            method(vm, class, name, move |vm, zelf, _| unary(vm, &zelf, f)),
        ));
    }
    if let Some(f) = number.power {
        out.push((
            "__pow__",
            method(vm, class, "__pow__", move |vm, zelf, args| {
                power(vm, &zelf, &one(vm, &args)?, args.args.get(1), f)
            }),
        ));
        out.push((
            "__rpow__",
            method(vm, class, "__rpow__", move |vm, zelf, args| {
                power(vm, &one(vm, &args)?, &zelf, args.args.get(1), f)
            }),
        ));
    }
    if let Some(f) = number.inplace_power {
        out.push((
            "__ipow__",
            method(vm, class, "__ipow__", move |vm, zelf, args| {
                power(vm, &zelf, &one(vm, &args)?, args.args.get(1), f)
            }),
        ));
    }
    out
}

fn slots(
    vm: &VirtualMachine,
    ty: *mut Type,
    class: &'static Py<PyType>,
) -> Vec<(&'static str, PyObjectRef)> {
    let t = unsafe { &*ty };
    let mut out: Vec<(&'static str, PyObjectRef)> = Vec::new();
    if let Some(f) = t.repr {
        out.push((
            "__repr__",
            method(vm, class, "__repr__", move |vm, zelf, _| {
                unary(vm, &zelf, f)
            }),
        ));
    }
    if let Some(f) = t.str {
        out.push((
            "__str__",
            method(vm, class, "__str__", move |vm, zelf, _| unary(vm, &zelf, f)),
        ));
    }
    if let Some(f) = t.iter {
        out.push((
            "__iter__",
            method(vm, class, "__iter__", move |vm, zelf, _| {
                unary(vm, &zelf, f)
            }),
        ));
    }
    if let Some(f) = t.iternext {
        out.push((
            "__next__",
            method(vm, class, "__next__", move |vm, zelf, _| {
                enter(vm, || {
                    let result = unsafe { call1(f, borrowed(vm, &zelf)) };
                    if result.is_null() && bridge::current_error().is_none() {
                        return Err(vm.new_stop_iteration(None));
                    }
                    finish(vm, result)
                })
            }),
        ));
    }
    if let Some(f) = t.hash {
        out.push((
            "__hash__",
            method(vm, class, "__hash__", move |vm, zelf, _| {
                enter(vm, || {
                    check(vm, unsafe { call1(f, borrowed(vm, &zelf)) })
                        .map(|hash| vm.ctx.new_int(hash).into())
                })
            }),
        ));
    }
    if let Some(f) = t.call {
        out.push((
            "__call__",
            method(vm, class, "__call__", move |vm, zelf, args| {
                enter(vm, || {
                    let tuple = vm.ctx.new_tuple(args.args.clone());
                    let kwargs = keywords(vm, &args)?;
                    finish(vm, unsafe {
                        call3(
                            f,
                            borrowed(vm, &zelf),
                            borrowed(vm, tuple.as_object()),
                            kwargs,
                        )
                    })
                })
            }),
        ));
    }
    if let Some(f) = t.richcompare {
        for (name, op) in [
            ("__lt__", LT),
            ("__le__", LE),
            ("__eq__", EQ),
            ("__ne__", NE),
            ("__gt__", GT),
            ("__ge__", GE),
        ] {
            out.push((
                name,
                method(vm, class, name, move |vm, zelf, args| {
                    let other = one(vm, &args)?;
                    enter(vm, || {
                        finish(vm, unsafe {
                            call3(f, borrowed(vm, &zelf), borrowed(vm, &other), op)
                        })
                    })
                }),
            ));
        }
    }
    let mapping = if t.as_mapping.is_null() {
        None
    } else {
        Some(unsafe { &*t.as_mapping })
    };
    let sequence = if t.as_sequence.is_null() {
        None
    } else {
        Some(unsafe { &*t.as_sequence })
    };
    if let Some(f) = mapping.and_then(|mapping| mapping.subscript) {
        out.push((
            "__getitem__",
            method(vm, class, "__getitem__", move |vm, zelf, args| {
                binary(vm, &zelf, &one(vm, &args)?, f)
            }),
        ));
    } else if let Some(f) = sequence.and_then(|sequence| sequence.item) {
        out.push((
            "__getitem__",
            method(vm, class, "__getitem__", move |vm, zelf, args| {
                let index = one(vm, &args)?
                    .try_index(vm)?
                    .try_to_primitive::<isize>(vm)?;
                enter(vm, || {
                    finish(vm, unsafe { call2(f, borrowed(vm, &zelf), index) })
                })
            }),
        ));
    }
    if let Some(f) = mapping
        .and_then(|mapping| mapping.length)
        .or_else(|| sequence.and_then(|sequence| sequence.length))
    {
        out.push((
            "__len__",
            method(vm, class, "__len__", move |vm, zelf, _| {
                enter(vm, || {
                    check(vm, unsafe { call1(f, borrowed(vm, &zelf)) })
                        .map(|length| vm.ctx.new_int(length).into())
                })
            }),
        ));
    }
    if let Some(f) = mapping.and_then(|mapping| mapping.ass_subscript) {
        out.push((
            "__setitem__",
            method(vm, class, "__setitem__", move |vm, zelf, args| {
                let (Some(key), Some(value)) = (args.args.first(), args.args.get(1)) else {
                    return Err(
                        vm.new_type_error("__setitem__ expects a key and a value".to_owned())
                    );
                };
                enter(vm, || {
                    check(vm, unsafe {
                        call3(
                            f,
                            borrowed(vm, &zelf),
                            borrowed(vm, key),
                            borrowed(vm, value),
                        )
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
        out.push((
            "__delitem__",
            method(vm, class, "__delitem__", move |vm, zelf, args| {
                let key = one(vm, &args)?;
                enter(vm, || {
                    check(vm, unsafe {
                        call3(f, borrowed(vm, &zelf), borrowed(vm, &key), null_mut())
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
    }
    if let Some(f) = sequence.and_then(|sequence| sequence.contains) {
        out.push((
            "__contains__",
            method(vm, class, "__contains__", move |vm, zelf, args| {
                let item = one(vm, &args)?;
                enter(vm, || {
                    check(
                        vm,
                        unsafe { call2(f, borrowed(vm, &zelf), borrowed(vm, &item)) } as isize,
                    )
                    .map(|found| vm.ctx.new_bool(found != 0).into())
                })
            }),
        ));
    }
    if !t.as_number.is_null() {
        out.extend(numbers(vm, class, unsafe { &*t.as_number }));
    }
    if !t.as_number.is_null()
        && let Some(f) = unsafe { (*t.as_number).bool }
    {
        out.push((
            "__bool__",
            method(vm, class, "__bool__", move |vm, zelf, _| {
                enter(vm, || {
                    check(vm, unsafe { call1(f, borrowed(vm, &zelf)) } as isize)
                        .map(|value| vm.ctx.new_bool(value != 0).into())
                })
            }),
        ));
    }
    if !t.descr_get.is_null() {
        let f = unsafe {
            transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut Object, *mut Object, *mut Object) -> *mut Object,
            >(t.descr_get)
        };
        out.push((
            "__get__",
            method(vm, class, "__get__", move |vm, zelf, args| {
                let instance = args.args.first().cloned().unwrap_or_else(|| vm.ctx.none());
                let owner = args.args.get(1).cloned();
                enter(vm, || {
                    let instance = if vm.is_none(&instance) {
                        null_mut()
                    } else {
                        borrowed(vm, &instance)
                    };
                    let owner = owner
                        .as_ref()
                        .map_or(null_mut(), |owner| borrowed(vm, owner));
                    finish(vm, unsafe {
                        call3(f, borrowed(vm, &zelf), instance, owner)
                    })
                })
            }),
        ));
    }
    if !t.descr_set.is_null() {
        let f = unsafe {
            transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut Object, *mut Object, *mut Object) -> c_int,
            >(t.descr_set)
        };
        out.push((
            "__set__",
            method(vm, class, "__set__", move |vm, zelf, args| {
                let (Some(instance), Some(value)) = (args.args.first(), args.args.get(1)) else {
                    return Err(
                        vm.new_type_error("__set__ expects an instance and a value".to_owned())
                    );
                };
                enter(vm, || {
                    check(vm, unsafe {
                        call3(
                            f,
                            borrowed(vm, &zelf),
                            borrowed(vm, instance),
                            borrowed(vm, value),
                        )
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
        out.push((
            "__delete__",
            method(vm, class, "__delete__", move |vm, zelf, args| {
                let instance = one(vm, &args)?;
                enter(vm, || {
                    check(vm, unsafe {
                        call3(f, borrowed(vm, &zelf), borrowed(vm, &instance), null_mut())
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
    }
    if let Some(f) = t.getattro
        && f as usize != crate::api::PyObject_GenericGetAttr as *const () as usize
    {
        out.push((
            "__getattribute__",
            method(vm, class, "__getattribute__", move |vm, zelf, args| {
                binary(vm, &zelf, &one(vm, &args)?, f)
            }),
        ));
    }
    if let Some(f) = t.setattro
        && f as usize != crate::api::PyObject_GenericSetAttr as *const () as usize
    {
        out.push((
            "__setattr__",
            method(vm, class, "__setattr__", move |vm, zelf, args| {
                let (Some(name), Some(value)) = (args.args.first(), args.args.get(1)) else {
                    return Err(
                        vm.new_type_error("__setattr__ expects a name and a value".to_owned())
                    );
                };
                enter(vm, || {
                    check(vm, unsafe {
                        call3(
                            f,
                            borrowed(vm, &zelf),
                            borrowed(vm, name),
                            borrowed(vm, value),
                        )
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
        out.push((
            "__delattr__",
            method(vm, class, "__delattr__", move |vm, zelf, args| {
                let name = one(vm, &args)?;
                enter(vm, || {
                    check(vm, unsafe {
                        call3(f, borrowed(vm, &zelf), borrowed(vm, &name), null_mut())
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
    }
    if let Some(f) = t.init {
        out.push((
            "__init__",
            method(vm, class, "__init__", move |vm, zelf, args| {
                enter(vm, || {
                    let tuple = vm.ctx.new_tuple(args.args.clone());
                    let kwargs = keywords(vm, &args)?;
                    check(vm, unsafe {
                        call3(
                            f,
                            borrowed(vm, &zelf),
                            borrowed(vm, tuple.as_object()),
                            kwargs,
                        )
                    } as isize)
                    .map(|_| vm.ctx.none())
                })
            }),
        ));
    }
    if let Some(f) = t.new {
        let ty = ty as usize;
        let new = function(vm, "__new__", move |vm, mut args| {
            let cls = first(vm, &mut args)?;
            let target = match cls.downcast_ref::<PyType>() {
                Some(class) => bridge::type_pointer(vm, class),
                None => ty as *mut Type,
            };
            let mut f = f;
            let mut base = unsafe { (*target).base_type };
            while !base.is_null() && base != ty as *mut Type {
                base = unsafe { (*base).base_type };
            }
            if base.is_null()
                && let Some(solid) = unsafe { (*target).new }
            {
                f = solid;
            }
            enter(vm, || {
                let tuple = vm.ctx.new_tuple(args.args.clone());
                let kwargs = keywords(vm, &args)?;
                finish(vm, unsafe {
                    call3(f, target, borrowed(vm, tuple.as_object()), kwargs)
                })
            })
        });
        if let Ok(new) = vm.ctx.types.staticmethod_type.as_object().call((new,), vm) {
            out.push(("__new__", new));
        }
    }
    out
}

pub fn ready_type(ty: *mut Type) {
    unsafe {
        let t = &mut *ty;
        if t.base.base.ty.is_null() {
            t.base.base.ty = bridge::PyType_Type.get();
        }
        if t.base_type.is_null() && ty != bridge::PyBaseObject_Type.get() {
            t.base_type = bridge::PyBaseObject_Type.get();
        }
        if !t.base_type.is_null() {
            t.flags |= (*t.base_type).flags & TPFLAGS_TYPE_SUBCLASS;
            if t.dealloc.is_none() {
                t.dealloc = (*t.base_type).dealloc;
            }
            if t.basicsize == 0 {
                t.basicsize = (*t.base_type).basicsize;
                t.itemsize = (*t.base_type).itemsize;
            }
            let own = t.as_buffer.cast_mut().cast::<BufferProcs>();
            if let Some(inherited) = crate::buffer::procs(t.base_type)
                && !own.is_null()
                && (*own).getbuffer.is_none()
            {
                (*own).getbuffer = inherited.getbuffer;
                (*own).release = inherited.release;
            }
        }
        if t.alloc.is_none() {
            t.alloc = Some(PyType_GenericAlloc);
        }
        // CPython gives a heap type without tp_new its base's tp_new; for
        // a direct subclass of object that is object_new, which allocates
        // the C struct through tp_alloc. Without it the class would inherit
        // the VM's object.__new__ and hand C code an object with no struct
        // behind it (sqlite3's Connection and Cursor crashed this way).
        if t.new.is_none()
            && t.flags & TPFLAGS_HEAPTYPE != 0
            && t.flags & TPFLAGS_DISALLOW_INSTANTIATION == 0
            && t.base_type == bridge::PyBaseObject_Type.get()
        {
            t.new = Some(PyType_GenericNew);
        }
        if t.free.is_none() {
            t.free = Some(heap::PyObject_Free);
        }
        t.flags |= TPFLAGS_READY;
    }
}

fn shadows_slot(vm: &VirtualMachine, class: &Py<PyType>, name: &str) -> bool {
    class
        .get_attr(vm.ctx.intern_str(name))
        .is_some_and(|found| found.class().is(vm.ctx.types.member_descriptor_type))
}

const SPECIAL_MEMBERS: [&str; 3] = [
    "__weaklistoffset__",
    "__dictoffset__",
    "__vectorcalloffset__",
];

pub fn c_type(vm: &VirtualMachine, ty: *mut Type) -> PyResult<PyTypeRef> {
    if let Some(class) = bridge::static_type(ty) {
        return Ok(class);
    }
    if bridge::proxy_type(ty)
        && let Ok(class) = bridge::arg(vm, ty.cast())?.downcast::<PyType>()
    {
        return Ok(class);
    }
    ready_type(ty);
    let full = text(unsafe { (*ty).name }).unwrap_or("?");
    let (module, name) = full.rsplit_once('.').unwrap_or(("builtins", full));
    let base_type = unsafe { (*ty).base_type };
    let declared = unsafe { (*ty).bases };
    let mut listed = Vec::new();
    if !declared.is_null() {
        let declared = bridge::arg(vm, declared)?;
        let declared = declared
            .downcast_ref::<PyTuple>()
            .ok_or_else(|| vm.new_type_error(format!("{full}: tp_bases must be a tuple")))?;
        for entry in declared.iter() {
            listed.push(
                entry
                    .clone()
                    .downcast::<PyType>()
                    .map_err(|_| vm.new_type_error(format!("{full}: tp_bases must hold types")))?,
            );
        }
    }
    let primary = if base_type.is_null() || base_type == bridge::PyBaseObject_Type.get() {
        CObject::class(&vm.ctx).to_owned()
    } else {
        c_type(vm, base_type)?
    };
    let string = vm.ctx.types.str_type;
    let base = if primary.fast_issubclass(string) || listed.iter().any(|entry| entry.is(string)) {
        string.to_owned()
    } else if primary.fast_issubclass(CObject::class(&vm.ctx))
        || primary.fast_issubclass(vm.ctx.types.type_type)
    {
        primary
    } else if let Some(found) = listed
        .iter()
        .find(|entry| entry.fast_issubclass(CObject::class(&vm.ctx)))
    {
        found.clone()
    } else if primary.slots.flags.has_feature(PyTypeFlags::BASETYPE) {
        primary
    } else {
        return Err(vm.new_system_error(format!(
            "{full}: a C type based on {} is not supported yet",
            primary.name()
        )));
    };
    let extra = listed.iter().any(|entry| !base.fast_issubclass(entry));
    let declared_meta = unsafe { (*ty).base.base.ty };
    let metaclass = if declared_meta.is_null() || declared_meta == bridge::PyType_Type.get() {
        vm.ctx.types.type_type.to_owned()
    } else {
        c_type(vm, declared_meta)?
    };
    let extra = extra || !metaclass.is(vm.ctx.types.type_type);
    let layout = {
        let mut slots = PyTypeSlots::heap_default();
        slots.member_count = base.slots.member_count;
        if unsafe { (*ty).flags } & TPFLAGS_BASETYPE != 0 {
            slots.flags |= PyTypeFlags::from_slice(&[PyTypeFlags::BASETYPE]);
        }
        if unsafe { (*ty).dictoffset } != 0 || unsafe { (*ty).flags } & TPFLAGS_MANAGED_DICT != 0 {
            slots.flags |= PyTypeFlags::from_slice(&[PyTypeFlags::HAS_DICT]);
        }
        if unsafe { (*ty).weaklistoffset } != 0 {
            slots.flags |=
                PyTypeFlags::from_slice(&[PyTypeFlags::HAS_WEAKREF, PyTypeFlags::MANAGED_WEAKREF]);
        }
        slots
    };
    let class = if !extra {
        vm.ctx.new_class(Some(module), name, base, layout)
    } else {
        PyType::new_heap_with_base(
            name,
            module,
            base.clone(),
            if listed.is_empty() {
                vec![base]
            } else {
                listed
            },
            layout,
            metaclass,
            &vm.ctx,
        )
        .map_err(|error| vm.new_type_error(format!("{full}: {error}")))?
    };
    if unsafe { (*ty).flags } & TPFLAGS_HEAPTYPE == 0 {
        class
            .slots
            .static_extension
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let types = &vm.ctx.types;
    let inherited = [
        (types.int_type, TPFLAGS_LONG_SUBCLASS),
        (types.list_type, TPFLAGS_LIST_SUBCLASS),
        (types.tuple_type, TPFLAGS_TUPLE_SUBCLASS),
        (types.bytes_type, TPFLAGS_BYTES_SUBCLASS),
        (types.str_type, TPFLAGS_UNICODE_SUBCLASS),
        (types.dict_type, TPFLAGS_DICT_SUBCLASS),
        (
            vm.ctx.exceptions.base_exception_type,
            TPFLAGS_BASE_EXC_SUBCLASS,
        ),
        (types.type_type, TPFLAGS_TYPE_SUBCLASS),
    ]
    .into_iter()
    .filter(|(builtin, _)| class.fast_issubclass(*builtin))
    .fold(0, |flags, (_, flag)| flags | flag);
    unsafe { (*ty).flags |= inherited };
    bridge::register_c_type(ty, &class);
    // The Python class made here lives for the whole run (it is leaked
    // below and kept in the type table), and it holds the C type, so it owns
    // one reference to it. Without that, a heap type C code made (pybind11's
    // classes) drops to zero when a tuple naming it is freed, and its
    // metaclass dealloc unregisters the class while Python still uses it.
    unsafe { bridge::incref(ty.cast()) };
    // C code walks tp_mro (pybind11 finds a buffer or a base's type info
    // through it), so it gets the class's MRO as a tuple it owns.
    unsafe {
        if (*ty).mro.is_null() {
            let mro: Vec<rustpython_vm::PyObjectRef> =
                class.mro_map_collect(|cls| cls.to_owned().into());
            (*ty).mro = bridge::new_ref(vm, vm.ctx.new_tuple(mro).as_object());
        }
    }
    if crate::buffer::procs(ty).is_some() {
        class.slots.as_buffer.store(Some(crate::buffer::export));
    }
    let leaked: &'static Py<PyType> = PyRef::leak(class.clone());
    let mut attributes: Vec<(&'static str, PyObjectRef)> = Vec::new();
    if let Some(doc) = text(unsafe { (*ty).doc }) {
        attributes.push(("__doc__", vm.ctx.new_str(doc).into()));
    }
    let mut def = unsafe { (*ty).methods };
    while !def.is_null() && !unsafe { (*def).name }.is_null() {
        let name = text(unsafe { (*def).name }).unwrap_or("?");
        let flags = unsafe { (*def).flags };
        let doc = text(unsafe { (*def).doc });
        let address = def as usize;
        let defining = ty as usize;
        let value: PyObjectRef = if flags & METH_CLASS != 0 {
            builtin(name, doc, PyMethodFlags::CLASS, move |vm, mut args| {
                let cls = first(vm, &mut args)?;
                invoke(
                    vm,
                    address as *const MethodDef,
                    defining as *mut Type,
                    &|vm| borrowed(vm, &cls),
                    args,
                )
            })
            .build_classmethod(&vm.ctx, leaked)
            .into()
        } else if flags & METH_STATIC != 0 {
            let f = function(vm, name, move |vm, args| {
                invoke(
                    vm,
                    address as *const MethodDef,
                    defining as *mut Type,
                    &|_| null_mut(),
                    args,
                )
            });
            vm.ctx.types.staticmethod_type.as_object().call((f,), vm)?
        } else {
            builtin(name, doc, PyMethodFlags::METHOD, move |vm, mut args| {
                let zelf = first(vm, &mut args)?;
                invoke(
                    vm,
                    address as *const MethodDef,
                    defining as *mut Type,
                    &|vm| borrowed(vm, &zelf),
                    args,
                )
            })
            .build_method(&vm.ctx, leaked)
            .into()
        };
        attributes.push((name, value));
        def = unsafe { def.add(1) };
    }
    let mut def = unsafe { (*ty).members };
    while !def.is_null() && !unsafe { (*def).name }.is_null() {
        let member_name = text(unsafe { (*def).name }).unwrap_or("?");
        if SPECIAL_MEMBERS.contains(&member_name) || shadows_slot(vm, &class, member_name) {
            def = unsafe { def.add(1) };
            continue;
        }
        attributes.push((
            text(unsafe { (*def).name }).unwrap_or("?"),
            member(vm, leaked, unsafe { &*def })?,
        ));
        def = unsafe { def.add(1) };
    }
    let mut def = unsafe { (*ty).getset };
    while !def.is_null() && !unsafe { (*def).name }.is_null() {
        if shadows_slot(vm, &class, text(unsafe { (*def).name }).unwrap_or("?")) {
            def = unsafe { def.add(1) };
            continue;
        }
        attributes.push((
            text(unsafe { (*def).name }).unwrap_or("?"),
            getset(vm, leaked, unsafe { &*def })?,
        ));
        def = unsafe { def.add(1) };
    }
    attributes.extend(slots(vm, ty, leaked));
    for (name, value) in attributes {
        let name = vm.ctx.intern_str(name);
        class.set_attr(name, value);
        if name.as_str().starts_with("__") && name.as_str().ends_with("__") {
            class.update_slot::<true>(name, &vm.ctx);
        }
    }
    if unsafe { (*ty).as_sequence.is_null() || (*(*ty).as_sequence).item.is_none() } {
        class.slots.as_sequence.item.store(None);
    }
    unsafe {
        if (*ty).bases.is_null() {
            let bases = class
                .bases
                .read()
                .iter()
                .map(|base| base.clone().into())
                .collect();
            (*ty).bases = new_ref(vm, vm.ctx.new_tuple(bases).as_object());
        }
        if (*ty).dict.is_null()
            && let Some(dict) = class.attributes.as_dict()
        {
            (*ty).dict = new_ref(vm, dict.as_object());
        }
    }
    intern_keys(vm, &class);
    Ok(class)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Ready(ty: *mut Type) -> c_int {
    ready_type(ty);
    if bridge::machine().is_none() {
        return 0;
    }
    bridge::guard(-1, |vm| c_type(vm, ty).map(|_| 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GenericAlloc(ty: *mut Type, items: isize) -> *mut Object {
    let size = unsafe {
        (*ty).basicsize.unsigned_abs() + items.unsigned_abs() * (*ty).itemsize.unsigned_abs()
    };
    let pointer = heap::allocate(size, true).cast::<Object>();
    if pointer.is_null() {
        return crate::api::PyErr_NoMemory();
    }
    unsafe {
        pointer.write(Object { refcnt: 1, ty });
        if (*ty).itemsize != 0 {
            (*pointer.cast::<VarObject>()).size = items;
        }
        if (*ty).flags & TPFLAGS_HEAPTYPE != 0 {
            incref(ty.cast());
        }
    }
    pointer
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GenericNew(
    ty: *mut Type,
    _args: *mut Object,
    _kwargs: *mut Object,
) -> *mut Object {
    unsafe {
        match (*ty).alloc {
            Some(alloc) => alloc(ty, 0),
            None => PyType_GenericAlloc(ty, 0),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_New(ty: *mut Type) -> *mut Object {
    let pointer = heap::allocate(unsafe { (*ty).basicsize.unsigned_abs() }, false).cast::<Object>();
    if pointer.is_null() {
        return crate::api::PyErr_NoMemory();
    }
    unsafe { PyObject_Init(pointer, ty) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_NewVar(ty: *mut Type, items: isize) -> *mut Object {
    let size = unsafe {
        (*ty).basicsize.unsigned_abs() + items.unsigned_abs() * (*ty).itemsize.unsigned_abs()
    };
    let pointer = heap::allocate(size, false).cast::<Object>();
    if pointer.is_null() {
        return crate::api::PyErr_NoMemory();
    }
    unsafe {
        (*pointer.cast::<VarObject>()).size = items;
        PyObject_Init(pointer, ty)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Init(pointer: *mut Object, ty: *mut Type) -> *mut Object {
    if pointer.is_null() {
        return pointer;
    }
    unsafe {
        (*pointer).refcnt = 1;
        (*pointer).ty = ty;
        if (*ty).flags & TPFLAGS_HEAPTYPE != 0 {
            incref(ty.cast());
        }
    }
    pointer
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_InitVar(
    pointer: *mut Object,
    ty: *mut Type,
    items: isize,
) -> *mut Object {
    if pointer.is_null() {
        return pointer;
    }
    unsafe {
        (*pointer.cast::<VarObject>()).size = items;
        PyObject_Init(pointer, ty)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_IsSubtype(a: *mut Type, b: *mut Type) -> c_int {
    if b == bridge::PyBaseObject_Type.get() {
        return 1;
    }
    let mut at = a;
    while !at.is_null() {
        if at == b {
            return 1;
        }
        at = unsafe { (*at).base_type };
    }
    if a.is_null() || unsafe { (*a).flags } & TPFLAGS_HEAPTYPE == 0 {
        return 0;
    }
    let Some(vm) = bridge::machine() else {
        return 0;
    };
    let (Ok(a), Ok(b)) = (arg(vm, a.cast()), arg(vm, b.cast())) else {
        return 0;
    };
    match (a.downcast_ref::<PyType>(), b.downcast_ref::<PyType>()) {
        (Some(a), Some(b)) => c_int::from(a.fast_issubclass(b)),
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetFlags(ty: *mut Type) -> std::ffi::c_ulong {
    unsafe { (*ty).flags }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Modified(ty: *mut Type) {
    if let Some(class) = bridge::static_type(ty) {
        class.modified();
        if let Some(vm) = bridge::machine() {
            intern_keys(vm, &class);
        }
    }
}

fn intern_keys(vm: &VirtualMachine, class: &PyType) {
    let Some(dict) = class.attributes.as_dict() else {
        return;
    };
    for key in dict.keys_vec() {
        if let Ok(key) = key.downcast_exact::<PyStr>(vm) {
            vm.ctx.intern_str(key);
        }
    }
}
