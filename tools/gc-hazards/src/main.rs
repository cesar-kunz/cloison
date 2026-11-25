//! A GC hazard analysis of cloison's Rust code, on MIR: a value that holds
//! a cell (a `Value`, an `Obj`, anything holding one) in a Rust local, live
//! across a call that may collect, and rooted nowhere else.
//!
//! Since Rust frames' native stack words no longer mark (`pin`, the
//! default, and `drop`, CI's checker; see docs/gc.md), such a value is a
//! use after free. The scripts under `CLOISON_GC_NO_NATIVE_ROOTS=drop` find
//! the ones their paths reach; this finds them on every path.
//!
//! The analysis, per function, then to a fixed point over all of them:
//! - *May collect*: a function that calls `Vm::collect` (or
//!   `collect_from`, `collect_full_now`), or a function that may, or
//!   calls through a function pointer or `dyn` (a native, a callback).
//! - *Rootedness* of each local, forward: rooted (a constant, read
//!   through the `Vm` or a `Handle`, put in `vm.handles`, in a live
//!   `brand::Scope` — `sc.root(v)`, `fresh.root(&sc)` — or a live
//!   `root_scope`), fresh (an allocation's, or what a call returns from
//!   one: a `Fresh`'s, taken or not), or as rooted as some parameters
//!   are (which then makes the callers' arguments the question). What
//!   holds no cell is rooted when read out of a tuple or struct (a
//!   `(Handle, f64)`'s number); a `Result`'s `Ok` part is apart from its
//!   error's (the thrown value `?` returns).
//! - *Liveness*, backward, with what references point to: a local is
//!   live after a call if it, or a reference to it, is used later.
//! - A *hazard*: a fresh local live after a call that may collect; or a
//!   fresh value passed to a parameter its callee holds across one; or
//!   captured by a closure that may collect.
//!
//! It runs as a rustc driver (`scripts/gc-hazards.sh`), on the `cloison`
//! library only, and passes every other crate to rustc. An annotation
//! naming no function fails it (a renamed one would assert nothing).
//! `GC_HAZARDS_DUMP=f,g` prints the analysis of functions `f`, `g` (paths
//! as rustc prints them, or what follows their impl's `>::`). It is a
//! heuristic, not a proof: what a value read from a heap object needs is
//! that object (not checked to stay its holder), and calls it can't
//! resolve are taken not to collect unless through a pointer.
#![feature(rustc_private)]

extern crate rustc_abi;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::def::DefKind;
use rustc_hir::def_id::DefId;
use rustc_middle::mir::*;
use rustc_middle::ty::{self, Instance, InstanceKind, Ty, TyCtxt, TypingEnv};
use rustc_span::Span;
use std::cell::RefCell;
use std::fmt::Write as _;

/// A value's rootedness: `un` — may be unrooted (fresh); `params` — as
/// rooted as these parameters (bits) are; `guards` — rooted while these
/// `root_scope` guards (bits) live. All clear: rooted.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
struct St {
    init: bool,
    un: bool,
    params: u64,
    /// Fresh from these calls (bits: the body's call sites).
    fresh: u64,
    guards: u64,
    /// The origins its guards root meanwhile (back when they die).
    gfresh: u64,
    gparams: u64,
    /// A new ordinary object (no property defined on it runs JS).
    plain: bool,
    /// An object no JS sees (an iterator's state array, all its elements
    /// own data properties): reading and writing them run no JS.
    internal: bool,
    /// A `Result` (or `?`'s `ControlFlow`) whose `Ok` part is only as
    /// unrooted as `ok_*` say: the rest is its error's (a thrown value),
    /// which `?` returns. The fields above are both parts'.
    split: bool,
    ok_un: bool,
    ok_fresh: u64,
    ok_params: u64,
}

const ROOTED: St = St {
    init: true,
    un: false,
    params: 0,
    fresh: 0,
    guards: 0,
    gfresh: 0,
    gparams: 0,
    plain: false,
    internal: false,
    split: false,
    ok_un: false,
    ok_fresh: 0,
    ok_params: 0,
};
const FRESH: St = St { un: true, ..ROOTED };

impl St {
    fn join(self, o: St) -> St {
        if !self.init {
            return o;
        }
        if !o.init {
            return self;
        }
        let mut j = St {
            init: true,
            un: self.un | o.un,
            params: self.params | o.params,
            fresh: self.fresh | o.fresh,
            guards: self.guards | o.guards,
            gfresh: self.gfresh | o.gfresh,
            gparams: self.gparams | o.gparams,
            plain: self.plain & o.plain,
            internal: self.internal & o.internal,
            ..ROOTED
        };
        if self.split || o.split {
            let (a, b) = (self.ok(), o.ok());
            j.split = true;
            j.ok_un = a.un | b.un;
            j.ok_fresh = a.fresh | b.fresh;
            j.ok_params = a.params | b.params;
        }
        j
    }
    /// The `Ok` part's state.
    fn ok(self) -> St {
        if !self.split {
            return self;
        }
        St { un: self.ok_un, fresh: self.ok_fresh, params: self.ok_params, split: false, ..self }
    }
    /// All of it the error part's (an `Err`, `?`'s residual).
    fn err_only(self) -> St {
        St { split: true, ok_un: false, ok_fresh: 0, ok_params: 0, ..self }
    }
    /// Not split: both parts' (an error itself).
    fn whole(self) -> St {
        St { split: false, ok_un: false, ok_fresh: 0, ok_params: 0, ..self }
    }
    /// May be unrooted.
    fn unrooted(self) -> bool {
        self.init && (self.un || self.fresh != 0)
    }
    /// Rooted, or rooted while guards live.
    fn rooted(self) -> bool {
        !self.init || !self.un && self.fresh == 0 && self.params == 0
    }
    fn fresh_at(site: Option<usize>) -> St {
        match site {
            Some(i) => St { fresh: 1 << i, ..ROOTED },
            None => FRESH,
        }
    }
    fn param(i: usize) -> St {
        if i < 64 { St { params: 1 << i, ..ROOTED } } else { ROOTED }
    }
}

/// What a function's callers see of it.
#[derive(Clone, PartialEq, Default, Debug)]
struct Summary {
    may_gc: bool,
    /// The callee that makes it collect (for the explanation).
    via: Option<DefId>,
    /// Parameters (and, for a closure, captures: bit `args + k`) it holds
    /// across a collection: its callers must pass them rooted.
    needs_root: u64,
    /// What it returns, in its parameters' terms.
    ret: St,
    /// What it stores through each parameter (a `&mut`), in their terms.
    out: Vec<St>,
    args: usize,
}

#[derive(Clone)]
struct Hazard {
    span: Span,
    func: String,
    what: String,
    ty: String,
    callee: String,
    chain: String,
    kind: &'static str,
}

/// What `annotations.txt` asserts.
#[derive(Default)]
struct Annotations {
    /// Functions (or prefixes, `…*`) asserted not to collect.
    no_gc: Vec<String>,
    /// Functions whose result is held by an argument (its index).
    holds: FxHashMap<String, usize>,
    /// Functions whose result is a new ordinary object.
    plain: FxHashSet<String>,
    /// Functions that collect only through an exotic receiver (this
    /// argument): not given a new ordinary object.
    exotic_gc: FxHashMap<String, usize>,
    /// Functions whose result is an object no JS sees.
    internal: FxHashSet<String>,
    /// Functions whose result is never a cell.
    no_cell: FxHashSet<String>,
    /// Functions that collect only through a receiver JS may see.
    internal_gc: FxHashMap<String, usize>,
    /// Functions whose result, from a receiver JS doesn't see, it holds.
    internal_holds: FxHashMap<String, usize>,
}

impl Annotations {
    fn read(path: &str) -> Annotations {
        let mut a = Annotations::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("gc-hazards: no annotations at {path}");
            return a;
        };
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            // (Paths have spaces: `props::<impl vm::Vm>::get`.)
            let (dir, rest) = line.split_once(' ').unwrap_or((line, ""));
            let rest = rest.trim();
            let indexed = || {
                let (f, i) = rest.rsplit_once(' ').unwrap_or_else(|| panic!("annotations: `{line}` wants an index"));
                (f.trim().to_string(), i.parse::<usize>().unwrap_or_else(|_| panic!("annotations: `{line}`'s index")))
            };
            match dir {
                "no-gc" => a.no_gc.push(rest.to_string()),
                "holds" => {
                    let (f, i) = indexed();
                    a.holds.insert(f, i);
                }
                "plain" => {
                    a.plain.insert(rest.to_string());
                }
                "exotic-gc" => {
                    let (f, i) = indexed();
                    a.exotic_gc.insert(f, i);
                }
                "internal" => {
                    a.internal.insert(rest.to_string());
                }
                "no-cell" => {
                    a.no_cell.insert(rest.to_string());
                }
                "internal-gc" => {
                    let (f, i) = indexed();
                    a.internal_gc.insert(f, i);
                }
                "internal-holds" => {
                    let (f, i) = indexed();
                    a.internal_holds.insert(f, i);
                }
                _ => panic!("annotations: can't read `{line}`"),
            }
        }
        a
    }

    fn is_no_gc(&self, path: &str) -> bool {
        self.no_gc.iter().any(|p| match p.strip_suffix('*') {
            Some(pre) => path.starts_with(pre),
            None => path == p || path.starts_with(&format!("{p}::{{closure")),
        })
    }
}

