//! test262's agents (`$262.agent`): each one a thread with a VM of its
//! own, which shares memory with the main one through `SharedArrayBuffer`s
//! broadcast to it (see `shared`).
//!
//! The main agent's `start(source)` starts one running `source`; there,
//! `receiveBroadcast(callback)` names what gets a buffer the main agent
//! `broadcast`s (it waits for every agent to have taken it), and
//! `report(value)` queues a string the main agent's `getReport()` takes
//! (`null` if there is none yet), and `leaving()` ends the agent once the
//! callback has returned (and its event loop run out). Both sides have
//! `sleep(ms)` and `monotonicNow()`.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::builtins::is_class;
use crate::object::{Class, Object, as_object, object_value};
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, string_units};

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// A broadcast: the buffer's block (a reference to it for the agent), and
/// whether it's growable, and the number sent with it.
struct Message {
    block: usize,
    growable: bool,
    id: Value,
}

// (A number or `undefined`: no cell.)
unsafe impl Send for Message {}

/// An agent's mailbox: the broadcast it hasn't taken yet.
struct Mailbox {
    message: Mutex<Option<Message>>,
    cv: Condvar,
}

/// The agents started, the reports they made, and when the process
/// started (`monotonicNow`).
static AGENTS: Mutex<Vec<Arc<Mailbox>>> = Mutex::new(Vec::new());
static REPORTS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An agent's VM's part: its `receiveBroadcast` callback, and whether it
/// is leaving.
#[derive(Default)]
pub(crate) struct State {
    callback: Option<Value>,
    leaving: bool,
}

impl State {
    pub(crate) fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        self.callback.iter().copied()
    }
}

fn source_of(v: Value) -> Result<String> {
    match string_units(v) {
        Some(u) => Ok(crate::vm::source_text(u)),
        None => Err(Error::type_error("$262.agent.start: the source is not a string")),
    }
}

/// `$262.agent.start(source)`.
fn agent_start(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let src = source_of(arg(a, 0))?;
    let mailbox = Arc::new(Mailbox { message: Mutex::new(None), cv: Condvar::new() });
    lock(&AGENTS).push(mailbox.clone());
    std::thread::Builder::new()
        .name("agent".into())
        .stack_size(64 << 20)
        .spawn(move || run_agent(&src, &mailbox))
        .map_err(|e| Error::type_error(format!("$262.agent.start: {e}")))?;
    Ok(Value::UNDEFINED)
}

/// An agent's thread: its script, then each broadcast's callback, until
/// it leaves (or throws).
fn run_agent(src: &str, mailbox: &Arc<Mailbox>) {
    run_agent_vm(src, mailbox);
    lock(&AGENTS).retain(|m| !Arc::ptr_eq(m, mailbox));
    // (A broadcast sent meanwhile.)
    if let Some(m) = lock(&mailbox.message).take() {
        crate::shared::release(m.block as *mut crate::shared::Block);
        mailbox.cv.notify_all();
    }
}

