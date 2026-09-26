//! Private Agents endpoint for CLI tests. The thread owns its listener and is
//! joined on drop so no server or state escapes a test's temporary directory.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[allow(dead_code)] // Shared by CLI suites that use different controls.
pub struct PolicyServer {
    pub wire: Arc<Mutex<Option<Vec<u8>>>>,
    pub delay: Arc<Mutex<Duration>>,
    pub after_first: Arc<Mutex<Option<Value>>>,
    pub reply: Arc<Mutex<Value>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl PolicyServer {
    pub fn new(root: &Path, workspaces: &[&str], excluded: &[&str]) -> Self {
        std::fs::create_dir_all(root).unwrap();
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let reply = Arc::new(Mutex::new(json!({"ok":true,"version":1,
            "herdr_socket":"/test/host.sock", "excluded_labels": if excluded.is_empty() {vec![]} else {vec!["delegated"]},
            "workspace_ids":workspaces,"excluded_workspace_ids":excluded,"show_excluded":false})));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wire: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let delay = Arc::new(Mutex::new(Duration::ZERO));
        let after_first = Arc::new(Mutex::new(None));
        let (raw, lag, after) = (wire.clone(), delay.clone(), after_first.clone());
        let (r, q, s) = (reply.clone(), requests.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !s.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut line = String::new();
                        if BufReader::new(&stream).read_line(&mut line).is_ok() {
                            q.lock().unwrap().push(serde_json::from_str(&line).unwrap());
                            let mut bytes = serde_json::to_vec(&*r.lock().unwrap()).unwrap();
                            bytes.push(b'\n');
                            if let Some(raw) = &*raw.lock().unwrap() {
                                bytes = raw.clone();
                            }
                            thread::sleep(*lag.lock().unwrap());
                            let _ = stream.write_all(&bytes);
                            if let Some(next) = after.lock().unwrap().take() {
                                *r.lock().unwrap() = next;
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            wire,
            delay,
            after_first,
            reply,
            requests,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for PolicyServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
