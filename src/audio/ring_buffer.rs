use std::cell::Cell;
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn ring_buffer(capacity: usize) -> (Producer, Consumer) {
    assert!(capacity > 0, "ring buffer capacity must be non-zero");
    let slots = (0..capacity)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let shared = Arc::new(Shared {
        slots,
        read: AtomicUsize::new(0),
        write: AtomicUsize::new(0),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
            _not_sync: PhantomData,
        },
        Consumer {
            shared,
            _not_sync: PhantomData,
        },
    )
}

struct Shared {
    slots: Box<[UnsafeCell<MaybeUninit<f32>>]>,
    read: AtomicUsize,
    write: AtomicUsize,
}

// Safety: `Producer` is the only writer of available slots. `Consumer` is the
// only reader and clears its current consumed slot before publishing the
// updated read index. A release store publishes an initialized slot before the
// consumer observes it; the consumer's release store publishes completion
// before a slot is reused.
unsafe impl Sync for Shared {}

pub(super) struct Producer {
    shared: Arc<Shared>,
    // CPAL may move the callback between threads, but it must never invoke two
    // producers concurrently. `Cell` keeps this handle `Send` and `!Sync`.
    _not_sync: PhantomData<Cell<()>>,
}

impl Producer {
    pub(super) fn push(&mut self, sample: f32) -> Result<(), f32> {
        let write = self.shared.write.load(Ordering::Relaxed);
        let read = self.shared.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= self.shared.slots.len() {
            return Err(sample);
        }

        let index = write % self.shared.slots.len();
        // Safety: only this producer writes this slot, and the acquire load of
        // `read` established that the consumer has finished with it.
        unsafe { (*self.shared.slots[index].get()).write(sample) };
        self.shared
            .write
            .store(write.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

pub(super) struct Consumer {
    shared: Arc<Shared>,
    _not_sync: PhantomData<Cell<()>>,
}

impl Consumer {
    pub(super) fn pop(&mut self) -> Option<f32> {
        let read = self.shared.read.load(Ordering::Relaxed);
        let write = self.shared.write.load(Ordering::Acquire);
        if read == write {
            return None;
        }

        let index = read % self.shared.slots.len();
        // Safety: the acquire load of `write` observes the producer's
        // initialization. Only this consumer reads and clears the slot, and it
        // publishes the new read index only after both operations complete.
        let sample = unsafe {
            let slot = &mut *self.shared.slots[index].get();
            let sample = slot.assume_init_read();
            slot.write(0.0);
            sample
        };
        self.shared
            .read
            .store(read.wrapping_add(1), Ordering::Release);
        Some(sample)
    }

    pub(super) fn producer_for_restart(&mut self) -> Option<Producer> {
        (Arc::strong_count(&self.shared) == 1).then(|| Producer {
            shared: Arc::clone(&self.shared),
            _not_sync: PhantomData,
        })
    }

    pub(super) fn clear(&mut self) {
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn fifo_wrap_and_overflow_are_bounded() {
        let (mut producer, mut consumer) = ring_buffer(3);
        producer.push(1.0).unwrap();
        producer.push(2.0).unwrap();
        producer.push(3.0).unwrap();
        assert_eq!(producer.push(4.0), Err(4.0));
        assert_eq!(consumer.pop(), Some(1.0));
        assert_eq!(consumer.pop(), Some(2.0));

        producer.push(4.0).unwrap();
        producer.push(5.0).unwrap();
        assert_eq!(consumer.pop(), Some(3.0));
        assert_eq!(consumer.pop(), Some(4.0));
        assert_eq!(consumer.pop(), Some(5.0));
        assert_eq!(consumer.pop(), None);
    }

    #[test]
    fn concurrent_producer_and_consumer_preserve_every_sample() {
        const SAMPLE_COUNT: usize = 100_000;
        let (producer, mut consumer) = ring_buffer(127);
        let writer = thread::spawn(move || {
            let mut producer = producer;
            for value in 0..SAMPLE_COUNT {
                let sample = value as f32;
                while producer.push(sample).is_err() {
                    thread::yield_now();
                }
            }
        });

        for expected in 0..SAMPLE_COUNT {
            let actual = loop {
                if let Some(sample) = consumer.pop() {
                    break sample;
                }
                thread::yield_now();
            };
            assert_eq!(actual, expected as f32);
        }
        writer.join().unwrap();
        assert_eq!(consumer.pop(), None);
    }

    #[test]
    fn consumed_samples_are_cleared_before_the_slot_is_released() {
        let (mut producer, mut consumer) = ring_buffer(2);
        producer.push(0.625).unwrap();

        assert_eq!(consumer.pop(), Some(0.625));
        // Safety: the sample was consumed, the read index has advanced, and
        // this test performs no concurrent producer work while inspecting it.
        let cleared = unsafe { (*consumer.shared.slots[0].get()).assume_init() };
        assert_eq!(cleared, 0.0);
    }

    #[test]
    fn restart_producer_requires_the_previous_producer_to_be_gone() {
        let (producer, mut consumer) = ring_buffer(2);
        assert!(consumer.producer_for_restart().is_none());
        drop(producer);

        let mut restarted = consumer.producer_for_restart().unwrap();
        restarted.push(7.0).unwrap();
        assert_eq!(consumer.pop(), Some(7.0));
    }
}
