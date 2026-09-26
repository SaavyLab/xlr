//! Synthetic end-to-end tests for the read-only ARC subscription-query
//! codec. Every byte below is constructed here; no capture is embedded.

use xlr_dante::arc::subscription::{ReceiverPageQuery, SubscriptionQuery};
use xlr_dante::error::{ArcCodecError, RecordLayoutError, StringEncodingError, StringOffsetError};
use xlr_dante::model::{ReceiverChannel, SubscriptionState, TransmitterSelector};

const QUERY_MAGIC: u16 = 0x280F;
const QUERY_REPLY_MAGIC: u16 = 0x2801;
const QUERY_COMMAND: u16 = 0x3000;
const SIBLING_REPLY_MAGIC: u16 = 0x2809;
const HEADER: usize = 12;
const RECORD: usize = 20;

fn channel(value: u16) -> ReceiverChannel {
    ReceiverChannel::new(value).expect("test channel in range")
}

/// Binds a query to a sequence and supported channel for concise call sites.
fn query(sequence: u16, value: u16) -> SubscriptionQuery {
    SubscriptionQuery::new(sequence, channel(value))
}

fn raw_word(frame: &[u8], word_index: usize) -> u16 {
    u16::from_be_bytes([frame[word_index * 2], frame[word_index * 2 + 1]])
}

/// One synthetic page entry: names are `None` for an absent field.
struct Entry {
    device: Option<&'static str>,
    source_channel: Option<&'static str>,
}

impl Entry {
    fn unsubscribed() -> Self {
        Self {
            device: None,
            source_channel: None,
        }
    }
    fn subscribed(device: &'static str, source_channel: &'static str) -> Self {
        Self {
            device: Some(device),
            source_channel: Some(source_channel),
        }
    }
}

/// Builds a fully formed synthetic response page.
///
/// All string offsets are absolute. Names are appended to a shared tail pool
/// in entry order, each field NUL-terminated; every record points its
/// record-end offset at the frame end so its strings resolve anywhere in the
/// pool. With `record_end_zero`, the last entry instead carries the observed
/// zero (unbounded) record-end form.
fn synthetic_page(
    sequence: u16,
    total_channels: u8,
    entries: &[Entry],
    start_channel: u16,
    record_end_zero: bool,
) -> Vec<u8> {
    assert!(!entries.is_empty() && entries.len() <= 16);
    let mut frame = Vec::new();
    let count_word = ((total_channels as u16) << 8) | entries.len() as u16;
    frame.extend(u16::to_be_bytes(QUERY_REPLY_MAGIC));
    // Placeholder declared length, patched once the pool is sized.
    frame.extend(0u16.to_be_bytes());
    frame.extend(sequence.to_be_bytes());
    frame.extend(QUERY_COMMAND.to_be_bytes());
    frame.extend(0u16.to_be_bytes());
    frame.extend(count_word.to_be_bytes());

    // Zero-filled record placeholders, patched after the pool exists.
    for index in 0..entries.len() {
        let channel_id = start_channel + index as u16;
        frame.extend(channel_id.to_be_bytes());
        frame.extend([0u8; RECORD - 2]);
    }

    for (index, entry) in entries.iter().enumerate() {
        let base = HEADER + index * RECORD;
        if let Some(device) = entry.device {
            let offset = frame.len();
            frame.extend_from_slice(device.as_bytes());
            frame.push(0);
            frame[base + 8..base + 10].copy_from_slice(&(offset as u16).to_be_bytes());
        }
        if let Some(source_channel) = entry.source_channel {
            let offset = frame.len();
            frame.extend_from_slice(source_channel.as_bytes());
            frame.push(0);
            frame[base + 6..base + 8].copy_from_slice(&(offset as u16).to_be_bytes());
        }
    }

    for index in 0..entries.len() {
        let base = HEADER + index * RECORD;
        let end_offset = if record_end_zero && index == entries.len() - 1 {
            0u16
        } else {
            frame.len() as u16
        };
        frame[base + 18..base + 20].copy_from_slice(&end_offset.to_be_bytes());
    }
    let declared = frame.len() as u16;
    frame[2..4].copy_from_slice(&declared.to_be_bytes());
    frame
}

/// Patches the declared-length word to the current frame size.
fn patch_declared(frame: &mut [u8]) {
    let declared = frame.len() as u16;
    frame[2..4].copy_from_slice(&declared.to_be_bytes());
}

