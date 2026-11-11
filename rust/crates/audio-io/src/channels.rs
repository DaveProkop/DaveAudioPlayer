//! Channel count conversion between a decoded stream and the output device.

/// Converts interleaved `input` with `in_ch` channels to `out_ch` channels, appending to `out`.
///
/// - mono → N: copied to every output channel
/// - N → mono: average of all channels
/// - otherwise: channels are mapped by index; missing ones are silent, extra ones dropped
pub fn remap_channels(input: &[f32], in_ch: usize, out_ch: usize, out: &mut Vec<f32>) {
    assert!(in_ch > 0 && out_ch > 0);
    if in_ch == out_ch {
        out.extend_from_slice(input);
        return;
    }
    let frames = input.len() / in_ch;
    out.reserve(frames * out_ch);
    for frame in input.chunks_exact(in_ch) {
        if in_ch == 1 {
            out.extend(std::iter::repeat_n(frame[0], out_ch));
        } else if out_ch == 1 {
            out.push(frame.iter().sum::<f32>() / in_ch as f32);
        } else {
            out.extend((0..out_ch).map(|c| frame.get(c).copied().unwrap_or(0.0)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remap(input: &[f32], i: usize, o: usize) -> Vec<f32> {
        let mut out = Vec::new();
        remap_channels(input, i, o, &mut out);
        out
    }

    #[test]
    fn conversions() {
        assert_eq!(remap(&[1.0, 2.0], 1, 2), [1.0, 1.0, 2.0, 2.0]);
        assert_eq!(remap(&[1.0, 3.0, 2.0, 4.0], 2, 1), [2.0, 3.0]);
        assert_eq!(remap(&[1.0, 2.0], 2, 4), [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(remap(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, 2), [1.0, 2.0]);
        assert_eq!(remap(&[1.0, 2.0], 2, 2), [1.0, 2.0]);
    }
}