struct Ctx<'tcx> {
    tcx: TyCtxt<'tcx>,
    ann: Annotations,
    sums: FxHashMap<DefId, Summary>,
    gc_ty: RefCell<FxHashMap<Ty<'tcx>, bool>>,
    holder_ty: RefCell<FxHashMap<Ty<'tcx>, bool>>,
    paths: RefCell<FxHashMap<DefId, String>>,
}

/// Types whose values hold cells.
const CELL_TYPES: &[&str] = &["value::Value", "object::Obj", "object::Object"];
/// Types that hold roots: what is read through them is rooted.
const HOLDER_TYPES: &[&str] = &["vm::Vm", "heap::Heap", "realm::Realm"];
/// Functions that collect.
const GC_SEEDS: &[&str] =
    &["gc::<impl vm::Vm>::collect", "gc::<impl vm::Vm>::collect_from", "gc::<impl vm::Vm>::collect_full_now"];
/// Functions whose result is a new cell, rooted nowhere.
const FRESH_SEEDS: &[&str] = &["heap::Heap::alloc"];

impl<'tcx> Ctx<'tcx> {
    fn path(&self, d: DefId) -> String {
        self.paths.borrow_mut().entry(d).or_insert_with(|| self.tcx.def_path_str(d)).clone()
    }

    fn adt_is(&self, d: DefId, names: &[&str]) -> bool {
        d.is_local() && {
            let p = self.path(d);
            names.iter().any(|n| p == *n)
        }
    }

    /// Whether `ty` holds a cell (through references and pointers too,
    /// not through a holder).
    fn is_gc(&self, ty: Ty<'tcx>) -> bool {
        if let Some(&b) = self.gc_ty.borrow().get(&ty) {
            return b;
        }
        self.gc_ty.borrow_mut().insert(ty, false);
        let b = match ty.kind() {
            ty::Adt(adt, args) => {
                if self.adt_is(adt.did(), CELL_TYPES) {
                    true
                } else if self.adt_is(adt.did(), HOLDER_TYPES) {
                    false
                } else {
                    args.types().any(|t| self.is_gc(t))
                        || (adt.did().is_local()
                            && adt.all_fields().any(|f| self.is_gc(f.ty(self.tcx, args).skip_norm_wip())))
                }
            }
            ty::Ref(_, t, _) | ty::RawPtr(t, _) | ty::Slice(t) | ty::Array(t, _) => self.is_gc(*t),
            ty::Tuple(ts) => ts.iter().any(|t| self.is_gc(t)),
            ty::Closure(_, args) => args.as_closure().upvar_tys().iter().any(|t| self.is_gc(t)),
            _ => false,
        };
        self.gc_ty.borrow_mut().insert(ty, b);
        b
    }

    /// Whether a value of `ty` may hold a cell: `is_gc`, or generic (a
    /// `T` that is one where it is used: `Fresh::take`, `Result::map`).
    fn may_hold(&self, ty: Ty<'tcx>) -> bool {
        self.is_gc(ty)
            || ty.walk().any(|a| a.as_type().is_some_and(|t| matches!(t.kind(), ty::Param(_) | ty::Alias(..))))
    }

    /// Whether `ty` is a `Result` or a `ControlFlow` (`?`'s).
    fn is_result(&self, ty: Ty<'tcx>) -> bool {
        match ty.kind() {
            ty::Adt(a, _) => {
                let p = self.path(a.did());
                matches!(
                    p.as_str(),
                    "std::result::Result" | "core::result::Result" | "std::ops::ControlFlow" | "core::ops::ControlFlow"
                )
            }
            _ => false,
        }
    }

    /// Whether `ty` is (a reference or pointer to) a holder of roots.
    fn is_holder(&self, ty: Ty<'tcx>) -> bool {
        if let Some(&b) = self.holder_ty.borrow().get(&ty) {
            return b;
        }
        let b = match ty.kind() {
            ty::Adt(adt, _) => self.adt_is(adt.did(), HOLDER_TYPES),
            ty::Ref(_, t, _) | ty::RawPtr(t, _) => self.is_holder(*t),
            _ => false,
        };
        self.holder_ty.borrow_mut().insert(ty, b);
        b
    }

    /// Closures and local functions a value of `ty` may call, as `call`
    /// does with it: what an argument's type brings into a callee.
    fn callables_in(&self, ty: Ty<'tcx>, out: &mut Vec<DefId>) {
        for a in ty.walk() {
            if let Some(t) = a.as_type() {
                match t.kind() {
                    ty::Closure(d, _) | ty::FnDef(d, _) if d.is_local() => out.push(*d),
                    _ => {}
                }
            }
        }
    }
}

/// A body's analysis state: rootedness per local.
type State = Vec<St>;

struct BodyAn<'a, 'tcx> {
    cx: &'a Ctx<'tcx>,
    body: &'a Body<'tcx>,
    def: DefId,
    typing_env: TypingEnv<'tcx>,
    is_closure: bool,
    /// What each local may point to (locals), transitively.
    refs: rustc_index::IndexVec<Local, FxHashSet<Local>>,
    /// Locals pointing into `vm.handles`.
    into_handles: rustc_index::IndexVec<Local, bool>,
    /// A local's single defining rvalue, if one.
    defs: FxHashMap<Local, &'a Rvalue<'tcx>>,
    /// `root_scope` guards: local to bit.
    guards: FxHashMap<Local, usize>,
    /// Closures created for `Vm::rooted`: captures rooted by its slice.
    rooted_caps: FxHashMap<Local, FxHashSet<Local>>,
    /// Call sites, for fresh values' origins.
    sites: FxHashMap<BasicBlock, usize>,
    /// Captures a closure body reads (bits), for a closure that may collect.
    env_read: u64,
    /// The closure's captures are used after a call that may collect.
    env_live_after_gc: bool,
    /// Captures (bits) the closure body roots itself.
    env_rooted: std::cell::Cell<u64>,
    /// The closure is `FnOnce`.
    once: bool,
}

struct Resolved<'tcx> {
    def: Option<DefId>,
    path: String,
    /// A call through a pointer or `dyn`.
    indirect: bool,
    virtual_trait: Option<String>,
    _args: Option<ty::GenericArgsRef<'tcx>>,
}

impl<'a, 'tcx> BodyAn<'a, 'tcx> {
    fn new(cx: &'a Ctx<'tcx>, def: DefId, body: &'a Body<'tcx>) -> Self {
        let tcx = cx.tcx;
        let n = body.local_decls.len();
        let mut an = BodyAn {
            cx,
            body,
            def,
            typing_env: body.typing_env(tcx),
            is_closure: tcx.is_closure_like(def),
            refs: rustc_index::IndexVec::from_elem_n(FxHashSet::default(), n),
            into_handles: rustc_index::IndexVec::from_elem_n(false, n),
            defs: FxHashMap::default(),
            guards: FxHashMap::default(),
            rooted_caps: FxHashMap::default(),
            sites: body
                .basic_blocks
                .iter_enumerated()
                .filter(|(_, b)| matches!(b.terminator().kind, TerminatorKind::Call { .. }))
                .map(|(bb, _)| bb)
                .take(64)
                .enumerate()
                .map(|(i, bb)| (bb, i))
                .collect(),
            env_read: 0,
            env_live_after_gc: false,
            env_rooted: Default::default(),
            once: tcx.is_closure_like(def)
                && match tcx.type_of(def).skip_binder().kind() {
                    ty::Closure(_, a) => a.as_closure().kind() == ty::ClosureKind::FnOnce,
                    _ => false,
                },
        };
        an.prepass();
        an
    }

    /// `GC_HAZARDS_DUMP`: a comma list of functions whose analysis to print.
    fn dumping(&self) -> bool {
        std::env::var("GC_HAZARDS_DUMP").is_ok_and(|f| {
            let p = self.cx.path(self.def);
            f.split(',').any(|f| p == f || p.ends_with(&format!(">::{f}")))
        })
    }