/// Patches one record's record-end offset word.
fn patch_record_end(frame: &mut [u8], base: usize, end: usize) {
    let value = end as u16;
    frame[base + 18..base + 20].copy_from_slice(&value.to_be_bytes());
}

/// Patches the last record's record-end offset to the frame end.
macro_rules! close_record {
    ($frame:expr) => {{
        let end = $frame.len();
        patch_record_end(&mut $frame, HEADER, end);
    }};
}

/// Lowercase hex rendering of a byte slice.
fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// 1. Query encoding binds variant, sequence, and starting channel.

#[test]
fn query_encodes_binds_variant_sequence_and_start_channel() {
    let encoded = SubscriptionQuery::new(0xBEEF, channel(3)).encode();

    assert_eq!(raw_word(&encoded, 0), QUERY_MAGIC);
    assert_eq!(raw_word(&encoded, 1), 16);
    assert_eq!(raw_word(&encoded, 2), 0xBEEF);
    assert_eq!(raw_word(&encoded, 3), QUERY_COMMAND);
    assert_eq!(raw_word(&encoded, 4), 0x0000);
    assert_eq!(raw_word(&encoded, 5), 0x0001);
    assert_eq!(raw_word(&encoded, 6), 1);
    assert_eq!(raw_word(&encoded, 7), 0x0000);
    assert_eq!(hex_string(&encoded), "280f0010beef30000000000100010000");
}

#[test]
fn query_start_channel_follows_page_boundary() {
    assert_eq!(
        raw_word(&SubscriptionQuery::new(1, channel(1)).encode(), 6),
        1
    );
    assert_eq!(
        raw_word(&SubscriptionQuery::new(1, channel(16)).encode(), 6),
        1
    );
    assert_eq!(
        raw_word(&SubscriptionQuery::new(1, channel(17)).encode(), 6),
        17
    );
    assert_eq!(
        raw_word(&SubscriptionQuery::new(1, channel(32)).encode(), 6),
        17
    );
}

// 2./3. Entry normalization.

#[test]
fn subscribed_entry_decodes_device_and_channel_names() {
    let query = SubscriptionQuery::new(0x0102, channel(1));
    let reply = synthetic_page(
        0x0102,
        32,
        &[Entry::subscribed("SYNTH-DEV-A", "OUT-1")],
        1,
        true,
    );

    let page = query.decode_response(&reply).expect("well-formed page");
    assert_eq!(page.subscriptions().len(), 1);

    let entry = &page.subscriptions()[0];
    assert_eq!(entry.receiver_channel(), channel(1));
    match entry.state() {
        SubscriptionState::Subscribed(selector) => {
            assert_eq!(
                selector,
                &TransmitterSelector::new("SYNTH-DEV-A".to_owned(), "OUT-1".to_owned())
                    .expect("valid test names")
            );
            assert_eq!(selector.device_name(), "SYNTH-DEV-A");
            assert_eq!(selector.channel_name(), "OUT-1");
        }
        other => panic!("expected subscribed state, got {other:?}"),
    }
}

#[test]
fn unsubscribed_entry_decodes_without_invented_selector_values() {
    let query = SubscriptionQuery::new(7, channel(1));
    let reply = synthetic_page(7, 64, &[Entry::unsubscribed()], 1, false);

    let page = query.decode_response(&reply).expect("well-formed page");
    let entry = &page.subscriptions()[0];
    assert_eq!(entry.receiver_channel(), channel(1));
    assert_eq!(entry.state(), &SubscriptionState::Unsubscribed);
}

#[test]
fn device_only_selector_is_rejected_as_partial() {
    // Entry names its transmitter device but carries no source-channel
    // offset: neither subscribed nor evidenced unsubscribed.
    let mut reply = synthetic_page(30, 16, &[Entry::unsubscribed()], 1, false);
    reply[HEADER + 8..HEADER + 10] // source-device offset word
        .copy_from_slice(&((HEADER + RECORD) as u16).to_be_bytes());
    reply.extend(b"SYNTH-DEV-G\0");
    close_record!(reply);
    patch_declared(&mut reply);

    assert_eq!(
        query(30, 1).decode_response(&reply),
        Err(ArcCodecError::PartialSelector {
            receiver_channel: 1,
            missing_field: "source-channel",
        })
    );
}

