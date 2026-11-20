/*
 * Builtin set type with a sequence of unique items.
 */
use super::{
    IterStatus, PositionIterInternal, PyDict, PyDictRef, PyGenericAlias, PyInt, PyStr, PyTupleRef,
    PyType, PyTypeRef, builtins_iter, locked_step,
};
use crate::{
    AsObject, Context, Py, PyObject, PyObjectRef, PyPayload, PyRef, PyResult, TryFromObject,
    atomic_func,
    class::{PyClassDef, PyClassImpl},
    common::{
        ascii,
        hash::PyHash,
        lock::{LazyLock, PyMutex, PyRwLock, PyRwLockReadGuard, PyRwLockWriteGuard},
        rc::PyRc,
        wtf8::Wtf8Buf,
    },
    convert::ToPyResult,
    function::{
        ArgIterable, FuncArgs, NameOthers, OptionalArg, PosArgs, PyArithmeticValue,
        PyComparisonValue,
    },
    protocol::{PyIterReturn, PyNumberMethods, PySequenceMethods},
    recursion::ReprGuard,
    types::AsNumber,
    types::{
        AsSequence, Comparable, Constructor, DefaultConstructor, Hashable, Initializer, IterNext,
        Iterable, PyComparisonOp, Representable, SelfIter,
    },
    utils::collection_repr,
    vm::VirtualMachine,
};
use core::{borrow::Borrow, fmt};
use rustpython_common::{
    atomic::{Ordering, PyAtomic, Radium},
    hash,
};

// The set's storage is a port of CPython 3.12's setobject.c: an
// open-addressing table of (hash, key) slots with dummies, probed linearly and
// then by perturbation, so sets iterate, pop and combine in CPython's order.
const SET_MINSIZE: usize = 8;
const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;
/// The hash a dummy slot keeps, which no live key has.
const DUMMY_HASH: PyHash = -1;

/// One slot: active with a key, empty (`hash == 0`) or a dummy
/// (`hash == DUMMY_HASH`) left by a removal.
#[derive(Clone)]
struct SetSlot {
    hash: PyHash,
    key: Option<PyObjectRef>,
}

impl SetSlot {
    const EMPTY: Self = Self { hash: 0, key: None };

    #[inline]
    fn is_dummy(&self) -> bool {
        self.key.is_none() && self.hash == DUMMY_HASH
    }
}

struct SetTableInner {
    /// `mask + 1` slots: a power of two, at least `SET_MINSIZE`.
    table: Vec<SetSlot>,
    /// Active and dummy slots.
    fill: usize,
    /// Active slots.
    used: usize,
    /// Where `pop` starts looking.
    finger: usize,
    /// Changes whenever `table` is rebuilt, as CPython's `so->table` pointer
    /// does: a probe interrupted by `__eq__` restarts when it moved.
    generation: u64,
}

impl Default for SetTableInner {
    fn default() -> Self {
        Self {
            table: vec![SetSlot::EMPTY; SET_MINSIZE],
            fill: 0,
            used: 0,
            finger: 0,
            generation: 0,
        }
    }
}

unsafe impl crate::object::Traverse for SetTableInner {
    fn traverse(&self, tracer_fn: &mut crate::object::TraverseFn<'_>) {
        for slot in &self.table {
            if let Some(key) = &slot.key {
                key.traverse(tracer_fn);
            }
        }
    }
}

/// The probe sequence of `set_lookkey` and `set_add_entry`: each bucket and,
/// when they fit, its `LINEAR_PROBES` neighbours, then `i = i*5 + 1 + perturb`.
#[derive(Clone, Copy)]
struct SetProbe {
    i: usize,
    j: usize,
    left: usize,
    perturb: u64,
}

impl SetProbe {
    #[inline]
    const fn run_len(i: usize, mask: usize) -> usize {
        if i + LINEAR_PROBES <= mask {
            LINEAR_PROBES
        } else {
            0
        }
    }

    #[inline]
    const fn new(hash: PyHash, mask: usize) -> Self {
        let i = (hash as u64 & mask as u64) as usize;
        Self {
            i,
            j: i,
            left: Self::run_len(i, mask),
            perturb: hash as u64,
        }
    }

    #[inline]
    const fn advance(&mut self, mask: usize) {
        if self.left > 0 {
            self.left -= 1;
            self.j += 1;
        } else {
            self.perturb >>= PERTURB_SHIFT;
            self.i = ((self.i as u64)
                .wrapping_mul(5)
                .wrapping_add(1)
                .wrapping_add(self.perturb)
                & mask as u64) as usize;
            self.j = self.i;
            self.left = Self::run_len(self.i, mask);
        }
    }
}

enum SetStep {
    /// The key is in this active slot.
    Found(usize),
    /// The probe reached this empty slot without finding the key.
    Missing(usize),
    /// This slot's key has the same hash and needs a Python `__eq__`.
    Compare(usize, PyObjectRef),
}

/// The set changed size under an iterator.
struct SetChanged;

/// Equality that runs no Python, so it may run under the table's lock: exact
/// `int`s and exact `str`s. `None` asks for a real `__eq__`.
#[inline]
fn set_fast_eq(stored: &PyObject, key: &PyObject, vm: &VirtualMachine) -> Option<bool> {
    let class = stored.class();
    if !class.is(key.class()) {
        return None;
    }
    if class.is(vm.ctx.types.int_type) {
        // SAFETY: both are exactly `int`.
        let (a, b) = unsafe {
            (
                stored.downcast_unchecked_ref::<PyInt>(),
                key.downcast_unchecked_ref::<PyInt>(),
            )
        };
        return Some(a.as_bigint() == b.as_bigint());
    }
    if class.is(vm.ctx.types.str_type) {
        // SAFETY: both are exactly `str`.
        let (a, b) = unsafe {
            (
                stored.downcast_unchecked_ref::<PyStr>(),
                key.downcast_unchecked_ref::<PyStr>(),
            )
        };
        return Some(a.as_wtf8() == b.as_wtf8());
    }
    None
}

/// `set_insert_clean`: place a key known to be absent into a table without dummies.
fn set_insert_clean(table: &mut [SetSlot], mask: usize, key: PyObjectRef, hash: PyHash) {
    let mut probe = SetProbe::new(hash, mask);
    loop {
        let slot = &mut table[probe.j];
        if slot.key.is_none() {
            *slot = SetSlot {
                hash,
                key: Some(key),
            };
            return;
        }
        probe.advance(mask);
    }
}

impl SetTableInner {
    #[inline]
    fn mask(&self) -> usize {
        self.table.len() - 1
    }

    /// Walk the probe sequence until the key turns up, an empty slot ends it,
    /// or an entry needs a Python `__eq__`. With `freeslot`, remember the last
    /// dummy passed, as `set_add_entry` does.
    #[inline]
    fn probe(
        &self,
        key: &PyObject,
        hash: PyHash,
        probe: &mut SetProbe,
        mut freeslot: Option<&mut Option<usize>>,
        vm: &VirtualMachine,
    ) -> SetStep {
        let mask = self.mask();
        loop {
            let slot = &self.table[probe.j];
            match &slot.key {
                None => {
                    if slot.hash != DUMMY_HASH {
                        return SetStep::Missing(probe.j);
                    }
                    if let Some(free) = freeslot.as_deref_mut() {
                        *free = Some(probe.j);
                    }
                }
                Some(stored) if slot.hash == hash => {
                    if stored.is(key) {
                        return SetStep::Found(probe.j);
                    }
                    match set_fast_eq(stored, key, vm) {
                        Some(true) => return SetStep::Found(probe.j),
                        Some(false) => {}
                        None => return SetStep::Compare(probe.j, stored.clone()),
                    }
                }
                Some(_) => {}
            }
            probe.advance(mask);
        }
    }

    /// Whether slot `j` of the same table still holds `startkey` after an
    /// `__eq__` ran unlocked.
    fn still_holds(&self, generation: u64, j: usize, startkey: &PyObject) -> bool {
        self.generation == generation
            && self
                .table
                .get(j)
                .and_then(|slot| slot.key.as_ref())
                .is_some_and(|key| key.is(startkey))
    }

