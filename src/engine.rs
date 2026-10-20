//! The engine protocol on standard input and output: how acalc-engine
//! controls engine-iso (acalc-engine's docs/ENGINE_PROTOCOL.md).
//!
//! engine-iso has no network interface. acalc-engine starts it, sends it
//! requests and serves clients, keys, jobs and farms. Each frame is a JSON
//! header line followed by `size` payload bytes. Requests run concurrently:
//! CPU work runs on the shared Rayon pool, frames are written whole under one
//! lock, and progress is reported at most every `PROGRESS_INTERVAL` per
//! request. End of input cancels all work.
//!
//! Divisible work: `plan` prepares a request's `solver::Plan` and keeps it
//! under its key (SHA-256 of the request bytes); `run_item` runs one item and
//! returns its `ItemOutput`; a `merge` session holds the `Merge` of one
//! calculation, gives each item its input (the live cutoffs) and merges
//! outputs in any order. Item outputs are exact and pair-complete, so
//! unpruned results do not depend on where or when items ran.
use crate::{
    area_raster,
    request::decode_submission,
    scene::Cutoffs,
    solver::{ItemOutput, Merge, Plan},
    types::*,
    validation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// The engine protocol version.
pub const PROTOCOL: u64 = 1;
/// Shortest interval between two progress events of one request.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A caught solver panic never holds these mutexes. Recovering metadata
    // nevertheless keeps an unrelated request from stopping the engine.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

static VERSION: OnceLock<&'static str> = OnceLock::new();

/// Sets the version this program reports, for a build packaged under its own
/// version. Fails once the engine has reported its version.
pub fn set_version(version: &'static str) -> Result<(), &'static str> {
    VERSION
        .set(version)
        .map_err(|_| "engine version was already reported")
}

/// The version this program reports: its package version unless a build set
/// its own.
pub fn version() -> &'static str {
    VERSION.get_or_init(|| env!("CARGO_PKG_VERSION"))
}

/// The engine's identity, the reply to `hello`.
pub fn hello(workers: usize) -> Value {
    json!({
        "protocol": PROTOCOL,
        "engine": "engine-iso",
        "version": version(),
        "build_revision": crate::capabilities::build_revision(),
        "algorithm_revision": crate::capabilities::algorithm_revision(),
        "workers": workers,
        "kernels": crate::kernels::name(),
        "title": "ISO 9613-2:2024 outdoor sound propagation",
        "divisible": true,
        "method": "ISO 9613-2:2024",
        "atmosphere": "ISO 9613-1:1993",
    })
}

/// A work item's input: the cutoffs it starts with.
#[derive(Serialize, Deserialize)]
struct ItemInput {
    #[serde(with = "optional_band_bits")]
    cutoffs: Cutoffs,
}

/// Progress of one request, in the snapshot form clients know: task points,
/// percent below 100 until the result exists, phase, path counts and model
/// summary.
struct Progress {
    value: Mutex<(Value, bool)>,
}
impl Progress {
    fn new(request: &Value) -> Arc<Self> {
        let total = validation::task_points(request);
        Arc::new(Self {
            value: Mutex::new((
                json!({"total_task_points": total, "completed_task_points": 0., "percent": 0.,
                       "phase": "preparation", "path_counts": {"checked": 0, "pruned": 0}}),
                false,
            )),
        })
    }
    fn get(&self) -> Value {
        lock(&self.value).0.clone()
    }
    fn points(&self, completed: f64, phase: &str) {
        if !completed.is_finite() || completed < 0. {
            return;
        }
        let mut value = lock(&self.value);
        let progress = &mut value.0;
        let total = progress["total_task_points"].as_f64().unwrap_or(1.);
        let previous = progress["completed_task_points"].as_f64().unwrap_or(0.);
        // Keep the last unfinished share until the result is published with
        // completion, even if the solver reports it.
        let next = if completed >= total {
            previous
        } else {
            previous.max(completed)
        };
        if next == previous && progress["phase"].as_str() == Some(phase) {
            return;
        }
        progress["completed_task_points"] = json!(next);
        progress["percent"] = json!((100. * next / total).min(99.99999999999999));
        progress["phase"] = json!(phase);
        value.1 = true;
    }
    fn path_counts(&self, checked: usize, pruned: usize) {
        let mut value = lock(&self.value);
        value.0["path_counts"] = json!({"checked": checked, "pruned": pruned});
        value.1 = true;
    }
    fn model_summary(&self, sources: usize, receivers: usize) {
        let mut value = lock(&self.value);
        value.0["model_summary"] = json!({"source_count": sources, "receiver_count": receivers});
        value.1 = true;
    }
    /// The progress if it changed since last taken.
    fn take(&self) -> Option<Value> {
        let mut value = lock(&self.value);
        std::mem::replace(&mut value.1, false).then(|| value.0.clone())
    }
    /// The progress of the completed result.
    fn completed(&self) -> Value {
        let mut progress = self.get();
        progress["completed_task_points"] = progress["total_task_points"].clone();
        progress["percent"] = json!(100.);
        progress["phase"] = json!("completed");
        progress
    }
}

/// A request to a merge session.
enum Command {
    Input {
        id: u64,
        item: usize,
    },
    Output {
        id: u64,
        item: usize,
        bytes: Vec<u8>,
    },
    Finish {
        id: u64,
    },
    Cancel,
}

/// A serialized result and its area rasters.
type Output = (Vec<u8>, Vec<area_raster::AreaRaster>);

struct Server {
    pool: Arc<rayon::ThreadPool>,
    out: Mutex<Box<dyn Write + Send>>,
    /// Cancellation flags of running requests.
    cancels: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    sessions: Mutex<HashMap<u64, mpsc::Sender<Command>>>,
    /// Prepared plans by key, least recently used first.
    plans: Mutex<VecDeque<(String, Arc<Plan>)>>,
    plan_cache: usize,
    /// Progress of running calculations and sessions, reported periodically.
    reports: Mutex<HashMap<u64, Arc<Progress>>>,
    open: AtomicBool,
}

impl Server {
    /// Write one frame; a broken output means acalc-engine is gone, and end
    /// of input follows.
    fn send(&self, mut header: Value, payload: &[u8]) {
        if !payload.is_empty() {
            header["size"] = json!(payload.len());
        }
        let mut line = serde_json::to_vec(&header).expect("headers serialize");
        line.push(b'\n');
        let mut out = lock(&self.out);
        let _ = out
            .write_all(&line)
            .and_then(|_| out.write_all(payload))
            .and_then(|_| out.flush());
    }
    fn ok(&self, id: u64, ok: Value, payload: &[u8]) {
        self.send(json!({"id": id, "ok": ok}), payload);
    }
    fn error(&self, id: u64, error: Diagnostic) {
        self.send(json!({"id": id, "error": error}), b"");
    }
    fn register(&self, id: u64) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        lock(&self.cancels).insert(id, flag.clone());
        flag
    }
    fn release(&self, id: u64) {
        lock(&self.cancels).remove(&id);
    }
    fn plan(&self, key: &str) -> Option<Arc<Plan>> {
        let mut plans = lock(&self.plans);
        let i = plans.iter().position(|(k, _)| k == key)?;
        let entry = plans.remove(i)?;
        let plan = entry.1.clone();
        plans.push_back(entry);
        Some(plan)
    }
    fn keep(&self, key: &str, plan: Arc<Plan>) {
        let mut plans = lock(&self.plans);
        plans.retain(|(k, _)| k != key);
        plans.push_back((key.into(), plan));
        while plans.len() > self.plan_cache {
            plans.pop_front();
        }
    }
    /// Send the progress of every request whose progress changed. Holding
    /// `reports` while sending orders these events before a final frame.
    fn report(&self) {
        for (id, progress) in lock(&self.reports).iter() {
            if let Some(value) = progress.take() {
                self.send(
                    json!({"id": id, "event": "progress", "progress": value}),
                    b"",
                );
            }
        }
    }
    /// The final frames of a calculation or merge session: its artifacts and
    /// result, or its error.
    fn complete(&self, id: u64, progress: &Progress, result: Result<Output, Diagnostic>) {
        lock(&self.reports).remove(&id);
        self.release(id);
        match result {
            Ok((bytes, rasters)) => {
                for raster in rasters {
                    self.send(
                        json!({"id": id, "event": "artifact", "name": raster.name,
                               "media_type": area_raster::MEDIA_TYPE}),
                        &raster.bytes,
                    );
                }
                self.ok(
                    id,
                    json!({"progress": progress.completed(), "media_type": "application/json"}),
                    &bytes,
                );
            }
            Err(error) => self.error(id, error),
        }
    }

    /// One request; long work continues on the pool or its own thread.
    fn handle(self: &Arc<Self>, header: Map<String, Value>, payload: Vec<u8>) {
        let id = header.get("id").and_then(Value::as_u64).unwrap_or(0);
        let text = |name: &str| {
            header
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let number = |name: &str| header.get(name).and_then(Value::as_u64);
        match header.get("op").and_then(Value::as_str).unwrap_or("") {
            "hello" => self.ok(id, hello(self.pool.current_num_threads()), b""),
            "capabilities" => self.ok(id, crate::capabilities(), b""),
            "cancel" => {
                let target = number("target").unwrap_or(0);
                if let Some(flag) = lock(&self.cancels).get(&target) {
                    flag.store(true, Ordering::Relaxed);
                }
                if let Some(session) = lock(&self.sessions).get(&target) {
                    let _ = session.send(Command::Cancel);
                }
                self.ok(id, Value::Null, b"");
            }
            "calculate" => {
                let cancel = self.register(id);
                let server = self.clone();
                let base = text("artifact_base");
                self.pool
                    .spawn(move || server.calculate(id, &payload, &base, &cancel));
            }
            "plan" => {
                let cancel = self.register(id);
                let server = self.clone();
                let key = text("key");
                self.pool.spawn(move || {
                    let result = catch(|| {
                        if let Some(plan) = server.plan(&key) {
                            return Ok(plan);
                        }
                        let plan = Arc::new(Plan::prepare(decode_submission(&payload)?, &cancel)?);
                        server.keep(&key, plan.clone());
                        Ok(plan)
                    });
                    server.release(id);
                    match result {
                        Ok(plan) => server.ok(id, summary(&plan), b""),
                        Err(error) => server.error(id, error),
                    }
                });
            }
            "run_item" => {
                let key = text("key");
                let Some(plan) = self.plan(&key) else {
                    return self.error(
                        id,
                        Diagnostic::new("unknown_plan", "The plan is not prepared on this engine."),
                    );
                };
                let item = number("item").unwrap_or(u64::MAX) as usize;
                let input: ItemInput = match serde_json::from_slice(&payload) {
                    Ok(input) => input,
                    Err(e) => {
                        return self.error(
                            id,
                            Diagnostic::new("invalid_json", format!("Invalid item input: {e}")),
                        );
                    }
                };
                let cancel = self.register(id);
                let server = self.clone();
                self.pool.spawn(move || {
                    let (result, seconds) = own_time(|| {
                        catch(|| {
                            let output = plan.run_item(item, &input.cutoffs, &cancel)?;
                            serde_json::to_vec(&output).map_err(|e| {
                                Diagnostic::new(
                                    "internal_error",
                                    format!("Cannot serialize item: {e}"),
                                )
                            })
                        })
                    });
                    server.release(id);
                    match result {
                        Ok(bytes) => server.ok(id, json!({"seconds": seconds}), &bytes),
                        Err(error) => server.error(id, error),
                    }
                });
            }
            "merge" => {
                let cancel = self.register(id);
                let (sender, receiver) = mpsc::channel();
                lock(&self.sessions).insert(id, sender);
                let server = self.clone();
                let (key, base) = (text("key"), text("artifact_base"));
                let started = std::thread::Builder::new()
                    .name("iso-merge".into())
                    .spawn(move || server.merge(id, &payload, &key, &base, &cancel, receiver));
                if let Err(e) = started {
                    lock(&self.sessions).remove(&id);
                    self.release(id);
                    self.error(
                        id,
                        Diagnostic::new("internal_error", format!("Cannot start: {e}")),
                    );
                }
            }
            op @ ("item_input" | "item_output" | "finish") => {
                let session = number("session").unwrap_or(0);
                let item = number("item").unwrap_or(u64::MAX) as usize;
                let command = match op {
                    "item_input" => Command::Input { id, item },
                    "item_output" => Command::Output {
                        id,
                        item,
                        bytes: payload,
                    },
                    _ => Command::Finish { id },
                };
                let sent = lock(&self.sessions)
                    .get(&session)
                    .is_some_and(|s| s.send(command).is_ok());
                if !sent {
                    self.error(
                        id,
                        Diagnostic::new("unknown_session", "No such merge session."),
                    );
                }
            }
            op => self.error(
                id,
                Diagnostic::new("unknown_operation", format!("Unknown operation {op:?}")),
            ),
        }
    }

    fn calculate(&self, id: u64, payload: &[u8], base: &str, cancel: &AtomicBool) {
        let request = match catch(|| decode_submission(payload)) {
            Ok(request) => request,
            Err(error) => {
                self.release(id);
                return self.error(id, error);
            }
        };
        let progress = Progress::new(&request);
        self.send(
            json!({"id": id, "event": "accepted", "progress": progress.get()}),
            b"",
        );
        lock(&self.reports).insert(id, progress.clone());
        let result = catch(|| {
            let value = crate::solver::calculate_with_summary(
                &request,
                cancel,
                &|points, phase: &str| progress.points(points, phase),
                &|sources, receivers| progress.model_summary(sources, receivers),
                &|checked, pruned| progress.path_counts(checked, pruned),
            )?;
            check_cancel(cancel)?;
            serialize(value, base)
        });
        self.complete(id, &progress, result);
    }

    /// A merge session (see the module comment).
    fn merge(
        &self,
        id: u64,
        payload: &[u8],
        key: &str,
        base: &str,
        cancel: &AtomicBool,
        commands: mpsc::Receiver<Command>,
    ) {
        let end = |server: &Self| {
            lock(&server.sessions).remove(&id);
        };
        let request = match catch(|| decode_submission(payload)) {
            Ok(request) => request,
            Err(error) => {
                end(self);
                self.release(id);
                return self.error(id, error);
            }
        };
        let progress = Progress::new(&request);
        self.send(
            json!({"id": id, "event": "accepted", "progress": progress.get()}),
            b"",
        );
        lock(&self.reports).insert(id, progress.clone());
        let plan = match catch(|| self.pool.install(|| Plan::prepare(request, cancel))) {
            Ok(plan) => Arc::new(plan),
            Err(error) => {
                end(self);
                return self.complete(id, &progress, Err(error));
            }
        };
        self.keep(key, plan.clone());
        progress.model_summary(plan.sources(), plan.receivers());
        progress.points(1., "propagation");
        let mut planned = summary(&plan);
        planned["id"] = json!(id);
        planned["event"] = json!("planned");
        self.send(planned, b"");
        let on_progress = |points, phase: &str| progress.points(points, phase);
        let on_paths = |checked, pruned| progress.path_counts(checked, pruned);
        let merge = Merge::new(&plan, &on_progress, &on_paths);
        let items = plan.items();
        let outside = |item: usize| {
            Diagnostic::invalid(format!(
                "Work item {item} is outside the plan's {items} items"
            ))
        };
        let finished = self.pool.in_place_scope(|scope| {
            loop {
                match commands.recv() {
                    Ok(Command::Input { id, item }) => {
                        if item >= items {
                            self.error(id, outside(item));
                            continue;
                        }
                        let input = ItemInput {
                            cutoffs: merge.cutoffs(item),
                        };
                        let bytes = serde_json::to_vec(&input).expect("cutoffs serialize");
                        self.ok(id, json!({}), &bytes);
                    }
                    Ok(Command::Output { id, item, bytes }) => {
                        let merge = &merge;
                        scope.spawn(move |_| {
                            let result = catch(|| {
                                let output: ItemOutput =
                                    serde_json::from_slice(&bytes).map_err(|e| {
                                        Diagnostic::new(
                                            "invalid_json",
                                            format!("Invalid item output: {e}"),
                                        )
                                    })?;
                                merge.add_output(item, output)
                            });
                            match result {
                                Ok(()) => self.ok(id, Value::Null, b""),
                                Err(error) => self.error(id, error),
                            }
                        });
                    }
                    Ok(Command::Finish { id }) => {
                        self.ok(id, Value::Null, b"");
                        break true;
                    }
                    Ok(Command::Cancel) | Err(_) => break false,
                }
            }
        });
        end(self);
        // Every request gets a final frame, also those queued behind a cancel.
        for command in commands.try_iter() {
            match command {
                Command::Input { id, .. } | Command::Output { id, .. } | Command::Finish { id } => {
                    self.error(id, Diagnostic::cancelled())
                }
                Command::Cancel => {}
            }
        }
        let result = if !finished || cancel.load(Ordering::Relaxed) {
            Err(Diagnostic::cancelled())
        } else {
            catch(|| {
                let value = self.pool.install(|| merge.finish(cancel))?;
                check_cancel(cancel)?;
                serialize(value, base)
            })
        };
        self.complete(id, &progress, result);
    }
}

/// Run `work`, turning a panic into an internal error.
fn catch<T>(work: impl FnOnce() -> Result<T, Diagnostic>) -> Result<T, Diagnostic> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| {
        Err(Diagnostic::new(
            "internal_error",
            "Calculation worker panicked; no partial result was published.",
        ))
    })
}
fn summary(plan: &Plan) -> Value {
    json!({"items": plan.items(), "sources": plan.sources(), "receivers": plan.receivers(),
           "build_revision": crate::capabilities::build_revision()})
}
/// The result document, with area samples moved into rasters linked under
/// `base` (where acalc-engine serves the job's artifacts).
fn serialize(mut value: Value, base: &str) -> Result<Output, Diagnostic> {
    let rasters = area_raster::split_area_rasters(&mut value, |name| format!("{base}{name}"))?;
    let bytes = serde_json::to_vec(&value).map_err(|e| {
        Diagnostic::new(
            "internal_error",
            format!("Cannot serialize completed result: {e}"),
        )
    })?;
    Ok((bytes, rasters))
}

