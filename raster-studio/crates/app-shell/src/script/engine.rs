//! W13-K: the JavaScript half of File ▸ Script — `boa_engine` on a thread of
//! its own, talking to the editor over two channels.
//!
//! # Why a thread
//!
//! A script reads the document between its edits (`doc.layers.length` after
//! an `artLayers.add()`), so every DOM call has to reach the live
//! [`crate::Editor`] and come back with an answer. `boa` wants its native
//! functions to be `'static`, and the editor is a `&mut` borrow for the length
//! of one frame. Rather than smuggle that borrow into the engine as a raw
//! pointer, the engine runs on its own thread and each DOM call is a message:
//! the interaction thread (which holds the `&mut Editor`) answers it and the
//! engine thread blocks until it has. Nothing on the engine thread can touch
//! the editor, a file or the network — the one native function it has is the
//! channel.
//!
//! # The budget
//!
//! The script runs through [`Script::evaluate_async_with_budget`], which
//! yields to its caller every [`CHUNK`] units of VM cost. The poll loop here
//! counts those yields against [`ScriptLimits::budget`] and checks the wall
//! clock against [`ScriptLimits::wall`]; past either, the evaluation is
//! dropped where it stands — so `while (true) {}` ends with a sentence, not a
//! hung window. Two engine limits back that up for code the budgeted loop
//! does not drive: a callback a builtin invokes (`[].forEach(fn)`) runs
//! nested, so `boa`'s per-loop iteration limit and recursion limit are set
//! too, and every DOM call checks the wall clock before it is sent.

use std::cell::RefCell;
use std::future::Future;
use std::sync::mpsc::{Receiver, Sender};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::{Duration, Instant};

use boa_engine::native_function::NativeFunction;
use boa_engine::{js_string, Context, JsArgs, JsError, JsNativeError, JsResult, JsString};
use boa_engine::{JsValue, Script, Source};

/// The DOM, in JavaScript, over the one native function.
pub(crate) const PRELUDE: &str = include_str!("prelude.js");

/// VM cost units between two budget checks.
pub(crate) const CHUNK: u32 = 10_000;

/// How much a run may do before it is stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScriptLimits {
    /// Total VM cost units (roughly instructions) the top-level evaluation
    /// may spend.
    pub budget: u64,
    /// Iterations any single loop may run (`boa`'s own limit).
    pub loop_iterations: u64,
    /// Nested function calls.
    pub recursion: usize,
    /// Wall-clock time for the whole run, DOM calls included.
    pub wall: Duration,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            budget: 400_000_000,
            loop_iterations: 50_000_000,
            recursion: 512,
            wall: Duration::from_secs(20),
        }
    }
}

/// How the engine thread's run ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EngineEnd {
    /// The script ran to its end.
    Completed,
    /// It did not parse; nothing ran.
    Syntax(String),
    /// It threw and nothing caught it.
    Uncaught(String),
    /// A limit stopped it.
    Stopped(String),
}

/// What the engine thread sends the interaction thread.
pub(crate) enum ToHost {
    /// One DOM call: the op name and its arguments as JSON text. Answered
    /// with one JSON reply on the reply channel.
    Call { op: String, args: String },
    /// The run is over.
    Finished(EngineEnd),
}

struct Link {
    to_host: Sender<ToHost>,
    replies: Receiver<String>,
    deadline: Instant,
}

thread_local! {
    /// The engine thread's end of the two channels. Only ever set on the
    /// thread [`run_on_thread`] runs on.
    static LINK: RefCell<Option<Link>> = const { RefCell::new(None) };
}

/// The engine thread's body: evaluate `source` and report how it ended.
pub(crate) fn run_on_thread(
    source: String,
    limits: ScriptLimits,
    to_host: Sender<ToHost>,
    replies: Receiver<String>,
) {
    let deadline = Instant::now() + limits.wall;
    LINK.with(|link| {
        *link.borrow_mut() = Some(Link {
            to_host: to_host.clone(),
            replies,
            deadline,
        })
    });
    let end = evaluate(&source, &limits, deadline);
    LINK.with(|link| link.borrow_mut().take());
    let _ = to_host.send(ToHost::Finished(end));
}