fn run_agent_vm(src: &str, mailbox: &Mailbox) {
    let mut vm = Vm::new();
    let module = match crate::js::compile(src) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("agent: SyntaxError: {e}");
            return;
        }
    };
    if let Err(e) = vm.run(&module) {
        eprintln!("agent: uncaught {}", vm.uncaught_message(&e));
        return;
    }
    loop {
        let m = {
            let mut slot = lock(&mailbox.message);
            loop {
                if let Some(m) = slot.take() {
                    mailbox.cv.notify_all();
                    break m;
                }
                slot = mailbox.cv.wait(slot).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(callback) = vm.agent.callback else {
            crate::shared::release(m.block as *mut crate::shared::Block);
            continue;
        };
        let r = vm.enter(|vm| {
            let sab = crate::shared::new_shared(vm, m.block as *mut crate::shared::Block, m.growable)?;
            vm.handles.push(sab);
            crate::interp::call_function(vm, callback, Value::UNDEFINED, &[sab, m.id])
        });
        if let Err(e) = r {
            eprintln!("agent: uncaught {}", vm.uncaught_message(&e));
            return;
        }
        if vm.agent.leaving {
            return;
        }
    }
}

/// `$262.agent.broadcast(sab, id)`: to every agent, once each has taken
/// it.
fn agent_broadcast(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let b = arg(a, 0);
    if !is_class(b, Class::SharedArrayBuffer) {
        return Err(Error::type_error("$262.agent.broadcast: not a SharedArrayBuffer"));
    }
    let o: *mut Object = as_object(b);
    let block = crate::shared::block_of(o);
    let growable = crate::typed::is_resizable(o);
    let id = arg(a, 1);
    let id = if id.is_number() { Value::number(id.to_number()) } else { Value::UNDEFINED };
    let agents: Vec<Arc<Mailbox>> = lock(&AGENTS).clone();
    for m in &agents {
        crate::shared::retain(block);
        let mut slot = lock(&m.message);
        // (The one before taken first.)
        while slot.is_some() {
            slot = m.cv.wait(slot).unwrap_or_else(|e| e.into_inner());
        }
        *slot = Some(Message { block: block as usize, growable, id });
        m.cv.notify_all();
    }
    for m in &agents {
        let mut slot = lock(&m.message);
        while slot.is_some() {
            slot = m.cv.wait(slot).unwrap_or_else(|e| e.into_inner());
        }
    }
    Ok(Value::UNDEFINED)
}

/// `$262.agent.getReport()`: the oldest report not taken, or `null`.
fn agent_get_report(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let r = lock(&REPORTS).pop_front();
    Ok(match r {
        Some(s) => {
            let u: Vec<u16> = s.encode_utf16().collect();
            vm.new_string(&u)
        }
        None => Value::NULL,
    })
}

/// `$262.agent.receiveBroadcast(callback)`.
fn agent_receive_broadcast(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    vm.agent.callback = Some(arg(a, 0));
    Ok(Value::UNDEFINED)
}

/// `$262.agent.report(value)`: its string queued.
fn agent_report(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let s = crate::intrinsics::to_string(vm, arg(a, 0))?;
    let s = String::from_utf16_lossy(string_units(s).unwrap_or(&[]));
    lock(&REPORTS).push_back(s);
    Ok(Value::UNDEFINED)
}

fn agent_leaving(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    vm.agent.leaving = true;
    Ok(Value::UNDEFINED)
}

/// `$262.agent.sleep(ms)`.
fn agent_sleep(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let ms = crate::typed::integer(vm, arg(a, 0))?;
    if ms > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(ms.min(1e9) / 1000.0));
    }
    Ok(Value::UNDEFINED)
}

/// `$262.agent.monotonicNow()`: milliseconds since the process started.
fn agent_monotonic_now(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let t = EPOCH.get_or_init(Instant::now).elapsed();
    Ok(Value::number(t.as_millis() as f64))
}

/// `$262.agent`, on `$262` (`host`).
pub(crate) fn install(vm: &mut Vm, host: *mut Object) {
    EPOCH.get_or_init(Instant::now);
    let op = object_value(vm.object_prototype);
    let agent = vm.new_object(op);
    let a_agent = vm.intern("agent");
    vm.add_property_with(host, a_agent, object_value(agent), crate::object::DONT_ENUM);
    vm.define_natives(
        agent,
        &[
            ("start", agent_start as NativeFn, 1),
            ("broadcast", agent_broadcast, 2),
            ("getReport", agent_get_report, 0),
            ("receiveBroadcast", agent_receive_broadcast, 1),
            ("report", agent_report, 1),
            ("leaving", agent_leaving, 0),
            ("sleep", agent_sleep, 1),
            ("monotonicNow", agent_monotonic_now, 0),
        ],
    );
}
