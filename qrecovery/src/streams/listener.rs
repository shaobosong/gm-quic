use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll, Waker},
};

use qbase::{
    error::Error as QuicError,
    frame::{ResetStreamFrame, SendFrame},
    sid::StreamId,
};

use crate::{
    recv::{ArcRecver, Reader},
    send::{ArcSender, Outgoing, Writer},
};

#[derive(Debug)]
struct RawListener<TX> {
    // 对方主动创建的流
    bi_streams: VecDeque<(StreamId, (ArcRecver, ArcSender<TX>))>,
    uni_streams: VecDeque<(StreamId, ArcRecver)>,
    bi_waker: Option<Waker>,
    uni_waker: Option<Waker>,
}

impl<TX> RawListener<TX>
where
    TX: SendFrame<ResetStreamFrame> + Clone + Send + 'static,
{
    fn new() -> Self {
        Self {
            bi_streams: VecDeque::with_capacity(4),
            uni_streams: VecDeque::with_capacity(2),
            bi_waker: None,
            uni_waker: None,
        }
    }

    fn push_bi_stream(&mut self, sid: StreamId, stream: (ArcRecver, ArcSender<TX>)) {
        self.bi_streams.push_back((sid, stream));
        if let Some(waker) = self.bi_waker.take() {
            waker.wake();
        }
    }

    fn push_recv_stream(&mut self, sid: StreamId, stream: ArcRecver) {
        self.uni_streams.push_back((sid, stream));
        if let Some(waker) = self.uni_waker.take() {
            waker.wake();
        }
    }

    #[allow(clippy::type_complexity)]
    fn poll_accept_bi_stream(
        &mut self,
        cx: &mut Context<'_>,
        send_wnd_size: u64,
    ) -> Poll<Result<(StreamId, (Reader, Writer<TX>)), QuicError>> {
        if let Some((sid, (recever, sender))) = self.bi_streams.pop_front() {
            let outgoing = Outgoing(sender);
            outgoing.update_window(send_wnd_size);
            Poll::Ready(Ok((sid, (Reader(recever), Writer(outgoing.0)))))
        } else {
            self.bi_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn poll_accept_recv_stream(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(StreamId, Reader), QuicError>> {
        if let Some((sid, reader)) = self.uni_streams.pop_front() {
            Poll::Ready(Ok((sid, Reader(reader))))
        } else {
            self.uni_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArcListener<TX>(Arc<Mutex<Result<RawListener<TX>, QuicError>>>);

impl<TX> ArcListener<TX>
where
    TX: SendFrame<ResetStreamFrame> + Clone + Send + 'static,
{
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Ok(RawListener::new()))))
    }

    pub(crate) fn guard(&self) -> Result<ListenerGuard<TX>, QuicError> {
        let guard = self.0.lock().unwrap();
        match guard.as_ref() {
            Ok(_) => Ok(ListenerGuard { inner: guard }),
            Err(e) => Err(e.clone()),
        }
    }

    pub fn accept_bi_stream(&self, send_wnd_size: u64) -> AcceptBiStream<TX> {
        AcceptBiStream {
            inner: self,
            send_wnd_size,
        }
    }

    pub fn accept_uni_stream(&self) -> AcceptUniStream<TX> {
        AcceptUniStream { inner: self }
    }

    #[allow(clippy::type_complexity)]
    pub fn poll_accept_bi_stream(
        &self,
        cx: &mut Context<'_>,
        send_wnd_size: u64,
    ) -> Poll<Result<(StreamId, (Reader, Writer<TX>)), QuicError>> {
        match self.0.lock().unwrap().as_mut() {
            Ok(set) => set.poll_accept_bi_stream(cx, send_wnd_size),
            Err(e) => Poll::Ready(Err(e.clone())),
        }
    }

    pub fn poll_accept_uni_stream(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(StreamId, Reader), QuicError>> {
        match self.0.lock().unwrap().as_mut() {
            Ok(set) => set.poll_accept_recv_stream(cx),
            Err(e) => Poll::Ready(Err(e.clone())),
        }
    }
}

pub(crate) struct ListenerGuard<'a, TX> {
    inner: MutexGuard<'a, Result<RawListener<TX>, QuicError>>,
}

impl<TX> ListenerGuard<'_, TX>
where
    TX: SendFrame<ResetStreamFrame> + Clone + Send + 'static,
{
    pub(crate) fn push_bi_stream(&mut self, sid: StreamId, stream: (ArcRecver, ArcSender<TX>)) {
        match self.inner.as_mut() {
            Ok(set) => set.push_bi_stream(sid, stream),
            Err(e) => unreachable!("listener is invalid: {e}"),
        }
    }

    pub(crate) fn push_uni_stream(&mut self, sid: StreamId, stream: ArcRecver) {
        match self.inner.as_mut() {
            Ok(set) => set.push_recv_stream(sid, stream),
            Err(e) => unreachable!("listener is invalid: {e}"),
        }
    }

    pub(crate) fn on_conn_error(&mut self, e: &QuicError) {
        match self.inner.as_mut() {
            Ok(set) => {
                if let Some(waker) = set.bi_waker.take() {
                    waker.wake();
                }
                if let Some(waker) = set.uni_waker.take() {
                    waker.wake();
                }
            }
            Err(e) => unreachable!("listener is invalid: {e}"),
        };
        *self.inner = Err(e.clone());
    }
}

/// Future to accept a bidirectional stream.
///
/// This future is created by `accept_bi_stream` method of `QuicConnection`.
///
/// When the peer created a new bidirectional stream, the future will resolve with a [`Reader`] and
/// a [`Writer`] to read and write data on the stream.
#[derive(Debug, Clone)]
pub struct AcceptBiStream<'l, TX> {
    inner: &'l ArcListener<TX>,
    send_wnd_size: u64,
}

impl<TX> Future for AcceptBiStream<'_, TX>
where
    TX: SendFrame<ResetStreamFrame> + Clone + Send + 'static,
{
    type Output = Result<(StreamId, (Reader, Writer<TX>)), QuicError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.poll_accept_bi_stream(cx, self.send_wnd_size)
    }
}

/// Future to accept a bidirectional stream.
///
/// This future is created by `accept_uni_stream` method of `QuicConnection`.
///
/// When the peer created a new bidirectional stream, the future will resolve with a [`Reader`] to
/// read data on the stream.
#[derive(Debug, Clone)]
pub struct AcceptUniStream<'l, TX> {
    inner: &'l ArcListener<TX>,
}

impl<TX> Future for AcceptUniStream<'_, TX>
where
    TX: SendFrame<ResetStreamFrame> + Clone + Send + 'static,
{
    type Output = Result<(StreamId, Reader), QuicError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.poll_accept_uni_stream(cx)
    }
}
