//! Read-only ARC receiver-subscription query and its tabular response.
//!
//! Supported envelope:
//!
//! - Request: magic `0x280F`, declared length 16, command `0x3000`, two
//!   fixed words (`0x0000`, `0x0001`), the first receiver channel of a
//!   16-channel page (1, 17, 33, …), trailing zero word. The request is 16
//!   bytes.
//! - Response: magic `0x2801`, command `0x3000`, a sequence matching the
//!   request, a fixed 12-byte header, up to 16 sequential 20-byte records,
//!   then a NUL-terminated ASCII string pool.
//! - The response count word's low byte is the number of entries carried
//!   in this page. Its high byte is not interpreted: observed devices
//!   report a value capped at the page size, so it is not a reliable total.
//!   A page past the device's last channel carries zero entries.
//! - A record's absolute string offsets address into that pool; an offset
//!   of zero means the field is absent. A record-end offset of zero means
//!   the record's strings are bounded only by the frame end.
//!
//! No other request or response form is implemented. Encoding has exactly
//! one shape; decoding accepts exactly one magic/command pair and refuses
//! sibling ARC envelope families explicitly.

use super::{LISTING_REPLY_MAGIC, REQUEST_MAGIC, check_header, resolve_string, word_at};
use crate::{
    error::{ArcCodecError, RecordLayoutError},
    model::{
        ReceiverChannel, ReceiverSubscription, ReceiverSubscriptionPage, SubscriptionState,
        TransmitterSelector,
    },
};

/// Wire command of the read-only receiver-state query in both directions.
pub(crate) const QUERY_COMMAND: u16 = 0x3000;

/// Encoded length of the query request, in bytes.
const REQUEST_LENGTH: usize = 16;
/// Bytes of fixed header before the entry table.
const RESPONSE_HEADER_LENGTH: usize = 12;
/// Maximum entries per page.
const MAX_ENTRIES_PER_PAGE: u8 = 16;
/// Channels per page: requests start at 1, 17, 33, ….
const ENTRIES_PER_PAGE: u16 = 16;

/// Fixed record width in bytes (ten big-endian words).
const RECORD_LENGTH: usize = 20;
/// Byte offset, within one record, of the record-end-offset field.
const RECORD_END_BYTE: usize = 18;
/// Byte offsets, within one record, of the absolute string offsets this
/// codec resolves. Words 1–2 and 6–8 remain semantically unknown and are
/// never interpreted as addresses or meaning.
const SOURCE_CHANNEL_OFFSET_BYTE: usize = 6;
const SOURCE_DEVICE_OFFSET_BYTE: usize = 8;
const RECEIVER_NAME_OFFSET_BYTE: usize = 10;

const SOURCE_CHANNEL_FIELD: &str = "source-channel";
const SOURCE_DEVICE_FIELD: &str = "source-device";
const RECEIVER_NAME_FIELD: &str = "receiver-name";

/// A read-only query for the 16-channel page containing one receiver
/// channel, bound to one wire sequence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReceiverPageQuery {
    sequence: u16,
    first_channel: u16,
}

impl ReceiverPageQuery {
    /// Queries the page that contains `channel`: channels 1–16, 17–32, and
    /// so on.
    pub fn containing(sequence: u16, channel: ReceiverChannel) -> Self {
        let first_channel = (channel.value() - 1) / ENTRIES_PER_PAGE * ENTRIES_PER_PAGE + 1;
        Self {
            sequence,
            first_channel,
        }
    }

    /// The bound sequence value that any correlated reply must echo.
    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    /// The first receiver channel this page covers.
    pub fn first_channel(&self) -> ReceiverChannel {
        ReceiverChannel::new(self.first_channel).expect("page starts are nonzero")
    }

    /// The first channel of the following page, if representable.
    pub fn next_first_channel(&self) -> Option<ReceiverChannel> {
        self.first_channel
            .checked_add(ENTRIES_PER_PAGE)
            .and_then(|value| ReceiverChannel::new(value).ok())
    }