#[test]
fn channel_only_selector_is_rejected_as_partial() {
    let mut reply = synthetic_page(31, 16, &[Entry::unsubscribed()], 1, false);
    reply[HEADER + 6..HEADER + 8] // source-channel offset word
        .copy_from_slice(&((HEADER + RECORD) as u16).to_be_bytes());
    reply.extend(b"IN-7\0");
    close_record!(reply);
    patch_declared(&mut reply);

    assert_eq!(
        query(31, 1).decode_response(&reply),
        Err(ArcCodecError::PartialSelector {
            receiver_channel: 1,
            missing_field: "source-device",
        })
    );
}

#[test]
fn present_offsets_decoding_to_empty_names_are_rejected() {
    // Both offsets are nonzero but the device field addresses a NUL byte
    // directly: an empty encoded name, not an absent field.
    let mut reply = synthetic_page(32, 16, &[Entry::unsubscribed()], 1, false);
    let empty_at = (reply.len()) as u16;
    reply.extend([0x00u8]);
    reply.extend(b"TAIL\0");
    reply[HEADER + 6..HEADER + 8].copy_from_slice(&(empty_at + 1).to_be_bytes());
    reply[HEADER + 8..HEADER + 10].copy_from_slice(&empty_at.to_be_bytes());
    close_record!(reply);
    patch_declared(&mut reply);

    assert_eq!(
        query(32, 1).decode_response(&reply),
        Err(ArcCodecError::InvalidStringEncoding(
            StringEncodingError::EmptyName {
                channel: 1,
                field: "source-device",
            }
        ))
    );
}

#[test]
fn multiple_entries_decode_with_total_and_page_counts() {
    let query = SubscriptionQuery::new(9, channel(3));
    let reply = synthetic_page(
        9,
        64,
        &[
            Entry::subscribed("SYNTH-DEV-C", "LEFT"),
            Entry::unsubscribed(),
            Entry::subscribed("SYNTH-DEV-D", "RIGHT"),
            Entry::unsubscribed(),
        ],
        1,
        true,
    );

    let page = query.decode_response(&reply).expect("well-formed page");
    assert_eq!(page.subscriptions().len(), 4);
    let channels: Vec<u16> = page
        .subscriptions()
        .iter()
        .map(|entry| entry.receiver_channel().value())
        .collect();
    assert_eq!(channels, vec![1, 2, 3, 4]);
    assert_eq!(
        page.subscriptions()[1].state(),
        &SubscriptionState::Unsubscribed
    );
    assert_eq!(
        page.subscriptions()[2].state(),
        &SubscriptionState::Subscribed(
            TransmitterSelector::new("SYNTH-DEV-D".to_owned(), "RIGHT".to_owned())
                .expect("valid test names")
        )
    );
}

#[test]
fn maximum_sixteen_entry_page_is_accepted_and_seventeen_rejected() {
    let query = SubscriptionQuery::new(11, channel(32));
    let full_page: Vec<Entry> = (0..16).map(|_| Entry::unsubscribed()).collect();
    let reply = synthetic_page(11, 32, &full_page, 17, true);
    let page = query.decode_response(&reply).expect("full page decodes");
    assert_eq!(page.subscriptions().len(), 16);
    assert_eq!(page.subscriptions()[15].receiver_channel(), channel(32));

    let mut too_many = reply.clone();
    too_many.truncate(too_many.len() - 1); // drop nothing structural yet
    too_many.extend([0u8; RECORD - 1]); // extend into one extra full record
    let too_many_len = too_many.len();
    patch_record_end(&mut too_many, HEADER + RECORD * 15, too_many_len);
    too_long_seventeenth(&mut too_many);
    assert_eq!(
        query.decode_response(&too_many),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::EntryCount {
                found: 17,
                maximum: 16
            }
        ))
    );
}

/// Extends a sixteen-entry frame by one zeroed record and declares seventeen
/// entries and a matching declared length.
fn too_long_seventeenth(frame: &mut [u8]) {
    frame[11] = 17; // low byte of count word: entries per page
    patch_declared(frame);
}

// 5. Absolute string offsets.

