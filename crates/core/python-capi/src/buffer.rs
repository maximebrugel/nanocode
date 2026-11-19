use std::{
    ffi::c_int,
    ptr::null_mut,
    slice,
    sync::atomic::{AtomicPtr, Ordering},
};

use rustpython_vm::{
    PyObject, PyPayload, PyResult, VirtualMachine,
    common::borrow::{BorrowedValue, BorrowedValueMut},
    protocol::{BufferDescriptor, BufferFlags, BufferMethods, PyBuffer},
    pyclass,
};

use crate::{
    abi::*,
    bridge::{borrowed, take_error, xdecref},
};

#[pyclass(module = "_capi", name = "buffer")]
#[derive(Debug, PyPayload)]
pub struct CBuffer {
    view: AtomicPtr<Buffer>,
}

#[pyclass]
impl CBuffer {}

impl CBuffer {
    pub fn view(&self) -> *mut Buffer {
        self.view.load(Ordering::Relaxed)
    }
}

pub fn procs(ty: *mut Type) -> Option<&'static BufferProcs> {
    if ty.is_null() {
        return None;
    }
    let procs = unsafe { (*ty).as_buffer }.cast::<BufferProcs>();
    (!procs.is_null())
        .then(|| unsafe { &*procs })
        .filter(|procs| procs.getbuffer.is_some())
}

fn bytes(buffer: &PyBuffer) -> (*mut u8, usize) {
    let view = buffer.obj_as::<CBuffer>().view();
    if view.is_null() {
        return (null_mut(), 0);
    }
    unsafe { ((*view).buf.cast::<u8>(), (*view).len.unsigned_abs()) }
}

static METHODS: BufferMethods = BufferMethods {
    obj_bytes: |buffer| match bytes(buffer) {
        (data, length) if !data.is_null() => {
            BorrowedValue::Ref(unsafe { slice::from_raw_parts(data, length) })
        }
        _ => BorrowedValue::Ref(&[]),
    },
    obj_bytes_mut: |buffer| match bytes(buffer) {
        (data, length) if !data.is_null() => {
            BorrowedValueMut::RefMut(unsafe { slice::from_raw_parts_mut(data, length) })
        }
        _ => BorrowedValueMut::RefMut(&mut []),
    },
    release: |buffer| {
        let view = buffer
            .obj_as::<CBuffer>()
            .view
            .swap(null_mut(), Ordering::Relaxed);
        if view.is_null() {
            return;
        }
        let owner = unsafe { (*view).obj };
        if !owner.is_null()
            && let Some(release) = procs(unsafe { (*owner).ty }).and_then(|procs| procs.release)
        {
            unsafe { release(owner, view) };
        }
        unsafe {
            xdecref(owner);
            drop(Box::from_raw(view));
        }
    },
    retain: |_| {},
};

