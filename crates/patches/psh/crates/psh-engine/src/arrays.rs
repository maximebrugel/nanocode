//! Array assignment and access on a vproc's variables (D1260): `a[i]=v`, `a=(…)`,
//! `a+=(…)`, element lookup with negative indices, `unset a[i]`.
//!
//! Subscripts of indexed arrays are arithmetic; of associative arrays, plain strings.
//! Errors are bash's messages without the `line N:` prefix.

use std::collections::BTreeMap;

use crate::state::{Vp, apply_case};
use crate::vars::{Assoc, Value};

/// One element of a compound assignment `(…)`, expanded: `[key]=value`, `[key]+=value`
/// or a plain `value`.
#[derive(Clone, Debug)]
pub(crate) struct ArrayItem {
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub append: bool,
    /// The element as bash shows it in a warning (`[""]=1`), when it has a key.
    pub src: Option<Vec<u8>>,
}

impl Vp {
    /// Applies `-i` and `-l`/`-u` of `name` to an element value; `old` is the element's
    /// current value for `+=`.
    fn convert(
        &mut self,
        name: &str,
        value: Vec<u8>,
        old: Option<Vec<u8>>,
        append: bool,
    ) -> Result<Vec<u8>, String> {
        let (integer, case) = match self.vars.get(name) {
            Some(v) => (v.integer, v.case),
            None => (false, crate::vars::Case::None),
        };
        if integer {
            let mut n = crate::arith::eval(self, &value)?;
            if append {
                n = n.wrapping_add(crate::arith::eval(self, &old.unwrap_or_default())?);
            }
            return Ok(n.to_string().into_bytes());
        }
        let value = if append {
            let mut o = old.unwrap_or_default();
            o.extend_from_slice(&value);
            o
        } else {
            value
        };
        Ok(apply_case(case, value))
    }

    fn readonly_check(&self, target: &str, name: &str) -> Result<(), String> {
        if self.vars.get(target).is_some_and(|v| v.readonly) {
            Err(format!("{name}: readonly variable"))
        } else {
            Ok(())
        }
    }

    /// The highest index of an indexed array (`None` when empty or not an array).
    fn max_index(&self, name: &str) -> Option<i64> {
        match self.vars.get(name).map(|v| &v.value) {
            Some(Value::Indexed(m)) => m.keys().next_back().copied(),
            Some(Value::Str(_)) => Some(0),
            _ => None,
        }
    }

    /// Evaluates an indexed subscript; negative counts from the end. `None` when it
    /// points before the start.
    pub(crate) fn resolve_index(&mut self, name: &str, sub: &[u8]) -> Result<Option<i64>, String> {
        let i = self.sub_arith(sub)?;
        if i >= 0 {
            return Ok(Some(i));
        }
        Ok(self.max_index(name).map(|m| m + 1 + i).filter(|&i| i >= 0))
    }

    /// The arithmetic value of a subscript; an error in the expression itself marks
    /// [`Vp::sub_fatal`].
    pub(crate) fn sub_arith(&mut self, sub: &[u8]) -> Result<i64, String> {
        crate::arith::eval(self, sub).inspect_err(|_| {
            if self.arith_syntax {
                self.sub_fatal = true;
            }
        })
    }

    /// Like [`Vp::resolve_index`] for a reference: a negative index counts from the
    /// end of an indexed array only; on a scalar it points nowhere (bash).
    pub(crate) fn resolve_read(&mut self, name: &str, sub: &[u8]) -> Result<Option<i64>, String> {
        let i = self.sub_arith(sub)?;
        if i >= 0 {
            return Ok(Some(i));
        }
        Ok(match self.vars.get(name).map(|v| &v.value) {
            Some(Value::Indexed(m)) => m.keys().next_back().map(|&m| m + 1 + i),
            _ => None,
        }
        .filter(|&i| i >= 0))
    }