#[test]
fn absolute_offsets_are_honored_wherever_strings_live() {
    // Handcraft a frame whose string pool is deliberately out of entry
    // order, separated by filler: entry 2's names sit ahead of entry 1's.
    let sequence: u16 = 0x0BAD;
    let mut frame = Vec::new();
    frame.extend(u16::to_be_bytes(QUERY_REPLY_MAGIC));
    frame.extend(0u16.to_be_bytes()); // declared length placeholder
    frame.extend(sequence.to_be_bytes());
    frame.extend(QUERY_COMMAND.to_be_bytes());
    frame.extend(0u16.to_be_bytes());
    frame.extend(((40u16 << 8) | 2u16).to_be_bytes());

    let pool_at: u16 = (HEADER + 2 * RECORD) as u16;

    // Fixed absolute layout, independent of where names will be placed.
    let second_device_at = pool_at + 4; // four filler bytes precede the pool
    let second_channel_at = second_device_at + "EPSILON".len() as u16 + 1;
    let first_device_at = second_channel_at + "CH-B".len() as u16 + 1;
    let first_channel_at = first_device_at + "DELTA".len() as u16 + 1;

    for index in 0..2u16 {
        let device_offset = if index == 0 {
            first_device_at
        } else {
            second_device_at
        };
        let source_offset = if index == 0 {
            first_channel_at
        } else {
            second_channel_at
        };
        frame.extend((1 + index).to_be_bytes());
        frame.extend([0u8; 4]); // raw words 1 and 2
        frame.extend(source_offset.to_be_bytes()); // word 3: source-channel
        frame.extend(device_offset.to_be_bytes()); // word 4: source-device
        frame.extend([0u8; 10]); // words 5..=9 raw + record end, patched below
    }
    let frame_end = first_channel_at + "CH-A".len() as u16 + 1;
    for index in 0..2usize {
        let base = HEADER + index * RECORD;
        frame[base + 18..base + 20].copy_from_slice(&frame_end.to_be_bytes());
    }

    assert_eq!(frame.len(), pool_at as usize);
    frame.extend([0xFFu8; 4]); // deliberate filler between table and pool
    frame.extend(b"EPSILON\0"); // entry 2 device
    frame.extend(b"CH-B\0"); // entry 2 channel
    frame.extend(b"DELTA\0"); // entry 1 device
    frame.extend(b"CH-A\0"); // entry 1 channel
    assert_eq!(frame.len(), frame_end as usize);
    patch_declared(&mut frame);

    let query = SubscriptionQuery::new(sequence, channel(2));
    let page = query
        .decode_response(&frame)
        .expect("out-of-order pool decodes");
    assert_eq!(
        page.subscriptions()[0].state(),
        &SubscriptionState::Subscribed(
            TransmitterSelector::new("DELTA".to_owned(), "CH-A".to_owned())
                .expect("valid test names")
        )
    );
    assert_eq!(
        page.subscriptions()[1].state(),
        &SubscriptionState::Subscribed(
            TransmitterSelector::new("EPSILON".to_owned(), "CH-B".to_owned())
                .expect("valid test names")
        )
    );
}

// 6. Zero record-end offset representation.

#[test]
fn zero_record_end_offset_is_accepted_with_strings_beyond_other_records() {
    // Entry 1 declares record_end 0 (unbounded), so its names may live at
    // the very tail of the frame, past entry 2's own string region.
    let sequence: u16 = 21;
    let mut frame = Vec::new();
    frame.extend(u16::to_be_bytes(QUERY_REPLY_MAGIC));
    frame.extend(0u16.to_be_bytes());
    frame.extend(sequence.to_be_bytes());
    frame.extend(QUERY_COMMAND.to_be_bytes());
    frame.extend(0u16.to_be_bytes());
    frame.extend(((48u16 << 8) | 2u16).to_be_bytes());

    let first_base = HEADER;
    let second_base = HEADER + RECORD;
    frame.extend(1u16.to_be_bytes()); // entry 1 channel id
    frame.extend([0u8; RECORD - 2]); // fields patched below
    frame.extend(2u16.to_be_bytes()); // entry 2 channel id
    frame.extend([0u8; RECORD - 2]); // fields patched below

    let second_device_at = frame.len() as u16;
    frame.extend(b"SIGMA\0");
    frame.extend(b"IN-9\0");
    let second_end = frame.len() as u16;
    // Entry 2 is bounded and keeps its strings strictly inside its region.
    frame[second_base + 8..second_base + 10].copy_from_slice(&second_device_at.to_be_bytes()); // device: SIGMA
    frame[second_base + 6..second_base + 8].copy_from_slice(&(second_device_at + 6).to_be_bytes()); // channel: IN-9
    frame[second_base + 18..second_base + 20].copy_from_slice(&second_end.to_be_bytes());

    // Entry 1 uses record_end 0 and points into the tail pool.
    let first_device_at = second_end;
    frame.extend(b"OMEGA-X\0");
    frame.extend(b"OUT-8\0");
    frame[first_base + 8..first_base + 10].copy_from_slice(&first_device_at.to_be_bytes()); // device: OMEGA-X
    frame[first_base + 6..first_base + 8].copy_from_slice(&(first_device_at + 8).to_be_bytes()); // channel: OUT-8
    frame[first_base + 18..first_base + 20].copy_from_slice(&0u16.to_be_bytes());
    patch_declared(&mut frame);

    let query = SubscriptionQuery::new(sequence, channel(1));
    let page = query
        .decode_response(&frame)
        .expect("zero end offset decodes");
    assert_eq!(
        page.subscriptions()[0].state(),
        &SubscriptionState::Subscribed(
            TransmitterSelector::new("OMEGA-X".to_owned(), "OUT-8".to_owned())
                .expect("valid test names")
        )
    );
    assert_eq!(
        page.subscriptions()[1].state(),
        &SubscriptionState::Subscribed(
            TransmitterSelector::new("SIGMA".to_owned(), "IN-9".to_owned())
                .expect("valid test names")
        )
    );
}

