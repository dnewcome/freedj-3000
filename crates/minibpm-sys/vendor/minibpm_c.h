/* C wrapper around MiniBPM for FFI use from Rust. */
#pragma once
#ifdef __cplusplus
extern "C" {
#endif

typedef struct MiniBpmHandle MiniBpmHandle;

MiniBpmHandle* minibpm_new(float sample_rate);
void           minibpm_free(MiniBpmHandle* h);
void           minibpm_set_bpm_range(MiniBpmHandle* h, double min, double max);

/* Batch: feed all mono samples at once, returns BPM (0 if detection failed). */
double         minibpm_estimate_tempo_of_samples(MiniBpmHandle* h,
                                                  const float*   samples,
                                                  int            nsamples);

#ifdef __cplusplus
}
#endif