    /// `name[sub]=value` (or `+=`).
    pub(crate) fn assign_elem(
        &mut self,
        name: &str,
        sub: &[u8],
        value: Vec<u8>,
        append: bool,
    ) -> Result<(), String> {
        self.sub_fatal = false;
        let shown = name;
        let name = self.resolve(name).into_owned();
        if name.contains('[') {
            return Err(format!("`{name}': not a valid identifier"));
        }
        self.readonly_check(&name, shown)?;
        if self.vars.get(&name).is_some_and(|v| v.is_assoc()) {
            let old = match self.vars.get(&name).map(|v| &v.value) {
                Some(Value::Assoc(a)) => a.get(sub).map(<[u8]>::to_vec),
                _ => None,
            };
            let v = self.convert(&name, value, old, append)?;
            let var = self.vars.entry(&name);
            var.declared_only = false;
            let aliases = var.dynamic == Some(crate::vars::Dynamic::BashAliases);
            if let Value::Assoc(a) = &mut var.value {
                a.insert(sub.to_vec(), v.clone());
            }
            if aliases {
                self.aliases.set(sub, &v);
            }
            return Ok(());
        }
        let all = sub == b"@" || sub == b"*";
        let Some(i) = (match all {
            true => None,
            false => self.resolve_index(&name, sub)?,
        }) else {
            return Err(format!(
                "{name}[{}]: bad array subscript",
                String::from_utf8_lossy(sub)
            ));
        };
        let old = self.elem(&name, i);
        let v = self.convert(&name, value, old, append)?;
        let var = self.vars.entry(&name);
        var.declared_only = false;
        to_indexed(&mut var.value).insert(i, v);
        Ok(())
    }

    pub(crate) fn elem(&self, name: &str, i: i64) -> Option<Vec<u8>> {
        match self.vars.get(name).map(|v| &v.value) {
            Some(Value::Indexed(m)) => m.get(&i).cloned(),
            Some(Value::Str(s)) if i == 0 => Some(s.clone()),
            _ => None,
        }
    }

    /// `name=(…)` or `name+=(…)`. Indexed unless the variable is associative.
    pub(crate) fn assign_array(
        &mut self,
        name: &str,
        items: Vec<ArrayItem>,
        append: bool,
    ) -> Result<(), String> {
        self.sub_fatal = false;
        let shown = name;
        let name = self.resolve(name).into_owned();
        if name.contains('[') {
            return Err(format!("`{name}': not a valid identifier"));
        }
        self.readonly_check(&name, shown)?;
        if self.vars.get(&name).is_some_and(|v| v.is_assoc()) {
            let var = self.vars.entry(&name);
            var.declared_only = false;
            if !append {
                var.value = Value::Assoc(Assoc::default());
            }
            // Without keys, the items are key-value pairs (bash 5.1+).
            let pairs = items.first().is_some_and(|i| i.key.is_none());
            let mut pending_key: Option<Vec<u8>> = None;
            for item in items {
                let (key, value, app) = match item.key {
                    // An empty key skips its element with a warning (bash).
                    Some(k) if k.is_empty() => {
                        let src = item.src.unwrap_or_else(|| {
                            let mut t = b"[]=".to_vec();
                            t.extend_from_slice(&item.value);
                            t
                        });
                        self.arith_notes.push(format!(
                            "{}: bad array subscript",
                            String::from_utf8_lossy(&src)
                        ));
                        continue;
                    }
                    Some(k) => (k, item.value, item.append),
                    // A bad element is skipped with a warning; the rest are assigned (bash).
                    None if !pairs => {
                        self.arith_notes.push(format!(
                            "{name}: {}: must use subscript when assigning associative array",
                            String::from_utf8_lossy(&item.value)
                        ));
                        continue;
                    }
                    None => match pending_key.take() {
                        Some(k) => (k, item.value, false),
                        None => {
                            pending_key = Some(item.value);
                            continue;
                        }
                    },
                };
                let old = match self.vars.get(&name).map(|v| &v.value) {
                    Some(Value::Assoc(a)) if append => a.get(&key).map(<[u8]>::to_vec),
                    _ => None,
                };
                let v = self.convert(&name, value, old, app)?;
                if let Value::Assoc(a) = &mut self.vars.entry(&name).value {
                    a.insert(key, v);
                }
            }
            if let Some(k) = pending_key
                && let Value::Assoc(a) = &mut self.vars.entry(&name).value
            {
                a.insert(k, Vec::new());
            }
            return Ok(());
        }
        let var = self.vars.entry(&name);
        var.declared_only = false;
        match append {
            true => {
                to_indexed(&mut var.value);
            }
            false => var.value = Value::Indexed(BTreeMap::new()),
        }
        let mut next = self.max_index(&name).map_or(0, |k| k + 1);
        for item in items {
            let i = match &item.key {
                // A bad subscript skips its element with a warning (bash).
                Some(k) if k == b"@" || k == b"*" => {
                    self.arith_notes.push(format!(
                        "[{}]={}: cannot assign to non-numeric index",
                        String::from_utf8_lossy(k),
                        String::from_utf8_lossy(&item.value)
                    ));
                    continue;
                }
                Some(k) => match (k.is_empty(), self.resolve_index(&name, k)?) {
                    (false, Some(i)) => i,
                    _ => {
                        self.arith_notes.push(format!(
                            "[{}]={}: bad array subscript",
                            String::from_utf8_lossy(k),
                            String::from_utf8_lossy(&item.value)
                        ));
                        continue;
                    }
                },
                None => next,
            };
            let old = self.elem(&name, i);
            let v = self.convert(&name, item.value, old, item.append)?;
            if let Value::Indexed(m) = &mut self.vars.entry(&name).value {
                m.insert(i, v);
            }
            next = i + 1;
        }
        Ok(())
    }

