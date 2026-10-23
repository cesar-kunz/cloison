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
//!
//! Agents belong to the VM that starts them: its broadcasts reach those
//! only, and their reports only it; dropping it closes their mailboxes
//! (one waiting for a broadcast ends) and joins their threads, those that
//! end within [`JOIN_WAIT`]. At most [`MAX_AGENTS`] run at once per VM.

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

/// An agent's mailbox: the broadcast it hasn't taken yet, and whether
/// the agent has ended (then it takes none: a broadcast skips it).
struct Mailbox {
    slot: Mutex<Slot>,
    cv: Condvar,
}

#[derive(Default)]
struct Slot {
    message: Option<Message>,
    closed: bool,
}

impl Mailbox {
    /// Ended: no broadcast waits for it any more (one it had not taken
    /// released).
    fn close(&self) {
        let mut slot = lock(&self.slot);
        slot.closed = true;
        if let Some(m) = slot.message.take() {
            crate::shared::release(m.block as *mut crate::shared::Block);
        }
        self.cv.notify_all();
    }
}

/// When the process started (`monotonicNow`).
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Agents one VM runs at once, at most: `start` past them is a
/// `TypeError` (a script starting agents in a loop made a thread, with
/// a 64 MiB stack and a heap, each time).
pub const MAX_AGENTS: usize = 64;
/// How long dropping a VM waits for its agents' threads to end, in all:
/// one still running then (a script that doesn't return, a callback
/// blocked) is left to end on its own.
pub const JOIN_WAIT: Duration = Duration::from_secs(1);

/// An agent a VM started: its mailbox and its thread.
struct Started {
    mailbox: Arc<Mailbox>,
    thread: std::thread::JoinHandle<()>,
}

type Reports = Arc<Mutex<VecDeque<String>>>;

/// A VM's part: the agents it started and the reports they made; if it
/// is an agent, its `receiveBroadcast` callback, whether it is leaving,
/// and where its reports go (its starter's).
#[derive(Default)]
pub(crate) struct State {
    callback: Option<Value>,
    leaving: bool,
    started: Vec<Started>,
    reports: Reports,
    report_to: Option<Reports>,
}

impl Drop for State {
    fn drop(&mut self) {
        for a in &self.started {
            a.mailbox.close();
        }
        let deadline = Instant::now() + JOIN_WAIT;
        for a in std::mem::take(&mut self.started) {
            while !a.thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if a.thread.is_finished() {
                let _ = a.thread.join();
            }
        }
    }
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
fn agent_start(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let src = source_of(arg(a, 0))?;
    // (Those that ended are joined.)
    let (ended, running): (Vec<_>, Vec<_>) =
        std::mem::take(&mut vm.agent.started).into_iter().partition(|a| a.thread.is_finished());
    vm.agent.started = running;
    for a in ended {
        let _ = a.thread.join();
    }
    if vm.agent.started.len() >= MAX_AGENTS {
        return Err(Error::type_error(format!("$262.agent.start: more than {MAX_AGENTS} agents")));
    }
    let mailbox = Arc::new(Mailbox { slot: Mutex::new(Slot::default()), cv: Condvar::new() });
    let (agent, reports) = (mailbox.clone(), vm.agent.reports.clone());
    let spawned = std::thread::Builder::new()
        .name("agent".into())
        .stack_size(64 << 20)
        .spawn(move || run_agent(&src, &agent, reports));
    match spawned {
        Ok(thread) => vm.agent.started.push(Started { mailbox, thread }),
        Err(e) => return Err(Error::type_error(format!("$262.agent.start: {e}"))),
    }
    Ok(Value::UNDEFINED)
}

/// An agent's thread: its script, then each broadcast's callback, until
/// it leaves (or throws, or its starter is dropped).
fn run_agent(src: &str, mailbox: &Arc<Mailbox>, reports: Reports) {
    run_agent_vm(src, mailbox, reports);
    // (A broadcaster may be posting to it: closed, under the lock it
    // posts and waits under, it takes nothing.)
    mailbox.close();
}

fn run_agent_vm(src: &str, mailbox: &Mailbox, reports: Reports) {
    let mut vm = Vm::new();
    vm.agent.report_to = Some(reports);
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
            let mut slot = lock(&mailbox.slot);
            loop {
                if let Some(m) = slot.message.take() {
                    mailbox.cv.notify_all();
                    break m;
                }
                if slot.closed {
                    return;
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
fn agent_broadcast(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let b = arg(a, 0);
    if !is_class(b, Class::SharedArrayBuffer) {
        return Err(Error::type_error("$262.agent.broadcast: not a SharedArrayBuffer"));
    }
    let o: *mut Object = as_object(b);
    let block = crate::shared::block_of(o);
    let growable = crate::typed::is_resizable(o);
    let id = arg(a, 1);
    let id = if id.is_number() { Value::number(id.to_number()) } else { Value::UNDEFINED };
    let agents: Vec<Arc<Mailbox>> = vm.agent.started.iter().map(|a| a.mailbox.clone()).collect();
    // (An agent that ends meanwhile is closed: skipped, not waited for.)
    for m in &agents {
        let mut slot = lock(&m.slot);
        // (The one before taken first.)
        while slot.message.is_some() && !slot.closed {
            slot = m.cv.wait(slot).unwrap_or_else(|e| e.into_inner());
        }
        if slot.closed {
            continue;
        }
        crate::shared::retain(block);
        slot.message = Some(Message { block: block as usize, growable, id });
        m.cv.notify_all();
    }
    for m in &agents {
        let mut slot = lock(&m.slot);
        while slot.message.is_some() && !slot.closed {
            slot = m.cv.wait(slot).unwrap_or_else(|e| e.into_inner());
        }
    }
    Ok(Value::UNDEFINED)
}

/// `$262.agent.getReport()`: the oldest report not taken, or `null`.
fn agent_get_report(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let r = lock(&vm.agent.reports).pop_front();
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

/// `$262.agent.report(value)`: its string queued, for the agent's
/// starter (a VM no agent's: its own).
fn agent_report(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let s = crate::intrinsics::to_string(vm, arg(a, 0))?;
    let s = String::from_utf16_lossy(string_units(s).unwrap_or(&[]));
    lock(vm.agent.report_to.as_ref().unwrap_or(&vm.agent.reports)).push_back(s);
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