    /// `set_table_resize`: rebuild into the smallest table larger than `minused`.
    fn resize(&mut self, minused: usize) {
        let mut newsize = SET_MINSIZE;
        while newsize <= minused {
            newsize <<= 1;
        }
        if newsize == SET_MINSIZE && self.table.len() == SET_MINSIZE && self.fill == self.used {
            // No dummies, so no point doing anything.
            return;
        }
        let old_len = self.table.len();
        let old = core::mem::replace(&mut self.table, vec![SetSlot::EMPTY; newsize]);
        let newmask = newsize - 1;
        for slot in old {
            if let Some(key) = slot.key {
                set_insert_clean(&mut self.table, newmask, key, slot.hash);
            }
        }
        self.fill = self.used;
        // A small table rebuilt in place keeps CPython's smalltable pointer.
        if !(old_len == SET_MINSIZE && newsize == SET_MINSIZE) {
            self.generation = self.generation.wrapping_add(1);
        }
    }

    #[inline]
    fn grow_minused(&self) -> usize {
        if self.used > 50000 {
            self.used * 2
        } else {
            self.used * 4
        }
    }
}

/// A set's elements. Like `dict_inner::Dict`, it takes a read or write lock
/// per operation and never runs Python under it: `__eq__` runs unlocked, and
/// removed keys are released after unlocking.
#[derive(Default)]
pub(crate) struct SetTable {
    inner: PyRwLock<SetTableInner>,
}

unsafe impl crate::object::Traverse for SetTable {
    fn traverse(&self, tracer_fn: &mut crate::object::TraverseFn<'_>) {
        self.inner.traverse(tracer_fn);
    }
}

impl SetTable {
    fn read(&self) -> PyRwLockReadGuard<'_, SetTableInner> {
        self.inner.read()
    }

    fn write(&self) -> PyRwLockWriteGuard<'_, SetTableInner> {
        self.inner.write()
    }

    fn len(&self) -> usize {
        self.read().used
    }

    fn sizeof(&self) -> usize {
        core::mem::size_of::<Self>() + self.read().table.len() * core::mem::size_of::<SetSlot>()
    }

    /// `set_contains_entry` (`set_lookkey`).
    fn contains(&self, vm: &VirtualMachine, key: &PyObject, hash: PyHash) -> PyResult<bool> {
        'restart: loop {
            let mut guard = self.read();
            let generation = guard.generation;
            let mut probe = SetProbe::new(hash, guard.mask());
            loop {
                match guard.probe(key, hash, &mut probe, None, vm) {
                    SetStep::Found(_) => return Ok(true),
                    SetStep::Missing(_) => return Ok(false),
                    SetStep::Compare(j, startkey) => {
                        drop(guard);
                        let eq = vm.bool_eq(&startkey, key)?;
                        guard = self.read();
                        if !guard.still_holds(generation, j, &startkey) {
                            drop(guard);
                            drop(startkey);
                            continue 'restart;
                        }
                        // Still in the table, so this is not the last reference.
                        drop(startkey);
                        if eq {
                            return Ok(true);
                        }
                        probe.advance(guard.mask());
                    }
                }
            }
        }
    }

    /// `set_add_entry`.
    fn add(&self, vm: &VirtualMachine, key: &PyObject, hash: PyHash) -> PyResult<()> {
        'restart: loop {
            let mut guard = self.write();
            let generation = guard.generation;
            let mut probe = SetProbe::new(hash, guard.mask());
            let mut freeslot = None;
            loop {
                match guard.probe(key, hash, &mut probe, Some(&mut freeslot), vm) {
                    SetStep::Found(_) => return Ok(()),
                    SetStep::Missing(j) => {
                        let inner = &mut *guard;
                        inner.used += 1;
                        if let Some(free) = freeslot
                            && inner.table[free].is_dummy()
                        {
                            inner.table[free] = SetSlot {
                                hash,
                                key: Some(key.to_owned()),
                            };
                            return Ok(());
                        }
                        inner.table[j] = SetSlot {
                            hash,
                            key: Some(key.to_owned()),
                        };
                        inner.fill += 1;
                        if inner.fill * 5 >= inner.mask() * 3 {
                            let minused = inner.grow_minused();
                            inner.resize(minused);
                        }
                        return Ok(());
                    }
                    SetStep::Compare(j, startkey) => {
                        drop(guard);
                        if vm.bool_eq(&startkey, key)? {
                            return Ok(());
                        }
                        guard = self.write();
                        if !guard.still_holds(generation, j, &startkey) {
                            drop(guard);
                            drop(startkey);
                            continue 'restart;
                        }
                        drop(startkey);
                        probe.advance(guard.mask());
                    }
                }
            }
        }
    }

    /// `set_discard_entry`: whether the key was there.
    fn discard(&self, vm: &VirtualMachine, key: &PyObject, hash: PyHash) -> PyResult<bool> {
        'restart: loop {
            let mut guard = self.write();
            let generation = guard.generation;
            let mut probe = SetProbe::new(hash, guard.mask());
            let found = loop {
                match guard.probe(key, hash, &mut probe, None, vm) {
                    SetStep::Found(j) => break j,
                    SetStep::Missing(_) => return Ok(false),
                    SetStep::Compare(j, startkey) => {
                        drop(guard);
                        let eq = vm.bool_eq(&startkey, key)?;
                        guard = self.write();
                        if !guard.still_holds(generation, j, &startkey) {
                            drop(guard);
                            drop(startkey);
                            continue 'restart;
                        }
                        drop(startkey);
                        if eq {
                            break j;
                        }
                        probe.advance(guard.mask());
                    }
                }
            };
            let removed = {
                let inner = &mut *guard;
                let slot = &mut inner.table[found];
                slot.hash = DUMMY_HASH;
                inner.used -= 1;
                slot.key.take()
            };
            drop(guard);
            drop(removed);
            return Ok(true);
        }
    }

    /// `set_pop`, from the search finger.
    fn pop(&self) -> Option<PyObjectRef> {
        let mut guard = self.write();
        if guard.used == 0 {
            return None;
        }
        let inner = &mut *guard;
        let mask = inner.mask();
        let mut i = inner.finger & mask;
        while inner.table[i].key.is_none() {
            i = (i + 1) & mask;
        }
        let slot = &mut inner.table[i];
        slot.hash = DUMMY_HASH;
        let key = slot.key.take();
        inner.used -= 1;
        inner.finger = i + 1;
        key
    }

    /// `set_clear_internal`.
    fn clear(&self) {
        let _removed = {
            let mut guard = self.write();
            guard.fill = 0;
            guard.used = 0;
            if guard.table.len() != SET_MINSIZE {
                guard.generation = guard.generation.wrapping_add(1);
            }
            core::mem::replace(&mut guard.table, vec![SetSlot::EMPTY; SET_MINSIZE])
        };
    }

    /// Take over `other`'s table, as `set_swap_bodies` does for an in-place
    /// update. The search finger stays.
    fn replace_with(&self, other: Self) {
        let replacement = other.inner.into_inner();
        let _removed = {
            let mut guard = self.write();
            let finger = guard.finger;
            let both_small =
                guard.table.len() == SET_MINSIZE && replacement.table.len() == SET_MINSIZE;
            let generation = if both_small {
                guard.generation
            } else {
                guard.generation.wrapping_add(1)
            };
            let old = core::mem::replace(&mut *guard, replacement);
            guard.finger = finger;
            guard.generation = generation;
            old
        };
    }

    /// The table's slots with its fill and size, or `None` when it is empty.
    fn snapshot(&self) -> Option<(Vec<SetSlot>, usize, usize)> {
        let guard = self.read();
        (guard.used != 0).then(|| (guard.table.clone(), guard.fill, guard.used))
    }

    /// The part of `set_merge` that runs no Python: one resize up front, then,
    /// into an empty table, a straight copy of the slots when the sizes match
    /// and there are no dummies, else `set_insert_clean`. Hands the slots back
    /// when this table is not empty and each key needs `set_add_entry`.
    fn merge_presized(
        &self,
        slots: Vec<SetSlot>,
        other_fill: usize,
        other_used: usize,
    ) -> Option<Vec<SetSlot>> {
        let mut guard = self.write();
        let inner = &mut *guard;
        if (inner.fill + other_used) * 5 >= inner.mask() * 3 {
            inner.resize((inner.used + other_used) * 2);
        }
        if inner.fill != 0 {
            return Some(slots);
        }
        if inner.mask() == slots.len() - 1 && other_fill == other_used {
            // This table holds no keys, so replacing it releases none. CPython
            // copies into the same table, so no probe needs to restart.
            inner.table = slots;
        } else {
            let mask = inner.mask();
            for slot in slots {
                if let Some(key) = slot.key {
                    set_insert_clean(&mut inner.table, mask, key, slot.hash);
                }
            }
        }
        inner.fill = other_used;
        inner.used = other_used;
        None
    }

    /// `set_merge`.
    fn merge(&self, other: &Self, vm: &VirtualMachine) -> PyResult<()> {
        if core::ptr::eq(self, other) {
            return Ok(());
        }
        let Some((slots, other_fill, other_used)) = other.snapshot() else {
            return Ok(());
        };
        if let Some(slots) = self.merge_presized(slots, other_fill, other_used) {
            for slot in slots {
                if let Some(key) = slot.key {
                    self.add(vm, &key, slot.hash)?;
                }
            }
        }
        Ok(())
    }

    /// `set_update_internal` for an exact dict's keys: presize, then add.
    fn merge_keys(&self, keys: Vec<(PyObjectRef, PyHash)>, vm: &VirtualMachine) -> PyResult<()> {
        {
            let mut guard = self.write();
            let size = keys.len();
            if (guard.fill + size) * 5 >= guard.mask() * 3 {
                let minused = (guard.used + size) * 2;
                guard.resize(minused);
            }
        }
        for (key, hash) in keys {
            self.add(vm, &key, hash)?;
        }
        Ok(())
    }

    /// `set_copy`: a new table merged from this one.
    fn copy(&self) -> Self {
        let new = Self::default();
        if let Some((slots, other_fill, other_used)) = self.snapshot() {
            let rest = new.merge_presized(slots, other_fill, other_used);
            debug_assert!(rest.is_none(), "a new table is empty");
        }
        new
    }

    /// The end of `set_difference_update_internal`: rebuild once dummies are
    /// over a quarter of the table.
    fn purge_dummies(&self) {
        let mut guard = self.write();
        if guard.fill - guard.used <= guard.mask() / 4 {
            return;
        }
        let minused = guard.grow_minused();
        guard.resize(minused);
    }

    fn keys(&self) -> Vec<PyObjectRef> {
        self.read()
            .table
            .iter()
            .filter_map(|slot| slot.key.clone())
            .collect()
    }

    fn keys_with_hashes(&self) -> Vec<(PyObjectRef, PyHash)> {
        self.read()
            .table
            .iter()
            .filter_map(|slot| slot.key.as_ref().map(|key| (key.clone(), slot.hash)))
            .collect()
    }

    /// Fold the live hashes under the read lock; `f` must not run Python.
    fn fold_hashes<Acc>(&self, init: Acc, f: impl FnMut(Acc, PyHash) -> Acc) -> Acc {
        self.read()
            .table
            .iter()
            .filter(|slot| slot.key.is_some())
            .map(|slot| slot.hash)
            .fold(init, f)
    }

    /// `setiter_iternext`: the first live slot at or after `position`, with
    /// the position after it, unless the set no longer has `used` keys.
    fn next_checked(
        &self,
        position: usize,
        used: usize,
    ) -> Result<Option<(usize, PyObjectRef)>, SetChanged> {
        let guard = self.read();
        if guard.used != used {
            return Err(SetChanged);
        }
        Ok(guard
            .table
            .iter()
            .enumerate()
            .skip(position)
            .find_map(|(i, slot)| slot.key.as_ref().map(|key| (i + 1, key.clone()))))
    }

    /// The keys an iterator at `position` has still to return.
    fn keys_from(&self, position: usize) -> Vec<PyObjectRef> {
        self.read()
            .table
            .iter()
            .skip(position)
            .filter_map(|slot| slot.key.clone())
            .collect()
    }
}

