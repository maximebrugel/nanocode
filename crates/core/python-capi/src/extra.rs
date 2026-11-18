use std::{
    ffi::{CStr, c_char, c_int, c_long, c_ulong, c_void},
    ptr::null_mut,
    sync::atomic::{AtomicU8, Ordering},
};

use nanocode_numeric::Complex64;
use rustpython_vm::{
    AsObject, PyObjectRef, PyPayload, VirtualMachine,
    builtins::{PyCapsule, PyDict, PySequenceIterator, PyStr},
    common::hash::hash_float,
};

use crate::{
    abi::*,
    bridge::{arg, guard, new_ref, object, status, text, truth},
    heap,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CComplex {
    real: f64,
    imag: f64,
}

fn complex(vm: &VirtualMachine, value: &PyObjectRef) -> rustpython_vm::PyResult<Complex64> {
    if let Some((value, _)) = value.try_complex(vm)? {
        return Ok(value);
    }
    Ok(Complex64::new(value.try_float(vm)?.to_f64(), 0.0))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_FromDoubles(real: f64, imag: f64) -> *mut Object {
    object(|vm| Ok(vm.ctx.new_complex(Complex64::new(real, imag)).into()))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_FromCComplex(value: CComplex) -> *mut Object {
    PyComplex_FromDoubles(value.real, value.imag)
}

#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_AsCComplex(pointer: *mut Object) -> CComplex {
    let failed = CComplex {
        real: -1.0,
        imag: 0.0,
    };
    guard(failed, |vm| {
        let value = complex(vm, &arg(vm, pointer)?)?;
        Ok(CComplex {
            real: value.re,
            imag: value.im,
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_RealAsDouble(pointer: *mut Object) -> f64 {
    guard(-1.0, |vm| Ok(complex(vm, &arg(vm, pointer)?)?.re))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_ImagAsDouble(pointer: *mut Object) -> f64 {
    guard(-1.0, |vm| Ok(complex(vm, &arg(vm, pointer)?)?.im))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyCapsule_SetName(pointer: *mut Object, name: *const c_char) -> c_int {
    status(|vm| {
        let capsule = arg(vm, pointer)?;
        let capsule = capsule.downcast_ref::<PyCapsule>().ok_or_else(|| {
            vm.new_value_error("PyCapsule_SetName called with invalid PyCapsule object".to_owned())
        })?;
        capsule.set_name((!name.is_null()).then(|| unsafe { CStr::from_ptr(name) }));
        Ok(())
    })
}

fn contextvars(vm: &VirtualMachine) -> rustpython_vm::PyResult<PyObjectRef> {
    vm.import("_contextvars", 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn PyContextVar_New(name: *const c_char, default: *mut Object) -> *mut Object {
    object(|vm| {
        let class = contextvars(vm)?.get_attr("ContextVar", vm)?;
        let name = vm.ctx.new_str(text(name).unwrap_or_default());
        if default.is_null() {
            class.call((name,), vm)
        } else {
            let kwargs = vec![("default".to_owned(), arg(vm, default)?)];
            class.call(
                rustpython_vm::function::FuncArgs::new(
                    vec![name.into()],
                    rustpython_vm::function::KwArgs::new(
                        kwargs
                            .into_iter()
                            .map(|(name, value)| (name.into(), value))
                            .collect(),
                    ),
                ),
                vm,
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_Get(
    var: *mut Object,
    default: *mut Object,
    value: *mut *mut Object,
) -> c_int {
    unsafe { *value = null_mut() };
    guard(-1, |vm| {
        let var = arg(vm, var)?;
        let found = if default.is_null() {
            match vm.call_method(&var, "get", ()) {
                Ok(found) => Some(found),
                Err(error) if error.fast_isinstance(vm.ctx.exceptions.lookup_error) => None,
                Err(error) => return Err(error),
            }
        } else {
            Some(vm.call_method(&var, "get", (arg(vm, default)?,))?)
        };
        if let Some(found) = found {
            unsafe { *value = new_ref(vm, &found) };
        }
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyContextVar_Set(var: *mut Object, value: *mut Object) -> *mut Object {
    object(|vm| vm.call_method(&*arg(vm, var)?, "set", (arg(vm, value)?,)))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyDictProxy_New(mapping: *mut Object) -> *mut Object {
    object(|vm| {
        vm.ctx
            .types
            .mappingproxy_type
            .as_object()
            .call((arg(vm, mapping)?,), vm)
    })
}

fn dict(
    vm: &VirtualMachine,
    pointer: *mut Object,
) -> rustpython_vm::PyResult<rustpython_vm::PyRef<PyDict>> {
    arg(vm, pointer)?
        .downcast::<PyDict>()
        .map_err(|_| vm.new_system_error("bad argument to internal function".to_owned()))
}

fn key(vm: &VirtualMachine, name: *const c_char) -> rustpython_vm::PyResult<PyObjectRef> {
    let name = unsafe { CStr::from_ptr(name) }
        .to_str()
        .map_err(|_| vm.new_value_error("key is not UTF-8".to_owned()))?;
    Ok(vm.ctx.new_str(name).into())
}

#[unsafe(no_mangle)]
pub extern "C" fn PyDict_ContainsString(pointer: *mut Object, name: *const c_char) -> c_int {
    truth(|vm| {
        let key = key(vm, name)?;
        Ok(dict(vm, pointer)?.contains_key(&*key, vm))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItemStringRef(
    pointer: *mut Object,
    name: *const c_char,
    result: *mut *mut Object,
) -> c_int {
    unsafe { *result = null_mut() };
    guard(-1, |vm| {
        let found = dict(vm, pointer)?.get_item_opt(&*key(vm, name)?, vm)?;
        Ok(match found {
            Some(value) => {
                unsafe { *result = new_ref(vm, &value) };
                1
            }
            None => 0,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetDefaultRef(
    pointer: *mut Object,
    key: *mut Object,
    default: *mut Object,
    result: *mut *mut Object,
) -> c_int {
    if !result.is_null() {
        unsafe { *result = null_mut() };
    }
    guard(-1, |vm| {
        let target = dict(vm, pointer)?;
        let key = arg(vm, key)?;
        let (value, present) = match target.get_item_opt(&*key, vm)? {
            Some(value) => (value, 1),
            None => {
                let value = arg(vm, default)?;
                target.set_item(&*key, value.clone(), vm)?;
                crate::api::dict_changed(pointer, &target);
                (value, 0)
            }
        };
        if !result.is_null() {
            unsafe { *result = new_ref(vm, &value) };
        }
        Ok(present)
    })
}

#[cfg(target_family = "wasm")]
fn errno() -> c_int {
    unsafe extern "C" {
        fn __errno_location() -> *mut c_int;
    }
    unsafe { *__errno_location() }
}

#[cfg(not(target_family = "wasm"))]
fn errno() -> c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn PyErr_SetFromErrno(ty: *mut Object) -> *mut Object {
    let code = errno();
    guard(null_mut(), |vm| {
        let ty = arg(vm, ty)?;
        let message = std::io::Error::from_raw_os_error(code).to_string();
        let message = message
            .split(" (os error")
            .next()
            .unwrap_or(&message)
            .to_owned();
        let error = ty.call((code, message), vm)?;
        let error = error
            .downcast::<rustpython_vm::builtins::PyBaseException>()
            .map_err(|_| {
                vm.new_type_error("exceptions must derive from BaseException".to_owned())
            })?;
        Err(error)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyInterpreterState_Main() -> *mut c_void {
    crate::api::PyInterpreterState_Get()
}

#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromVoidPtr(pointer: *mut c_void) -> *mut Object {
    object(|vm| Ok(vm.ctx.new_int(pointer as usize).into()))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyLong_AsVoidPtr(pointer: *mut Object) -> *mut c_void {
    guard(null_mut(), |vm| {
        let value = arg(vm, pointer)?.try_int(vm)?;
        let value = value.as_bigint();
        let address = if let Ok(address) = usize::try_from(value) {
            address
        } else {
            isize::try_from(value).map_err(|_| {
                vm.new_overflow_error("Python int too large to convert to C pointer".to_owned())
            })? as usize
        };
        Ok(address as *mut c_void)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_Lock(mutex: *mut u8) {
    let mutex = unsafe { AtomicU8::from_ptr(mutex) };
    while mutex
        .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        std::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_Unlock(mutex: *mut u8) {
    unsafe { AtomicU8::from_ptr(mutex) }.store(0, Ordering::Release);
}

macro_rules! unary_numbers {
    ($($name:ident => $method:ident),* $(,)?) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $name(a: *mut Object) -> *mut Object {
                object(|vm| vm.$method(&*arg(vm, a)?))
            }
        )*
    };
}

unary_numbers! {
    PyNumber_Absolute => _abs,
    PyNumber_Invert => _invert,
    PyNumber_Negative => _neg,
    PyNumber_Positive => _pos,
}

#[unsafe(no_mangle)]
pub extern "C" fn PyNumber_Divmod(a: *mut Object, b: *mut Object) -> *mut Object {
    object(|vm| vm._divmod(&*arg(vm, a)?, &*arg(vm, b)?))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyNumber_Power(a: *mut Object, b: *mut Object, c: *mut Object) -> *mut Object {
    object(|vm| vm._pow(&*arg(vm, a)?, &*arg(vm, b)?, &*arg(vm, c)?))
}

#[unsafe(no_mangle)]
pub extern "C" fn PySequence_Concat(a: *mut Object, b: *mut Object) -> *mut Object {
    object(|vm| vm._add(&*arg(vm, a)?, &*arg(vm, b)?))
}

#[unsafe(no_mangle)]
pub extern "C" fn PySequence_InPlaceConcat(a: *mut Object, b: *mut Object) -> *mut Object {
    object(|vm| vm._iadd(&*arg(vm, a)?, &*arg(vm, b)?))
}

#[unsafe(no_mangle)]
pub extern "C" fn PySequence_InPlaceRepeat(a: *mut Object, count: isize) -> *mut Object {
    object(|vm| vm._imul(&*arg(vm, a)?, vm.ctx.new_int(count).as_object()))
}

#[unsafe(no_mangle)]
pub extern "C" fn PySeqIter_New(sequence: *mut Object) -> *mut Object {
    object(|vm| {
        Ok(PySequenceIterator::new(arg(vm, sequence)?, vm)?
            .into_ref(&vm.ctx)
            .into())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyObject_LengthHint(pointer: *mut Object, default: isize) -> isize {
    guard(-1, |vm| {
        let hint = vm
            .import("operator", 0)?
            .get_attr("length_hint", vm)?
            .call((arg(vm, pointer)?, default), vm)?;
        hint.try_int(vm)?.try_to_primitive::<isize>(vm)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyObject_AsFileDescriptor(pointer: *mut Object) -> c_int {
    guard(-1, |vm| {
        let value = arg(vm, pointer)?;
        let fd = if value.fast_isinstance(vm.ctx.types.int_type) {
            value
        } else {
            vm.call_method(&value, "fileno", ()).map_err(|_| {
                vm.new_type_error("argument must be an int, or have a fileno() method.".to_owned())
            })?
        };
        let fd = fd.try_int(vm)?.try_to_primitive::<c_int>(vm)?;
        if fd < 0 {
            return Err(vm.new_value_error(format!(
                "file descriptor cannot be a negative integer ({fd})"
            )));
        }
        Ok(fd)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyObject_Print(pointer: *mut Object, file: *mut c_void, flags: c_int) -> c_int {
    unsafe extern "C" {
        fn fwrite(data: *const c_void, size: usize, count: usize, file: *mut c_void) -> usize;
    }
    status(|vm| {
        let text = if pointer.is_null() {
            "<nil>".to_owned()
        } else {
            let value = arg(vm, pointer)?;
            if flags & 1 != 0 {
                value.str(vm)?
            } else {
                value.repr(vm)?
            }
            .to_string_lossy()
            .into_owned()
        };
        unsafe { fwrite(text.as_ptr().cast(), 1, text.len(), file) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyTraceMalloc_Track(_domain: u32, _pointer: usize, _size: usize) -> c_int {
    -2
}

#[unsafe(no_mangle)]
pub extern "C" fn PyTraceMalloc_Untrack(_domain: u32, _pointer: usize) -> c_int {
    -2
}

fn string(
    vm: &VirtualMachine,
    pointer: *mut Object,
) -> rustpython_vm::PyResult<rustpython_vm::PyRef<PyStr>> {
    arg(vm, pointer)?
        .downcast::<PyStr>()
        .map_err(|_| vm.new_type_error("bad argument type for built-in operation".to_owned()))
}

#[unsafe(no_mangle)]
pub extern "C" fn PyUnicode_AsLatin1String(pointer: *mut Object) -> *mut Object {
    object(|vm| vm.call_method(&*arg(vm, pointer)?, "encode", ("latin-1",)))
}

fn code_points(vm: &VirtualMachine, pointer: *mut Object) -> rustpython_vm::PyResult<Vec<u32>> {
    Ok(string(vm, pointer)?
        .as_wtf8()
        .code_points()
        .map(|point| point.to_u32())
        .collect())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUCS4(
    pointer: *mut Object,
    buffer: *mut u32,
    length: isize,
    copy_null: c_int,
) -> *mut u32 {
    guard(null_mut(), |vm| {
        let points = code_points(vm, pointer)?;
        let needed = points.len() + usize::from(copy_null != 0);
        if usize::try_from(length).map_or(true, |length| length < needed) {
            return Err(vm.new_system_error("string is longer than the buffer".to_owned()));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(points.as_ptr(), buffer, points.len());
            if copy_null != 0 {
                *buffer.add(points.len()) = 0;
            }
        }
        Ok(buffer)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyUnicode_AsUCS4Copy(pointer: *mut Object) -> *mut u32 {
    guard(null_mut(), |vm| {
        let points = code_points(vm, pointer)?;
        let buffer = heap::allocate((points.len() + 1) * 4, false).cast::<u32>();
        if buffer.is_null() {
            return Err(vm.new_memory_error(String::new()));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(points.as_ptr(), buffer, points.len());
            *buffer.add(points.len()) = 0;
        }
        Ok(buffer)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyUnicode_FromEncodedObject(
    pointer: *mut Object,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut Object {
    object(|vm| {
        let value = arg(vm, pointer)?;
        if value.fast_isinstance(vm.ctx.types.str_type) {
            return Err(vm.new_type_error("decoding str is not supported".to_owned()));
        }
        vm.call_method(
            &value,
            "decode",
            (
                text(encoding).unwrap_or("utf-8"),
                text(errors).unwrap_or("strict"),
            ),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyUnicode_Replace(
    pointer: *mut Object,
    old: *mut Object,
    new: *mut Object,
    count: isize,
) -> *mut Object {
    object(|vm| {
        vm.call_method(
            &*arg(vm, pointer)?,
            "replace",
            (arg(vm, old)?, arg(vm, new)?, count),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyUnicode_Tailmatch(
    pointer: *mut Object,
    substring: *mut Object,
    start: isize,
    end: isize,
    direction: c_int,
) -> isize {
    guard(-1, |vm| {
        let method = if direction > 0 {
            "endswith"
        } else {
            "startswith"
        };
        let found = vm.call_method(
            &*arg(vm, pointer)?,
            method,
            (arg(vm, substring)?, start, end),
        )?;
        Ok(isize::from(found.try_to_bool(vm)?))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyLong_Sign(pointer: *mut Object) -> c_int {
    guard(0, |vm| {
        let value = arg(vm, pointer)?.try_int(vm)?;
        Ok(match value.as_bigint().sign() {
            nanocode_numeric::Sign::Minus => -1,
            nanocode_numeric::Sign::NoSign => 0,
            nanocode_numeric::Sign::Plus => 1,
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsWhitespace(point: u32) -> c_int {
    c_int::from(
        matches!(point, 0x1c..=0x1f) || char::from_u32(point).is_some_and(char::is_whitespace),
    )
}

const fn ascii_whitespace() -> [u8; 128] {
    let mut table = [0; 128];
    let mut at = 0;
    let spaces = [0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x1c, 0x1d, 0x1e, 0x1f, 0x20];
    while at < spaces.len() {
        table[spaces[at]] = 1;
        at += 1;
    }
    table
}

#[unsafe(no_mangle)]
pub static _Py_ascii_whitespace: [u8; 128] = ascii_whitespace();

#[unsafe(no_mangle)]
pub extern "C" fn _Py_HashDouble(pointer: *mut Object, value: f64) -> isize {
    hash_float(value).map_or_else(
        || {
            let address = pointer as usize;
            (address >> 4 | address << (usize::BITS - 4)) as isize
        },
        |hash| hash as isize,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_string_to_double(
    start: *const c_char,
    end: *mut *mut c_char,
    overflow: *mut Object,
) -> f64 {
    let bytes = unsafe { CStr::from_ptr(start) }.to_bytes();
    let (value, used) = float_prefix(bytes);
    guard(-1.0, |vm| {
        if used == 0 || (end.is_null() && used != bytes.len()) {
            if !end.is_null() {
                unsafe { *end = start.cast_mut() };
            }
            return Err(vm.new_value_error(format!(
                "could not convert string to float: '{}'",
                String::from_utf8_lossy(bytes)
            )));
        }
        if !end.is_null() {
            unsafe { *end = start.add(used).cast_mut() };
        }
        if value.is_infinite() && !overflow.is_null() {
            let text = String::from_utf8_lossy(&bytes[..used]).to_ascii_lowercase();
            if !text.contains("inf") {
                let ty = arg(vm, overflow)?;
                let error = ty.call(
                    (format!("value too large to convert to float: '{text}'"),),
                    vm,
                )?;
                return Err(error
                    .downcast::<rustpython_vm::builtins::PyBaseException>()
                    .map_err(|_| vm.new_type_error("bad exception type".to_owned()))?);
            }
        }
        Ok(value)
    })
}

fn integer_prefix(text: &[u8], base: c_int) -> (bool, u128, usize, bool) {
    let mut at = 0;
    while at < text.len() && text[at].is_ascii_whitespace() {
        at += 1;
    }
    let negative = text.get(at) == Some(&b'-');
    if matches!(text.get(at), Some(b'-' | b'+')) {
        at += 1;
    }
    let mut base = base as u32;
    let prefixed = text.get(at) == Some(&b'0')
        && matches!(
            text.get(at + 1),
            Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
        );
    if prefixed {
        let radix = match text[at + 1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            _ => 2,
        };
        if base == 0 || base == radix {
            base = radix;
            at += 2;
        }
    } else if base == 0 {
        base = if text.get(at) == Some(&b'0') { 8 } else { 10 };
    }
    let begin = at;
    let mut value: u128 = 0;
    let mut overflow = false;
    while let Some(digit) = text.get(at).and_then(|&byte| (byte as char).to_digit(base)) {
        match value
            .checked_mul(u128::from(base))
            .and_then(|value| value.checked_add(u128::from(digit)))
        {
            Some(next) => value = next,
            None => overflow = true,
        }
        at += 1;
    }
    if at == begin {
        return (false, 0, 0, false);
    }
    (negative, value, at, overflow)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_strtoul(
    start: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_ulong {
    let bytes = unsafe { CStr::from_ptr(start) }.to_bytes();
    let (negative, value, used, overflow) = integer_prefix(bytes, base);
    if !end.is_null() {
        unsafe { *end = start.add(used).cast_mut() };
    }
    if overflow || value > u128::from(c_ulong::MAX) {
        return c_ulong::MAX;
    }
    let value = value as c_ulong;
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_strtol(
    start: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_long {
    let bytes = unsafe { CStr::from_ptr(start) }.to_bytes();
    let (negative, value, used, overflow) = integer_prefix(bytes, base);
    if !end.is_null() {
        unsafe { *end = start.add(used).cast_mut() };
    }
    let limit = if negative {
        c_long::MAX as u128 + 1
    } else {
        c_long::MAX as u128
    };
    if overflow || value > limit {
        return if negative { c_long::MIN } else { c_long::MAX };
    }
    if negative {
        (value as i128).wrapping_neg() as c_long
    } else {
        value as c_long
    }
}

fn float_prefix(text: &[u8]) -> (f64, usize) {
    let count = |at: usize| {
        text[at.min(text.len())..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let mut at = usize::from(matches!(text.first(), Some(b'+' | b'-')));
    let sign = if text.first() == Some(&b'-') {
        -1.0
    } else {
        1.0
    };
    let rest = text[at..].to_ascii_lowercase();
    for (name, value) in [
        ("infinity", f64::INFINITY),
        ("inf", f64::INFINITY),
        ("nan", f64::NAN),
    ] {
        if rest.starts_with(name.as_bytes()) {
            return (sign * value, at + name.len());
        }
    }
    let whole = count(at);
    at += whole;
    let mut fraction = 0;
    if text.get(at) == Some(&b'.') {
        fraction = count(at + 1);
        if whole + fraction > 0 {
            at += 1 + fraction;
        }
    }
    if whole + fraction == 0 {
        return (0.0, 0);
    }
    if matches!(text.get(at), Some(b'e' | b'E')) {
        let signed = usize::from(matches!(text.get(at + 1), Some(b'+' | b'-')));
        let digits = count(at + 1 + signed);
        if digits > 0 {
            at += 1 + signed + digits;
        }
    }
    let value = std::str::from_utf8(&text[..at])
        .ok()
        .and_then(|number| number.parse::<f64>().ok())
        .unwrap_or(0.0);
    (value, at)
}

#[unsafe(no_mangle)]
pub extern "C" fn PyFloat_FromString(pointer: *mut Object) -> *mut Object {
    object(|vm| {
        vm.ctx
            .types
            .float_type
            .as_object()
            .call((arg(vm, pointer)?,), vm)
    })
}

fn character(point: u32) -> Option<char> {
    char::from_u32(point)
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsAlpha(point: u32) -> c_int {
    c_int::from(character(point).is_some_and(char::is_alphabetic))
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsLowercase(point: u32) -> c_int {
    c_int::from(character(point).is_some_and(char::is_lowercase))
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsUppercase(point: u32) -> c_int {
    c_int::from(character(point).is_some_and(char::is_uppercase))
}

fn string_test(point: u32, method: &str) -> c_int {
    let Some(value) = character(point) else {
        return 0;
    };
    guard(0, |vm| {
        let found = vm.call_method(vm.ctx.new_str(value.to_string()).as_object(), method, ())?;
        Ok(c_int::from(found.try_to_bool(vm)?))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsTitlecase(point: u32) -> c_int {
    string_test(point, "istitle")
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsDecimalDigit(point: u32) -> c_int {
    string_test(point, "isdecimal")
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsDigit(point: u32) -> c_int {
    string_test(point, "isdigit")
}

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsNumeric(point: u32) -> c_int {
    string_test(point, "isnumeric")
}

pub unsafe extern "C" fn forward_iter(pointer: *mut Object) -> *mut Object {
    crate::api::PyObject_GetIter(pointer)
}

pub unsafe extern "C" fn forward_iternext(pointer: *mut Object) -> *mut Object {
    crate::api::PyIter_Next(pointer)
}

pub unsafe extern "C" fn forward_bool(pointer: *mut Object) -> std::ffi::c_int {
    crate::api::PyObject_IsTrue(pointer)
}

pub unsafe extern "C" fn forward_int(pointer: *mut Object) -> *mut Object {
    crate::api::PyNumber_Long(pointer)
}

pub unsafe extern "C" fn forward_float(pointer: *mut Object) -> *mut Object {
    crate::api::PyNumber_Float(pointer)
}

pub unsafe extern "C" fn forward_index(pointer: *mut Object) -> *mut Object {
    crate::api::PyNumber_Index(pointer)
}

fn construct(
    pick: fn(&VirtualMachine) -> &'static rustpython_vm::Py<rustpython_vm::builtins::PyType>,
    subtype: *mut Type,
    args: *mut Object,
    kwargs: *mut Object,
) -> *mut Object {
    object(|vm| {
        let class = pick(vm);
        let subtype = crate::types::c_type(vm, subtype)?;
        let mut positional = vec![subtype.into()];
        if !args.is_null() {
            positional.extend(
                arg(vm, args)?
                    .downcast::<rustpython_vm::builtins::PyTuple>()
                    .map_err(|_| vm.new_type_error("tp_new arguments must be a tuple".to_owned()))?
                    .iter()
                    .cloned(),
            );
        }
        let mut keywords = rustpython_vm::function::KwArgs::default();
        if !kwargs.is_null() {
            let kwargs = arg(vm, kwargs)?
                .downcast::<PyDict>()
                .map_err(|_| vm.new_type_error("tp_new keywords must be a dict".to_owned()))?;
            keywords = kwargs
                .into_iter()
                .map(|(key, value)| Ok((key.str(vm)?.to_string(), value)))
                .collect::<rustpython_vm::PyResult<Vec<_>>>()?
                .into_iter()
                .collect();
        }
        let new = class.as_object().get_attr("__new__", vm)?;
        new.call(
            rustpython_vm::function::FuncArgs::new(positional, keywords),
            vm,
        )
    })
}

macro_rules! constructors {
    ($($name:ident => $field:ident),* $(,)?) => {
        $(
            pub unsafe extern "C" fn $name(
                subtype: *mut Type,
                args: *mut Object,
                kwargs: *mut Object,
            ) -> *mut Object {
                construct(|vm| vm.ctx.types.$field, subtype, args, kwargs)
            }
        )*
    };
}

constructors! {
    new_str => str_type,
    new_bytes => bytes_type,
    new_float => float_type,
    new_int => int_type,
    new_complex => complex_type,
    new_tuple => tuple_type,
}

fn unbound(
    pick: fn(&VirtualMachine) -> &'static rustpython_vm::Py<rustpython_vm::builtins::PyType>,
    name: &'static str,
    pointer: *mut Object,
) -> *mut Object {
    object(|vm| {
        let method = pick(vm).as_object().get_attr(name, vm)?;
        method.call((arg(vm, pointer)?,), vm)
    })
}

macro_rules! unbound_slots {
    ($($name:ident => $field:ident . $method:literal),* $(,)?) => {
        $(
            pub unsafe extern "C" fn $name(pointer: *mut Object) -> *mut Object {
                unbound(|vm| vm.ctx.types.$field, $method, pointer)
            }
        )*
    };
}

unbound_slots! {
    repr_str => str_type."__repr__",
    str_str => str_type."__str__",
    repr_bytes => bytes_type."__repr__",
    str_bytes => bytes_type."__str__",
}

pub unsafe extern "C" fn hash_identity(pointer: *mut Object) -> isize {
    let address = pointer as usize;
    (address >> 4 | address << (usize::BITS - 4)) as isize
}

pub unsafe extern "C" fn dealloc_free(pointer: *mut Object) {
    unsafe {
        let ty = (*pointer).ty;
        match (*ty).free {
            Some(free) => free(pointer.cast()),
            None => crate::heap::PyObject_Free(pointer.cast()),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyStaticMethod_New(callable: *mut Object) -> *mut Object {
    object(|vm| {
        vm.ctx
            .types
            .staticmethod_type
            .as_object()
            .call((arg(vm, callable)?,), vm)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn PyClassMethod_New(callable: *mut Object) -> *mut Object {
    object(|vm| {
        vm.ctx
            .types
            .classmethod_type
            .as_object()
            .call((arg(vm, callable)?,), vm)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_double_to_string(
    value: f64,
    code: c_char,
    precision: c_int,
    flags: c_int,
    kind: *mut c_int,
) -> *mut c_char {
    let value = if flags & 0x08 != 0 && value == 0.0 {
        0.0
    } else {
        value
    };
    if !kind.is_null() {
        let tag = if value.is_nan() {
            2
        } else if value.is_infinite() {
            1
        } else {
            0
        };
        unsafe { *kind = tag };
    }
    guard(null_mut(), |vm| {
        let code = code as u8;
        let mut text = if code == b'r' {
            vm.ctx.new_float(value).as_object().repr(vm)?.to_string_lossy().into_owned()
        } else {
            let mut spec = String::new();
            if flags & 0x01 != 0 {
                spec.push('+');
            }
            if flags & 0x04 != 0 {
                spec.push('#');
            }
            spec.push('.');
            spec.push_str(&precision.to_string());
            spec.push(char::from(code));
            vm.format(vm.ctx.new_float(value).as_object(), vm.ctx.new_str(spec))?
                .to_string_lossy()
                .into_owned()
        };
        if code == b'r' && flags & 0x01 != 0 && !text.starts_with('-') {
            text.insert(0, '+');
        }
        if flags & 0x02 != 0
            && value.is_finite()
            && text.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'+')
        {
            text.push_str(".0");
        }
        let buffer = heap::PyMem_Malloc(text.len() + 1).cast::<u8>();
        if buffer.is_null() {
            return Err(vm.new_memory_error(String::new()));
        }
        unsafe {
            buffer.copy_from_nonoverlapping(text.as_ptr(), text.len());
            *buffer.add(text.len()) = 0;
        }
        Ok(buffer.cast())
    })
}