    /// Encodes the exactly-observed 16-byte query envelope.
    ///
    /// Word layout (big-endian): magic, declared length, bound sequence,
    /// command, two fixed words (`0x0000`, `0x0001`), the page's first
    /// channel, and a final zero word.
    pub fn encode(&self) -> [u8; REQUEST_LENGTH] {
        let words: [u16; 8] = [
            REQUEST_MAGIC,
            REQUEST_LENGTH as u16,
            self.sequence,
            QUERY_COMMAND,
            0x0000,
            0x0001,
            self.first_channel,
            0x0000,
        ];
        let mut frame = [0u8; REQUEST_LENGTH];
        for (slot, word) in frame.as_chunks_mut::<2>().0.iter_mut().zip(words) {
            *slot = word.to_be_bytes();
        }
        frame
    }

    /// Decodes and validates a datagram believed to answer this query.
    ///
    /// A page with zero entries is valid: it means the device has no
    /// receiver channels at or beyond [`Self::first_channel`].
    pub fn decode_response(&self, frame: &[u8]) -> Result<ReceiverSubscriptionPage, ArcCodecError> {
        decode_page(frame, self.sequence, self.first_channel, None)
    }
}

/// A read-only query for one receiver channel's subscription, bound to one
/// wire sequence. Encodes the query for the page containing the channel.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubscriptionQuery {
    page: ReceiverPageQuery,
    receiver_channel: ReceiverChannel,
}

impl SubscriptionQuery {
    /// Binds a 16-bit sequence and one receiver channel to this query
    /// instance. The binding cannot change afterwards.
    pub fn new(sequence: u16, receiver_channel: ReceiverChannel) -> Self {
        Self {
            page: ReceiverPageQuery::containing(sequence, receiver_channel),
            receiver_channel,
        }
    }

    /// The bound sequence value that any correlated reply must echo.
    pub fn sequence(&self) -> u16 {
        self.page.sequence
    }

    /// The addressed receiver channel.
    pub fn receiver_channel(&self) -> ReceiverChannel {
        self.receiver_channel
    }

    /// Encodes the query for the page containing the addressed channel.
    pub fn encode(&self) -> [u8; REQUEST_LENGTH] {
        self.page.encode()
    }

    /// Decodes and validates a datagram believed to answer this query.
    ///
    /// Correlation (magic, command, sequence, page membership of the bound
    /// receiver channel), framing, record layout, and string integrity are
    /// all verified before any name is normalized.
    pub fn decode_response(&self, frame: &[u8]) -> Result<ReceiverSubscriptionPage, ArcCodecError> {
        decode_page(
            frame,
            self.page.sequence,
            self.page.first_channel,
            Some(self.receiver_channel.value()),
        )
    }
}