// Domain channel space versus this variant's paged support.

#[test]
fn domain_receiver_channels_extend_beyond_the_query_paging() {
    // Channel 33 is a legal Dante channel: the shared domain type spans it.
    assert!(ReceiverChannel::new(33).is_ok());
}

#[test]
fn queries_beyond_channel_32_address_their_own_page() {
    let start = |value| raw_word(&SubscriptionQuery::new(2, channel(value)).encode(), 6);
    assert_eq!(start(33), 33);
    assert_eq!(start(48), 33);
    assert_eq!(start(300), 289);
}

#[test]
fn receiver_page_query_accepts_an_empty_page_past_the_last_channel() {
    let query = ReceiverPageQuery::containing(21, channel(33));
    assert_eq!(query.first_channel(), channel(33));
    let mut frame = Vec::new();
    for word in [QUERY_REPLY_MAGIC, 12, 21, QUERY_COMMAND, 0x0001, 0x1000] {
        frame.extend(word.to_be_bytes());
    }
    let page = query.decode_response(&frame).expect("empty page is valid");
    assert!(page.subscriptions().is_empty());
    assert!(
        query_decode_is_entry_count_error(&frame),
        "a single-channel query still requires its entry"
    );
}

fn query_decode_is_entry_count_error(frame: &[u8]) -> bool {
    matches!(
        query(21, 33).decode_response(frame),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::EntryCount { found: 0, .. }
        ))
    )
}

#[test]
fn receiver_page_query_steps_to_the_next_page() {
    let query = ReceiverPageQuery::containing(1, channel(20));
    assert_eq!(query.first_channel(), channel(17));
    assert_eq!(query.next_first_channel(), Some(channel(33)));
    assert_eq!(
        ReceiverPageQuery::containing(1, channel(u16::MAX)).next_first_channel(),
        None
    );
}

#[test]
fn receiver_name_is_decoded_when_present() {
    let mut reply = synthetic_page(0x0303, 2, &[Entry::unsubscribed()], 1, false);
    let offset = reply.len() as u16;
    reply.extend_from_slice(b"Left\0");
    reply[HEADER + 10..HEADER + 12].copy_from_slice(&offset.to_be_bytes());
    close_record!(reply);
    patch_declared(&mut reply);
    let page = query(0x0303, 1)
        .decode_response(&reply)
        .expect("named page");
    assert_eq!(page.subscriptions()[0].name(), Some("Left"));

    let unnamed = synthetic_page(0x0303, 2, &[Entry::unsubscribed()], 1, false);
    let page = query(0x0303, 1)
        .decode_response(&unnamed)
        .expect("unnamed page");
    assert_eq!(page.subscriptions()[0].name(), None);
}

// Correlation rejections.

