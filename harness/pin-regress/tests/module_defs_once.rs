//! ddd5f7a60: python-capi builds each extension's module definition once per process.

use rustpython_vm::Interpreter;

const INTERPRETERS: usize = 200;

#[test]
fn module_defs_are_built_once_per_process() {
    let mut first: Option<Vec<usize>> = None;
    for at in 0..INTERPRETERS {
        let builder = Interpreter::builder(pin_regress::settings());
        let defs = nanocode_python_capi::module_defs(&builder.ctx);
        let addresses: Vec<usize> = defs
            .iter()
            .map(|def| *def as *const _ as usize)
            .collect();
        assert!(!addresses.is_empty(), "python-capi defines no extensions");
        let interpreter = builder.add_native_modules(&defs).build();
        drop(interpreter);
        match &first {
            None => first = Some(addresses),
            Some(first) => assert_eq!(
                first, &addresses,
                "interpreter {at} got new module definitions: they are built per interpreter"
            ),
        }
    }
}
