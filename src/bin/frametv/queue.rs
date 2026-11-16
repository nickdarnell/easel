//! Turns at talking to a TV. The TV answers one thing at a time and slowly, so each TV gets a
//! queue: whoever holds the [`Turn`] talks to it, and the rest wait, most important first.
//!
//! Waiting is just an `await`, so a request whose caller has gone away (the page was closed, an
//! image was scrolled past) leaves the queue when its future is dropped, without ever reaching
//! the TV.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// Who goes first, most important first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prio {
    /// Someone asked the TV to do something.
    Command,
    /// Someone is waiting for the answer (an image on screen).
    Read,
    /// Keeping what the server knows up to date.
    Background,
}

#[derive(Default)]
pub struct Queue {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    busy: bool,
    seq: u64,
    waiting: BTreeMap<(Prio, u64), oneshot::Sender<()>>,
}

/// The right to talk to the TV until it's dropped.
pub struct Turn {
    queue: Arc<Queue>,
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.queue.pass_on();
    }
}

/// A place in the queue; leaving it (dropping it) gives up the place, or the turn if it had
/// just been handed over.
struct Waiting {
    queue: Arc<Queue>,
    key: (Prio, u64),
    rx: Option<oneshot::Receiver<()>>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let Some(mut rx) = self.rx.take() else { return };
        if self.queue.inner.lock().unwrap().waiting.remove(&self.key).is_some() {
            return;
        }
        // Handed the turn in the moment it left: give it to the next in line.
        if rx.try_recv().is_ok() {
            self.queue.pass_on();
        }
    }
}

impl Queue {
    /// Wait for a turn.
    pub async fn turn(self: &Arc<Self>, prio: Prio) -> Turn {
        let mut waiting = {
            let mut q = self.inner.lock().unwrap();
            if !q.busy {
                q.busy = true;
                return Turn { queue: Arc::clone(self) };
            }
            q.seq += 1;
            let key = (prio, q.seq);
            let (tx, rx) = oneshot::channel();
            q.waiting.insert(key, tx);
            Waiting { queue: Arc::clone(self), key, rx: Some(rx) }
        };
        if let Some(rx) = waiting.rx.as_mut() {
            // Senders are only dropped after sending.
            let _ = rx.await;
        }
        waiting.rx = None;
        Turn { queue: Arc::clone(self) }
    }

    /// How many are waiting.
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.inner.lock().unwrap().waiting.len()
    }

    /// How many are waiting at each priority.
    pub fn waiting_by(&self) -> [u32; 3] {
        let mut n = [0u32; 3];
        for (prio, _) in self.inner.lock().unwrap().waiting.keys() {
            n[*prio as usize] += 1;
        }
        n
    }

    fn pass_on(&self) {
        let mut q = self.inner.lock().unwrap();
        while let Some((_, tx)) = q.waiting.pop_first() {
            if tx.send(()).is_ok() {
                return;
            }
        }
        q.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn most_important_first_and_leavers_skipped() {
        let q = Arc::new(Queue::default());
        let first = q.turn(Prio::Background).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for (name, prio) in [("bg", Prio::Background), ("read", Prio::Read), ("cmd", Prio::Command)] {
            let (q, order) = (Arc::clone(&q), Arc::clone(&order));
            tasks.push(tokio::spawn(async move {
                let _t = q.turn(prio).await;
                order.lock().unwrap().push(name);
            }));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Someone who gives up while waiting never gets a turn.
        let leaver = {
            let (q, order) = (Arc::clone(&q), Arc::clone(&order));
            tokio::spawn(async move {
                let _t = q.turn(Prio::Command).await;
                order.lock().unwrap().push("leaver");
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(q.waiting(), 4);
        leaver.abort();
        let _ = leaver.await;
        assert_eq!(q.waiting(), 3);
        drop(first);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["cmd", "read", "bg"]);
        // And the queue is free again.
        let _t = tokio::time::timeout(Duration::from_secs(1), q.turn(Prio::Background)).await.unwrap();
    }
}
