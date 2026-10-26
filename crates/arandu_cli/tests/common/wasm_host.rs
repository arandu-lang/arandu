//! Wasmtime host for core modules produced by `arandu emit-wasm`.
//!
//! The import contract mirrors the Node adapter used by the smith differential
//! oracle (`arandu_fuzz_support::smith::oracle::node_runner_script`): every
//! allocation import is routed back into the module's own `cabi_realloc`, so
//! the module's allocator remains the single owner of linear memory, and
//! `io.*` writes are captured instead of reaching the test process' stdout.
//! Unknown imports are a link error: a missing host function must fail the
//! test instead of being stubbed.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use wasmtime::{Caller, Engine, Extern, Instance, Linker, Module, Store};

#[derive(Default)]
struct Host {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Observable result of running `main`: its i32 result (or the trap) and the
/// bytes written through `io.print`/`io.println`/`io.eprint`.
pub struct WasmRun {
    pub result: Result<i32, String>,
    pub stdout: String,
    pub stderr: String,
}

impl WasmRun {
    /// Assert `main` returned `expected` without trapping.
    pub fn assert_returns(&self, expected: i32, context: &str) {
        match &self.result {
            Ok(code) => assert_eq!(
                *code, expected,
                "{context}: Wasm main returned {code}\nstdout: {}\nstderr: {}",
                self.stdout, self.stderr
            ),
            Err(trap) => panic!(
                "{context}: Wasm main trapped: {trap}\nstdout: {}\nstderr: {}",
                self.stdout, self.stderr
            ),
        }
    }

    /// Assert `main` stopped with a trap (the Wasm form of `intrinsics.abort`).
    pub fn assert_traps(&self, context: &str) {
        assert!(
            self.result.is_err(),
            "{context}: Wasm main returned {:?} instead of trapping\nstdout: {}",
            self.result,
            self.stdout
        );
    }
}

fn realloc(
    caller: &mut Caller<'_, Host>,
    pointer: i32,
    old_size: i32,
    align: i32,
    new_size: i32,
) -> wasmtime::Result<i32> {
    let function = caller
        .get_export("cabi_realloc")
        .and_then(Extern::into_func)
        .ok_or_else(|| wasmtime::Error::msg("module does not export cabi_realloc"))?;
    let typed = function.typed::<(i32, i32, i32, i32), i32>(&*caller)?;
    typed.call(caller, (pointer, old_size, align, new_size))
}

fn memory_bytes(
    caller: &mut Caller<'_, Host>,
    pointer: i32,
    len: i32,
) -> wasmtime::Result<Vec<u8>> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::Error::msg("module does not export memory"))?;
    let start = u32::try_from(pointer)? as usize;
    let size = u32::try_from(len)? as usize;
    let data = memory.data(&*caller);
    let end = start
        .checked_add(size)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| wasmtime::Error::msg("io write outside linear memory"))?;
    Ok(data[start..end].to_vec())
}

/// Port of the Node adapter's `ar_string_push_str`: `owner` addresses a
/// `{ data: u32, len: u32, capacity: u32 }` string header in linear memory.
fn push_str(
    caller: &mut Caller<'_, Host>,
    owner: i32,
    source: i32,
    count: i32,
) -> wasmtime::Result<i32> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::Error::msg("module does not export memory"))?;
    let (Ok(owner), Ok(source), Ok(count)) = (
        u32::try_from(owner),
        u32::try_from(source),
        u32::try_from(count),
    ) else {
        return Ok(0);
    };
    let (owner, source, count) = (owner as usize, source as usize, count as usize);
    let size = memory.data_size(&*caller);
    if owner == 0
        || owner + 12 > size
        || (count > 0 && (source == 0 || source + count > size))
        || count > 0x7fff_ffff
    {
        return Ok(0);
    }
    let word = |bytes: &[u8], at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };
    let (mut data, length, capacity, bytes) = {
        let memory_data = memory.data(&*caller);
        (
            word(memory_data, owner),
            word(memory_data, owner + 4),
            word(memory_data, owner + 8),
            memory_data[source..source + count].to_vec(),
        )
    };
    if length > capacity
        || (capacity > 0 && data == 0)
        || data > size
        || capacity > size - data
        || count > 0x7fff_ffff - length
    {
        return Ok(0);
    }
    let required = length + count;
    if required > capacity {
        let mut next = capacity.max(8);
        while next < required {
            next = if next > 0x3fff_ffff {
                0x7fff_ffff
            } else {
                next * 2
            };
        }
        let replacement = realloc(caller, data as i32, capacity as i32, 8, next as i32)?;
        let replacement = u32::try_from(replacement).unwrap_or(0) as usize;
        if replacement == 0 || next > memory.data_size(&*caller).saturating_sub(replacement) {
            return Ok(0);
        }
        data = replacement;
        let memory_data = memory.data_mut(&mut *caller);
        memory_data[owner..owner + 4].copy_from_slice(&(data as u32).to_le_bytes());
        memory_data[owner + 8..owner + 12].copy_from_slice(&(next as u32).to_le_bytes());
    }
    let memory_data = memory.data_mut(&mut *caller);
    memory_data[data + length..data + required].copy_from_slice(&bytes);
    memory_data[owner + 4..owner + 8].copy_from_slice(&(required as u32).to_le_bytes());
    Ok(1)
}