/// The one native function: `__host(op, argsJson) -> replyJson`.
fn host_call(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let op = args
        .get_or_undefined(0)
        .to_string(ctx)?
        .to_std_string_escaped();
    let json = args
        .get_or_undefined(1)
        .to_string(ctx)?
        .to_std_string_escaped();
    let reply = LINK.with(|link| -> JsResult<String> {
        let link = link.borrow();
        let link = link
            .as_ref()
            .ok_or_else(|| JsNativeError::error().with_message("the editor is not listening"))?;
        if Instant::now() > link.deadline {
            return Err(JsNativeError::runtime_limit()
                .with_message("the script ran past its time limit")
                .into());
        }
        link.to_host
            .send(ToHost::Call { op, args: json })
            .map_err(|_| JsNativeError::error().with_message("the editor stopped listening"))?;
        link.replies.recv().map_err(|_| {
            JsNativeError::error()
                .with_message("the editor stopped listening")
                .into()
        })
    })?;
    Ok(JsValue::from(JsString::from(reply.as_str())))
}

/// Parse and run `source` under `limits`.
pub(crate) fn evaluate(source: &str, limits: &ScriptLimits, deadline: Instant) -> EngineEnd {
    let mut ctx = Context::default();
    {
        let engine_limits = ctx.runtime_limits_mut();
        engine_limits.set_loop_iteration_limit(limits.loop_iterations);
        engine_limits.set_recursion_limit(limits.recursion);
    }
    if let Err(e) = ctx.register_global_builtin_callable(
        js_string!("__host"),
        2,
        NativeFunction::from_fn_ptr(host_call),
    ) {
        return EngineEnd::Uncaught(format!("the script runner could not start: {e}"));
    }
    if let Err(e) = ctx.eval(Source::from_bytes(PRELUDE)) {
        let (_, message) = describe(e, &mut ctx);
        return EngineEnd::Uncaught(format!("the script runner could not start: {message}"));
    }
    let script = match Script::parse(Source::from_bytes(source), None, &mut ctx) {
        Ok(script) => script,
        Err(e) => return EngineEnd::Syntax(first_line(&describe(e, &mut ctx).1)),
    };

    enum Run {
        Done(JsResult<JsValue>),
        OverBudget,
        OverTime,
    }
    let run = {
        let future = script.evaluate_async_with_budget(&mut ctx, CHUNK);
        let mut future = std::pin::pin!(future);
        let mut task = TaskContext::from_waker(Waker::noop());
        let mut spent: u64 = 0;
        loop {
            match future.as_mut().poll(&mut task) {
                Poll::Ready(result) => break Run::Done(result),
                Poll::Pending => {
                    spent = spent.saturating_add(u64::from(CHUNK));
                    if spent > limits.budget {
                        break Run::OverBudget;
                    }
                    if Instant::now() > deadline {
                        break Run::OverTime;
                    }
                }
            }
        }
    };
    match run {
        Run::Done(Ok(_)) => EngineEnd::Completed,
        Run::Done(Err(e)) => match describe(e, &mut ctx) {
            (true, message) => EngineEnd::Stopped(first_line(&message)),
            (false, message) => EngineEnd::Uncaught(first_line(&message)),
        },
        Run::OverBudget => EngineEnd::Stopped(format!(
            "stopped after {} steps: the script ran past its step budget (an endless loop?)",
            limits.budget
        )),
        Run::OverTime => EngineEnd::Stopped(format!(
            "stopped after {} s: the script ran past its time limit",
            limits.wall.as_secs_f32()
        )),
    }
}

/// `(is a runtime-limit stop, the sentence)`.
fn describe(e: JsError, ctx: &mut Context) -> (bool, String) {
    if let Some(native) = e.as_native() {
        if native.is_runtime_limit() {
            return (true, native.message().to_string());
        }
    }
    match e.try_native(ctx) {
        Ok(native) => (false, native.to_string()),
        Err(_) => (false, e.to_string()),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}