#[test]
fn wrong_sequence_is_rejected() {
    let reply = synthetic_page(100, 32, &[Entry::unsubscribed()], 1, false);
    let query = SubscriptionQuery::new(200, channel(1));
    assert_eq!(
        query.decode_response(&reply),
        Err(ArcCodecError::SequenceMismatch {
            received: 100,
            expected: 200,
        })
    );
}

#[test]
fn sibling_envelope_magics_are_refused_as_unsupported_variant() {
    for sibling_magic in [QUERY_MAGIC, SIBLING_REPLY_MAGIC] {
        let mut reply = synthetic_page(3, 32, &[Entry::unsubscribed()], 1, false);
        reply[0..2].copy_from_slice(&sibling_magic.to_be_bytes());
        assert_eq!(
            SubscriptionQuery::new(3, channel(1)).decode_response(&reply),
            Err(ArcCodecError::UnsupportedVariant {
                found: sibling_magic
            })
        );
    }
}

#[test]
fn unknown_magic_is_rejected_and_wrong_command_is_rejected() {
    let query = SubscriptionQuery::new(5, channel(1));

    let mut unknown = synthetic_page(5, 32, &[Entry::unsubscribed()], 1, false);
    unknown[0..2].copy_from_slice(&0x1234u16.to_be_bytes());
    assert_eq!(
        query.decode_response(&unknown),
        Err(ArcCodecError::InvalidMagic { found: 0x1234 })
    );

    let mut wrong_command = synthetic_page(5, 32, &[Entry::unsubscribed()], 1, false);
    wrong_command[6..8].copy_from_slice(&0x3010u16.to_be_bytes());
    assert_eq!(
        query.decode_response(&wrong_command),
        Err(ArcCodecError::UnexpectedCommand {
            found: 0x3010,
            expected: QUERY_COMMAND,
        })
    );
}

#[test]
fn requested_channel_outside_page_is_rejected() {
    // Record ids match the page-one start the query implies, but the page
    // stops short of the addressed channel.
    let reply = synthetic_page(
        6,
        48,
        &[
            Entry::unsubscribed(),
            Entry::unsubscribed(),
            Entry::unsubscribed(),
        ],
        17,
        true,
    );
    let query = SubscriptionQuery::new(6, channel(20));
    assert_eq!(
        query.decode_response(&reply),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::RequestedChannelOutsidePage {
                requested: 20,
                first_entry_channel: 17,
                entry_count: 3,
            }
        ))
    );
}

// Truncation rejections.

#[test]
fn truncated_frame_is_rejected() {
    let query = SubscriptionQuery::new(8, channel(1));
    let full = synthetic_page(8, 32, &[Entry::unsubscribed()], 1, false);

    let mut cut_mid_record = full[..HEADER + RECORD - 6].to_vec();
    let cut_len = cut_mid_record.len();
    cut_mid_record[2..4].copy_from_slice(&(cut_len as u16).to_be_bytes());
    assert_eq!(
        query.decode_response(&cut_mid_record),
        Err(ArcCodecError::TruncatedFrame {
            required: HEADER + RECORD,
            available: HEADER + RECORD - 6,
        })
    );

    assert_eq!(
        query.decode_response(&full[..6]),
        Err(ArcCodecError::TruncatedFrame {
            required: HEADER,
            available: 6,
        })
    );

    let inflated_len = full.len();
    let mut inflated_declaration = full.clone();
    inflated_declaration[2..4].copy_from_slice(&((inflated_len + 10) as u16).to_be_bytes());
    assert_eq!(
        query.decode_response(&inflated_declaration),
        Err(ArcCodecError::LengthMismatch {
            declared: inflated_len + 10,
            actual: inflated_len,
        })
    );

    // Declaring fewer bytes than arrive is also a mismatch, not silently
    // accepted padding.
    let mut deflated_declaration = full.clone();
    let declared_under = deflated_declaration.len() - 2;
    deflated_declaration[2..4].copy_from_slice(&(declared_under as u16).to_be_bytes());
    assert_eq!(
        query.decode_response(&deflated_declaration),
        Err(ArcCodecError::LengthMismatch {
            declared: declared_under,
            actual: deflated_declaration.len(),
        })
    );

    // A consistent declaration on genuinely missing bytes stays truncation:
    // here the frame honestly claims 30 bytes but cannot hold one record.
    let mut shortened_frame = full;
    shortened_frame.pop();
    shortened_frame.pop();
    let short_len = shortened_frame.len();
    shortened_frame[2..4].copy_from_slice(&(short_len as u16).to_be_bytes());
    assert_eq!(
        query.decode_response(&shortened_frame),
        Err(ArcCodecError::TruncatedFrame {
            required: HEADER + RECORD,
            available: short_len,
        })
    );
}

