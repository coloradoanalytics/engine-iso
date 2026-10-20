#![allow(dead_code)]
//! An in-process engine-iso driven through the engine protocol, as
//! acalc-engine drives it, over OS pipes.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

pub struct Frame {
    pub header: Value,
    pub payload: Vec<u8>,
}
impl Frame {
    pub fn is_final(&self) -> bool {
        self.header.get("ok").is_some() || self.header.get("error").is_some()
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.payload).unwrap()
    }
}

pub struct Engine {
    writer: Mutex<std::io::PipeWriter>,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Frame>>>>,
    next: AtomicU64,
}
pub struct Call {
    pub id: u64,
    frames: mpsc::Receiver<Frame>,
}
impl Call {
    pub fn next(&self) -> Frame {
        self.frames
            .recv_timeout(Duration::from_secs(600))
            .expect("the engine did not answer")
    }
    /// Events, then the final frame.
    pub fn finish(self) -> (Vec<Frame>, Frame) {
        let mut events = vec![];
        loop {
            let frame = self.next();
            if frame.is_final() {
                return (events, frame);
            }
            events.push(frame);
        }
    }
}

impl Engine {
    pub fn start(workers: usize) -> Self {
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()
                .unwrap(),
        );
        let (requests, engine_input) = std::io::pipe().unwrap();
        let (replies, engine_output) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            iso_rust::engine::serve(pool, 2, BufReader::new(requests), engine_output).unwrap()
        });
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Frame>>>> = Arc::default();
        let routes = pending.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(replies);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let header: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                let mut payload = vec![0; header["size"].as_u64().unwrap_or(0) as usize];
                reader.read_exact(&mut payload).unwrap();
                let id = header["id"].as_u64().unwrap();
                let frame = Frame { header, payload };
                let last = frame.is_final();
                let mut routes = routes.lock().unwrap();
                if let Some(call) = routes.get(&id) {
                    let _ = call.send(frame);
                }
                if last {
                    routes.remove(&id);
                }
            }
        });
        Self {
            writer: Mutex::new(engine_input),
            pending,
            next: AtomicU64::new(1),
        }
    }
    pub fn call(&self, mut header: Value, payload: &[u8]) -> Call {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        header["id"] = json!(id);
        if !payload.is_empty() {
            header["size"] = json!(payload.len());
        }
        let (sender, frames) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(format!("{header}\n").as_bytes()).unwrap();
        writer.write_all(payload).unwrap();
        writer.flush().unwrap();
        Call { id, frames }
    }
    /// The final frame of a request.
    pub fn request(&self, header: Value, payload: &[u8]) -> Frame {
        self.call(header, payload).finish().1
    }
    pub fn cancel(&self, target: u64) {
        self.request(json!({"op": "cancel", "target": target}), b"");
    }
}

pub const BASE: &str = "/v1/calculations/test/artifacts/";

/// A finished calculation as acalc-engine sees it.
pub struct Calculation {
    pub accepted: Value,
    /// Progress objects, in order.
    pub progress: Vec<Value>,
    pub artifacts: Vec<(String, String, Vec<u8>)>,
    /// The final frame's `ok`, or its error.
    pub outcome: Result<Value, Value>,
    pub result: Value,
}
impl Calculation {
    pub fn final_progress(&self) -> &Value {
        &self.outcome.as_ref().unwrap()["progress"]
    }
}
/// Follow `call` (an accepted calculation or merge session) to its end.
pub fn follow(call: Call, accepted: Value) -> Calculation {
    let (events, last) = call.finish();
    let mut calculation = Calculation {
        accepted,
        progress: vec![],
        artifacts: vec![],
        outcome: match last.header.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(last.header["ok"].clone()),
        },
        result: Value::Null,
    };
    for event in events {
        match event.header["event"].as_str() {
            Some("progress") => calculation.progress.push(event.header["progress"].clone()),
            Some("artifact") => calculation.artifacts.push((
                event.header["name"].as_str().unwrap().into(),
                event.header["media_type"].as_str().unwrap().into(),
                event.payload,
            )),
            Some("planned") => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    if calculation.outcome.is_ok() {
        calculation.result = last.json();
    }
    calculation
}
/// Submit a request: its rejection, or the accepted calculation.
pub fn submit(engine: &Engine, request: &Value) -> Result<Calculation, Value> {
    let call = engine.call(
        json!({"op": "calculate", "artifact_base": BASE}),
        request.to_string().as_bytes(),
    );
    let first = call.next();
    if first.is_final() {
        return Err(first.header["error"].clone());
    }
    assert_eq!(first.header["event"], "accepted");
    Ok(follow(call, first.header["progress"].clone()))
}
/// A completed calculation's result and its progress record, checked for
/// monotone task points below 100% until completion.
pub fn calculate(engine: &Engine, request: &Value) -> Calculation {
    let calculation = submit(engine, request).expect("the engine rejected the request");
    assert!(calculation.outcome.is_ok(), "{:?}", calculation.outcome);
    let total = calculation.accepted["total_task_points"].as_f64().unwrap();
    let mut previous = 0.;
    for progress in std::iter::once(&calculation.accepted).chain(&calculation.progress) {
        assert_eq!(progress["total_task_points"].as_f64().unwrap(), total);
        let points = progress["completed_task_points"].as_f64().unwrap();
        assert!(points >= previous);
        assert!(progress["percent"].as_f64().unwrap() < 100.);
        previous = points;
    }
    let last = calculation.final_progress();
    assert_eq!(last["percent"], 100.);
    assert_eq!(last["completed_task_points"].as_f64(), Some(total));
    calculation
}