    fn ty(&self, l: Local) -> Ty<'tcx> {
        self.body.local_decls[l].ty
    }

    fn prepass(&mut self) {
        let mut assigned: FxHashMap<Local, usize> = FxHashMap::default();
        for bb in self.body.basic_blocks.iter() {
            for s in &bb.statements {
                if let StatementKind::Assign(b) = &s.kind {
                    let (p, rv) = &**b;
                    if p.projection.is_empty() {
                        *assigned.entry(p.local).or_default() += 1;
                        self.defs.insert(p.local, rv);
                    }
                }
            }
            if let TerminatorKind::Call { destination, .. } = &bb.terminator().kind {
                *assigned.entry(destination.local).or_default() += 2;
            }
        }
        self.defs.retain(|l, _| assigned.get(l) == Some(&1));

        // What references point to, to a fixed point.
        let mut changed = true;
        while changed {
            changed = false;
            for bb in self.body.basic_blocks.iter() {
                for s in &bb.statements {
                    let StatementKind::Assign(b) = &s.kind else { continue };
                    let (p, rv) = &**b;
                    let mut add: Vec<Local> = Vec::new();
                    let mut handles = false;
                    match rv {
                        Rvalue::Ref(_, _, q) | Rvalue::RawPtr(_, q) | Rvalue::Reborrow(_, _, q) => {
                            if q.projection.first() == Some(&ProjectionElem::Deref) {
                                add.extend(self.refs[q.local].iter().copied());
                                handles |= self.into_handles[q.local];
                            } else {
                                add.push(q.local);
                            }
                            handles |= self.is_handles_place(q);
                        }
                        _ => {
                            self.rv_locals(rv, &mut |l| {
                                add.push(l);
                            });
                            let ls = add.clone();
                            add.clear();
                            for l in ls {
                                add.extend(self.refs[l].iter().copied());
                                handles |= self.into_handles[l];
                            }
                        }
                    }
                    for l in add {
                        changed |= self.refs[p.local].insert(l);
                    }
                    if handles && !self.into_handles[p.local] {
                        self.into_handles[p.local] = true;
                        changed = true;
                    }
                }
                if let TerminatorKind::Call { args, destination, .. } = &bb.terminator().kind {
                    // A reference a call returns points where its arguments do
                    // (`index_mut`, `as_mut_ptr`).
                    if self.ty(destination.local).is_any_ptr() || self.ty(destination.local).is_ref() {
                        let mut add = Vec::new();
                        let mut handles = false;
                        for a in args.iter() {
                            if let Some(l) = op_local(&a.node) {
                                add.extend(self.refs[l].iter().copied());
                                handles |= self.into_handles[l];
                            }
                        }
                        for l in add {
                            changed |= self.refs[destination.local].insert(l);
                        }
                        if handles && !self.into_handles[destination.local] {
                            self.into_handles[destination.local] = true;
                            changed = true;
                        }
                    }
                }
            }
        }

        // Guards and `rooted` closures.
        for bb in self.body.basic_blocks.iter() {
            let TerminatorKind::Call { func, args, destination, .. } = &bb.terminator().kind else { continue };
            let Some(r) = self.resolve(func) else { continue };
            if r.path.ends_with("Vm::root_scope") || r.path == "brand::Scope::new" {
                let n = self.guards.len();
                if n < 64 {
                    self.guards.insert(destination.local, n);
                }
            } else if r.path.ends_with("Vm::rooted") && args.len() == 3 {
                let srcs = self.slice_sources(&args[1].node);
                if let Some(c) = op_local(&args[2].node) {
                    self.rooted_caps.insert(c, srcs.into_iter().collect());
                }
            }
        }
    }

    /// Whether `p` is `(*vm).handles`.
    fn is_handles_place(&self, p: &Place<'tcx>) -> bool {
        let tcx = self.cx.tcx;
        let mut ty = PlaceTy::from_ty(self.ty(p.local));
        for e in p.projection.iter() {
            if let ProjectionElem::Field(f, _) = e
                && let ty::Adt(adt, _) = ty.ty.kind()
                && self.cx.adt_is(adt.did(), &["vm::Vm"])
                && adt.non_enum_variant().fields[f].name.as_str() == "handles"
            {
                return true;
            }
            ty = ty.projection_ty(tcx, e);
        }
        false
    }

    /// The locals an rvalue reads.
    fn rv_locals(&self, rv: &Rvalue<'tcx>, f: &mut dyn FnMut(Local)) {
        let op = |o: &Operand<'tcx>, f: &mut dyn FnMut(Local)| {
            if let Some(p) = o.place() {
                f(p.local);
            }
        };
        match rv {
            Rvalue::Use(o, ..) | Rvalue::Repeat(o, _) | Rvalue::Cast(_, o, _) | Rvalue::UnaryOp(_, o) => op(o, f),
            Rvalue::WrapUnsafeBinder(o, _) => op(o, f),
            Rvalue::BinaryOp(_, b) => {
                op(&b.0, f);
                op(&b.1, f);
            }
            Rvalue::Aggregate(_, ops) => ops.iter().for_each(|o| op(o, f)),
            Rvalue::CopyForDeref(p) | Rvalue::Discriminant(p) => f(p.local),
            Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) | Rvalue::Reborrow(_, _, p) => f(p.local),
            _ => {}
        }
    }

    /// The locals whose values a `&[..]` operand's array holds (`&[a, b]`).
    fn slice_sources(&self, o: &Operand<'tcx>) -> Vec<Local> {
        let mut out = Vec::new();
        let mut l = match op_local(o) {
            Some(l) => l,
            None => return out,
        };
        let dbg = self.dumping();
        for _ in 0..8 {
            if dbg {
                eprintln!("slice {l:?}: {:?}", self.defs.get(&l));
            }
            match self.defs.get(&l) {
                Some(Rvalue::Cast(_, o, _)) | Some(Rvalue::Use(o, ..)) => match op_local(o) {
                    Some(n) => l = n,
                    None => return out,
                },
                Some(Rvalue::Ref(_, _, p)) | Some(Rvalue::Reborrow(_, _, p)) | Some(Rvalue::RawPtr(_, p))
                    if p.projection.is_empty() || p.projection[..] == [ProjectionElem::Deref] =>
                {
                    l = p.local
                }
                Some(Rvalue::Aggregate(k, ops)) if matches!(**k, AggregateKind::Array(_)) => {
                    for o in ops.iter() {
                        if let Some(p) = o.place()
                            && p.projection.first() != Some(&ProjectionElem::Deref)
                        {
                            // And what that temporary copies.
                            let mut l = p.local;
                            out.push(l);
                            for _ in 0..4 {
                                match self.defs.get(&l) {
                                    Some(Rvalue::Use(o, ..))
                                        if let Some(q) = o.place()
                                            && q.projection.first() != Some(&ProjectionElem::Deref) =>
                                    {
                                        l = q.local;
                                        out.push(l);
                                        if !q.projection.is_empty() {
                                            break;
                                        }
                                    }
                                    _ => break,
                                }
                            }
                        }
                    }
                    return out;
                }
                _ => return out,
            }
        }
        out
    }

    fn resolve(&self, func: &Operand<'tcx>) -> Option<Resolved<'tcx>> {
        let tcx = self.cx.tcx;
        let fty = func.ty(self.body, tcx);
        match fty.kind() {
            ty::FnDef(d, args) => {
                let args = args.skip_binder();
                let inst = Instance::try_resolve(tcx, self.typing_env, *d, args).ok().flatten();
                match inst {
                    Some(i) => match i.def {
                        InstanceKind::Item(id) => Some(Resolved {
                            def: Some(id),
                            path: self.cx.path(id),
                            indirect: false,
                            virtual_trait: None,
                            _args: Some(i.args),
                        }),
                        InstanceKind::Virtual(id, _) => {
                            let tr = tcx.trait_of_assoc(id).map(|t| self.cx.path(t));
                            Some(Resolved {
                                def: None,
                                path: self.cx.path(id),
                                indirect: true,
                                virtual_trait: tr,
                                _args: None,
                            })
                        }
                        _ => {
                            // A shim: a closure called through `Fn*`, a fn
                            // pointer's `call`, drop glue.
                            let self_ty = args.types().next();
                            let clo = self_ty.and_then(|t| match t.peel_refs().kind() {
                                ty::Closure(c, _) | ty::FnDef(c, _) => Some(*c),
                                _ => None,
                            });
                            let ptr = self_ty.is_some_and(|t| t.peel_refs().is_fn_ptr());
                            Some(Resolved {
                                def: clo,
                                path: clo.map(|c| self.cx.path(c)).unwrap_or_else(|| self.cx.path(*d)),
                                indirect: ptr,
                                virtual_trait: None,
                                _args: None,
                            })
                        }
                    },
                    None => Some(Resolved {
                        def: None,
                        path: self.cx.path(*d),
                        indirect: false,
                        virtual_trait: None,
                        _args: None,
                    }),
                }
            }
            ty::FnPtr(..) => Some(Resolved {
                def: None,
                path: format!("fn pointer `{fty}`"),
                indirect: true,
                virtual_trait: None,
                _args: None,
            }),
            _ => None,
        }
    }

    /// Whether a call may collect, and through what.
    /// A place's state, without noting what it reads.
    fn peek(&self, st: &State, p: PlaceRef<'tcx>) -> St {
        if self.env_field(p).is_some() || self.cx.is_holder(self.ty(p.local)) {
            return ROOTED;
        }
        let mut s = st[p.local.as_usize()];
        if p.projection.first() == Some(&ProjectionElem::Deref) {
            for &q in &self.refs[p.local] {
                s = s.join(st[q.as_usize()]);
            }
        }
        s
    }

    fn peek_op(&self, st: &State, o: &Operand<'tcx>) -> St {
        o.place().map_or(ROOTED, |p| self.peek(st, p.as_ref()))
    }

    fn call_may_gc(
        &self,
        st: &State,
        r: &Resolved<'tcx>,
        args: &[rustc_span::Spanned<Operand<'tcx>>],
    ) -> Option<DefId> {
        let sums = &self.cx.sums;
        if r.def.is_some_and(|d| self.cx.ann.is_no_gc(&self.cx.path(d))) {
            return None;
        }
        if let Some(&i) = r.def.and_then(|d| self.cx.ann.exotic_gc.get(&self.cx.path(d)))
            && let Some(a) = args.get(i)
            && let Some(p) = a.node.place()
        {
            let s = self.peek(st, p.as_ref());
            if s.init && (s.plain || s.internal) {
                return None;
            }
        }
        if let Some(&i) = r.def.and_then(|d| self.cx.ann.internal_gc.get(&self.cx.path(d)))
            && let Some(a) = args.get(i)
            && let Some(p) = a.node.place()
        {
            let s = self.peek(st, p.as_ref());
            if s.init && s.internal {
                return None;
            }
        }
        if let Some(d) = r.def
            && let Some(s) = sums.get(&d)
            && s.may_gc
        {
            return Some(d);
        }
        if r.def.is_some_and(|d| GC_SEEDS.contains(&self.cx.path(d).as_str())) {
            return r.def;
        }
        if r.indirect {
            let quiet = r.virtual_trait.as_deref().is_some_and(|t| {
                t.starts_with("std::fmt")
                    || t.starts_with("core::fmt")
                    || t.starts_with("std::io")
                    || t.starts_with("std::hash")
                    || t.starts_with("core::hash")
                    || t.starts_with("std::error")
                    || t.starts_with("std::any")
                    || t.starts_with("core::any")
            });
            if !quiet {
                return Some(self.def);
            }
        }
        // A closure or function an argument brings (`map(|x| ..)`).
        let mut c = Vec::new();
        for a in args {
            self.cx.callables_in(a.node.ty(self.body, self.cx.tcx), &mut c);
        }
        c.into_iter().find(|d| sums.get(d).is_some_and(|s| s.may_gc))
    }

    // --- Rootedness ---

    fn env_field(&self, p: PlaceRef<'tcx>) -> Option<(usize, usize)> {
        // `_1.k` or `(*_1).k` in a closure body: capture `k`; returns it and
        // how many projections that took.
        if !self.is_closure || p.local != Local::from_u32(1) {
            return None;
        }
        match p.projection {
            [ProjectionElem::Field(f, _), ..] => Some((f.as_usize(), 1)),
            [ProjectionElem::Deref, ProjectionElem::Field(f, _), ..] => Some((f.as_usize(), 2)),
            _ => None,
        }
    }

    fn place_st(&mut self, st: &State, p: PlaceRef<'tcx>) -> St {
        if let Some((k, _)) = self.env_field(p) {
            if k < 64 {
                self.env_read |= 1 << k;
            }
            return St::param(self.body.arg_count + k);
        }
        let l = p.local;
        if self.cx.is_holder(self.ty(l)) {
            return ROOTED;
        }
        if self.into_handles[l] && p.projection.first() == Some(&ProjectionElem::Deref) {
            // (`vm.handles[i]`.)
            return ROOTED;
        }
        let mut s = st[l.as_usize()];
        if p.projection.first() == Some(&ProjectionElem::Deref) {
            for &q in &self.refs[l] {
                s = s.join(st[q.as_usize()]);
            }
        }
        if !s.init {
            return ROOTED;
        }
        // A `Result`'s parts: its `Ok`'s, its error's.
        for e in p.projection {
            if let ProjectionElem::Downcast(Some(v), _) = e {
                match v.as_str() {
                    "Ok" | "Continue" => s = s.ok(),
                    "Err" | "Break" => s = s.whole().err_only(),
                    _ => {}
                }
            }
        }
        if s.split && !self.cx.is_result(p.ty(self.body, self.cx.tcx).ty) {
            s = s.whole();
        }
        s
    }

    /// Whether `p` is a field holding no cell (a tuple's number, a
    /// `Handle`) of an aggregate that isn't a cell itself: rooted, whatever
    /// its aggregate's state. (Not a `Value`'s bits, nor a pointer the
    /// heap gave: cells travel as those.)
    fn cell_free_field(&self, p: PlaceRef<'tcx>) -> bool {
        let Some((&ProjectionElem::Field(_, fty), base)) = p.projection.split_last() else { return false };
        let parent = PlaceTy::from_ty(self.ty(p.local));
        let parent = base.iter().fold(parent, |t, e| t.projection_ty(self.cx.tcx, *e)).ty;
        let cell = matches!(parent.kind(), ty::Adt(a, _) if self.cx.adt_is(a.did(), CELL_TYPES));
        !cell && !self.cx.may_hold(fty)
    }

    fn op_st(&mut self, st: &State, o: &Operand<'tcx>) -> St {
        match o.place() {
            // (What holds no cell is rooted: a tuple's number, a `Handle`,
            // of its aggregate's kind.)
            Some(p) if self.cell_free_field(p.as_ref()) => {
                let s = self.place_st(st, p.as_ref());
                self.kind_only(s)
            }
            Some(p) => self.place_st(st, p.as_ref()),
            _ => ROOTED,
        }
    }

    /// Rooted, of `s`'s kind (a new ordinary object's, an internal one's).
    fn kind_only(&self, s: St) -> St {
        St { plain: s.init && s.plain, internal: s.init && s.internal, ..ROOTED }
    }

    fn rv_st(&mut self, st: &State, rv: &Rvalue<'tcx>) -> St {
        match rv {
            Rvalue::Use(o, ..) | Rvalue::Repeat(o, _) | Rvalue::Cast(_, o, _) | Rvalue::UnaryOp(_, o) => {
                self.op_st(st, o)
            }
            Rvalue::WrapUnsafeBinder(o, _) => self.op_st(st, o),
            Rvalue::BinaryOp(_, b) => {
                let a = self.op_st(st, &b.0);
                a.join(self.op_st(st, &b.1))
            }
            Rvalue::Aggregate(k, ops) => {
                let mut s = ROOTED;
                for o in ops.iter() {
                    s = s.join(self.op_st(st, o));
                }
                // `Err(e)`: all its error's; `Ok(v)`: all its value's.
                if let AggregateKind::Adt(d, v, ..) = **k
                    && self.cx.tcx.adt_def(d).is_enum()
                    && self.cx.is_result(self.cx.tcx.type_of(d).skip_binder())
                {
                    let name = self.cx.tcx.adt_def(d).variant(v).name;
                    s = match name.as_str() {
                        "Err" | "Break" => s.whole().err_only(),
                        _ => s.whole(),
                    };
                }
                s
            }
            Rvalue::CopyForDeref(p) | Rvalue::Ref(_, _, p) | Rvalue::RawPtr(_, p) | Rvalue::Reborrow(_, _, p) => {
                let s = self.place_st(st, p.as_ref());
                if self.cell_free_field(p.as_ref()) { self.kind_only(s) } else { s }
            }
            _ => ROOTED,
        }
    }

    /// Store `s` to place `p`.
    fn store(&mut self, st: &mut State, p: &Place<'tcx>, s: St, out: &mut Vec<St>) {
        let l = p.local;
        if p.projection.is_empty() {
            st[l.as_usize()] = s;
            return;
        }
        if p.projection.first() != Some(&ProjectionElem::Deref) {
            st[l.as_usize()] = st[l.as_usize()].join(s);
            return;
        }
        // Through a pointer: into what it points to; into `vm.handles`
        // (rooted then); and as stored through the parameters it is.
        if self.into_handles[l] || self.is_handles_place(p) {
            // (`vm.handles[i] = v`.)
            self.root_under(st, s, 0);
            return;
        }
        let ptr = if self.cx.is_holder(self.ty(l)) { ROOTED } else { st[l.as_usize()] };
        let targets: Vec<Local> = self.refs[l].iter().copied().collect();
        if targets.is_empty() {
            self.stored_into(st, ptr, s, out);
        }
        for q in targets {
            st[q.as_usize()] = st[q.as_usize()].join(s);
        }
    }

    /// `s` stored into what a pointer or cell in state `at` is: rooted with
    /// it if it is rooted; and as stored through the parameters it is.
    fn stored_into(&self, st: &mut State, at: St, s: St, out: &mut [St]) {
        if !at.init {
            return;
        }
        if at.rooted() {
            self.root_under(st, s, at.guards);
        }
        for b in 0..self.body.arg_count.min(64) {
            if at.params & (1 << b) != 0 && b < out.len() {
                out[b] = out[b].join(s);
            }
        }
    }

    /// What has `s`'s origins rooted from here on (while `guards` live).
    fn root_under(&self, st: &mut State, s: St, guards: u64) {
        if !s.init || s.fresh == 0 && s.params == 0 {
            return;
        }
        let a = self.body.arg_count.min(63);
        if self.is_closure {
            self.env_rooted.set(self.env_rooted.get() | s.params >> a);
        }
        for v in st.iter_mut() {
            if v.fresh & s.fresh != 0 || v.params & s.params != 0 {
                if guards != 0 {
                    v.gfresh |= v.fresh & s.fresh;
                    v.gparams |= v.params & s.params;
                }
                v.fresh &= !s.fresh;
                v.params &= !s.params;
                v.guards |= guards;
            }
            v.ok_fresh &= !s.fresh;
            v.ok_params &= !s.params;
        }
    }

    /// Kill a guard: what it rooted is unrooted.
    fn kill_guard(&self, st: &mut State, l: Local) {
        if let Some(&g) = self.guards.get(&l) {
            for s in st.iter_mut() {
                if s.guards & (1 << g) != 0 {
                    s.guards &= !(1 << g);
                    // (What it rooted is as it was: a parameter again, or
                    // fresh; unknown, if the guard rooted it whole.)
                    if s.gfresh == 0 && s.gparams == 0 {
                        s.un = true;
                        s.ok_un |= s.split;
                    }
                    if s.guards == 0 {
                        if s.split {
                            s.ok_fresh |= s.gfresh;
                            s.ok_params |= s.gparams;
                        }
                        s.fresh |= s.gfresh;
                        s.params |= s.gparams;
                        s.gfresh = 0;
                        s.gparams = 0;
                    }
                }
            }
        }
    }

    /// Map a callee's state into the caller's, given the call's arguments.
    fn map_st(
        &mut self,
        st: &State,
        s: St,
        args: &[rustc_span::Spanned<Operand<'tcx>>],
        callee_args: usize,
        site: Option<usize>,
    ) -> St {
        if !s.init {
            return ROOTED;
        }
        if s.split {
            // (Each part as its own.)
            let all = self.map_st(st, s.whole(), args, callee_args, site);
            let ok = self.map_st(st, s.ok(), args, callee_args, site).ok();
            return St { split: true, ok_un: ok.un, ok_fresh: ok.fresh, ok_params: ok.params, ..all.whole() };
        }
        // (Bottom first: what a parameter passes through keeps its kind.)
        let mut r = if s.unrooted() { St::fresh_at(site) } else { St::default() };
        for b in 0..64 {
            if s.params & (1 << b) != 0 {
                // A closure's captures and its call's tuple: the closure.
                let i = if b >= callee_args { 0 } else { b.min(args.len().saturating_sub(1)) };
                if let Some(a) = args.get(i) {
                    r = r.join(self.op_st(st, &a.node));
                }
            }
        }
        if r.init { r } else { ROOTED }
    }

    fn run(&mut self, entry: State, report: Option<&mut Vec<Hazard>>) -> (Summary, Vec<Hazard>) {
        let body = self.body;
        let n = body.basic_blocks.len();
        let mut ins: Vec<Option<State>> = vec![None; n];
        ins[0] = Some(entry);
        let live = liveness(body);
        let mut work: Vec<BasicBlock> = vec![START_BLOCK];
        let mut sum = Summary { args: body.arg_count, out: vec![St::default(); body.arg_count], ..Default::default() };
        let mut iters = 0;
        while let Some(bb) = work.pop() {
            iters += 1;
            if iters > 200_000 {
                break;
            }
            let mut st = ins[bb.as_usize()].clone().unwrap();
            let succs = self.block(bb, &mut st, &mut sum, None, &live);
            for (t, s) in succs {
                let old = &mut ins[t.as_usize()];
                let new = match old {
                    None => s,
                    Some(o) => o.iter().zip(&s).map(|(a, b)| a.join(*b)).collect(),
                };
                if old.as_ref() != Some(&new) {
                    *old = Some(new);
                    work.push(t);
                }
            }
        }
        // The reporting pass, on the fixed point.
        let mut hz = Vec::new();
        let mut sum = Summary { args: body.arg_count, out: vec![St::default(); body.arg_count], ..Default::default() };
        for (i, s) in ins.iter().enumerate() {
            if let Some(s) = s {
                let mut st = s.clone();
                self.block(BasicBlock::from_usize(i), &mut st, &mut sum, Some(&mut hz), &live);
            }
        }
        // Captures read through the closure after it may have collected, or
        // read at all by one that may collect and may be called again.
        if self.is_closure && sum.may_gc && (self.env_live_after_gc || !self.once) {
            // (Not the ones it roots itself, first thing: `vm.handles.push(c)`.)
            sum.needs_root |= (self.env_read & !self.env_rooted.get()) << body.arg_count.min(63);
        }
        let _ = report;
        (sum, hz)
    }

    fn hazard(
        &self,
        span: Span,
        what: String,
        ty: Ty<'tcx>,
        callee: &str,
        via: Option<DefId>,
        kind: &'static str,
    ) -> Hazard {
        let mut chain = String::new();
        let mut at = via;
        let mut seen = FxHashSet::default();
        for _ in 0..6 {
            let Some(d) = at else { break };
            if !seen.insert(d) {
                break;
            }
            let _ = write!(chain, " → {}", self.cx.path(d));
            at = self.cx.sums.get(&d).and_then(|s| s.via);
        }
        Hazard { span, func: self.cx.path(self.def), what, ty: ty.to_string(), callee: callee.to_string(), chain, kind }
    }

    fn local_name(&self, l: Local) -> String {
        let mut l = l;
        for _ in 0..6 {
            match self.defs.get(&l) {
                Some(Rvalue::Use(o, ..)) | Some(Rvalue::Cast(_, o, _)) if let Some(p) = o.place() => l = p.local,
                Some(Rvalue::Ref(_, _, p))
                | Some(Rvalue::Reborrow(_, _, p))
                | Some(Rvalue::RawPtr(_, p))
                | Some(Rvalue::CopyForDeref(p)) => l = p.local,
                _ => break,
            }
        }
        for v in &self.body.var_debug_info {
            if let VarDebugInfoContents::Place(p) = &v.value
                && p.local == l
                && v.composite.is_none()
            {
                return if p.projection.is_empty() { v.name.to_string() } else { format!("{}(part)", v.name) };
            }
        }
        format!("{l:?}")
    }

    fn block(
        &mut self,
        bb: BasicBlock,
        st: &mut State,
        sum: &mut Summary,
        mut hz: Option<&mut Vec<Hazard>>,
        live: &Liveness,
    ) -> Vec<(BasicBlock, State)> {
        let body = self.body;
        let data = &body.basic_blocks[bb];
        if data.is_cleanup {
            return Vec::new();
        }
        for s in &data.statements {
            match &s.kind {
                StatementKind::Assign(b) => {
                    let (p, rv) = &**b;
                    let mut v = self.rv_st(st, rv);
                    if let Rvalue::Aggregate(k, ops) = rv
                        && let AggregateKind::Closure(cdef, _) = **k
                    {
                        self.check_captures(st, sum, hz.as_deref_mut(), s.source_info.span, p.local, cdef, ops);
                        // (A closure's state: its captures'.)
                        let _ = &mut v;
                    }
                    let mut out = std::mem::take(&mut sum.out);
                    self.store(st, p, v, &mut out);
                    sum.out = out;
                    if hz.is_some() && self.dumping() {
                        eprintln!("{bb:?} {p:?} = {rv:?}: {v:?}");
                    }
                }
                StatementKind::StorageDead(l) => self.kill_guard(st, *l),
                _ => {}
            }
        }
        let term = data.terminator();
        let mut succ = Vec::new();
        match &term.kind {
            TerminatorKind::Call { func, args, destination, target, .. } => {
                let r = self.resolve(func);
                let site = self.sites.get(&bb).copied();
                let mut external = false;
                let mut ret = St::fresh_at(site);
                if let Some(r) = &r {
                    let gc = self.call_may_gc(st, r, args);
                    let callee_sum = r.def.and_then(|d| self.cx.sums.get(&d)).cloned();
                    if let Some(via) = gc {
                        if !sum.may_gc {
                            sum.may_gc = true;
                            sum.via = Some(r.def.unwrap_or(via));
                        }
                        // Locals live after the call (but what `vm.rooted(&[..], f)`
                        // roots, while it runs).
                        let mut during = St::default();
                        if r.path.ends_with("Vm::rooted")
                            && let Some(a) = args.get(1)
                        {
                            for l in self.slice_sources(&a.node) {
                                during = during.join(st[l.as_usize()]);
                            }
                            // (A slice not built here: `vm.rooted(args, ..)`.)
                            during = during.join(self.peek_op(st, &a.node));
                        }
                        if let Some(t) = target {
                            let lv = live.live_in(*t, &self.refs);
                            for l in lv {
                                if l == destination.local {
                                    continue;
                                }
                                if self.is_closure && l.as_usize() == 1 {
                                    self.env_live_after_gc = true;
                                    continue;
                                }
                                let ty = self.ty(l);
                                if !self.cx.is_gc(ty) {
                                    continue;
                                }
                                let mut s = self.place_st(st, Place::from(l).as_ref());
                                if during.init && !s.un {
                                    s.fresh &= !during.fresh;
                                    s.params &= !during.params;
                                }
                                if s.unrooted() {
                                    if let Some(h) = hz.as_deref_mut() {
                                        h.push(self.hazard(
                                            term.source_info.span,
                                            self.local_name(l),
                                            ty,
                                            &r.path,
                                            gc,
                                            "live across",
                                        ));
                                    }
                                } else {
                                    sum.needs_root |= s.params;
                                }
                            }
                        }
                    }
                    // Arguments the callee holds across a collection.
                    if let Some(cs) = &callee_sum
                        && gc.is_some()
                    {
                        for (i, a) in args.iter().enumerate() {
                            let bits = if r.def.is_some_and(|d| self.cx.tcx.is_closure_like(d)) {
                                // Through `Fn*::call*(closure, (args,))`.
                                if i == 0 { 1 } else { cs.needs_root & !1 & ((1u64 << cs.args.min(63)) - 1) }
                            } else if i < 64 {
                                1 << i
                            } else {
                                0
                            };
                            if cs.needs_root & bits == 0
                                || i == 0 && r.def.is_some_and(|d| self.cx.tcx.is_closure_like(d))
                            {
                                continue;
                            }
                            let ty = a.node.ty(body, self.cx.tcx);
                            if !self.cx.is_gc(ty) {
                                continue;
                            }
                            let s = self.op_st(st, &a.node);
                            if s.unrooted() {
                                if let Some(h) = hz.as_deref_mut() {
                                    let what = op_local(&a.node)
                                        .map(|l| self.local_name(l))
                                        .unwrap_or_else(|| format!("argument {i}"));
                                    h.push(self.hazard(a.span, what, ty, &r.path, gc.or(r.def), "passed to"));
                                }
                            } else {
                                sum.needs_root |= s.params;
                            }
                        }
                    }
                    // What the callee returns, and stores.
                    if r.def.is_some_and(|d| FRESH_SEEDS.iter().any(|f| self.cx.path(d).starts_with(f))) {
                        ret = St::fresh_at(site);
                    } else if let Some(&i) = r.def.and_then(|d| self.cx.ann.holds.get(&self.cx.path(d))) {
                        // (Not of its holder's kind.)
                        ret = St {
                            plain: false,
                            internal: false,
                            ..self.map_st(st, St::param(i), args, usize::MAX, site)
                        };
                    } else if let Some(cs) = &callee_sum {
                        ret = self.map_st(st, cs.ret, args, cs.args, site);
                        for (i, o) in cs.out.clone().iter().enumerate() {
                            if o.init
                                && let Some(a) = args.get(i)
                            {
                                let v = self.map_st(st, *o, args, cs.args, site);
                                let aty = a.node.ty(body, self.cx.tcx);
                                if !aty.is_any_ptr() && self.cx.is_gc(aty) {
                                    // Into the object a `Value` argument is.
                                    let at = self.op_st(st, &a.node);
                                    self.stored_into(st, at, v, &mut sum.out);
                                } else if let Some(p) = a.node.place()
                                    && aty.is_any_ptr()
                                {
                                    let deref = p.project_deeper(&[ProjectionElem::Deref], self.cx.tcx);
                                    let mut out = std::mem::take(&mut sum.out);
                                    self.store(st, &deref, v, &mut out);
                                    sum.out = out;
                                }
                            }
                        }
                    } else if r.indirect || r.def.is_some_and(|d| d.is_local()) {
                        external = false;
                        // A native's result, or a local function not yet summed.
                        ret = if r.indirect { St::fresh_at(site) } else { ROOTED };
                    } else {
                        external = true;
                        // Outside the crate: as rooted as its arguments; what
                        // it may store through a `&mut` argument, too.
                        let handles =
                            args.first().and_then(|a| op_local(&a.node)).is_some_and(|l| self.into_handles[l]);
                        for (i, a) in args.iter().enumerate() {
                            if let Some(l) = op_local(&a.node)
                                && matches!(self.ty(l).kind(), ty::Ref(_, _, ty::Mutability::Mut))
                                && self.cx.is_gc(self.ty(l))
                            {
                                let mut v = ROOTED;
                                for (j, b) in args.iter().enumerate() {
                                    if j != i {
                                        v = v.join(self.op_st(st, &b.node));
                                    }
                                }
                                if handles && i == 0 {
                                    continue;
                                }
                                let p = Place::from(l).project_deeper(&[ProjectionElem::Deref], self.cx.tcx);
                                let mut out = std::mem::take(&mut sum.out);
                                self.store(st, &p, v, &mut out);
                                sum.out = out;
                            }
                        }
                        // `vm.handles.push(v)`, `.extend_from_slice(&[..])`:
                        // what goes there is rooted.
                        if handles {
                            for a in args.iter().skip(1) {
                                let s = self.op_st(st, &a.node);
                                self.root_under(st, s, 0);
                                for l in op_local(&a.node).into_iter().chain(self.slice_sources(&a.node)) {
                                    let s = st[l.as_usize()];
                                    self.root_under(st, s, 0);
                                    st[l.as_usize()] = ROOTED;
                                }
                            }
                        } else {
                            // Through a pointer into rooted memory (`ptr::write`).
                            for (i, a) in args.iter().enumerate() {
                                let aty = a.node.ty(body, self.cx.tcx);
                                if let Some(l) = op_local(&a.node)
                                    && aty.is_any_ptr()
                                    && self.refs[l].is_empty()
                                    && !matches!(aty.kind(), ty::Ref(_, _, ty::Mutability::Not))
                                {
                                    let at = st[l.as_usize()];
                                    if at.init && at.rooted() {
                                        for (j, b) in args.iter().enumerate() {
                                            if j != i {
                                                let v = self.op_st(st, &b.node);
                                                self.root_under(st, v, at.guards);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if external {
                        // (After what it rooted.)
                        let mut s = ROOTED;
                        for a in args.iter() {
                            s = s.join(self.op_st(st, &a.node));
                        }
                        ret = s;
                    }
                    // What annotations say of what it returns.
                    if let Some(d) = r.def {
                        let p = self.cx.path(d);
                        if self.cx.ann.plain.contains(&p) {
                            ret.plain = true;
                        }
                        if self.cx.ann.internal.contains(&p) {
                            ret.internal = true;
                        }
                        if self.cx.ann.no_cell.contains(&p) {
                            ret = ROOTED;
                        }
                        if let Some(&i) = self.cx.ann.internal_holds.get(&p)
                            && let Some(a) = args.get(i)
                            && self.peek_op(st, &a.node).internal
                        {
                            ret = St { internal: false, plain: false, ..self.op_st(st, &a.node) };
                        }
                    }
                    // A `Scope`'s roots (`sc.root(v)`, `sc.roots([a, b])`,
                    // `fresh.root(&sc)`): rooted while the scope lives; for
                    // the whole call, a scope the caller gave.
                    if let Some((v, sc)) = scope_rooting(&r.path)
                        && let (Some(va), Some(sa)) = (args.get(v), args.get(sc))
                    {
                        let g = op_local(&sa.node).map_or(0, |l| {
                            let mut g = self.guards.get(&l).map_or(0, |&g| 1u64 << g);
                            for q in &self.refs[l] {
                                if let Some(&b) = self.guards.get(q) {
                                    g |= 1 << b;
                                }
                            }
                            g
                        });
                        let mut srcs = self.slice_sources(&va.node);
                        srcs.extend(op_local(&va.node));
                        let mut s = self.op_st(st, &va.node);
                        for &l in &srcs {
                            s = s.join(st[l.as_usize()]);
                        }
                        self.root_under(st, s, g);
                        for l in srcs {
                            let v = &mut st[l.as_usize()];
                            if v.init && v.un {
                                *v = St { guards: g, plain: v.plain, internal: v.internal, ..ROOTED };
                            }
                        }
                        // (The handle: of the value's kind.)
                        ret = St { plain: s.plain, internal: s.internal, ..ROOTED };
                    }
                    // A handle's value: rooted, of the kind it was rooted as.
                    if matches!(
                        r.path.as_str(),
                        "brand::Handle::<'_>::v" | "brand::Handle::<'_>::peek" | "brand::Handle::<'_>::obj"
                    ) && let Some(a) = args.first()
                    {
                        let h = self.peek_op(st, &a.node);
                        ret = St { plain: h.init && h.plain, internal: h.init && h.internal, ..ROOTED };
                    }
                    // `root_scope(&[..])`: its values are rooted while the guard lives.
                    if r.path.ends_with("Vm::root_scope")
                        && let Some(&g) = self.guards.get(&destination.local)
                        && let Some(a) = args.get(1)
                    {
                        let srcs = self.slice_sources(&a.node);
                        let mut s = St::default();
                        for &l in &srcs {
                            s = s.join(st[l.as_usize()]);
                        }
                        self.root_under(st, s, 1 << g);
                        for l in srcs {
                            let v = &mut st[l.as_usize()];
                            if !v.init || v.un {
                                *v = St { guards: 1 << g, ..ROOTED };
                            }
                        }
                        ret = ROOTED;
                    }
                }
                if hz.is_some() && self.dumping() {
                    let a: Vec<St> = args.iter().map(|a| self.peek_op(st, &a.node)).collect();
                    eprintln!(
                        "{bb:?} call {} args {a:?} -> {destination:?} = {ret:?}",
                        r.as_ref().map_or("?".into(), |r| r.path.clone())
                    );
                }
                if let Some(t) = target {
                    let mut out = std::mem::take(&mut sum.out);
                    self.store(st, destination, ret, &mut out);
                    sum.out = out;
                    succ.push((*t, st.clone()));
                }
            }
            TerminatorKind::Drop { place, target, .. } => {
                if place.projection.is_empty() {
                    self.kill_guard(st, place.local);
                }
                succ.push((*target, st.clone()));
            }
            TerminatorKind::Return => {
                let s = st[0];
                let s = if s.init { s } else { ROOTED };
                sum.ret = sum.ret.join(St { guards: 0, ..s });
            }
            _ => {
                for t in term.successors() {
                    if !body.basic_blocks[t].is_cleanup {
                        succ.push((t, st.clone()));
                    }
                }
            }
        }
        succ
    }

    fn check_captures(
        &mut self,
        st: &State,
        sum: &mut Summary,
        hz: Option<&mut Vec<Hazard>>,
        span: Span,
        clo: Local,
        cdef: DefId,
        ops: &rustc_index::IndexVec<rustc_abi::FieldIdx, Operand<'tcx>>,
    ) {
        let Some(cs) = self.cx.sums.get(&cdef).cloned() else { return };
        let rooted: FxHashSet<Local> = self.rooted_caps.get(&clo).cloned().unwrap_or_default();
        // What the slice roots, by origin (`&[object_value(out.ptr())]`
        // roots `out`).
        let mut during = St::default();
        for &l in &rooted {
            during = during.join(st[l.as_usize()]);
        }
        let mut hz = hz;
        for (k, o) in ops.iter().enumerate() {
            let bit = cs.args + k;
            if bit >= 64 || cs.needs_root & (1 << bit) == 0 {
                continue;
            }
            let ty = o.ty(self.body, self.cx.tcx);
            if !self.cx.is_gc(ty) {
                continue;
            }
            if let Some(p) = o.place() {
                let base = if p.projection.is_empty() { Some(p.local) } else { None };
                let via_ref = self.defs.get(&p.local).and_then(|rv| match rv {
                    Rvalue::Ref(_, _, q) | Rvalue::RawPtr(_, q) | Rvalue::Reborrow(_, _, q)
                        if q.projection.is_empty() =>
                    {
                        Some(q.local)
                    }
                    _ => None,
                });
                if base.is_some_and(|b| rooted.contains(&b)) || via_ref.is_some_and(|b| rooted.contains(&b)) {
                    continue;
                }
            }
            let mut s = self.op_st(st, o);
            if during.init && !s.un {
                s.fresh &= !during.fresh;
                s.params &= !during.params;
            }
            if s.unrooted() {
                if let Some(h) = hz.as_deref_mut() {
                    let what = o
                        .place()
                        .map(|p| {
                            let l = self.defs.get(&p.local).and_then(|rv| match rv {
                                Rvalue::Ref(_, _, q) | Rvalue::RawPtr(_, q) | Rvalue::Reborrow(_, _, q) => {
                                    Some(q.local)
                                }
                                _ => None,
                            });
                            self.local_name(l.unwrap_or(p.local))
                        })
                        .unwrap_or_else(|| format!("capture {k}"));
                    h.push(self.hazard(span, what, ty, &self.cx.path(cdef), Some(cdef), "captured by"));
                }
            } else {
                sum.needs_root |= s.params;
            }
        }
    }
}

/// A call that roots a value in a `brand::Scope`: the value's argument and
/// the scope's (or the handle's, whose scope it is).
fn scope_rooting(path: &str) -> Option<(usize, usize)> {
    match path {
        "brand::Scope::root" | "brand::Scope::roots" | "brand::Scope::hold" => Some((1, 0)),
        "brand::Fresh::<'a, T>::root" | "brand::Gc::<'a>::root" => Some((0, 1)),
        "brand::Handle::<'_>::set" => Some((1, 0)),
        _ if path.ends_with("as brand::Rootable>::root_in") => Some((0, 1)),
        _ => None,
    }
}

fn op_local(o: &Operand<'_>) -> Option<Local> {
    o.place().filter(|p| p.projection.is_empty()).map(|p| p.local)
}

/// Locals live at each block's entry (backward), not counting a drop's use.
struct Liveness {
    ins: Vec<Vec<bool>>,
}

impl Liveness {
    /// Live at `bb`'s entry, with what live references point to.
    fn live_in(&self, bb: BasicBlock, refs: &rustc_index::IndexVec<Local, FxHashSet<Local>>) -> Vec<Local> {
        let v = &self.ins[bb.as_usize()];
        let mut out: FxHashSet<Local> = FxHashSet::default();
        for (i, &b) in v.iter().enumerate() {
            if b {
                out.insert(Local::from_usize(i));
                out.extend(refs[Local::from_usize(i)].iter().copied());
            }
        }
        let mut o: Vec<Local> = out.into_iter().collect();
        o.sort();
        o
    }
}

fn liveness(body: &Body<'_>) -> Liveness {
    let n = body.local_decls.len();
    let nb = body.basic_blocks.len();
    let mut ins = vec![vec![false; n]; nb];
    let use_place = |p: &Place<'_>, live: &mut Vec<bool>| {
        live[p.local.as_usize()] = true;
        for e in p.projection.iter() {
            if let ProjectionElem::Index(l) = e {
                live[l.as_usize()] = true;
            }
        }
    };
    let use_op = |o: &Operand<'_>, live: &mut Vec<bool>| {
        if let Some(p) = o.place() {
            use_place(&p, live);
        }
    };
    let mut changed = true;
    while changed {
        changed = false;
        for bb in (0..nb).rev() {
            let data = &body.basic_blocks[BasicBlock::from_usize(bb)];
            let mut live = vec![false; n];
            for t in data.terminator().successors() {
                for (i, &b) in ins[t.as_usize()].iter().enumerate() {
                    live[i] |= b;
                }
            }
            match &data.terminator().kind {
                TerminatorKind::Call { func, args, destination, .. } => {
                    if destination.projection.is_empty() {
                        live[destination.local.as_usize()] = false;
                    } else {
                        use_place(destination, &mut live);
                    }
                    use_op(func, &mut live);
                    for a in args.iter() {
                        use_op(&a.node, &mut live);
                    }
                }
                TerminatorKind::SwitchInt { discr, .. } => use_op(discr, &mut live),
                TerminatorKind::Return => live[0] = true,
                TerminatorKind::Assert { cond, .. } => use_op(cond, &mut live),
                TerminatorKind::Drop { .. } => {}
                TerminatorKind::InlineAsm { operands, .. } => {
                    for o in operands.iter() {
                        match o {
                            InlineAsmOperand::In { value, .. } => use_op(value, &mut live),
                            InlineAsmOperand::InOut { in_value, .. } => use_op(in_value, &mut live),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            for s in data.statements.iter().rev() {
                match &s.kind {
                    StatementKind::Assign(b) => {
                        let (p, rv) = &**b;
                        if p.projection.is_empty() {
                            live[p.local.as_usize()] = false;
                        } else {
                            use_place(p, &mut live);
                        }
                        let mut uses = Vec::new();
                        match rv {
                            Rvalue::Use(o, ..)
                            | Rvalue::Repeat(o, _)
                            | Rvalue::Cast(_, o, _)
                            | Rvalue::UnaryOp(_, o) => uses.push(o.clone()),
                            Rvalue::WrapUnsafeBinder(o, _) => uses.push(o.clone()),
                            Rvalue::BinaryOp(_, b) => {
                                uses.push(b.0.clone());
                                uses.push(b.1.clone());
                            }
                            Rvalue::Aggregate(_, ops) => uses.extend(ops.iter().cloned()),
                            Rvalue::CopyForDeref(p)
                            | Rvalue::Discriminant(p)
                            | Rvalue::Ref(_, _, p)
                            | Rvalue::RawPtr(_, p)
                            | Rvalue::Reborrow(_, _, p) => use_place(p, &mut live),
                            _ => {}
                        }
                        for o in &uses {
                            use_op(o, &mut live);
                        }
                    }
                    StatementKind::SetDiscriminant { place, .. } => use_place(place, &mut live),
                    StatementKind::Intrinsic(i) => match &**i {
                        NonDivergingIntrinsic::Assume(o) => use_op(o, &mut live),
                        NonDivergingIntrinsic::CopyNonOverlapping(c) => {
                            use_op(&c.src, &mut live);
                            use_op(&c.dst, &mut live);
                            use_op(&c.count, &mut live);
                        }
                    },
                    _ => {}
                }
            }
            if ins[bb] != live {
                ins[bb] = live;
                changed = true;
            }
        }
    }
    Liveness { ins }
}

fn analyze<'tcx>(tcx: TyCtxt<'tcx>) {
    let ann = std::env::var("GC_HAZARDS_ANNOTATIONS").unwrap_or_else(|_| "tools/gc-hazards/annotations.txt".into());
    let mut cx = Ctx {
        tcx,
        ann: Annotations::read(&ann),
        sums: FxHashMap::default(),
        gc_ty: Default::default(),
        holder_ty: Default::default(),
        paths: Default::default(),
    };
    let mut defs: Vec<DefId> = Vec::new();
    for &d in tcx.mir_keys(()).iter() {
        let did = d.to_def_id();
        match tcx.def_kind(did) {
            DefKind::Fn | DefKind::AssocFn | DefKind::Closure => {}
            _ => continue,
        }
        if tcx.is_coroutine(did) || !tcx.is_mir_available(did) {
            continue;
        }
        defs.push(did);
    }
    // Annotations that name no function (renamed, gone): fail, or they
    // would assert nothing, silently.
    {
        let all: FxHashSet<String> = defs.iter().map(|&d| cx.path(d)).collect();
        let a = &cx.ann;
        let names = a
            .no_gc
            .iter()
            .chain(a.holds.keys())
            .chain(&a.plain)
            .chain(a.exotic_gc.keys())
            .chain(&a.internal)
            .chain(&a.no_cell)
            .chain(a.internal_gc.keys())
            .chain(a.internal_holds.keys());
        let mut stale: Vec<&String> = names
            .filter(|n| match n.strip_suffix('*') {
                Some(pre) => !all.iter().any(|p| p.starts_with(pre)),
                None => !all.contains(*n),
            })
            .collect();
        stale.sort();
        stale.dedup();
        for n in &stale {
            eprintln!("gc-hazards: annotations.txt names no function: `{n}`");
        }
        if !stale.is_empty() {
            std::process::exit(2);
        }
    }
    let trace = std::env::var("GC_HAZARDS_TRACE").ok();
    for &d in &defs {
        if GC_SEEDS.contains(&cx.path(d).as_str()) {
            cx.sums.insert(d, Summary { may_gc: true, ..Default::default() });
        }
    }
    let seeds: FxHashSet<DefId> = cx.sums.keys().copied().collect();
    if seeds.len() != GC_SEEDS.len() {
        for &d in &defs {
            let p = cx.path(d);
            if p.ends_with("collect") || p.ends_with("collect_from") {
                eprintln!("  candidate: {p}");
            }
        }
        eprintln!("gc-hazards: found {} of the {} functions that collect", seeds.len(), GC_SEEDS.len());
    }
    let entry = |b: &Body<'_>, closure: bool| -> State {
        let mut s = vec![St::default(); b.local_decls.len()];
        for i in 1..=b.arg_count {
            s[i] = if closure && i == 1 { ROOTED } else { St::param(i - 1) };
        }
        s
    };
    let mut round = 0;
    loop {
        round += 1;
        let mut changed = 0;
        for &d in &defs {
            if seeds.contains(&d) {
                continue;
            }
            let body = tcx.optimized_mir(d);
            let mut an = BodyAn::new(&cx, d, body);
            let e = entry(body, an.is_closure);
            let (s, _) = an.run(e, None);
            let mut s = s;
            if cx.ann.is_no_gc(&cx.path(d)) {
                s.may_gc = false;
                s.via = None;
            }
            let old = cx.sums.get(&d);
            let new = match old {
                Some(o) => Summary {
                    may_gc: o.may_gc | s.may_gc,
                    via: o.via.or(s.via),
                    needs_root: o.needs_root | s.needs_root,
                    ret: o.ret.join(s.ret),
                    out: o.out.iter().zip(&s.out).map(|(a, b)| a.join(*b)).collect(),
                    args: s.args,
                },
                None => s,
            };
            if old != Some(&new) {
                changed += 1;
                cx.sums.insert(d, new);
            }
        }
        eprintln!("gc-hazards: round {round}: {changed} summaries changed");
        if changed == 0 || round > 40 {
            break;
        }
    }
    let mut all = Vec::new();
    for &d in &defs {
        if seeds.contains(&d) {
            continue;
        }
        if cx.ann.is_no_gc(&cx.path(d)) {
            continue;
        }
        let body = tcx.optimized_mir(d);
        let mut an = BodyAn::new(&cx, d, body);
        let e = entry(body, an.is_closure);
        let (_, hz) = an.run(e, None);
        all.extend(hz);
    }
    let gc = cx.sums.values().filter(|s| s.may_gc).count();
    eprintln!("gc-hazards: {} functions, {} may collect, {} hazards", defs.len(), gc, all.len());
    if let Some(t) = trace {
        for &d in &defs {
            let p = cx.path(d);
            if p.contains(&t) {
                let mut chain = String::new();
                let mut at = cx.sums.get(&d).and_then(|s| s.via);
                let mut seen = FxHashSet::default();
                while let Some(v) = at {
                    if !seen.insert(v) {
                        break;
                    }
                    let _ = write!(chain, " → {}", cx.path(v));
                    at = cx.sums.get(&v).and_then(|s| s.via);
                }
                eprintln!(
                    "  {p}: needs_root {:b}, ret {:?}, may_gc{chain}",
                    cx.sums.get(&d).map_or(0, |s| s.needs_root),
                    cx.sums.get(&d).map(|s| s.ret)
                );
            }
        }
    }
    // One line per function, value and kind of hazard: the first call, and
    // how many calls more.
    let sm = tcx.sess.source_map();
    let mut first: std::collections::BTreeMap<(String, String, &str), (String, usize, usize, String)> =
        Default::default();
    for h in &all {
        let loc = sm.span_to_location_info(h.span);
        let file = loc.0.map(|f| f.name.prefer_local_unconditionally().to_string()).unwrap_or_default();
        let at = format!("{file}:{}:{}", loc.1, loc.2);
        let text =
            format!("{} `{}: {}` {} `{}`\t{}", h.kind, h.what, h.ty, "→", h.callee, h.chain.trim_start_matches(" → "));
        let e = first.entry((h.func.clone(), h.what.clone(), h.kind)).or_insert((at.clone(), loc.1, 0, text.clone()));
        e.2 += 1;
        if (file.as_str(), loc.1) < (e.0.split(':').next().unwrap_or(""), e.1) {
            e.0 = at;
            e.1 = loc.1;
            e.3 = text;
        }
    }
    let mut lines: Vec<String> = first
        .iter()
        .map(|((func, _, _), (at, _, n, text))| {
            format!("{at}\t{func}\t{text}{}", if *n > 1 { format!("\t(+{} calls)", n - 1) } else { String::new() })
        })
        .collect();
    lines.sort();
    let out = std::env::var("GC_HAZARDS_OUT").unwrap_or_else(|_| "gc-hazards.txt".into());
    std::fs::write(&out, lines.join("\n") + "\n").expect("write the hazards");
    eprintln!("gc-hazards: wrote {} lines to {out}", lines.len());
}

struct Cb;

impl rustc_driver::Callbacks for Cb {
    fn config(&mut self, config: &mut rustc_interface::interface::Config) {
        config.opts.unstable_opts.mir_opt_level = Some(0);
    }

    fn after_analysis<'tcx>(
        &mut self,
        _: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> rustc_driver::Compilation {
        analyze(tcx);
        rustc_driver::Compilation::Continue
    }
}

fn main() {
    // As `RUSTC_WORKSPACE_WRAPPER`: argv[1] is rustc. Only the `cloison`
    // library is analyzed.
    let args: Vec<String> = std::env::args().collect();
    let rustc_args = &args[1..];
    let is_target = rustc_args.windows(2).any(|w| w[0] == "--crate-name" && w[1] == "cloison")
        && rustc_args.iter().any(|a| a == "--crate-type" || a.starts_with("--crate-type"))
        && !rustc_args.iter().any(|a| a == "--test")
        && rustc_args.windows(2).any(|w| w[0] == "--crate-type" && w[1] == "lib");
    if !is_target {
        let status = std::process::Command::new(&rustc_args[0]).args(&rustc_args[1..]).status().expect("run rustc");
        std::process::exit(status.code().unwrap_or(1));
    }
    rustc_driver::compiler_entrypoint(rustc_args, &mut Cb);
}
