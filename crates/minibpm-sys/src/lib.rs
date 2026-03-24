use std::ffi::c_int;

#[repr(C)]
pub struct MiniBpmHandle {
    _private: [u8; 0],
}

extern "C" {
    pub fn minibpm_new(sample_rate: f32) -> *mut MiniBpmHandle;
    pub fn minibpm_free(h: *mut MiniBpmHandle);
    pub fn minibpm_set_bpm_range(h: *mut MiniBpmHandle, min: f64, max: f64);
    pub fn minibpm_estimate_tempo_of_samples(
        h:       *mut MiniBpmHandle,
        samples: *const f32,
        nsamples: c_int,
    ) -> f64;
}

/// Safe wrapper around MiniBPM.
pub struct MiniBpm {
    ptr: *mut MiniBpmHandle,
}

// MiniBPM is a single-threaded estimator; we only ever touch it from one thread.
unsafe impl Send for MiniBpm {}

impl MiniBpm {
    pub fn new(sample_rate: f32) -> Self {
        let ptr = unsafe { minibpm_new(sample_rate) };
        assert!(!ptr.is_null(), "minibpm_new returned null");
        Self { ptr }
    }

    pub fn set_bpm_range(&mut self, min: f64, max: f64) {
        unsafe { minibpm_set_bpm_range(self.ptr, min, max) }
    }

    /// Feed all mono samples at once and return the estimated BPM.
    /// Returns `None` if detection failed (returned 0).
    pub fn estimate_tempo(&mut self, samples: &[f32]) -> Option<f64> {
        let bpm = unsafe {
            minibpm_estimate_tempo_of_samples(
                self.ptr,
                samples.as_ptr(),
                samples.len() as c_int,
            )
        };
        if bpm > 0.0 { Some(bpm) } else { None }
    }
}

impl Drop for MiniBpm {
    fn drop(&mut self) {
        unsafe { minibpm_free(self.ptr) }
    }
}
