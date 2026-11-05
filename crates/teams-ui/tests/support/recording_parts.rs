// A made-up meeting recording streamed in parts, for the views' tests (`include!`d where a fake engine streams one):
// its presentation, shaped like the media transform service's MPD (an audio and a video set, each a template with a
// timeline, on a made-up `-mediap.svc.ms` host), and its parts, the made-up DASH segments of teams-media's tests
// (`FFmpeg`'s test pattern and a tone, 3 s). Every host, id and value here is made up.

/// The recording's presentation, as the engine would read it.
///
/// # Panics
/// Never: the made-up MPD reads.
#[must_use]
pub fn made_up_presentation() -> std::sync::Arc<teams_proto::Presentation> {
    let mpd = r#"<MPD type="static" mediaPresentationDuration="PT0H0M3.000S" xmlns="urn:mpeg:DASH:schema:MPD:2011">
  <BaseURL>https://madeupregion1-mediap.svc.ms/transform/</BaseURL>
  <Period>
    <AdaptationSet id="0" contentType="audio" mimeType="audio/mp4">
      <SegmentTemplate timescale="16000" initialization="videotranscode?part=initsegment&amp;track=audio&amp;quality=$RepresentationID$" media="videotranscode?part=mediasegment&amp;track=audio&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline><S t="0" d="14336" /><S d="16384" r="1" /><S d="896" /></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="audcopy" bandwidth="24000" />
    </AdaptationSet>
    <AdaptationSet id="1" contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="16384" initialization="videotranscode?part=initsegment&amp;track=video&amp;quality=$RepresentationID$" media="videotranscode?part=mediasegment&amp;track=video&amp;quality=$RepresentationID$&amp;segmentTime=$Time$">
        <SegmentTimeline><S t="0" d="16384" r="2" /></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="vcopy" bandwidth="120000" />
    </AdaptationSet>
  </Period>
</MPD>"#;
    std::sync::Arc::new(teams_proto::recording::read_manifest(mpd.as_bytes()).expect("the made-up presentation reads"))
}

/// The bytes of the part of `presentation` at `link`; `None` for an address that isn't one of its parts.
#[must_use]
pub fn made_up_part(
    presentation: &teams_proto::Presentation,
    link: &teams_proto::MediaSegmentLink,
) -> Option<Vec<u8>> {
    const AUDIO: [&[u8]; 5] = [
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/init-1.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-1-00001.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-1-00002.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-1-00003.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-1-00004.m4s"),
    ];
    const VIDEO: [&[u8]; 4] = [
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/init-0.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-0-00001.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-0-00002.m4s"),
        include_bytes!("../../../teams-media/tests/fixtures/media/dash/chunk-0-00003.m4s"),
    ];
    presentation.tracks.iter().find_map(|track| {
        let parts: &[&[u8]] = match track.kind {
            teams_proto::TrackKind::Audio => &AUDIO,
            teams_proto::TrackKind::Video => &VIDEO,
        };
        if track.init() == link {
            return Some(parts[0].to_vec());
        }
        (0..track.segments.len())
            .find(|index| track.segment(*index).as_ref() == Some(link))
            .and_then(|index| parts.get(index + 1))
            .map(|bytes| bytes.to_vec())
    })
}
