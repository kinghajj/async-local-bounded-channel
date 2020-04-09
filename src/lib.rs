//! A same-producer, same-consumer channel, bounded to a single async task.
//!
//! # Implementation details
//!
//! Internally, this uses the `generic-array` crate, which utilizes types from
//! `typenum` to specify the capacity at compile time, allowing the space for
//! the queue to be allocated inline. Thus, this channel also requires
//! specifying the capacity upfront at compile time.
//!
//! # Examples
//!
//! Used together with `futures::future::select`, this can implement something
//! like a coroutine, where two asynchronous generators cooperate producing
//! and consuming values.
//!
//! ```
//! # use futures::pin_mut;
//! # use futures::future::{Either, select};
//! # use typenum::U8;
//! # use async_local_bounded_channel::channel;
//! futures::executor::block_on(async move {
//!     // create a new channel with a capacity of 8 items
//!     let mut channel = channel::<_, U8>();
//!     let (mut tx, mut rx) = channel.split();
//!     let producer = async move {
//!         for i in 0..100 {
//!             tx.send(i).await.expect("consumer still alive");
//!         }
//!     };
//!     let consumer = async move {
//!         let mut expected = 0;
//!         loop {
//!             if let Ok(v) = rx.receive().await {
//!                 assert_eq!(v, expected);
//!                 expected += 1;
//!             } else {
//!                 break;
//!             }
//!         }
//!     };
//!     pin_mut!(producer, consumer);
//!     let remaining = select(producer, consumer).await.factor_first().1;
//!     match remaining {
//!         Either::Left(f) => f.await,
//!         Either::Right(f) => f.await,
//!     }
//! });
//! ```
//!
//! This can be useful, for example, when implementing a server. One task can
//! handle each client, where the producer waits for incoming requests and
//! writes responses; and the consumer waits for requests, handles them, and
//! then generates a response.
//!
//! # Usage notes
//!
//! Once the transmission endpoints have been acquired via `split()`, the
//! channel cannot be moved. This is required for safety, since each endpoint
//! contains a reference back to the channel; thus, if the channel were to move,
//! those references would become dangling.
//!
//! ```compile_fail
//! # use typenum::U8;
//! # use async_local_bounded_channel::channel;
//! let mut channel = channel::<isize, U8>();
//! let (tx, rx) = channel.split();
//! std::thread::spawn(move || {
//!     // nope!
//!     let channel = channel;
//!     let tx = tx;
//!     let rx = rx;
//! });
//! ```
//!
//! Further, endpoints must remain anchored to a single thread, since access
//! to the underlying data structures is not thread-safe. Unfortunately, this
//! _isn't_ enforced by the compiler, and scoped thread libraries can allow
//! unsafe usage. For example:
//!
//! ```
//! # use typenum::U8;
//! # use async_local_bounded_channel::channel;
//! // shouldn't compile, but unfortunately does.
//! let mut channel = channel::<isize, U8>();
//! crossbeam::thread::scope(|s| {
//!     let (tx, rx) = channel.split();
//!     // don't do this!
//!     s.spawn(move |_| {
//!         let tx = tx;
//!     });
//!     s.spawn(move |_| {
//!         let rx = rx;
//!     });
//! });
//! ```
//!
//! If there are no open endpoints, though, a channel can be safely moved and
//! sent. A channel can even be re-used after the endpoints are dropped.
//!
//! ```
//! # use futures::executor::block_on;
//! # use futures::pin_mut;
//! # use futures::future::{Either, select};
//! # use typenum::U8;
//! # use async_local_bounded_channel::channel;
//! type C = async_local_bounded_channel::Channel<isize, U8>;
//!
//! async fn test_channel(mut channel: C) -> C {
//!     // run the producer-consumer example above.
//!     # {
//!     #     let (mut tx, mut rx) = channel.split();
//!     #     let producer = async move {
//!     #         for i in 0..100 {
//!     #             tx.send(i).await.expect("consumer still alive");
//!     #         }
//!     #     };
//!     #     let consumer = async move {
//!     #         let mut expected = 0;
//!     #         loop {
//!     #             if let Ok(v) = rx.receive().await {
//!     #                 assert_eq!(v, expected);
//!     #                 expected += 1;
//!     #             } else {
//!     #                 break;
//!     #             }
//!     #         }
//!     #     };
//!     #     pin_mut!(producer, consumer);
//!     #     let remaining = select(producer, consumer).await.factor_first().1;
//!     #     match remaining {
//!     #         Either::Left(f) => f.await,
//!     #         Either::Right(f) => f.await,
//!     #     }
//!     # }
//!     channel
//! }
//!
//! let channel = channel();
//! let t = std::thread::spawn(move || {
//!     let channel = block_on(async move {
//!        test_channel(channel).await
//!     });
//!     block_on(async move {
//!         test_channel(channel).await
//!     });
//! });
//! t.join().expect("test to pass");
//! ```
#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use generic_array::ArrayLength;
use queue::Queue;