// Layout and offset rejections.

#[test]
fn layout_violations_are_typed() {
    let query = SubscriptionQuery::new(4, channel(1));

    let mut entry_count_zero = synthetic_page(4, 32, &[Entry::unsubscribed()], 1, false);
    entry_count_zero[11] = 0;
    assert_eq!(
        query.decode_response(&entry_count_zero),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::EntryCount {
                found: 0,
                maximum: 16
            }
        ))
    );

    let mut wrong_channel_id = synthetic_page(4, 32, &[Entry::unsubscribed()], 1, false);
    wrong_channel_id[12..14].copy_from_slice(&9u16.to_be_bytes());
    assert_eq!(
        query.decode_response(&wrong_channel_id),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::EntryChannelId {
                found: 9,
                expected: 1
            }
        ))
    );

    let mut bad_record_end = synthetic_page(4, 32, &[Entry::unsubscribed()], 1, false);
    bad_record_end[HEADER + 18..HEADER + 20].copy_from_slice(&20u16.to_be_bytes());
    assert_eq!(
        query.decode_response(&bad_record_end),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::RecordEndOffset {
                channel: 1,
                found: 20,
                limit: bad_record_end.len(),
            }
        ))
    );
}

#[test]
fn out_of_bounds_string_offset_is_rejected() {
    let query = SubscriptionQuery::new(13, channel(1));

    let mut before_table = synthetic_page(13, 32, &[Entry::unsubscribed()], 1, false);
    before_table[HEADER + 8..HEADER + 10].copy_from_slice(&4u16.to_be_bytes()); // header zone
    assert_eq!(
        query.decode_response(&before_table),
        Err(ArcCodecError::InvalidStringOffset(
            StringOffsetError::OutOfBounds {
                channel: 1,
                offset: 4,
            }
        ))
    );

    // A bounded record whose end sits mid-frame rejects offsets that reach
    // past its record-end even though those bytes exist later in the frame.
    let sequence: u16 = 13;
    let mut frame = Vec::new();
    frame.extend(u16::to_be_bytes(QUERY_REPLY_MAGIC));
    frame.extend(0u16.to_be_bytes()); // declared length, patched below
    frame.extend(sequence.to_be_bytes());
    frame.extend(QUERY_COMMAND.to_be_bytes());
    frame.extend(0u16.to_be_bytes());
    frame.extend(((16u16 << 8) | 1u16).to_be_bytes()); // total high byte, one entry

    let entries_end = (HEADER + RECORD) as u16;
    let beyond_end = entries_end + 4u16;
    frame.extend(1u16.to_be_bytes()); // entry 1 channel id
    frame.extend([0u8; 4]); // raw words 1 and 2
    frame.extend(0u16.to_be_bytes()); // source-channel offset stays absent
    frame.extend(beyond_end.to_be_bytes()); // source-device offset past record end
    frame.extend([0u8; 10]); // words 5..=9 raw, record end patched below
    patch_record_end(&mut frame, HEADER, HEADER + RECORD + 4);
    frame.extend([0xEEu8; 8]); // reachable tail bytes the offset now misses
    patch_declared(&mut frame);

    let query = SubscriptionQuery::new(sequence, channel(1));
    assert_eq!(
        query.decode_response(&frame),
        Err(ArcCodecError::InvalidStringOffset(
            StringOffsetError::OutOfBounds {
                channel: 1,
                offset: beyond_end,
            }
        ))
    );
}