#[pyclass(module = false, name = "set", unhashable = true, traverse)]
#[derive(Default)]
pub struct PySet {
    pub(super) inner: PySetInner,
}

impl PySet {
    #[deprecated(note = "Use `PySet::default().into_ref(ctx)` instead")]
    pub fn new_ref(ctx: &Context) -> PyRef<Self> {
        Self::default().into_ref(ctx)
    }

    #[must_use]
    pub fn elements(&self) -> Vec<PyObjectRef> {
        self.inner.elements()
    }

    fn fold_op(
        &self,
        others: impl core::iter::Iterator<Item = ArgIterable>,
        op: fn(&PySetInner, ArgIterable, &VirtualMachine) -> PyResult<PySetInner>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        Ok(Self {
            inner: self.inner.fold_op(others, op, vm)?,
        })
    }

    fn op(
        &self,
        other: AnySet,
        op: fn(&PySetInner, ArgIterable, &VirtualMachine) -> PyResult<PySetInner>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        Ok(Self {
            inner: self
                .inner
                .fold_op(core::iter::once(other.into_iterable(vm)?), op, vm)?,
        })
    }
}

#[pyclass(module = false, name = "frozenset", unhashable = true)]
pub struct PyFrozenSet {
    inner: PySetInner,
    hash: PyAtomic<PyHash>,
}

impl Default for PyFrozenSet {
    fn default() -> Self {
        Self {
            inner: PySetInner::default(),
            hash: hash::SENTINEL.into(),
        }
    }
}

impl PyFrozenSet {
    // Also used by ssl.rs windows.
    pub fn from_iter(
        vm: &VirtualMachine,
        it: impl IntoIterator<Item = PyObjectRef>,
    ) -> PyResult<Self> {
        let inner = PySetInner::default();
        for elem in it {
            inner.add(&elem, vm)?;
        }
        // FIXME: empty set check
        Ok(Self {
            inner,
            ..Default::default()
        })
    }

    pub fn elements(&self) -> Vec<PyObjectRef> {
        self.inner.elements()
    }

    pub fn add_unshared(&self, item: &PyObject, vm: &VirtualMachine) -> PyResult<()> {
        self.inner.add(item, vm)
    }

    fn fold_op(
        &self,
        others: impl core::iter::Iterator<Item = ArgIterable>,
        op: fn(&PySetInner, ArgIterable, &VirtualMachine) -> PyResult<PySetInner>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        Ok(Self {
            inner: self.inner.fold_op(others, op, vm)?,
            ..Default::default()
        })
    }

    fn op(
        &self,
        other: AnySet,
        op: fn(&PySetInner, ArgIterable, &VirtualMachine) -> PyResult<PySetInner>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        Ok(Self {
            inner: self
                .inner
                .fold_op(core::iter::once(other.into_iterable(vm)?), op, vm)?,
            ..Default::default()
        })
    }
}

impl fmt::Debug for PySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: implement more detailed, non-recursive Debug formatter
        f.write_str("set")
    }
}

impl fmt::Debug for PyFrozenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: implement more detailed, non-recursive Debug formatter
        f.write_str("PyFrozenSet ")?;
        f.debug_set().entries(self.elements().iter()).finish()
    }
}

impl PyPayload for PySet {
    #[inline]
    fn class(ctx: &Context) -> &'static Py<PyType> {
        ctx.types.set_type
    }
}

impl PyPayload for PyFrozenSet {
    #[inline]
    fn class(ctx: &Context) -> &'static Py<PyType> {
        ctx.types.frozenset_type
    }
}

#[derive(Default, Clone)]
pub(super) struct PySetInner {
    content: PyRc<SetTable>,
}

unsafe impl crate::object::Traverse for PySetInner {
    fn traverse(&self, tracer_fn: &mut crate::object::TraverseFn<'_>) {
        // FIXME(discord9): Rc means shared ref, so should it be traced?
        self.content.traverse(tracer_fn)
    }
}

impl PySetInner {
    pub(super) fn from_iter<T>(iter: T, vm: &VirtualMachine) -> PyResult<Self>
    where
        T: IntoIterator<Item = PyResult<PyObjectRef>>,
    {
        let set = Self::default();
        for item in iter {
            let item = item?;
            set.add(&item, vm)?;
        }
        Ok(set)
    }

    /// Build a set from an arbitrary object, reusing stored hashes when the
    /// source is a set/frozenset/dict.
    fn from_object(iterable: PyObjectRef, vm: &VirtualMachine) -> PyResult<Self> {
        let set = Self::default();
        set.update_internal(iterable, vm)?;
        Ok(set)
    }

    fn fold_op<O>(
        &self,
        others: impl core::iter::Iterator<Item = O>,
        op: fn(&Self, O, &VirtualMachine) -> PyResult<Self>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        let mut res = self.copy();
        for other in others {
            res = op(&res, other, vm)?;
        }
        Ok(res)
    }

