//! Settings of the Scarlett 18i20 3rd Gen, read from fixed configuration
//! offsets.
//!
//! The offsets are those the Linux `scarlett2` mixer driver documents for
//! this model. Every value read must be 0 or 1; anything else is refused
//! rather than guessed at, since it would mean the offsets do not match this
//! firmware. Preamp gain is analogue on this generation and not readable.

use std::{error::Error, fmt};

/// Number of mic/line inputs.
pub const INPUTS: u8 = 8;
/// Inputs with a software line/instrument switch.
pub const INSTRUMENT_INPUTS: u8 = 2;
/// Phantom power switches, each covering four consecutive inputs.
pub const PHANTOM_GROUPS: u8 = 2;
const PHANTOM_GROUP_SIZE: u8 = 4;

/// One configuration read: what it is, where, and how many bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Read {
    pub field: Field,
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Field {
    /// Monitor mute and dim, in that order.
    MuteDim,
    Instrument,
    Pad,
    Air,
    Phantom,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Self::MuteDim => "mute/dim",
            Self::Instrument => "instrument",
            Self::Pad => "pad",
            Self::Air => "air",
            Self::Phantom => "phantom",
        }
    }
}

/// Every read [`Settings::decode`] needs.
pub const READS: [Read; 5] = [
    Read {
        field: Field::MuteDim,
        offset: 0x31,
        size: 2,
    },
    Read {
        field: Field::Instrument,
        offset: 0x7c,
        size: INSTRUMENT_INPUTS as u32,
    },
    Read {
        field: Field::Pad,
        offset: 0x84,
        size: INPUTS as u32,
    },
    Read {
        field: Field::Air,
        offset: 0x8c,
        size: INPUTS as u32,
    },
    Read {
        field: Field::Phantom,
        offset: 0x9c,
        size: PHANTOM_GROUPS as u32,
    },
];

/// Front-panel input switches for one input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Input {
    /// Input number, 1–8.
    pub number: u8,
    /// Line/instrument switch; only inputs 1–2 have one.
    pub instrument: Option<bool>,
    pub pad: bool,
    pub air: bool,
    /// Phantom power, as set by this input's group switch.
    pub phantom: bool,
}

/// The decoded settings of one device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub inputs: Vec<Input>,
    /// The two phantom switches: inputs 1–4 and 5–8.
    pub phantom_groups: [bool; 2],
    pub mute: bool,
    pub dim: bool,
}

impl Settings {
    /// Decodes the results of [`READS`], given in the same order.
    pub fn decode(results: &[Vec<u8>]) -> Result<Self, SettingsError> {
        if results.len() != READS.len() {
            return Err(SettingsError::ReadCount {
                found: results.len(),
            });
        }
        let mut flags: Vec<Vec<bool>> = Vec::with_capacity(READS.len());
        for (read, bytes) in READS.iter().zip(results) {
            if bytes.len() != read.size as usize {
                return Err(SettingsError::Length {
                    field: read.field.name(),
                    found: bytes.len(),
                });
            }
            let decoded = bytes
                .iter()
                .enumerate()
                .map(|(index, &value)| match value {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(SettingsError::Value {
                        field: read.field.name(),
                        index,
                        value,
                    }),
                })
                .collect::<Result<Vec<_>, _>>()?;
            flags.push(decoded);
        }
        let [mute_dim, instrument, pad, air, phantom] = flags.try_into().expect("five reads");
        let phantom_groups = [phantom[0], phantom[1]];
        let inputs = (0..INPUTS)
            .map(|index| Input {
                number: index + 1,
                instrument: instrument.get(usize::from(index)).copied(),
                pad: pad[usize::from(index)],
                air: air[usize::from(index)],
                phantom: phantom_groups[usize::from(index / PHANTOM_GROUP_SIZE)],
            })
            .collect();
        Ok(Self {
            inputs,
            phantom_groups,
            mute: mute_dim[0],
            dim: mute_dim[1],
        })
    }
}

/// Configuration bytes that do not look like this model's settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsError {
    ReadCount {
        found: usize,
    },
    Length {
        field: &'static str,
        found: usize,
    },
    Value {
        field: &'static str,
        index: usize,
        value: u8,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadCount { found } => write!(formatter, "expected 5 reads, got {found}"),
            Self::Length { field, found } => {
                write!(formatter, "{field} read returned {found} bytes")
            }
            Self::Value {
                field,
                index,
                value,
            } => write!(
                formatter,
                "{field}[{index}] is {value}, not 0 or 1; this firmware's layout is not supported"
            ),
        }
    }
}

impl Error for SettingsError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(
        mute_dim: [u8; 2],
        inst: [u8; 2],
        pad: [u8; 8],
        air: [u8; 8],
        phantom: [u8; 2],
    ) -> Vec<Vec<u8>> {
        vec![
            mute_dim.to_vec(),
            inst.to_vec(),
            pad.to_vec(),
            air.to_vec(),
            phantom.to_vec(),
        ]
    }

    #[test]
    fn decodes_switches_per_input() {
        let settings = Settings::decode(&results(
            [0, 1],
            [1, 0],
            [0, 0, 1, 0, 0, 0, 0, 0],
            [1, 0, 0, 0, 0, 0, 0, 1],
            [1, 0],
        ))
        .unwrap();
        assert!(!settings.mute && settings.dim);
        assert_eq!(settings.phantom_groups, [true, false]);
        let first = settings.inputs[0];
        assert_eq!(first.instrument, Some(true));
        assert!(first.air && first.phantom && !first.pad);
        assert!(settings.inputs[2].pad);
        let last = settings.inputs[7];
        assert_eq!(last.instrument, None);
        assert!(last.air && !last.phantom);
        assert!(settings.inputs[3].phantom && !settings.inputs[4].phantom);
    }

    #[test]
    fn values_other_than_zero_or_one_are_refused() {
        let error = Settings::decode(&results(
            [0, 0],
            [0, 0],
            [0, 0, 0, 2, 0, 0, 0, 0],
            [0; 8],
            [0, 0],
        ))
        .unwrap_err();
        assert_eq!(
            error,
            SettingsError::Value {
                field: "pad",
                index: 3,
                value: 2
            }
        );
    }

    #[test]
    fn wrong_shapes_are_refused() {
        assert!(matches!(
            Settings::decode(&[]),
            Err(SettingsError::ReadCount { found: 0 })
        ));
        let mut short = results([0, 0], [0, 0], [0; 8], [0; 8], [0, 0]);
        short[2].pop();
        assert!(matches!(
            Settings::decode(&short),
            Err(SettingsError::Length {
                field: "pad",
                found: 7
            })
        ));
    }
}