mod queue;

/// Create a bounded channel for communicating within a task.
pub fn channel<T, N: ArrayLength<Option<T>>>() -> Channel<T, N> {
    Channel {
        queue: Queue::new(),
        close_count: 0,
        waiter: None,
    }
}

/// A same-producer, same-consumer channel.
pub struct Channel<T, N: ArrayLength<Option<T>>> {
    // The underlying queue.
    queue: Queue<T, N>,
    // A count of the number of endpoints which have been dropped.
    close_count: u8,
    // The waker for whichever endpoint is waiting on the other.
    waiter: Option<Waker>,
}

impl<T, N: ArrayLength<Option<T>>> Channel<T, N> {
    /// Split a channel into a pair of sender and receiver endpoints.
    ///
    /// This is safe for reasons analogous to why `split_at_mut` works: each
    /// endpoint has exclusive access to disjoin regions within the collection.
    /// Since both endpoints must stay within the same task, they execute at any
    /// moment within one thread, so mutual exclusivity is maintained.
    pub fn split(&mut self) -> (Sender<'_, T, N>, Receiver<'_, T, N>) {
        let channel: *mut _ = self as *mut _;
        let sender = Sender {
            channel: unsafe { &mut *channel },
        };
        let receiver = Receiver {
            channel: unsafe { &mut *channel },
        };
        (sender, receiver)
    }

    // register the drop of an endpoint.
    fn close(&mut self) {
        self.close_count += 1;
        // if both endpoints have closed, reset the count, so that the channel
        // could be used further.
        if self.close_count == 2 {
            self.close_count = 0;
        }
    }

    // determine whether at least one endpoint has dropped.
    fn pair_endpoint_closed(&self) -> bool {
        self.close_count > 0
    }
}

/// The endpoint of a channel for sending values.
pub struct Sender<'a, T, N: ArrayLength<Option<T>>> {
    channel: &'a mut Channel<T, N>,
}

/// The endpoint of a channel for receiving values.
pub struct Receiver<'a, T, N: ArrayLength<Option<T>>> {
    channel: &'a mut Channel<T, N>,
}

impl<'a, T: Unpin, N: ArrayLength<Option<T>>> Sender<'a, T, N> {
    /// Asynchronously send a value through the channel.
    ///
    /// If the channel is already at full capacity, this will wait until the
    /// Receiver consumes a value, and then notify that the channel is ready.
    /// If the receiver endpoint has been dropped, this returns `Err(value)`,
    /// regardless of whether there is enough capacity.
    pub fn send(&mut self, value: T) -> impl Future<Output = Result<(), T>> + '_ {
        Sending {
            channel: self.channel,
            value: Some(value),
        }
    }
}

impl<'a, T: Unpin, N: ArrayLength<Option<T>>> Receiver<'a, T, N> {
    /// Asynchronously receive a value through the channel.
    ///
    /// If the channel is empty, this will wait until the sender produces a
    /// value, and then notify that the channel is ready. If the sender has
    /// been dropped and the channel is empty, however, this returns `Err(())`.
    pub fn receive(&mut self) -> impl Future<Output = Result<T, ()>> + '_ {
        Receiving {
            channel: self.channel,
        }
    }
}

impl<'a, T, N: ArrayLength<Option<T>>> Drop for Sender<'a, T, N> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

impl<'a, T, N: ArrayLength<Option<T>>> Drop for Receiver<'a, T, N> {
    fn drop(&mut self) {
        self.channel.close();
    }
}

struct Sending<'a, T, N: ArrayLength<Option<T>>> {
    channel: &'a mut Channel<T, N>,
    value: Option<T>,
}

impl<'a, T: Unpin, N: ArrayLength<Option<T>>> Future for Sending<'_, T, N> {
    type Output = Result<(), T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let value = self.as_mut().value.take().expect("poll not called again");
        let channel = &mut self.as_mut().channel;
        if channel.pair_endpoint_closed() {
            return Poll::Ready(Err(value));
        }
        match channel.queue.enqueue(value) {
            Ok(()) => {
                if let Some(receiver) = channel.waiter.take() {
                    receiver.wake()
                }
                Poll::Ready(Ok(()))
            }
            Err(value) => {
                channel.waiter = Some(cx.waker().clone());
                self.as_mut().value = Some(value);
                Poll::Pending
            }
        }
    }
}

struct Receiving<'a, T, N: ArrayLength<Option<T>>> {
    channel: &'a mut Channel<T, N>,
}

impl<'a, T: Unpin, N: ArrayLength<Option<T>>> Future for Receiving<'_, T, N> {
    type Output = Result<T, ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let channel = &mut self.as_mut().channel;
        match channel.queue.dequeue() {
            Some(value) => {
                if let Some(sender) = channel.waiter.take() {
                    sender.wake();
                }
                Poll::Ready(Ok(value))
            }
            None => {
                if channel.pair_endpoint_closed() {
                    Poll::Ready(Err(()))
                } else {
                    channel.waiter = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }
}