    fn intersection_multi(
        &self,
        mut others: impl core::iter::Iterator<Item = ArgIterable>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        let Some(other) = others.next() else {
            return Ok(self.copy());
        };
        let mut result = self.intersection(other, vm)?;
        for other in others {
            result = result.intersection(other, vm)?;
        }
        Ok(result)
    }

    fn difference_multi(
        &self,
        mut others: impl core::iter::Iterator<Item = ArgIterable>,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        let Some(other) = others.next() else {
            return Ok(self.copy());
        };
        let result = self.difference_new(other, vm)?;
        result.difference_update(others, vm)?;
        Ok(result)
    }

    fn len(&self) -> usize {
        self.content.len()
    }

    fn sizeof(&self) -> usize {
        self.content.sizeof()
    }

    /// `set_copy`: a new set merged from this one, which can lay its keys out
    /// differently from the original.
    fn copy(&self) -> Self {
        Self {
            content: PyRc::new(self.content.copy()),
        }
    }

    fn contains(&self, needle: &PyObject, vm: &VirtualMachine) -> PyResult<bool> {
        let result = self.retry_op_with_frozenset(needle, vm, |needle, vm| {
            let hash = needle.hash(vm)?;
            self.content.contains(vm, needle, hash)
        });
        Self::wrap_unhashable_error(result, needle, vm)
    }

    /// Look up a key whose hash is already known, without a frozenset retry.
    fn contains_known_hash(
        &self,
        needle: &PyObject,
        hash: PyHash,
        vm: &VirtualMachine,
    ) -> PyResult<bool> {
        self.content.contains(vm, needle, hash)
    }