#[test]
fn unterminated_string_is_rejected() {
    let sequence: u16 = 17;
    let mut frame = Vec::new();
    frame.extend(u16::to_be_bytes(QUERY_REPLY_MAGIC));
    frame.extend(0u16.to_be_bytes());
    frame.extend(sequence.to_be_bytes());
    frame.extend(QUERY_COMMAND.to_be_bytes());
    frame.extend(0u16.to_be_bytes());
    frame.extend(((16u16 << 8) | 1u16).to_be_bytes());

    let device_offset = (HEADER + RECORD) as u16;
    frame.extend(1u16.to_be_bytes()); // entry 1 channel id
    frame.extend([0u8; 4]); // raw words 1 and 2
    frame.extend(0u16.to_be_bytes()); // source-channel offset stays zero
    frame.extend(device_offset.to_be_bytes()); // source-device offset
    frame.extend([0u8; 10]); // words 5..=8 and record end, patched below
    let record_end = device_offset as usize + 5;
    frame[HEADER + 18..HEADER + 20].copy_from_slice(&(record_end as u16).to_be_bytes());
    frame.extend(b"NULLESS"); // no terminator inside the bounded record
    patch_declared(&mut frame);

    let query = SubscriptionQuery::new(sequence, channel(1));
    assert_eq!(
        query.decode_response(&frame),
        Err(ArcCodecError::InvalidStringOffset(
            StringOffsetError::NotTerminated {
                channel: 1,
                field: "source-device",
            }
        ))
    );
}

#[test]
fn non_ascii_name_is_rejected_as_an_encoding_error() {
    let query = SubscriptionQuery::new(19, channel(1));

    let mut mojibake = synthetic_page(19, 16, &[Entry::unsubscribed()], 1, false);
    let offset = mojibake.len();
    mojibake.extend([b'C', 0xC3, 0x89, 0xF0, 0x90, 0x80, 0x80, 0]); // UTF-8 only, not ASCII
    mojibake[HEADER + 8..HEADER + 10].copy_from_slice(&(offset as u16).to_be_bytes());
    let end = mojibake.len();
    patch_record_end(&mut mojibake, HEADER, end);
    patch_declared(&mut mojibake);
    assert_eq!(
        query.decode_response(&mojibake),
        Err(ArcCodecError::InvalidStringEncoding(
            StringEncodingError::NonAscii {
                channel: 1,
                field: "source-device",
            }
        ))
    );
}

#[test]
fn out_of_bounds_receiver_label_offset_is_rejected() {
    let mut reply = synthetic_page(40, 16, &[Entry::unsubscribed()], 1, false);
    reply[HEADER + 10..HEADER + 12].copy_from_slice(&4u16.to_be_bytes()); // header zone
    assert_eq!(
        query(40, 1).decode_response(&reply),
        Err(ArcCodecError::InvalidStringOffset(
            StringOffsetError::OutOfBounds {
                channel: 1,
                offset: 4,
            }
        ))
    );
}

#[test]
fn unterminated_receiver_name_is_rejected() {
    let mut reply = synthetic_page(41, 16, &[Entry::unsubscribed()], 1, false);
    let label_at = reply.len() as u16;
    reply.extend(b"NULLESS"); // never terminated within the bounded region
    reply[HEADER + 10..HEADER + 12].copy_from_slice(&label_at.to_be_bytes());
    close_record!(reply);
    patch_declared(&mut reply);

    assert_eq!(
        query(41, 1).decode_response(&reply),
        Err(ArcCodecError::InvalidStringOffset(
            StringOffsetError::NotTerminated {
                channel: 1,
                field: "receiver-name",
            }
        ))
    );
}

#[test]
fn absent_receiver_name_offset_is_accepted() {
    // Unsubscribed pages carry no label; decoding still succeeds and the
    // normalized model exposes only subscription state.
    let reply = synthetic_page(42, 16, &[Entry::unsubscribed()], 1, false);
    let page = query(42, 1)
        .decode_response(&reply)
        .expect("label-free page decodes");
    assert_eq!(
        page.subscriptions()[0].state(),
        &SubscriptionState::Unsubscribed
    );
}

#[test]
fn oversized_name_is_rejected_as_an_encoding_error() {
    let query = SubscriptionQuery::new(19, channel(1));

    let mut too_long = synthetic_page(
        19,
        16,
        &[Entry::subscribed("SYNTH-DEV-F", "CH-Y")],
        1,
        false,
    );
    let long_offset = too_long.len();
    too_long.extend(vec![b'a'; 256]);
    too_long.push(0);
    too_long[HEADER + 8..HEADER + 10].copy_from_slice(&(long_offset as u16).to_be_bytes());
    let end = too_long.len();
    patch_record_end(&mut too_long, HEADER, end);
    patch_declared(&mut too_long);
    assert_eq!(
        query.decode_response(&too_long),
        Err(ArcCodecError::InvalidStringEncoding(
            StringEncodingError::TooLong {
                channel: 1,
                field: "source-device",
                length: 256,
            }
        ))
    );
}