thread_local! {
    /// Time spent on requests run within the current one on this thread.
    static NESTED: std::cell::Cell<f64> = const { std::cell::Cell::new(0.) };
}
/// Run `work` and return its time in seconds, excluding requests run within
/// it: a worker waiting on parallel work inside an item, as in extended-
/// source integration, may meanwhile run other items on its own stack.
fn own_time<T>(work: impl FnOnce() -> T) -> (T, f64) {
    let enclosing = NESTED.replace(0.);
    let start = Instant::now();
    let result = work();
    let elapsed = start.elapsed().as_secs_f64();
    let nested = NESTED.replace(enclosing + elapsed);
    (result, elapsed - nested)
}

/// Serve the engine protocol: requests from `input`, frames to `output`,
/// until end of input, which cancels every running request.
pub fn serve(
    pool: Arc<rayon::ThreadPool>,
    plan_cache: usize,
    mut input: impl BufRead,
    output: impl Write + Send + 'static,
) -> std::io::Result<()> {
    let server = Arc::new(Server {
        pool,
        out: Mutex::new(Box::new(output)),
        cancels: Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        plans: Mutex::new(VecDeque::new()),
        plan_cache: plan_cache.max(1),
        reports: Mutex::new(HashMap::new()),
        open: AtomicBool::new(true),
    });
    let reporter = server.clone();
    std::thread::Builder::new()
        .name("iso-progress".into())
        .spawn(move || {
            while reporter.open.load(Ordering::Acquire) {
                std::thread::sleep(PROGRESS_INTERVAL);
                reporter.report();
            }
        })?;
    let mut line = Vec::new();
    let result = loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) => break Ok(()),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let header: Map<String, Value> = match serde_json::from_slice(&line) {
            Ok(header) => header,
            Err(e) => {
                break Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unreadable request header: {e}"),
                ));
            }
        };
        let size = header.get("size").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut payload = vec![0; size];
        if let Err(e) = input.read_exact(&mut payload) {
            break Err(e);
        }
        server.handle(header, payload);
    };
    // acalc-engine is gone: stop everything.
    server.open.store(false, Ordering::Release);
    for flag in lock(&server.cancels).values() {
        flag.store(true, Ordering::Relaxed);
    }
    lock(&server.sessions).clear();
    result
}
