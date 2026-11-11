//! Lock-free single-producer/single-consumer PCM ring buffer (decoder thread → audio thread).
//!
//! Both ends move whole frames only, so channels never get out of step.

use rtrb::RingBuffer;

/// Creates a ring holding `capacity_frames` frames of `channels` interleaved samples.
pub fn pcm_ring(capacity_frames: usize, channels: usize) -> (PcmProducer, PcmConsumer) {
    assert!(channels > 0 && capacity_frames > 0);
    let (producer, consumer) = RingBuffer::new(capacity_frames * channels);
    (
        PcmProducer {
            inner: producer,
            channels,
        },
        PcmConsumer {
            inner: consumer,
            channels,
        },
    )
}

pub struct PcmProducer {
    inner: rtrb::Producer<f32>,
    channels: usize,
}

impl PcmProducer {
    /// Free space in frames.
    pub fn free_frames(&self) -> usize {
        self.inner.slots() / self.channels
    }

    /// Writes as many whole frames from `samples` as fit; returns the number of samples written.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let frames = (samples.len() / self.channels).min(self.free_frames());
        let n = frames * self.channels;
        if n == 0 {
            return 0;
        }
        match self.inner.write_chunk_uninit(n) {
            Ok(chunk) => chunk.fill_from_iter(samples[..n].iter().copied()),
            Err(_) => 0,
        }
    }

    /// True once the consumer has been dropped (the audio stream is gone).
    pub fn is_abandoned(&self) -> bool {
        self.inner.is_abandoned()
    }
}

pub struct PcmConsumer {
    inner: rtrb::Consumer<f32>,
    channels: usize,
}

impl PcmConsumer {
    /// Buffered audio in frames.
    pub fn available_frames(&self) -> usize {
        self.inner.slots() / self.channels
    }

    /// Fills `out` with as many whole frames as available; returns the number of samples
    /// written. Real-time safe.
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let frames = (out.len() / self.channels).min(self.available_frames());
        let n = frames * self.channels;
        if n == 0 {
            return 0;
        }
        let Ok(chunk) = self.inner.read_chunk(n) else {
            return 0;
        };
        let (first, second) = chunk.as_slices();
        out[..first.len()].copy_from_slice(first);
        out[first.len()..n].copy_from_slice(second);
        chunk.commit_all();
        n
    }

    /// Drops all buffered audio (after a seek or stop).
    pub fn clear(&mut self) {
        let n = self.inner.slots();
        if let Ok(chunk) = self.inner.read_chunk(n) {
            chunk.commit_all();
        }
    }

    /// True once the producer has been dropped (the decoder is finished or gone).
    pub fn is_abandoned(&self) -> bool {
        self.inner.is_abandoned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_order_across_wraparound() {
        let (mut tx, mut rx) = pcm_ring(4, 2);
        let mut out = [0.0f32; 8];
        let mut next = 0.0f32;
        let mut expected = 0.0f32;
        for _ in 0..10 {
            let data = [
                next,
                next + 1.0,
                next + 2.0,
                next + 3.0,
                next + 4.0,
                next + 5.0,
            ];
            assert_eq!(tx.push(&data), 6);
            next += 6.0;
            let n = rx.pop_into(&mut out);
            assert_eq!(n, 6);
            for &v in &out[..n] {
                assert_eq!(v, expected);
                expected += 1.0;
            }
        }
    }

    #[test]
    fn only_whole_frames_move() {
        let (mut tx, mut rx) = pcm_ring(3, 2);
        assert_eq!(tx.push(&[1.0, 2.0, 3.0]), 2, "odd sample is not written");
        assert_eq!(tx.push(&[0.0; 10]), 4, "only the remaining 2 frames fit");
        assert_eq!(tx.free_frames(), 0);
        let mut out = [0.0f32; 3];
        assert_eq!(
            rx.pop_into(&mut out),
            2,
            "output with room for 1.5 frames takes 1 frame"
        );
        assert_eq!(rx.available_frames(), 2);
    }

    #[test]
    fn clear_and_abandon() {
        let (mut tx, mut rx) = pcm_ring(8, 1);
        tx.push(&[1.0; 5]);
        rx.clear();
        assert_eq!(rx.available_frames(), 0);
        drop(tx);
        assert!(rx.is_abandoned());
    }

    #[test]
    fn works_across_threads() {
        let (mut tx, mut rx) = pcm_ring(64, 2);
        let total = 100_000usize;
        let writer = std::thread::spawn(move || {
            let mut i = 0usize;
            while i < total {
                let data: Vec<f32> = (i..(i + 32).min(total)).map(|v| v as f32).collect();
                let mut off = 0;
                while off < data.len() {
                    off += tx.push(&data[off..]);
                    std::thread::yield_now();
                }
                i += data.len();
            }
        });
        let mut got = 0usize;
        let mut buf = [0.0f32; 48];
        while got < total {
            let n = rx.pop_into(&mut buf);
            for &v in &buf[..n] {
                assert_eq!(v, got as f32);
                got += 1;
            }
            if n == 0 {
                std::thread::yield_now();
            }
        }
        writer.join().unwrap();
    }
}
