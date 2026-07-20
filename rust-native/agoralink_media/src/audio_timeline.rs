//! Pure audio timeline accounting used by the WASAPI integration.
//!
//! It distinguishes a media timestamp submitted to the output device from an
//! estimate of media that has become audible after the device's current
//! padding. Silence added for prebuffering, post-stream keepalive, or device
//! underflow is intentionally excluded from the media playhead.

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioTimelineSnapshot {
    pub latest_received_timestamp_us: Option<u64>,
    pub submitted_media_timestamp_us: Option<u64>,
    pub audible_media_timestamp_us: Option<u64>,
    pub device_padding_frames: u32,
    pub media_samples_submitted_total: u64,
    pub media_samples_audible_estimated_total: u64,
    pub discontinuities: u64,
    pub master_reanchors: u64,
    pub valid: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioTimeline {
    sample_rate: u32,
    snapshot: AudioTimelineSnapshot,
    awaiting_reanchor: bool,
}

impl AudioTimeline {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            snapshot: AudioTimelineSnapshot::default(),
            awaiting_reanchor: false,
        }
    }

    pub fn observe_received_packet(&mut self, timestamp_us: u64) {
        self.snapshot.latest_received_timestamp_us = Some(timestamp_us);
    }

    /// Records only actual media samples submitted to the output device.
    /// The supplied timestamp is the media endpoint after this submission.
    pub fn submit_media(
        &mut self,
        submitted_media_timestamp_us: u64,
        media_frames: u64,
        device_padding_frames: u32,
    ) -> AudioTimelineSnapshot {
        if self.awaiting_reanchor {
            self.awaiting_reanchor = false;
            self.snapshot.master_reanchors = self.snapshot.master_reanchors.saturating_add(1);
        }
        self.snapshot.valid = true;
        self.snapshot.submitted_media_timestamp_us = Some(submitted_media_timestamp_us);
        self.snapshot.device_padding_frames = device_padding_frames;
        self.snapshot.media_samples_submitted_total = self
            .snapshot
            .media_samples_submitted_total
            .saturating_add(media_frames);
        let padding_us = u64::from(device_padding_frames).saturating_mul(1_000_000)
            / u64::from(self.sample_rate.max(1));
        self.snapshot.audible_media_timestamp_us =
            Some(submitted_media_timestamp_us.saturating_sub(padding_us));
        self.snapshot.media_samples_audible_estimated_total = self
            .snapshot
            .media_samples_submitted_total
            .saturating_sub(u64::from(device_padding_frames));
        self.snapshot
    }

    /// Marks a queue trim, capture gap, timestamp error, or output timeline
    /// discontinuity. The previous master must not be used until a later media
    /// submission anchors the new timeline.
    pub fn mark_discontinuity(&mut self) -> AudioTimelineSnapshot {
        self.snapshot.valid = false;
        self.snapshot.discontinuities = self.snapshot.discontinuities.saturating_add(1);
        self.awaiting_reanchor = true;
        self.snapshot
    }

    pub fn mark_inactive(&mut self) -> AudioTimelineSnapshot {
        self.snapshot.valid = false;
        self.snapshot
    }

    pub fn snapshot(&self) -> AudioTimelineSnapshot {
        self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audible_playhead_accounts_for_device_padding() {
        let mut timeline = AudioTimeline::new(48_000);
        timeline.observe_received_packet(100_000);
        let snapshot = timeline.submit_media(100_000, 4_800, 2_880);
        assert_eq!(snapshot.submitted_media_timestamp_us, Some(100_000));
        assert_eq!(snapshot.audible_media_timestamp_us, Some(40_000));
        assert_eq!(snapshot.media_samples_submitted_total, 4_800);
        assert_eq!(snapshot.media_samples_audible_estimated_total, 1_920);
    }

    #[test]
    fn silence_does_not_advance_media_playhead() {
        let mut timeline = AudioTimeline::new(48_000);
        timeline.submit_media(50_000, 2_400, 0);
        let before = timeline.snapshot();
        let after = timeline.snapshot();
        assert_eq!(
            before.submitted_media_timestamp_us,
            after.submitted_media_timestamp_us
        );
        assert_eq!(
            before.audible_media_timestamp_us,
            after.audible_media_timestamp_us
        );
    }

    #[test]
    fn discontinuity_invalidates_then_reanchors_on_media_submission() {
        let mut timeline = AudioTimeline::new(48_000);
        timeline.submit_media(100_000, 480, 0);
        let invalid = timeline.mark_discontinuity();
        assert!(!invalid.valid);
        assert_eq!(invalid.discontinuities, 1);
        let reanchored = timeline.submit_media(220_000, 480, 0);
        assert!(reanchored.valid);
        assert_eq!(reanchored.master_reanchors, 1);
        assert_eq!(reanchored.audible_media_timestamp_us, Some(220_000));
    }
}
