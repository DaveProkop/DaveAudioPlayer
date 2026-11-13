// C interface to Signalsmith Stretch for interleaved float audio.
#include <signalsmith-stretch.h>

#include <cstddef>

namespace {

// Lets the library index interleaved buffers as [channel][frame].
template <typename T>
struct Interleaved {
    T *data;
    int channels;
    struct Channel {
        T *data;
        int channel;
        int stride;
        T &operator[](size_t frame) const { return data[frame * stride + channel]; }
    };
    Channel operator[](size_t channel) const {
        return Channel{data, static_cast<int>(channel), channels};
    }
};

}  // namespace

struct dap_stretch {
    signalsmith::stretch::SignalsmithStretch<float> stretch;
    int channels;
};

extern "C" {

dap_stretch *dap_stretch_new(int channels, float sample_rate) {
    auto *s = new dap_stretch;
    s->channels = channels;
    s->stretch.presetDefault(channels, sample_rate);
    return s;
}

void dap_stretch_free(dap_stretch *s) { delete s; }

void dap_stretch_reset(dap_stretch *s) { s->stretch.reset(); }

int dap_stretch_input_latency(const dap_stretch *s) { return s->stretch.inputLatency(); }

int dap_stretch_output_latency(const dap_stretch *s) { return s->stretch.outputLatency(); }

void dap_stretch_seek(dap_stretch *s, const float *input, int in_frames, double rate) {
    Interleaved<const float> in{input, s->channels};
    s->stretch.seek(in, in_frames, rate);
}

void dap_stretch_process(dap_stretch *s, const float *input, int in_frames, float *output,
                         int out_frames) {
    Interleaved<const float> in{input, s->channels};
    Interleaved<float> out{output, s->channels};
    s->stretch.process(in, in_frames, out, out_frames);
}

void dap_stretch_flush(dap_stretch *s, float *output, int out_frames) {
    Interleaved<float> out{output, s->channels};
    s->stretch.flush(out, out_frames);
}

}  // extern "C"