/// Decodes one receiver page. When `requested` is set, the page must carry
/// at least one entry and must contain that channel.
fn decode_page(
    frame: &[u8],
    sequence: u16,
    start_channel: u16,
    requested: Option<u16>,
) -> Result<ReceiverSubscriptionPage, ArcCodecError> {
    check_header(
        frame,
        &[LISTING_REPLY_MAGIC],
        QUERY_COMMAND,
        sequence,
        RESPONSE_HEADER_LENGTH,
    )?;

    // Low byte: entries in page. The high byte is deliberately ignored.
    let entry_count = frame[11];
    if entry_count > MAX_ENTRIES_PER_PAGE || (requested.is_some() && entry_count == 0) {
        return Err(RecordLayoutError::EntryCount {
            found: entry_count,
            maximum: MAX_ENTRIES_PER_PAGE,
        }
        .into());
    }
    let entries_end = RESPONSE_HEADER_LENGTH + usize::from(entry_count) * RECORD_LENGTH;
    if entries_end > frame.len() {
        return Err(ArcCodecError::TruncatedFrame {
            required: entries_end,
            available: frame.len(),
        });
    }
    let record = |index: usize, byte: usize| {
        word_at(frame, RESPONSE_HEADER_LENGTH + index * RECORD_LENGTH + byte)
    };

    for index in 0..usize::from(entry_count) {
        let channel_id = record(index, 0);
        let expected_channel_id = start_channel + index as u16;
        if channel_id != expected_channel_id {
            return Err(RecordLayoutError::EntryChannelId {
                found: channel_id,
                expected: expected_channel_id,
            }
            .into());
        }
    }
    if let Some(requested) = requested
        && (!(start_channel..start_channel + ENTRIES_PER_PAGE).contains(&requested)
            || requested >= start_channel + u16::from(entry_count))
    {
        return Err(RecordLayoutError::RequestedChannelOutsidePage {
            requested,
            first_entry_channel: start_channel,
            entry_count,
        }
        .into());
    }

    // Structural pass over every record before any name is decoded.
    for index in 0..usize::from(entry_count) {
        let record_end_offset = record(index, RECORD_END_BYTE);
        if record_end_offset != 0
            && !(entries_end..=frame.len()).contains(&usize::from(record_end_offset))
        {
            return Err(RecordLayoutError::RecordEndOffset {
                channel: start_channel + index as u16,
                found: record_end_offset,
                limit: frame.len(),
            }
            .into());
        }
    }

    let mut subscriptions = Vec::with_capacity(usize::from(entry_count));
    for index in 0..usize::from(entry_count) {
        let channel = start_channel + index as u16;
        let source_channel_offset = record(index, SOURCE_CHANNEL_OFFSET_BYTE);
        let source_device_offset = record(index, SOURCE_DEVICE_OFFSET_BYTE);
        let record_end_offset = record(index, RECORD_END_BYTE);
        let string_end = if record_end_offset == 0 {
            frame.len()
        } else {
            usize::from(record_end_offset)
        };

        // Strings must address at or after the entry-table end and
        // terminate before their record's end (or frame end when the
        // record-end offset is zero).
        let string_region = entries_end..string_end;
        let source_device = resolve_string(
            frame,
            source_device_offset,
            string_region.clone(),
            SOURCE_DEVICE_FIELD,
            channel,
        )?;
        let source_channel = resolve_string(
            frame,
            source_channel_offset,
            string_region.clone(),
            SOURCE_CHANNEL_FIELD,
            channel,
        )?;
        let receiver_name = resolve_string(
            frame,
            record(index, RECEIVER_NAME_OFFSET_BYTE),
            string_region,
            RECEIVER_NAME_FIELD,
            channel,
        )?;

        let state = normalize_state(
            source_device_offset,
            source_device,
            source_channel_offset,
            source_channel,
            channel,
        )?;
        subscriptions.push(ReceiverSubscription::new(
            ReceiverChannel::new(channel).expect("page channels are nonzero"),
            (!receiver_name.is_empty()).then_some(receiver_name),
            state,
        ));
    }

    Ok(ReceiverSubscriptionPage::new(subscriptions))
}

/// Normalizes an entry into domain state.
///
/// - both fields absent → `Unsubscribed`;
/// - both fields present with non-empty decoded names → `Subscribed`;
/// - anything else → a typed error, because a half-present selector is
///   malformed evidence rather than a trustworthy unsubscribe signal.
///
/// A nonzero offset decoding to an empty name was already rejected by
/// [`resolve_string`] as an encoding violation, so presence here implies a
/// non-empty name on both sides.
fn normalize_state(
    source_device_offset: u16,
    source_device: String,
    source_channel_offset: u16,
    source_channel: String,
    receiver_channel: u16,
) -> Result<SubscriptionState, ArcCodecError> {
    match (source_device_offset != 0, source_channel_offset != 0) {
        (false, false) => Ok(SubscriptionState::Unsubscribed),
        (true, false) => Err(ArcCodecError::PartialSelector {
            receiver_channel,
            missing_field: SOURCE_CHANNEL_FIELD,
        }),
        (false, true) => Err(ArcCodecError::PartialSelector {
            receiver_channel,
            missing_field: SOURCE_DEVICE_FIELD,
        }),
        (true, true) => Ok(SubscriptionState::Subscribed(
            TransmitterSelector::from_decoded(source_device, source_channel),
        )),
    }
}
