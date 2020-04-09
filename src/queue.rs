use generic_array::{ArrayLength, GenericArray};

/// A heap-less bounded FIFO queue.
///
/// Implemented as a ring buffer using the `generic-array` crate to allocate the
/// space inline within the structure. This is not as efficient as possible,
/// however, since the array type is `Option<T>` rather than just `T`. There
/// doesn't seem to be a way yet in stable Rust to construct an array with
/// `MaybeUninit::uninit_array`. Rust is pretty good about reducing or even
/// eliminating the extra padding needed for `Option`s, though, and this design
/// has the added benefit of being naturally drop-safe.
pub struct Queue<T, N: ArrayLength<Option<T>>> {
    buf: GenericArray<Option<T>, N>,
    len: usize,
    head: usize,
    tail: usize,
}

impl<T, N: ArrayLength<Option<T>>> Queue<T, N> {
    /// Create a new, empty queue.
    pub fn new() -> Self {
        Queue {
            buf: GenericArray::default(),
            len: 0,
            head: 0,
            tail: 0,
        }
    }

    /// Try to enqueue an item.
    ///
    /// On success, returns `Ok(())`. If the queue is full, it returns back
    /// the item with `Err(T)`.
    pub fn enqueue(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        // invariant: self.tail always in the range [0, capacity).
        let slot = unsafe { self.buf.get_unchecked_mut(self.tail) };
        // invariant: if not full, self.tail indexes an empty slot.
        *slot = Some(value);
        self.len += 1;
        self.tail += 1;
        if self.tail == N::to_usize() {
            self.tail = 0;
        }
        Ok(())
    }

    /// Try to dequeue an item.
    ///
    /// On success, returns `Some(T)`. If the queue is empty, it returns back
    /// the with `None`.
    pub fn dequeue(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        // invariant: self.head always in the range [0, capacity).
        let slot = unsafe { self.buf.get_unchecked_mut(self.head) };
        // invariant: if not empty, self.head indexes a filled slot.
        let value = slot.take().expect("slot at head filled");
        self.len -= 1;
        self.head += 1;
        if self.head == N::to_usize() {
            self.head = 0;
        }
        Some(value)
    }

    #[inline(always)]
    fn is_full(&self) -> bool {
        self.len == N::to_usize()
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::Queue;
    use typenum::U4;

    #[test]
    fn smoke() {
        let mut queue: Queue<i32, U4> = Queue::new();
        queue.enqueue(0).expect("space in queue");
        queue.enqueue(1).expect("space in queue");
        queue.enqueue(2).expect("space in queue");
        queue.enqueue(3).expect("space in queue");
        assert!(queue.enqueue(4).is_err());
        assert_eq!(0, queue.dequeue().expect("item in queue"));
        assert_eq!(1, queue.dequeue().expect("item in queue"));
        assert_eq!(2, queue.dequeue().expect("item in queue"));
        assert_eq!(3, queue.dequeue().expect("item in queue"));
        assert!(queue.dequeue().is_none());
        queue.enqueue(0).expect("space in queue");
        assert_eq!(0, queue.dequeue().expect("item in queue"));
        queue.enqueue(1).expect("space in queue");
        assert_eq!(1, queue.dequeue().expect("item in queue"));
        queue.enqueue(2).expect("space in queue");
        assert_eq!(2, queue.dequeue().expect("item in queue"));
    }
}
