use attack_shark_x3::{
    ProfileControlFraming, ProfileControlReport, ProfileId, ProfileMetadata, ProfileMetadataReport,
    ProtocolError, ReadSelector, ReadbackRequest, ReadinessStatus,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCatalog {
    schema_version: u8,
    profile_controls: Vec<Fixture<ProfileControlState>>,
    metadata_readbacks: Vec<Fixture<ProfileState>>,
    read_selectors: Vec<Fixture<ReadSelectorState>>,
    readiness_statuses: Vec<Fixture<ReadinessState>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture<T> {
    name: String,
    evidence: String,
    source: String,
    packet_hex: String,
    decoded: T,
}

#[derive(Debug, Deserialize)]
struct ProfileState {
    current: u8,
    maximum: u8,
}

#[derive(Debug, Deserialize)]
struct ProfileControlState {
    current: u8,
    maximum: u8,
    framing: FixtureFraming,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum FixtureFraming {
    Compact,
    Full,
}

impl From<FixtureFraming> for ProfileControlFraming {
    fn from(value: FixtureFraming) -> Self {
        match value {
            FixtureFraming::Compact => Self::Compact,
            FixtureFraming::Full => Self::Full,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadSelectorState {
    report: FixtureReadReport,
    profile: Option<u8>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum FixtureReadReport {
    Version,
    ProfileMetadata,
    Dpi,
    Preferences,
    Buttons,
}

#[derive(Debug, Deserialize)]
struct ReadinessState {
    status: FixtureReadiness,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum FixtureReadiness {
    NotReady,
    Ready,
}

fn fixtures() -> FixtureCatalog {
    serde_json::from_str(include_str!("../../../fixtures/protocol/profile.json"))
        .expect("the checked-in profile fixture catalog must be valid JSON")
}

fn profile(value: u8) -> ProfileId {
    ProfileId::try_from(value).expect("fixture profiles must be in range")
}

fn metadata(current: u8, maximum: u8) -> ProfileMetadata {
    ProfileMetadata::new(profile(current), profile(maximum))
        .expect("fixture metadata must satisfy current <= maximum")
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "fixture hex must have complete bytes");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = decode_nibble(pair[0]);
            let low = decode_nibble(pair[1]);
            (high << 4) | low
        })
        .collect()
}

fn decode_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => panic!("fixture hex must be lowercase ASCII"),
    }
}

fn request(decoded: &ReadSelectorState) -> ReadbackRequest {
    match decoded.report {
        FixtureReadReport::Version => ReadbackRequest::Version,
        FixtureReadReport::ProfileMetadata => ReadbackRequest::ProfileMetadata,
        FixtureReadReport::Dpi => ReadbackRequest::Dpi(profile(
            decoded.profile.expect("DPI selectors require a profile"),
        )),
        FixtureReadReport::Preferences => ReadbackRequest::Preferences(profile(
            decoded
                .profile
                .expect("preferences selectors require a profile"),
        )),
        FixtureReadReport::Buttons => ReadbackRequest::Buttons(profile(
            decoded.profile.expect("button selectors require a profile"),
        )),
    }
}

#[test]
fn fixture_catalog_has_explicit_provenance() {
    let catalog = fixtures();
    assert_eq!(catalog.schema_version, 1);
    let entries = catalog
        .profile_controls
        .iter()
        .map(|fixture| (&fixture.name, &fixture.evidence, &fixture.source))
        .chain(
            catalog
                .metadata_readbacks
                .iter()
                .map(|fixture| (&fixture.name, &fixture.evidence, &fixture.source)),
        )
        .chain(
            catalog
                .read_selectors
                .iter()
                .map(|fixture| (&fixture.name, &fixture.evidence, &fixture.source)),
        )
        .chain(
            catalog
                .readiness_statuses
                .iter()
                .map(|fixture| (&fixture.name, &fixture.evidence, &fixture.source)),
        );
    for (name, evidence, source) in entries {
        assert!(!name.is_empty());
        assert!(
            matches!(
                evidence.as_str(),
                "live-confirmed" | "capture-confirmed" | "static-analysis" | "inference"
            ),
            "fixture {name} has an unsupported evidence label"
        );
        assert!(!source.is_empty(), "fixture {name} must cite a source");
    }
}

#[test]
fn profile_controls_match_fixtures() {
    for fixture in fixtures().profile_controls {
        let expected = decode_hex(&fixture.packet_hex);
        let state = metadata(fixture.decoded.current, fixture.decoded.maximum);
        let encoded = ProfileControlReport::encode(state, fixture.decoded.framing.into());
        assert_eq!(encoded.as_bytes(), expected, "fixture {}", fixture.name);
    }
}

#[test]
fn full_profile_control_framing_adds_only_zero_padding() {
    let report = ProfileControlReport::encode(metadata(2, 5), ProfileControlFraming::Full);
    assert_eq!(report.as_bytes(), decode_hex("0c0a02fd05fa00000000"));
}

#[test]
fn metadata_readbacks_decode_without_conflating_working_state() {
    for fixture in fixtures().metadata_readbacks {
        let packet = decode_hex(&fixture.packet_hex);
        let decoded = ProfileMetadataReport::decode(&packet)
            .unwrap_or_else(|error| panic!("fixture {} failed: {error}", fixture.name));
        assert_eq!(
            decoded.metadata,
            metadata(fixture.decoded.current, fixture.decoded.maximum),
            "fixture {}",
            fixture.name
        );
        assert_eq!(
            decoded.as_bytes(),
            packet.as_slice(),
            "fixture {}",
            fixture.name
        );
    }
}

#[test]
fn read_selectors_match_profile_aware_fixtures() {
    for fixture in fixtures().read_selectors {
        let request = request(&fixture.decoded);
        let expected_profile = fixture.decoded.profile.map(profile);
        assert_eq!(request.target_profile(), expected_profile);
        assert_eq!(
            ReadSelector::encode(request).as_bytes(),
            decode_hex(&fixture.packet_hex).as_slice(),
            "fixture {}",
            fixture.name
        );
    }
}

#[test]
fn every_supported_profile_backed_selector_carries_its_target() {
    let target = profile(5);
    let cases = [
        (ReadbackRequest::Dpi(target), "a004380005000000"),
        (ReadbackRequest::Preferences(target), "a0050f0005000000"),
        (ReadbackRequest::Buttons(target), "a0083b0005000000"),
    ];
    for (request, expected) in cases {
        assert_eq!(request.target_profile(), Some(target));
        assert_eq!(
            ReadSelector::encode(request).as_bytes(),
            decode_hex(expected).as_slice()
        );
    }
}

#[test]
fn readiness_statuses_match_live_fixtures() {
    for fixture in fixtures().readiness_statuses {
        let expected = match fixture.decoded.status {
            FixtureReadiness::NotReady => ReadinessStatus::NotReady,
            FixtureReadiness::Ready => ReadinessStatus::Ready,
        };
        assert_eq!(
            ReadinessStatus::decode(&decode_hex(&fixture.packet_hex)),
            Ok(expected),
            "fixture {}",
            fixture.name
        );
    }
}

#[test]
fn profile_metadata_rejects_invalid_relationships() {
    assert_eq!(
        ProfileMetadata::new(profile(2), profile(1)),
        Err(ProtocolError::InvalidProfileRange {
            current: 2,
            maximum: 1,
        })
    );

    let packet = decode_hex("0c0a0102fd01fe000000");
    assert_eq!(
        ProfileMetadataReport::decode(&packet),
        Err(ProtocolError::InvalidProfileRange {
            current: 2,
            maximum: 1,
        })
    );
}

#[test]
fn profile_metadata_rejects_malformed_packets() {
    let valid = decode_hex("0c0a0102fd05fa000000");

    assert!(matches!(
        ProfileMetadataReport::decode(&valid[..9]),
        Err(ProtocolError::InvalidReportLength { .. })
    ));

    let mut wrong_id = valid.clone();
    wrong_id[0] = 0x04;
    assert!(matches!(
        ProfileMetadataReport::decode(&wrong_id),
        Err(ProtocolError::UnexpectedReportId { .. })
    ));

    let mut wrong_length = valid.clone();
    wrong_length[1] = 0x09;
    assert!(matches!(
        ProfileMetadataReport::decode(&wrong_length),
        Err(ProtocolError::UnexpectedDeclaredLength { .. })
    ));

    for offset in [2, 7, 8, 9] {
        let mut wrong_fixed = valid.clone();
        wrong_fixed[offset] ^= 1;
        assert!(matches!(
            ProfileMetadataReport::decode(&wrong_fixed),
            Err(ProtocolError::InvalidFixedByte { .. })
        ));
    }

    for offset in [4, 6] {
        let mut wrong_complement = valid.clone();
        wrong_complement[offset] ^= 1;
        assert!(matches!(
            ProfileMetadataReport::decode(&wrong_complement),
            Err(ProtocolError::InvalidComplement { .. })
        ));
    }
}

#[test]
fn readiness_rejects_unknown_or_malformed_status() {
    assert_eq!(
        ReadinessStatus::decode(&decode_hex("a002000000000000")),
        Err(ProtocolError::InvalidReadinessStatus { value: 2 })
    );

    let mut malformed = decode_hex("a001000000000000");
    malformed[4] = 1;
    assert!(matches!(
        ReadinessStatus::decode(&malformed),
        Err(ProtocolError::InvalidFixedByte { offset: 4, .. })
    ));
}