    fn compare(&self, other: &Self, op: PyComparisonOp, vm: &VirtualMachine) -> PyResult<bool> {
        if op == PyComparisonOp::Ne {
            return self.compare(other, PyComparisonOp::Eq, vm).map(|eq| !eq);
        }
        if !op.eval_ord(self.len().cmp(&other.len())) {
            return Ok(false);
        }

        let (superset, subset) = match op {
            PyComparisonOp::Lt | PyComparisonOp::Le | PyComparisonOp::Eq => (other, self),
            _ => (self, other),
        };

        for (key, hash) in subset.content.keys_with_hashes() {
            if !superset.contains_known_hash(&key, hash, vm)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `set_update_internal` into this set, returning it: what the dict
    /// views' `|` does to the set it builds, and `set_union` to its copy.
    pub(super) fn union(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<Self> {
        self.update_internal(other.as_object().to_owned(), vm)?;
        Ok(self.clone())
    }

    pub(super) fn intersection(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<Self> {
        if let Some(other_set) = extract_set(other.as_object()) {
            return self.intersection_set(other_set, vm);
        }
        let set = Self::default();
        for item in other.iter(vm)? {
            let obj = item?;
            let hash = obj.hash(vm)?;
            if self.contains_known_hash(&obj, hash, vm)? {
                set.add_known_hash(&obj, hash, vm)?;
                if set.len() >= self.len() {
                    break;
                }
            }
        }
        Ok(set)
    }

    fn intersection_set(&self, other: &Self, vm: &VirtualMachine) -> PyResult<Self> {
        if PyRc::ptr_eq(&self.content, &other.content) {
            return Ok(self.copy());
        }
        let (target, source) = if self.len() < other.len() {
            (other, self)
        } else {
            (self, other)
        };
        let set = Self::default();
        for (obj, hash) in source.content.keys_with_hashes() {
            if target.contains_known_hash(&obj, hash, vm)? {
                set.add_known_hash(&obj, hash, vm)?;
            }
        }
        Ok(set)
    }

    /// `set_difference`.
    fn difference_new(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<Self> {
        let other_size = if let Some(other_set) = extract_set(other.as_object()) {
            Some(other_set.len())
        } else {
            other
                .as_object()
                .downcast_ref_if_exact::<PyDict>(vm)
                .map(|dict| dict._as_dict_inner().len())
        };
        let Some(other_size) = other_size else {
            return self.copy_and_difference(other, vm);
        };
        // A much larger left side is cheaper to copy than to scan.
        if self.len() >> 2 > other_size {
            return self.copy_and_difference(other, vm);
        }
        let result = Self::default();
        if let Some(other_set) = extract_set(other.as_object()) {
            for (key, hash) in self.content.keys_with_hashes() {
                if !other_set.contains_known_hash(&key, hash, vm)? {
                    result.content.add(vm, &key, hash)?;
                }
            }
        } else if let Some(dict) = other.as_object().downcast_ref_if_exact::<PyDict>(vm) {
            for (key, hash) in self.content.keys_with_hashes() {
                if !dict._as_dict_inner().contains_known_hash(vm, &*key, hash)? {
                    result.content.add(vm, &key, hash)?;
                }
            }
        }
        Ok(result)
    }

    /// `set_copy_and_difference`.
    fn copy_and_difference(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<Self> {
        let result = self.copy();
        result.difference_update_internal(other, vm)?;
        Ok(result)
    }

    /// `difference_update` in place, returning this set: the dict views' `-`.
    pub(super) fn difference(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<Self> {
        self.difference_update_internal(other, vm)?;
        Ok(self.clone())
    }

    /// `symmetric_difference_update` in place, returning this set: the dict
    /// views' `^`.
    pub(super) fn symmetric_difference(
        &self,
        other: ArgIterable,
        vm: &VirtualMachine,
    ) -> PyResult<Self> {
        self.symmetric_difference_update_internal(other, vm)?;
        Ok(self.clone())
    }

    /// `set_symmetric_difference`: a copy of `other` toggled by this set's keys.
    fn symmetric_difference_new(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<Self> {
        let result = Self::from_object(other.as_object().to_owned(), vm)?;
        result.toggle(self.content.keys_with_hashes(), vm)?;
        Ok(result)
    }

    /// Remove each key that is present and add each that is not.
    fn toggle(&self, keys: Vec<(PyObjectRef, PyHash)>, vm: &VirtualMachine) -> PyResult<()> {
        for (key, hash) in keys {
            if !self.content.discard(vm, &key, hash)? {
                self.content.add(vm, &key, hash)?;
            }
        }
        Ok(())
    }

    fn issuperset(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        if let Some(other_set) = extract_set(other.as_object()) {
            return self.compare(other_set, PyComparisonOp::Ge, vm);
        }
        for item in other.iter(vm)? {
            if !self.contains(&*item?, vm)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn issubset(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        if let Some(other_set) = extract_set(other.as_object()) {
            return self.compare(other_set, PyComparisonOp::Le, vm);
        }
        Ok(self.intersection(other, vm)?.len() == self.len())
    }

    pub(super) fn isdisjoint(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        if let Some(other_set) = extract_set(other.as_object()) {
            if core::ptr::eq(self, other_set) {
                return Ok(self.len() == 0);
            }
            let other_type = other.as_object().class();
            if other_type.is(vm.ctx.types.set_type) || other_type.is(vm.ctx.types.frozenset_type) {
                let (target, source) = if self.len() < other_set.len() {
                    (other_set, self)
                } else {
                    (self, other_set)
                };
                for (key, hash) in source.content.keys_with_hashes() {
                    if target.contains_known_hash(&key, hash, vm)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
        }
        for item in other.iter(vm)? {
            if self.contains(&*item?, vm)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn repr(&self, class_name: Option<&str>, vm: &VirtualMachine) -> PyResult<Wtf8Buf> {
        let empty = format!("{}()", class_name.unwrap_or("set"));
        collection_repr(
            class_name,
            "{",
            "}",
            &empty,
            self.elements().iter().map(|o| &**o),
            vm,
        )
    }

    fn add(&self, item: &PyObject, vm: &VirtualMachine) -> PyResult<()> {
        let result = item
            .hash(vm)
            .and_then(|hash| self.content.add(vm, item, hash));
        Self::wrap_unhashable_error(result, item, vm)
    }

    /// [`Self::add`] with a known hash.
    fn add_known_hash(&self, item: &PyObject, hash: PyHash, vm: &VirtualMachine) -> PyResult<()> {
        self.content.add(vm, item, hash)
    }

    fn remove(&self, item: &PyObject, vm: &VirtualMachine) -> PyResult<()> {
        let result = self.retry_op_with_frozenset(item, vm, |item, vm| {
            let hash = item.hash(vm)?;
            if self.content.discard(vm, item, hash)? {
                Ok(())
            } else {
                Err(vm.new_key_error(item.to_owned()))
            }
        });
        Self::wrap_unhashable_error(result, item, vm)
    }

    fn discard(&self, item: &PyObject, vm: &VirtualMachine) -> PyResult<bool> {
        let result = self.retry_op_with_frozenset(item, vm, |item, vm| {
            let hash = item.hash(vm)?;
            self.content.discard(vm, item, hash)
        });
        Self::wrap_unhashable_error(result, item, vm)
    }

    fn clear(&self) {
        self.content.clear()
    }

    fn elements(&self) -> Vec<PyObjectRef> {
        self.content.keys()
    }

    fn pop(&self, vm: &VirtualMachine) -> PyResult {
        if let Some(key) = self.content.pop() {
            Ok(key)
        } else {
            let err_msg = vm.ctx.new_str(ascii!("pop from an empty set")).into();
            Err(vm.new_key_error(err_msg))
        }
    }

    /// `set_update_internal`.
    fn update_internal(&self, iterable: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        // check AnySet
        if let Ok(any_set) = AnySet::try_from_object(vm, iterable.to_owned()) {
            self.merge_set(any_set, vm)
        // check Dict
        } else if let Ok(dict) = iterable.to_owned().downcast_exact::<PyDict>(vm) {
            self.merge_dict(&dict, vm)
        } else {
            // add iterable that is not AnySet or Dict
            for item in iterable.try_into_value::<ArgIterable>(vm)?.iter(vm)? {
                let item = item?;
                self.add(&item, vm)?;
            }
            Ok(())
        }
    }

    fn merge_set(&self, any_set: AnySet, vm: &VirtualMachine) -> PyResult<()> {
        self.content.merge(&any_set.as_inner().content, vm)
    }

    fn merge_dict(&self, dict: &Py<PyDict>, vm: &VirtualMachine) -> PyResult<()> {
        self.content
            .merge_keys(dict._as_dict_inner().keys_with_hashes(), vm)
    }

    fn intersection_update(
        &self,
        others: impl core::iter::Iterator<Item = ArgIterable>,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        let temp_inner = self.intersection_multi(others, vm)?;
        let content = PyRc::try_unwrap(temp_inner.content).unwrap_or_else(|table| table.copy());
        self.content.replace_with(content);
        Ok(())
    }

    /// `set_difference_update_internal`.
    fn difference_update_internal(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<()> {
        if let Some(other_set) = extract_set(other.as_object()) {
            if PyRc::ptr_eq(&self.content, &other_set.content) {
                self.clear();
                return Ok(());
            }
            // Against a much larger set, remove only the common keys.
            let keys = if other_set.len() >> 3 > self.len() {
                self.intersection_set(other_set, vm)?
                    .content
                    .keys_with_hashes()
            } else {
                other_set.content.keys_with_hashes()
            };
            for (key, hash) in keys {
                self.content.discard(vm, &key, hash)?;
            }
        } else {
            for item in other.iter(vm)? {
                let key = item?;
                let hash = key.hash(vm)?;
                self.content.discard(vm, &key, hash)?;
            }
        }
        self.content.purge_dummies();
        Ok(())
    }

    fn difference_update(
        &self,
        others: impl core::iter::Iterator<Item = ArgIterable>,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        for other in others {
            self.difference_update_internal(other, vm)?;
        }
        Ok(())
    }

    /// `set_symmetric_difference_update`.
    fn symmetric_difference_update_internal(
        &self,
        other: ArgIterable,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        let keys = if let Some(other_set) = extract_set(other.as_object()) {
            if PyRc::ptr_eq(&self.content, &other_set.content) {
                self.clear();
                return Ok(());
            }
            other_set.content.keys_with_hashes()
        } else if let Some(dict) = other.as_object().downcast_ref_if_exact::<PyDict>(vm) {
            dict._as_dict_inner().keys_with_hashes()
        } else {
            // A set of the other keys first, which drops their duplicates.
            Self::from_object(other.as_object().to_owned(), vm)?
                .content
                .keys_with_hashes()
        };
        self.toggle(keys, vm)
    }

    fn symmetric_difference_update(
        &self,
        others: impl core::iter::Iterator<Item = ArgIterable>,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        for other in others {
            self.symmetric_difference_update_internal(other, vm)?;
        }
        Ok(())
    }

    fn hash(&self) -> PyHash {
        let hasher = self.content.fold_hashes(
            hash::FrozenSetHash::new(self.len()),
            |mut hasher, element_hash| {
                hasher.add(element_hash);
                hasher
            },
        );
        hasher.finish()
    }

    // Run operation, on failure, if item is a set/set subclass, convert it
    // into a frozenset and try the operation again. Propagates original error
    // on failure to convert and restores item in KeyError on failure (remove).
    fn retry_op_with_frozenset<T, F>(
        &self,
        item: &PyObject,
        vm: &VirtualMachine,
        op: F,
    ) -> PyResult<T>
    where
        F: Fn(&PyObject, &VirtualMachine) -> PyResult<T>,
    {
        op(item, vm).or_else(|original_err| {
            item.downcast_ref::<PySet>()
                // Keep original error around.
                .ok_or(original_err)
                .and_then(|set| {
                    op(
                        &PyFrozenSet {
                            inner: set.inner.copy(),
                            ..Default::default()
                        }
                        .into_pyobject(vm),
                        vm,
                    )
                    // If operation raised KeyError, report original set (set.remove)
                    .map_err(|op_err| {
                        if op_err.fast_isinstance(vm.ctx.exceptions.key_error) {
                            vm.new_key_error(item.to_owned())
                        } else {
                            op_err
                        }
                    })
                })
        })
    }

    fn wrap_unhashable_error<T>(
        result: PyResult<T>,
        item: &PyObject,
        vm: &VirtualMachine,
    ) -> PyResult<T> {
        match result {
            // CPython 3.12 (the parity reference) raises the hash error unchanged.
            result => {
                let _ = (item, vm);
                result
            }
        }
    }
}

fn extract_set(obj: &PyObject) -> Option<&PySetInner> {
    match_class!(match obj {
        ref set @ PySet => Some(&set.inner),
        ref frozen @ PyFrozenSet => Some(&frozen.inner),
        _ => None,
    })
}

/// Elements of `obj` with their stored hashes, or `None` unless `obj` is exactly
/// a `set` or `frozenset` — `PyAnySet_CheckExact`, where [`extract_set`] is the
/// subclass-inclusive `PyAnySet_Check`.
pub(super) fn exact_set_keys_with_hashes(
    obj: &PyObject,
    vm: &VirtualMachine,
) -> Option<Vec<(PyObjectRef, PyHash)>> {
    let inner = obj
        .downcast_ref_if_exact::<PySet>(vm)
        .map(|set| &set.inner)
        .or_else(|| {
            obj.downcast_ref_if_exact::<PyFrozenSet>(vm)
                .map(|frozen| &frozen.inner)
        })?;
    Some(inner.content.keys_with_hashes())
}

fn reduce_set(zelf: &PyObject, vm: &VirtualMachine) -> (PyTypeRef, PyTupleRef, Option<PyDictRef>) {
    (
        zelf.class().to_owned(),
        #[expect(clippy::or_fun_call, reason = "changing this won't compile")]
        vm.new_tuple((extract_set(zelf)
            .unwrap_or(&PySetInner::default())
            .elements(),)),
        zelf.dict(),
    )
}

impl PySet {
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    pub fn contains(&self, needle: &PyObject, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.contains(needle, vm)
    }

    fn __or__(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(self.op(
                other,
                PySetInner::union,
                vm,
            )?))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __and__(
        &self,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: self.inner.intersection(other.into_iterable(vm)?, vm)?,
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __sub__(
        &self,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: self.inner.difference_new(other.into_iterable(vm)?, vm)?,
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __rsub__(
        zelf: PyRef<Self>,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: other
                    .as_inner()
                    .difference_new(ArgIterable::try_from_object(vm, zelf.into())?, vm)?,
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __xor__(
        &self,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: self
                    .inner
                    .symmetric_difference_new(other.into_iterable(vm)?, vm)?,
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __ior__(zelf: PyRef<Self>, set: AnySet, vm: &VirtualMachine) -> PyResult<PyRef<Self>> {
        zelf.inner.merge_set(set, vm)?;
        Ok(zelf)
    }

    fn __iand__(zelf: PyRef<Self>, set: AnySet, vm: &VirtualMachine) -> PyResult<PyRef<Self>> {
        zelf.inner
            .intersection_update(core::iter::once(set.into_iterable(vm)?), vm)?;
        Ok(zelf)
    }

    fn __isub__(zelf: PyRef<Self>, set: AnySet, vm: &VirtualMachine) -> PyResult<PyRef<Self>> {
        if set.is(zelf.as_object()) {
            zelf.inner.clear();
        } else {
            zelf.inner
                .difference_update(set.into_iterable_iter(vm)?, vm)?;
        }
        Ok(zelf)
    }

    fn __ixor__(zelf: PyRef<Self>, set: AnySet, vm: &VirtualMachine) -> PyResult<PyRef<Self>> {
        if set.is(zelf.as_object()) {
            zelf.inner.clear();
        } else {
            zelf.inner
                .symmetric_difference_update(set.into_iterable_iter(vm)?, vm)?;
        }
        Ok(zelf)
    }

    pub fn add(&self, object: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        self.inner.add(&object, vm)
    }
}

#[pyclass(
    with(
        Constructor,
        Initializer,
        AsSequence,
        Comparable,
        Iterable,
        AsNumber,
        Representable
    ),
    flags(BASETYPE, _MATCH_SELF, HAS_WEAKREF)
)]
impl Py<PySet> {
    /// `_PySet_Update`, which `SET_UPDATE` runs: a set or dict operand merges
    /// its table, so a folded frozenset constant lands in CPython's order.
    pub(crate) fn update_from(&self, iterable: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        self.inner.update_internal(iterable, vm)
    }

    #[pymethod(coexist)]
    fn __contains__(&self, object: PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
        self.contains(&object, vm)
    }

    #[pymethod]
    fn __sizeof__(&self) -> usize {
        core::mem::size_of::<PySet>() + self.inner.sizeof()
    }

    #[pymethod]
    fn copy(&self) -> PySet {
        PySet {
            inner: self.inner.copy(),
        }
    }

    #[pymethod]
    fn union(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<PySet> {
        self.fold_op(others.into_iter(), PySetInner::union, vm)
    }

    #[pymethod]
    fn intersection(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<PySet> {
        Ok(PySet {
            inner: self.inner.intersection_multi(others.into_iter(), vm)?,
        })
    }

    #[pymethod]
    fn difference(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<PySet> {
        Ok(PySet {
            inner: self.inner.difference_multi(others.into_iter(), vm)?,
        })
    }

    #[pymethod]
    fn symmetric_difference(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<PySet> {
        Ok(PySet {
            inner: self.inner.symmetric_difference_new(other, vm)?,
        })
    }

    #[pymethod]
    fn issubset(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.issubset(other, vm)
    }

    #[pymethod]
    fn issuperset(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.issuperset(other, vm)
    }

    #[pymethod]
    fn isdisjoint(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.isdisjoint(other, vm)
    }

    #[pymethod]
    pub fn add(&self, object: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        self.payload.add(object, vm)
    }

    #[pymethod]
    fn remove(&self, object: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        self.inner.remove(&object, vm)
    }

    #[pymethod]
    pub fn discard(&self, object: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        self.inner.discard(&object, vm).map(|_| ())
    }

    #[pymethod]
    pub fn clear(&self) {
        self.inner.clear()
    }

    #[pymethod]
    pub fn pop(&self, vm: &VirtualMachine) -> PyResult {
        self.inner.pop(vm)
    }

    #[pymethod]
    fn update(
        &self,
        others: PosArgs<PyObjectRef, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        for iterable in others {
            self.inner.update_internal(iterable, vm)?;
        }
        Ok(())
    }

    #[pymethod]
    fn intersection_update(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        self.inner.intersection_update(others.into_iter(), vm)?;
        Ok(())
    }

    #[pymethod]
    fn difference_update(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        self.inner.difference_update(others.into_iter(), vm)
    }

    #[pymethod]
    fn symmetric_difference_update(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<()> {
        self.inner
            .symmetric_difference_update(core::iter::once(other), vm)
    }

    #[pymethod]
    fn __reduce__(
        zelf: PyRef<PySet>,
        vm: &VirtualMachine,
    ) -> (PyTypeRef, PyTupleRef, Option<PyDictRef>) {
        reduce_set(zelf.as_ref(), vm)
    }

    #[pyclassmethod]
    fn __class_getitem__(
        cls: PyTypeRef,
        object: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyGenericAlias> {
        PyGenericAlias::from_args(cls, object, vm)
    }
}

impl DefaultConstructor for PySet {}

impl Initializer for PySet {
    type Args = crate::function::PositionalIterable;

    fn init(zelf: &Py<Self>, args: Self::Args, vm: &VirtualMachine) -> PyResult<()> {
        zelf.clear();
        if let OptionalArg::Present(it) = args.iterable {
            zelf.update(PosArgs::<PyObjectRef, NameOthers>::named(vec![it]), vm)?;
        }
        Ok(())
    }
}

impl AsSequence for PySet {
    fn as_sequence() -> &'static PySequenceMethods {
        static AS_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
            length: atomic_func!(|seq, _vm| Ok(PySet::sequence_downcast(seq).__len__())),
            contains: atomic_func!(
                |seq, needle, vm| PySet::sequence_downcast(seq).contains(needle, vm)
            ),
            ..PySequenceMethods::NOT_IMPLEMENTED
        });
        &AS_SEQUENCE
    }
}

impl Comparable for PySet {
    fn cmp(
        zelf: &crate::Py<Self>,
        other: &PyObject,
        op: PyComparisonOp,
        vm: &VirtualMachine,
    ) -> PyResult<PyComparisonValue> {
        extract_set(other).map_or(Ok(PyComparisonValue::NotImplemented), |other| {
            Ok(zelf.inner.compare(other, op, vm)?.into())
        })
    }
}

impl Iterable for PySet {
    fn iter(zelf: PyRef<Self>, vm: &VirtualMachine) -> PyResult {
        Ok(PySetIterator::new(AnySet {
            object: zelf.into(),
        })
        .into_pyobject(vm))
    }
}

impl AsNumber for PySet {
    fn as_number() -> &'static PyNumberMethods {
        static AS_NUMBER: PyNumberMethods = PyNumberMethods {
            // Binary ops check both operands are sets (like CPython's set_sub, etc.)
            // This is needed because __rsub__ swaps operands: a.__rsub__(b) calls subtract(b, a)
            subtract: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__sub__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    // When called via __rsub__, a might be PyFrozenSet
                    a.__sub__(b.to_owned(), vm)
                        .map(|r| r.map(|s| PySet { inner: s.inner }))
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            and: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__and__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__and__(b.to_owned(), vm)
                        .map(|r| r.map(|s| PySet { inner: s.inner }))
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            xor: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__xor__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__xor__(b.to_owned(), vm)
                        .map(|r| r.map(|s| PySet { inner: s.inner }))
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            or: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__or__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__or__(b.to_owned(), vm)
                        .map(|r| r.map(|s| PySet { inner: s.inner }))
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            inplace_subtract: Some(|a, b, vm| {
                if let Some(a) = a.downcast_ref::<PySet>() {
                    PySet::__isub__(a.to_owned(), AnySet::try_from_object(vm, b.to_owned())?, vm)
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            inplace_and: Some(|a, b, vm| {
                if let Some(a) = a.downcast_ref::<PySet>() {
                    PySet::__iand__(a.to_owned(), AnySet::try_from_object(vm, b.to_owned())?, vm)
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            inplace_xor: Some(|a, b, vm| {
                if let Some(a) = a.downcast_ref::<PySet>() {
                    PySet::__ixor__(a.to_owned(), AnySet::try_from_object(vm, b.to_owned())?, vm)
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            inplace_or: Some(|a, b, vm| {
                if let Some(a) = a.downcast_ref::<PySet>() {
                    PySet::__ior__(a.to_owned(), AnySet::try_from_object(vm, b.to_owned())?, vm)
                        .to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            ..PyNumberMethods::NOT_IMPLEMENTED
        };
        &AS_NUMBER
    }
}

impl Representable for PySet {
    #[inline]
    fn repr_wtf8(zelf: &crate::Py<Self>, vm: &VirtualMachine) -> PyResult<Wtf8Buf> {
        let class = zelf.class();
        let borrowed_name = class.name();
        let class_name = &*borrowed_name;

        if zelf.inner.len() == 0 {
            return Ok(Wtf8Buf::from(format!("{class_name}()")));
        }

        if let Some(_guard) = ReprGuard::enter(vm, zelf.as_object()) {
            let name = (class_name != "set").then_some(class_name);
            zelf.inner.repr(name, vm)
        } else {
            Ok(Wtf8Buf::from(format!("{class_name}(...)")))
        }
    }
}

impl Constructor for PyFrozenSet {
    type Args = crate::function::PositionalIterable;

    fn slot_new(cls: PyTypeRef, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
        let is_exact_frozenset = cls.is(vm.ctx.types.frozenset_type);
        let is_frozenset_init = {
            let cls_init = cls
                .slots
                .init
                .load()
                .map(|init| crate::types::fn_addr(init));
            let frozenset_init = vm
                .ctx
                .types
                .frozenset_type
                .slots
                .init
                .load()
                .map(|init| crate::types::fn_addr(init));
            cls_init == frozenset_init
        };

        // Optimizations for exact frozenset type
        let iterable_opt = if is_exact_frozenset || is_frozenset_init {
            let iterable: crate::function::PositionalIterable = args.bind_for(vm, Self::NAME)?;
            let iterable = iterable.iterable;

            // Return exact frozenset as-is
            if is_exact_frozenset
                && let OptionalArg::Present(input) = &iterable
                && input.class().is(vm.ctx.types.frozenset_type)
            {
                return Ok(input.clone());
            }

            iterable
        } else {
            match &args.args[..] {
                [] => OptionalArg::Missing,
                [iterable] => OptionalArg::Present(iterable.clone()),
                slice => {
                    return Err(vm.new_arity_type_error(Self::NAME, 0..=1, slice.len()));
                }
            }
        };

        let payload = Self::py_new(
            &cls,
            Self::Args {
                iterable: iterable_opt,
            },
            vm,
        )?;

        // Return empty frozenset singleton
        if is_exact_frozenset && payload.inner.len() == 0 {
            return Ok(vm.ctx.empty_frozenset.clone().into());
        }

        payload.into_ref_with_type(vm, cls).map(Into::into)
    }

    fn py_new(_cls: &Py<PyType>, args: Self::Args, vm: &VirtualMachine) -> PyResult<Self> {
        let inner = match args.iterable {
            OptionalArg::Present(iterable) => PySetInner::from_object(iterable, vm)?,
            OptionalArg::Missing => PySetInner::default(),
        };
        Ok(Self {
            inner,
            ..Default::default()
        })
    }
}

impl PyFrozenSet {
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    pub fn contains(&self, needle: &PyObject, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.contains(needle, vm)
    }

    fn __or__(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(set) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(self.op(
                set,
                PySetInner::union,
                vm,
            )?))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __and__(
        &self,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: self.inner.intersection(other.into_iterable(vm)?, vm)?,
                ..Default::default()
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __sub__(
        &self,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: self.inner.difference_new(other.into_iterable(vm)?, vm)?,
                ..Default::default()
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __rsub__(
        zelf: PyRef<Self>,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: other
                    .as_inner()
                    .difference_new(ArgIterable::try_from_object(vm, zelf.into())?, vm)?,
                ..Default::default()
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }

    fn __xor__(
        &self,
        other: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyArithmeticValue<Self>> {
        if let Ok(other) = AnySet::try_from_object(vm, other) {
            Ok(PyArithmeticValue::Implemented(Self {
                inner: self
                    .inner
                    .symmetric_difference_new(other.into_iterable(vm)?, vm)?,
                ..Default::default()
            }))
        } else {
            Ok(PyArithmeticValue::NotImplemented)
        }
    }
}

#[pyclass(
    flags(BASETYPE, _MATCH_SELF, HAS_WEAKREF),
    with(
        Constructor,
        AsSequence,
        Hashable,
        Comparable,
        Iterable,
        AsNumber,
        Representable
    )
)]
impl Py<PyFrozenSet> {
    #[pymethod(coexist)]
    fn __contains__(&self, object: PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
        self.contains(&object, vm)
    }

    #[pymethod]
    fn __sizeof__(&self) -> usize {
        core::mem::size_of::<PyFrozenSet>() + self.inner.sizeof()
    }

    #[pymethod]
    fn copy(zelf: PyRef<PyFrozenSet>, vm: &VirtualMachine) -> PyRef<PyFrozenSet> {
        if zelf.class().is(vm.ctx.types.frozenset_type) {
            zelf
        } else {
            PyFrozenSet {
                inner: zelf.inner.copy(),
                ..Default::default()
            }
            .into_ref(&vm.ctx)
        }
    }

    #[pymethod]
    fn union(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<PyFrozenSet> {
        self.fold_op(others.into_iter(), PySetInner::union, vm)
    }

    #[pymethod]
    fn intersection(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<PyFrozenSet> {
        Ok(PyFrozenSet {
            inner: self.inner.intersection_multi(others.into_iter(), vm)?,
            ..Default::default()
        })
    }

    #[pymethod]
    fn difference(
        &self,
        others: PosArgs<ArgIterable, NameOthers>,
        vm: &VirtualMachine,
    ) -> PyResult<PyFrozenSet> {
        Ok(PyFrozenSet {
            inner: self.inner.difference_multi(others.into_iter(), vm)?,
            ..Default::default()
        })
    }

    #[pymethod]
    fn symmetric_difference(
        &self,
        other: ArgIterable,
        vm: &VirtualMachine,
    ) -> PyResult<PyFrozenSet> {
        Ok(PyFrozenSet {
            inner: self.inner.symmetric_difference_new(other, vm)?,
            ..Default::default()
        })
    }

    #[pymethod]
    fn issubset(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.issubset(other, vm)
    }

    #[pymethod]
    fn issuperset(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.issuperset(other, vm)
    }

    #[pymethod]
    fn isdisjoint(&self, other: ArgIterable, vm: &VirtualMachine) -> PyResult<bool> {
        self.inner.isdisjoint(other, vm)
    }

    #[pymethod]
    fn __reduce__(
        zelf: PyRef<PyFrozenSet>,
        vm: &VirtualMachine,
    ) -> (PyTypeRef, PyTupleRef, Option<PyDictRef>) {
        reduce_set(zelf.as_ref(), vm)
    }

    #[pyclassmethod]
    fn __class_getitem__(
        cls: PyTypeRef,
        object: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<PyGenericAlias> {
        PyGenericAlias::from_args(cls, object, vm)
    }
}

impl AsSequence for PyFrozenSet {
    fn as_sequence() -> &'static PySequenceMethods {
        static AS_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
            length: atomic_func!(|seq, _vm| Ok(PyFrozenSet::sequence_downcast(seq).__len__())),
            contains: atomic_func!(
                |seq, needle, vm| PyFrozenSet::sequence_downcast(seq).contains(needle, vm)
            ),
            ..PySequenceMethods::NOT_IMPLEMENTED
        });
        &AS_SEQUENCE
    }
}

impl Hashable for PyFrozenSet {
    #[inline]
    fn hash(zelf: &crate::Py<Self>, _vm: &VirtualMachine) -> PyResult<PyHash> {
        let hash = match zelf.hash.load(Ordering::Relaxed) {
            hash::SENTINEL => {
                let hash = zelf.inner.hash();
                match Radium::compare_exchange(
                    &zelf.hash,
                    hash::SENTINEL,
                    hash::fix_sentinel(hash),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => hash,
                    Err(prev_stored) => prev_stored,
                }
            }
            hash => hash,
        };
        Ok(hash)
    }
}

impl Comparable for PyFrozenSet {
    fn cmp(
        zelf: &crate::Py<Self>,
        other: &PyObject,
        op: PyComparisonOp,
        vm: &VirtualMachine,
    ) -> PyResult<PyComparisonValue> {
        extract_set(other).map_or(Ok(PyComparisonValue::NotImplemented), |other| {
            Ok(zelf.inner.compare(other, op, vm)?.into())
        })
    }
}

impl Iterable for PyFrozenSet {
    fn iter(zelf: PyRef<Self>, vm: &VirtualMachine) -> PyResult {
        Ok(PySetIterator::new(AnySet {
            object: zelf.into(),
        })
        .into_pyobject(vm))
    }
}

impl AsNumber for PyFrozenSet {
    fn as_number() -> &'static PyNumberMethods {
        static AS_NUMBER: PyNumberMethods = PyNumberMethods {
            // Binary ops check both operands are sets (like CPython's set_sub, etc.)
            // __rsub__ swaps operands. Result type follows first operand's type.
            subtract: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__sub__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PySet>() {
                    // When called via __rsub__, a might be PySet - return set (not frozenset)
                    a.__sub__(b.to_owned(), vm).to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            and: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__and__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__and__(b.to_owned(), vm).to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            xor: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__xor__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__xor__(b.to_owned(), vm).to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            or: Some(|a, b, vm| {
                if !AnySet::check(a, vm) || !AnySet::check(b, vm) {
                    return Ok(vm.ctx.not_implemented());
                }
                if let Some(a) = a.downcast_ref::<PyFrozenSet>() {
                    a.__or__(b.to_owned(), vm).to_pyresult(vm)
                } else if let Some(a) = a.downcast_ref::<PySet>() {
                    a.__or__(b.to_owned(), vm).to_pyresult(vm)
                } else {
                    Ok(vm.ctx.not_implemented())
                }
            }),
            ..PyNumberMethods::NOT_IMPLEMENTED
        };
        &AS_NUMBER
    }
}

impl Representable for PyFrozenSet {
    #[inline]
    fn repr_wtf8(zelf: &crate::Py<Self>, vm: &VirtualMachine) -> PyResult<Wtf8Buf> {
        let inner = &zelf.inner;
        let class = zelf.class();
        let class_name = class.name();
        if inner.len() == 0 {
            return Ok(Wtf8Buf::from(format!("{class_name}()")));
        }
        if let Some(_guard) = ReprGuard::enter(vm, zelf.as_object()) {
            inner.repr(Some(&class_name), vm)
        } else {
            Ok(Wtf8Buf::from(format!("{class_name}(...)")))
        }
    }
}

struct AnySet {
    object: PyObjectRef,
}

impl Borrow<PyObject> for AnySet {
    #[inline(always)]
    fn borrow(&self) -> &PyObject {
        &self.object
    }
}

impl AnySet {
    /// Check if object is a set or frozenset (including subclasses)
    /// Equivalent to CPython's PyAnySet_Check
    fn check(obj: &PyObject, vm: &VirtualMachine) -> bool {
        let ctx = &vm.ctx;
        obj.fast_isinstance(ctx.types.set_type) || obj.fast_isinstance(ctx.types.frozenset_type)
    }

    fn into_iterable(self, vm: &VirtualMachine) -> PyResult<ArgIterable> {
        self.object.try_into_value(vm)
    }

    fn into_iterable_iter(
        self,
        vm: &VirtualMachine,
    ) -> PyResult<impl core::iter::Iterator<Item = ArgIterable>> {
        Ok(core::iter::once(self.into_iterable(vm)?))
    }

    fn as_inner(&self) -> &PySetInner {
        match_class!(match self.object.as_object() {
            ref set @ PySet => &set.inner,
            ref frozen @ PyFrozenSet => &frozen.inner,
            _ => unreachable!("AnySet is always PySet or PyFrozenSet"), // should not be called.
        })
    }
}

impl TryFromObject for AnySet {
    fn try_from_object(vm: &VirtualMachine, obj: PyObjectRef) -> PyResult<Self> {
        let class = obj.class();
        if class.fast_issubclass(vm.ctx.types.set_type)
            || class.fast_issubclass(vm.ctx.types.frozenset_type)
        {
            Ok(Self { object: obj })
        } else {
            Err(vm.new_type_error(format!("{class} is not a subtype of set or frozenset")))
        }
    }
}

#[pyclass(module = false, name = "set_iterator")]
pub(crate) struct PySetIterator {
    /// The set's size when the iterator was made: `si_used`.
    used: usize,
    /// What `__length_hint__` answers: `si->len`, one less per key returned.
    remaining: PyAtomic<usize>,
    /// Whether the set was found to have changed, which `setiter_iternext()`
    /// records by writing a size no set can have. Sticky: what it makes the
    /// iterator answer, it answers from then on.
    changed: PyAtomic<bool>,
    /// The next table slot to look at: `si_pos`.
    internal: PyMutex<PositionIterInternal<AnySet>>,
}

impl fmt::Debug for PySetIterator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: implement more detailed, non-recursive Debug formatter
        f.write_str("set_iterator")
    }
}

impl PyPayload for PySetIterator {
    #[inline]
    fn class(ctx: &Context) -> &'static Py<PyType> {
        ctx.types.set_iterator_type
    }
}

impl PySetIterator {
    fn new(set: AnySet) -> Self {
        let used = set.as_inner().len();
        Self {
            used,
            remaining: Radium::new(used),
            changed: Radium::new(false),
            internal: PyMutex::new(PositionIterInternal::new(set, 0)),
        }
    }
}

#[pyclass(flags(DISALLOW_INSTANTIATION), with(IterNext, Iterable))]
impl Py<PySetIterator> {
    #[pymethod]
    fn __length_hint__(&self) -> usize {
        // `setiter_len()` answers for a set it can no longer walk with nothing,
        // comparing the size it captured against the set's own every time it is
        // asked.
        if self.changed.load(Ordering::Relaxed) {
            return 0;
        }
        match &self.internal.lock().status {
            IterStatus::Active(set) if set.as_inner().len() == self.used => {
                self.remaining.load(Ordering::Relaxed)
            }
            _ => 0,
        }
    }

    #[pymethod]
    fn __reduce__(
        zelf: PyRef<PySetIterator>,
        vm: &VirtualMachine,
    ) -> PyResult<(PyObjectRef, (PyObjectRef,))> {
        let internal = zelf.internal.lock();
        Ok((
            builtins_iter(vm)?,
            (vm.ctx
                .new_list(match &internal.status {
                    IterStatus::Exhausted => vec![],
                    IterStatus::Active(set) => set.as_inner().content.keys_from(internal.position),
                })
                .into(),),
        ))
    }
}

impl SelfIter for PySetIterator {}
impl IterNext for PySetIterator {
    fn next(zelf: &crate::Py<Self>, vm: &VirtualMachine) -> PyResult<PyIterReturn> {
        locked_step(&zelf.internal, |internal| {
            let IterStatus::Active(set) = &internal.status else {
                return (Ok(PyIterReturn::StopIteration(None)), None);
            };
            let mutated = || vm.new_runtime_error("Set changed size during iteration");
            if zelf.changed.load(Ordering::Relaxed) {
                // The set is not looked at again once it has been found to
                // change: an iterator that has raised keeps raising.
                return (Err(mutated()), None);
            }
            match set
                .as_inner()
                .content
                .next_checked(internal.position, zelf.used)
            {
                Err(SetChanged) => {
                    zelf.changed.store(true, Ordering::Relaxed);
                    (Err(mutated()), None)
                }
                Ok(Some((position, key))) => {
                    internal.position = position;
                    let left = zelf.remaining.load(Ordering::Relaxed);
                    zelf.remaining
                        .store(left.saturating_sub(1), Ordering::Relaxed);
                    (Ok(PyIterReturn::Return(key)), None)
                }
                Ok(None) => (Ok(PyIterReturn::StopIteration(None)), internal.exhaust()),
            }
        })
    }
}

fn vectorcall_set(
    zelf_obj: &PyObject,
    args: Vec<PyObjectRef>,
    nargs: usize,
    kwnames: Option<&[PyObjectRef]>,
    vm: &VirtualMachine,
) -> PyResult {
    let zelf: &Py<PyType> = zelf_obj.downcast_ref().unwrap();
    let obj = PySet::default().into_ref_with_type(vm, zelf.to_owned())?;
    let func_args = FuncArgs::from_vectorcall_owned(args, nargs, kwnames);
    PySet::slot_init(obj.as_object(), func_args, vm)?;
    Ok(obj.into())
}

fn vectorcall_frozenset(
    zelf_obj: &PyObject,
    args: Vec<PyObjectRef>,
    nargs: usize,
    kwnames: Option<&[PyObjectRef]>,
    vm: &VirtualMachine,
) -> PyResult {
    let zelf: &Py<PyType> = zelf_obj.downcast_ref().unwrap();
    let func_args = FuncArgs::from_vectorcall_owned(args, nargs, kwnames);
    (zelf.slots.new.load().unwrap())(zelf.to_owned(), func_args, vm)
}

pub(crate) fn init(context: &'static Context) {
    PySet::extend_class(context, context.types.set_type);
    context
        .types
        .set_type
        .slots
        .vectorcall
        .store(Some(vectorcall_set));

    PyFrozenSet::extend_class(context, context.types.frozenset_type);
    context
        .types
        .frozenset_type
        .slots
        .vectorcall
        .store(Some(vectorcall_frozenset));

    PySetIterator::extend_class(context, context.types.set_iterator_type);
}
