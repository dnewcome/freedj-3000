#include "minibpm_c.h"
#include "MiniBpm.h"

using breakfastquay::MiniBPM;

extern "C" {

MiniBpmHandle* minibpm_new(float sample_rate) {
    return reinterpret_cast<MiniBpmHandle*>(new MiniBPM(sample_rate));
}

void minibpm_free(MiniBpmHandle* h) {
    delete reinterpret_cast<MiniBPM*>(h);
}

void minibpm_set_bpm_range(MiniBpmHandle* h, double min, double max) {
    reinterpret_cast<MiniBPM*>(h)->setBPMRange(min, max);
}

double minibpm_estimate_tempo_of_samples(MiniBpmHandle* h,
                                          const float*   samples,
                                          int            nsamples) {
    return reinterpret_cast<MiniBPM*>(h)->estimateTempoOfSamples(samples, nsamples);
}

} // extern "C"