    pub(crate) fn drop_nameref(&mut self, name: &str) -> Option<String> {
        if !self.vars.get(name).is_some_and(|v| v.nameref) {
            return None;
        }
        self.vars.entry(name).nameref = false;
        Some(format!("warning: {name}: removing nameref attribute"))
    }

    /// `${name[sub]}`: the element, `None` if unset. Errors for subscripts before the
    /// start of an indexed array.
    pub(crate) fn get_elem(&mut self, name: &str, sub: &[u8]) -> Result<Option<Vec<u8>>, String> {
        self.sub_fatal = false;
        let name = self.resolve(name).into_owned();
        match self.vars.get(&name).map(|v| &v.value) {
            Some(Value::Assoc(_)) if sub.is_empty() => {
                return Err(format!("{name}: bad array subscript"));
            }
            Some(Value::Assoc(a)) => return Ok(a.get(sub).map(<[u8]>::to_vec)),
            None | Some(Value::Unset) => {
                // Still evaluated, for its side effects and errors.
                self.sub_arith(sub)?;
                return Ok(None);
            }
            _ => {}
        }
        match self.resolve_read(&name, sub)? {
            Some(i) => Ok(self.elem(&name, i)),
            None => Err(format!("{name}: bad array subscript")),
        }
    }

    /// `unset name[sub]`.
    pub(crate) fn unset_elem(&mut self, name: &str, sub: &[u8]) -> Result<(), String> {
        self.sub_fatal = false;
        let name = self.resolve(name).into_owned();
        // An unset variable: nothing to do, and the subscript is not evaluated (bash).
        if self.vars.get(&name).is_none() {
            return Ok(());
        }
        if self.vars.get(&name).is_some_and(|v| v.readonly) {
            return Err(format!("{name}: cannot unset: readonly variable"));
        }
        let assoc = self.vars.get(&name).is_some_and(|v| v.is_assoc());
        if (sub == b"@" || sub == b"*") && !assoc {
            return match self.vars.get_mut(&name).map(|v| &mut v.value) {
                Some(Value::Indexed(m)) => {
                    m.clear();
                    Ok(())
                }
                Some(_) => Err(format!("{name}: not an array variable")),
                None => Ok(()),
            };
        }
        if assoc {
            if let Some(Value::Assoc(a)) = self.vars.get_mut(&name).map(|v| &mut v.value) {
                a.remove(sub);
            }
            return Ok(());
        }
        let indexed = matches!(
            self.vars.get(&name).map(|v| &v.value),
            Some(Value::Indexed(_))
        );
        let Some(i) = self.resolve_read(&name, sub)? else {
            if !indexed {
                return Err(format!("{name}: not an array variable"));
            }
            return Err(format!(
                "[{}]: bad array subscript",
                String::from_utf8_lossy(sub)
            ));
        };
        match self.vars.get_mut(&name).map(|v| &mut v.value) {
            Some(Value::Indexed(m)) => {
                m.remove(&i);
            }
            // `unset 'a[0]'` on a scalar unsets it, as `unset a` does.
            Some(Value::Str(_) | Value::Unset) if i == 0 => {
                self.vars.remove(&name);
            }
            Some(Value::Str(_) | Value::Unset) => {
                return Err(format!("{name}: not an array variable"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Makes a value an indexed array (a scalar becomes element 0) and returns its map.
pub(crate) fn to_indexed(v: &mut Value) -> &mut BTreeMap<i64, Vec<u8>> {
    if !matches!(v, Value::Indexed(_)) {
        let m = match std::mem::take(v) {
            Value::Str(s) => BTreeMap::from([(0, s)]),
            _ => BTreeMap::new(),
        };
        *v = Value::Indexed(m);
    }
    match v {
        Value::Indexed(m) => m,
        _ => unreachable!(),
    }
}
