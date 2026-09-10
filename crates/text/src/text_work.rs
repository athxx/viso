//! The worker scheduling seam: describe shaping / layout / rasterization work
//! as jobs so all heavy text work runs off the main thread with zero main-thread
//! work in the steady state.
//!
//! The main thread submits typed requests and reads results through queues; it
//! never shapes, breaks, or rasterizes inline. Prediction prewarm lets the
//! worker begin likely-next work (the next input glyphs) before it is demanded.
//! This crate owns the job description; the facade owns the thread pool.

/// A unit of text work handed to a worker.
#[derive(Debug)]
pub enum TextJob {
    /// Shape and lay out a paragraph.
    Paragraph,
    /// Rasterize a glyph into its resolved representation.
    Rasterize,
    // TODO(TF-P0): prediction-prewarm variants.
}

/// The job description seam. The facade drives the actual worker pool.
#[derive(Debug, Default)]
pub struct TextWork {
    // TODO(TF-P0): pending-job description queue + result routing keys.
}

impl TextWork {
    /// Submit a job for a worker to run off the main thread.
    pub fn submit(&mut self, _job: TextJob) {
        todo!("TF-P0: enqueue worker job, zero main-thread work")
    }
}