fn linker(engine: &Engine) -> Linker<Host> {
    let mut linker = Linker::new(engine);
    linker
        .func_wrap(
            "env",
            "ar_vec_malloc",
            |mut caller: Caller<'_, Host>, size: i32| realloc(&mut caller, 0, 0, 8, size),
        )
        .unwrap();
    linker
        .func_wrap(
            "env",
            "ar_vec_realloc",
            |mut caller: Caller<'_, Host>, pointer: i32, old: i32, new: i32| {
                realloc(&mut caller, pointer, old, 8, new)
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "env",
            "ar_vec_buf_free",
            |mut caller: Caller<'_, Host>, pointer: i32, size: i32| {
                realloc(&mut caller, pointer, size, 8, 0).map(|_| ())
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "env",
            "ar_rt_alloc_aligned",
            |mut caller: Caller<'_, Host>, size: i32, align: i32| {
                realloc(&mut caller, 0, 0, if align == 0 { 8 } else { align }, size)
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "env",
            "ar_rt_free_aligned",
            |mut caller: Caller<'_, Host>, pointer: i32, size: i32, align: i32| {
                realloc(
                    &mut caller,
                    pointer,
                    size,
                    if align == 0 { 8 } else { align },
                    0,
                )
                .map(|_| ())
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "env",
            "ar_string_push_str",
            |mut caller: Caller<'_, Host>, owner: i32, source: i32, count: i32| {
                push_str(&mut caller, owner, source, count)
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "io",
            "print",
            |mut caller: Caller<'_, Host>, pointer: i32, len: i32| {
                let bytes = memory_bytes(&mut caller, pointer, len)?;
                caller.data_mut().stdout.extend_from_slice(&bytes);
                Ok(())
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "io",
            "println",
            |mut caller: Caller<'_, Host>, pointer: i32, len: i32| {
                let bytes = memory_bytes(&mut caller, pointer, len)?;
                let stdout = &mut caller.data_mut().stdout;
                stdout.extend_from_slice(&bytes);
                stdout.push(b'\n');
                Ok(())
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "io",
            "eprint",
            |mut caller: Caller<'_, Host>, pointer: i32, len: i32| {
                let bytes = memory_bytes(&mut caller, pointer, len)?;
                caller.data_mut().stderr.extend_from_slice(&bytes);
                Ok(())
            },
        )
        .unwrap();
    linker
}

/// Instantiate an emitted core module and run its exported `main`.
///
/// Instantiation or link failures panic: they are backend/host contract
/// violations, distinct from a program-level trap reported in `result`.
pub fn run_main(bytes: &[u8]) -> WasmRun {
    assert!(
        arandu_backend_wasm::is_core_module(bytes),
        "emitted Wasm must be a valid core module"
    );
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("emitted module must compile");
    let mut store = Store::new(&engine, Host::default());
    let instance: Instance = linker(&engine)
        .instantiate(&mut store, &module)
        .expect("emitted module must link against the Arandu Wasm host");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .expect("module must export `main: () -> i32`");
    let result = main
        .call(&mut store, ())
        .map_err(|trap| format!("{trap:?}"));
    let host = store.into_data();
    WasmRun {
        result,
        stdout: String::from_utf8_lossy(&host.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&host.stderr).into_owned(),
    }
}
