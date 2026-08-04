use std::cell::UnsafeCell;
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
        },
        Consumer { shared },
    )
}

struct Shared {
    slots: Box<[UnsafeCell<MaybeUninit<f32>>]>,
    read: AtomicUsize,
    write: AtomicUsize,
}

// Safety: `Producer` is the only writer and `Consumer` is the only reader. A
// release store publishes an initialized slot before the consumer observes it;
// the consumer's release store publishes completion before a slot is reused.
unsafe impl Sync for Shared {}

pub(super) struct Producer {
    shared: Arc<Shared>,
}

impl Producer {
    pub(super) fn push(&self, sample: f32) -> Result<(), f32> {
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
}

impl Consumer {
    pub(super) fn pop(&self) -> Option<f32> {
        let read = self.shared.read.load(Ordering::Relaxed);
        let write = self.shared.write.load(Ordering::Acquire);
        if read == write {
            return None;
        }

        let index = read % self.shared.slots.len();
        // Safety: the acquire load of `write` observes the producer's
        // initialization, and only this consumer reads the slot.
        let sample = unsafe { (*self.shared.slots[index].get()).assume_init_read() };
        self.shared
            .read
            .store(read.wrapping_add(1), Ordering::Release);
        Some(sample)
    }

    pub(super) fn producer_for_restart(&self) -> Producer {
        Producer {
            shared: Arc::clone(&self.shared),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn fifo_wrap_and_overflow_are_bounded() {
        let (producer, consumer) = ring_buffer(3);
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
        let (producer, consumer) = ring_buffer(127);
        let writer = thread::spawn(move || {
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
}