/// The exporter's shape, strides and format, so a memoryview of it looks as
/// it does in CPython; a view without them is one run of bytes.
fn describe(view: *mut Buffer, length: usize, readonly: bool) -> BufferDescriptor {
    let view = unsafe { &*view };
    let ndim = view.ndim.max(0) as usize;
    if view.itemsize <= 0 || view.shape.is_null() || ndim == 0 || !view.suboffsets.is_null() {
        return BufferDescriptor::simple(length, readonly);
    }
    let shape = unsafe { slice::from_raw_parts(view.shape, ndim) };
    let itemsize = view.itemsize as usize;
    let mut dim_desc = Vec::with_capacity(ndim);
    let mut stride = itemsize as isize;
    let mut contiguous = vec![0isize; ndim];
    for i in (0..ndim).rev() {
        contiguous[i] = stride;
        stride *= shape[i];
    }
    for i in 0..ndim {
        let step = if view.strides.is_null() {
            contiguous[i]
        } else {
            unsafe { *view.strides.add(i) }
        };
        if step < 0 {
            return BufferDescriptor::simple(length, readonly);
        }
        dim_desc.push((shape[i] as usize, step, 0));
    }
    let format = if view.format.is_null() {
        std::borrow::Cow::Borrowed("B")
    } else {
        std::borrow::Cow::Owned(
            unsafe { std::ffi::CStr::from_ptr(view.format) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    BufferDescriptor {
        len: length,
        offset: 0,
        readonly,
        itemsize,
        format,
        dim_desc,
    }
}

pub fn export(object: &PyObject, flags: BufferFlags, vm: &VirtualMachine) -> PyResult<PyBuffer> {
    let pointer = borrowed(vm, object);
    let Some(getbuffer) = procs(unsafe { (*pointer).ty }).and_then(|procs| procs.getbuffer) else {
        return Err(vm.new_type_error(format!(
            "a bytes-like object is required, not '{}'",
            object.class().name()
        )));
    };
    let view = Box::into_raw(Box::new(unsafe { std::mem::zeroed::<Buffer>() }));
    if unsafe { getbuffer(pointer, view, flags.bits() as c_int) } < 0 {
        unsafe { drop(Box::from_raw(view)) };
        return Err(take_error().unwrap_or_else(|| {
            vm.new_system_error("bf_getbuffer failed without an exception".to_owned())
        }));
    }
    let (length, readonly) = unsafe { ((*view).len.unsigned_abs(), (*view).readonly != 0) };
    let holder = CBuffer {
        view: AtomicPtr::new(view),
    }
    .into_pyobject(vm);
    Ok(PyBuffer::new(
        holder,
        describe(view, length, readonly),
        &METHODS,
    ))
}

/// A C view of a memoryview the runtime made, kept as long as the memoryview.
struct BuiltView {
    view: Buffer,
    _dims: Vec<isize>,
    _format: std::ffi::CString,
    _keep: rustpython_vm::PyObjectRef,
}

unsafe impl Send for BuiltView {}

static BUILT: std::sync::Mutex<Vec<(usize, Box<BuiltView>)>> = std::sync::Mutex::new(Vec::new());

/// What PyMemoryView_GET_BUFFER reads: memoryviews are runtime objects, not
/// CPython's struct, so numpy and others get the exporter's own C view when a
/// C type exported it unchanged, and otherwise a C view built from the
/// memoryview's shape and format (kept while the memoryview lives).
#[unsafe(no_mangle)]
pub extern "C" fn _PyMemoryView_GetBuffer(pointer: *mut Object) -> *mut Buffer {
    crate::bridge::guard(null_mut(), |vm| {
        let object = crate::bridge::arg(vm, pointer)?;
        let Some(memoryview) = object.downcast_ref::<rustpython_vm::builtins::PyMemoryView>()
        else {
            return Err(vm.new_type_error("expected a memoryview".to_owned()));
        };
        let buffer = memoryview.exporter_buffer();
        let desc = memoryview.view_desc();
        if let Some(holder) = buffer.obj.downcast_ref::<CBuffer>()
            && desc.len == buffer.desc.len
            && desc.offset == buffer.desc.offset
            && desc.format == buffer.desc.format
            && desc.dim_desc == buffer.desc.dim_desc
            && !holder.view().is_null()
        {
            return Ok(holder.view());
        }
        let key = pointer as usize;
        let mut built = BUILT.lock().unwrap();
        // C reads a view through the memoryview's C object, so a view is
        // dead once that object has no C references: drop those, so views
        // don't pile up for the run (the bridge parks a proxy whose object
        // this table still holds, which kept both forever). _keep holds the
        // object, so the bridge hasn't freed the C object and its count is
        // still readable.
        built.retain(|(key, _)| unsafe { (*(*key as *mut Object)).refcnt } != 0);
        if let Some((_, view)) = built.iter_mut().find(|(k, _)| *k == key) {
            return Ok(&raw mut view.view);
        }
        let base = buffer.obj_bytes().as_ptr() as isize + desc.offset;
        let ndim = desc.dim_desc.len();
        let mut dims: Vec<isize> = desc.dim_desc.iter().map(|d| d.0 as isize).collect();
        dims.extend(desc.dim_desc.iter().map(|d| d.1));
        let format = std::ffi::CString::new(desc.format.as_bytes()).unwrap_or_default();
        let mut entry = Box::new(BuiltView {
            view: unsafe { std::mem::zeroed() },
            _dims: dims,
            _format: format,
            _keep: object.clone(),
        });
        let dims_ptr = entry._dims.as_mut_ptr();
        entry.view = Buffer {
            buf: base as *mut std::ffi::c_void,
            obj: pointer,
            len: desc.len as isize,
            itemsize: desc.itemsize as isize,
            readonly: desc.readonly as c_int,
            ndim: ndim as c_int,
            format: entry._format.as_ptr().cast_mut(),
            shape: dims_ptr,
            strides: unsafe { dims_ptr.add(ndim) },
            suboffsets: null_mut(),
            internal: null_mut(),
        };
        built.push((key, entry));
        let view = &raw mut built.last_mut().unwrap().1.view;
        Ok(view)
    })
}
