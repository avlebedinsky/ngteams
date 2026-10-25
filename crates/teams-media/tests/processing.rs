//! The microphone's audio processing on the device seam's 10 ms frames: what the speaker plays comes back into the
//! microphone after a delay and an attenuation, and is handed to the processing as the far end on the same frames;
//! the echo is suppressed by a set margin, a near-end voice is kept while both talk, and noise under a voice is
//! lowered. The signals are `teams_media::test_signal`'s (a steady tone is noise to the noise suppression).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use teams_media::devices::AudioFrame;
use teams_media::processing::{AudioProcessing, ProcessingConfig};
use teams_media::test_signal::{Noise, gap_level, rms, speech_like, voice, voice_level};
use teams_media::tone::decibels;

/// How far below what the microphone hears the echo must be once the canceller has converged, in dB.
const ECHO_MARGIN_DB: f32 = 25.0;
/// How far a near-end voice may fall while the far end talks too, in dB.
const KEPT_DB: f32 = 6.0;
/// How far steady noise must be lowered, in dB.
const NOISE_MARGIN_DB: f32 = 6.0;
/// How far the voice under that noise may fall, in dB.
const VOICE_UNDER_NOISE_DB: f32 = 3.0;

/// Frames the canceller is given to converge before it is judged (3 s).
const CONVERGE: usize = 300;
/// Frames skipped at each change of the voice: the processing delays its output by up to about 15 ms.
const SKIP: usize = 2;
/// How much louder the far end is played than the microphone hears it back.
const ATTENUATION_DB: f32 = 6.0;
const FAR_PEAK: f32 = 0.5;
const VOICE: f32 = 0.2;

/// The sink looped back into the source: each far-end frame is handed to the processing as it is played, and the
/// microphone hears it `delay` frames later, [`ATTENUATION_DB`] down, over `near`'s frame. Returns what the
/// microphone heard and what the processing made of it.
fn looped(
    processing: &AudioProcessing,
    far: &[AudioFrame],
    near: &[AudioFrame],
    delay: usize,
) -> (Vec<AudioFrame>, Vec<AudioFrame>) {
    assert_eq!(far.len(), near.len());
    let gain = 10.0_f32.powf(-ATTENUATION_DB / 20.0);
    let mut heard = Vec::with_capacity(far.len());
    let mut processed = Vec::with_capacity(far.len());
    for (index, played) in far.iter().enumerate() {
        processing.far_end(played).unwrap();
        let mut microphone = near[index].clone();
        if let Some(echoed) = index.checked_sub(delay).map(|at| &far[at]) {
            for (sample, echo) in microphone.samples_mut().iter_mut().zip(echoed.samples()) {
                *sample += gain * echo;
            }
        }
        heard.push(microphone.clone());
        processing.near_end(&mut microphone).unwrap();
        processed.push(microphone);
    }
    (heard, processed)
}

fn silence(frames: usize) -> Vec<AudioFrame> {
    vec![AudioFrame::silence(); frames]
}

#[test]
fn the_echo_of_what_the_speaker_plays_is_suppressed_by_the_margin() {
    // 100 ms, and the UI sandbox's loopback's 250 ms
    for delay in [10, 25] {
        let processing = AudioProcessing::new(ProcessingConfig::CALL).unwrap();
        let far = speech_like(1000, 7, FAR_PEAK);
        let (heard, processed) = looped(&processing, &far, &silence(1000), delay);
        let (echo, left) = (rms(&heard[CONVERGE..]), rms(&processed[CONVERGE..]));
        let suppressed = -decibels(left, echo);
        eprintln!("{delay}0 ms: the echo at {echo:.4} comes out at {left:.6}, {suppressed:.1} dB down");
        assert!(echo > 0.02, "the microphone hears the echo: {echo}");
        assert!(
            suppressed >= ECHO_MARGIN_DB,
            "{delay}0 ms: the echo is only {suppressed:.1} dB down"
        );
    }
}

#[test]
fn with_the_processing_off_the_microphone_s_frames_pass_unchanged() {
    let processing = AudioProcessing::new(ProcessingConfig::OFF).unwrap();
    assert!(!processing.config().any());
    let far = speech_like(300, 7, FAR_PEAK);
    let (heard, processed) = looped(&processing, &far, &voice(300, VOICE), 10);
    assert_eq!(heard, processed);
}

#[test]
fn a_near_end_voice_is_kept_while_both_talk() {
    let processing = AudioProcessing::new(ProcessingConfig::CALL).unwrap();
    let far = speech_like(2000, 11, FAR_PEAK);
    // the far end alone for 10 s, then both
    let mut near = silence(1000);
    near.extend(voice(1000, VOICE));
    let (heard, processed) = looped(&processing, &far, &near, 10);
    let (heard, processed) = (&heard[1000..], &processed[1000..]);
    // judged from 1 s into the double talk, once the gain control has settled on the voice
    let (spoken, kept) = (
        voice_level(heard, 100, 1000, SKIP),
        voice_level(processed, 100, 1000, SKIP),
    );
    let change = decibels(kept, spoken);
    eprintln!("the voice at {spoken:.4} comes out at {kept:.4} ({change:.1} dB) while the far end talks");
    assert!(change >= -KEPT_DB, "the voice falls {:.1} dB while both talk", -change);
    // and between the voice's segments, the echo is still taken out
    let (echo, left) = (gap_level(heard, 0, 1000, SKIP), gap_level(processed, 0, 1000, SKIP));
    let suppressed = -decibels(left, echo);
    eprintln!("between the voice's segments the echo at {echo:.4} comes out at {left:.6}, {suppressed:.1} dB down");
    assert!(
        suppressed >= ECHO_MARGIN_DB,
        "the echo between the voice's segments is only {suppressed:.1} dB down"
    );
}

#[test]
fn noise_under_a_voice_is_lowered_and_the_voice_kept() {
    let processing = AudioProcessing::new(ProcessingConfig::CALL).unwrap();
    // noise alone for 3 s, then the voice over it
    let mut near = silence(CONVERGE);
    near.extend(voice(1200, VOICE));
    Noise::new(3).add_to(&mut near, 0.05);
    let (heard, processed) = looped(&processing, &silence(near.len()), &near, 0);
    let voiced = |frames: &[AudioFrame]| voice_level(&frames[CONVERGE..], 200, 1200, SKIP);
    let (spoken, kept) = (voiced(&heard), voiced(&processed));
    let gaps = |frames: &[AudioFrame]| gap_level(&frames[CONVERGE..], 200, 1200, SKIP);
    let (noise, left) = (gaps(&heard), gaps(&processed));
    let lowered = -decibels(left, noise);
    let change = decibels(kept, spoken);
    eprintln!(
        "noise at {noise:.4} comes out at {left:.4} ({lowered:.1} dB down); the voice at {spoken:.4} at {kept:.4} \
         ({change:.1} dB)"
    );
    assert!(lowered >= NOISE_MARGIN_DB, "the noise is only {lowered:.1} dB down");
    assert!(change >= -VOICE_UNDER_NOISE_DB, "the voice falls {:.1} dB", -change);
    // before the voice, the noise alone is lowered too
    let (alone, left_alone) = (rms(&heard[100..CONVERGE]), rms(&processed[100..CONVERGE]));
    let lowered_alone = -decibels(left_alone, alone);
    assert!(
        lowered_alone >= NOISE_MARGIN_DB,
        "noise alone is only {lowered_alone:.1} dB down"
    );
}
