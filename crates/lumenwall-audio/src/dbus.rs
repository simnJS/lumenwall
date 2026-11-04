//! Helpers over zbus shared by the UPower and MPRIS watchers (Linux).
//!
//! Each watcher is one thread running `zbus::block_on`: it sleeps in the runtime's reactor on
//! the bus socket until a signal arrives or [`Stop::stop`] wakes it. No timer is armed while
//! nothing happens (method calls only wait for their own reply).

use std::future::poll_fn;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Poll, Waker};

use futures_core::Stream;
use zbus::message::Type;
use zbus::{MatchRule, Message, MessageStream};

/// A stop request that also wakes the watcher's `block_on`.
#[derive(Clone, Default)]
pub(crate) struct Stop(Arc<StopInner>);

#[derive(Default)]
struct StopInner {
    stopped: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Stop {
    pub(crate) fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        let waker = self
            .0
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(w) = waker {
            w.wake();
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }
}

/// Stops the watcher when dropped (without waiting for its thread: the caller may be the
/// render thread).
pub(crate) struct StopOnDrop(pub(crate) Stop);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

pub(crate) enum Next {
    Stop,
    /// A message of the stream at this index.
    Message(usize, Message),
    /// A stream ended: the connection is gone.
    Closed,
}

/// Waits for the next message of any of `streams`, or for `stop`.
pub(crate) async fn next(stop: &Stop, streams: &mut [Pin<Box<MessageStream>>]) -> Next {
    poll_fn(|cx| {
        *stop.0.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
        if stop.is_stopped() {
            return Poll::Ready(Next::Stop);
        }
        for (i, s) in streams.iter_mut().enumerate() {
            loop {
                match s.as_mut().poll_next(cx) {
                    Poll::Ready(Some(Ok(m))) => return Poll::Ready(Next::Message(i, m)),
                    Poll::Ready(Some(Err(e))) => tracing::debug!("D-Bus stream: {e}"),
                    Poll::Ready(None) => return Poll::Ready(Next::Closed),
                    Poll::Pending => break,
                }
            }
        }
        Poll::Pending
    })
    .await
}

/// Signals `interface.member`, optionally only on `path`.
pub(crate) fn signal_rule(
    interface: &'static str,
    member: &'static str,
    path: Option<&'static str>,
    arg0: Option<&'static str>,
) -> zbus::Result<MatchRule<'static>> {
    let mut b = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(interface)
        .member(member);
    if let Some(p) = path {
        b = b.path(p);
    }
    if let Some(a) = arg0 {
        b = b.arg(0, a);
    }
    b.build()
}

/// A boxed stream of the messages matching `rule` (the match rule is added to the bus).
pub(crate) async fn stream(
    conn: &zbus::Connection,
    rule: MatchRule<'static>,
) -> zbus::Result<Pin<Box<MessageStream>>> {
    Ok(Box::pin(
        MessageStream::for_match_rule(rule, conn, None).await?,
    ))
}
