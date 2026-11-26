//! Presence, not ownership: the queue lock alone still serializes passes.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use trk_core::error::Result;
use trk_core::ops::CHECK_WATCH_HEARTBEAT_SECS;
use trk_core::system::hostname;

use crate::env::Env;

const HEARTBEAT: Duration = Duration::from_secs(CHECK_WATCH_HEARTBEAT_SECS);

/// Exec keeps the PID and durable row, but discards this guard and its thread
/// without running Drop. The replacement refreshes the same registration.
pub(super) struct Watch<'a> {
    env: &'a Env,
    actor: String,
    stop: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl<'a> Watch<'a> {
    pub(super) fn register(env: &'a Env) -> Result<Self> {
        let actor = env.actor()?.to_owned();
        let pid = i64::from(std::process::id());
        let host = hostname();
        let mut store = env.store()?;
        store.write(&actor, false, |c| c.check_watch_register(pid, host))?;
        let mut watch = Self {
            env,
            actor: actor.clone(),
            stop: None,
            worker: None,
        };
        let (stop, receiver) = mpsc::channel();
        watch.stop = Some(stop);
        // A separate connection also refreshes during long checks and GC.
        watch.worker = Some(
            thread::Builder::new()
                .name("check-heartbeat".into())
                .spawn(move || {
                    loop {
                        match receiver.recv_timeout(HEARTBEAT) {
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => {
                                if let Err(error) = store
                                    .write(&actor, false, |c| c.check_watch_heartbeat(pid, host))
                                {
                                    eprintln!(
                                        "trk check: watcher heartbeat failed: {}",
                                        error.message()
                                    );
                                }
                            }
                        }
                    }
                })?,
        );
        Ok(watch)
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Err(error) = self.env.store().and_then(|mut store| {
            store.write(&self.actor, false, |c| {
                c.check_watch_release(i64::from(std::process::id()), hostname())
            })
        }) {
            eprintln!("trk check: watcher cleanup failed: {}", error.message());
        }
    }
}
