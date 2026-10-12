//! The SpaceNavigator's HID input reports, id byte first as hidapi returns them on Windows.

/// What one report says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// Axis values starting at axis `first` (0: tx ... 5: rz).
    Axes { first: usize, values: Vec<i16> },
    /// The buttons held, as bits (1: left, 2: right).
    Buttons(u8),
}

/// A report into an update; `None` for anything not understood (unknown id, too short).
pub fn parse(buf: &[u8]) -> Option<Update> {
    let (&id, data) = buf.split_first()?;
    let axes = |n: usize| -> Vec<i16> {
        data.as_chunks::<2>().0.iter().take(n).map(|c| i16::from_le_bytes(*c)).collect()
    };
    match id {
        // Newer firmware: all six axes in one report.
        1 if data.len() >= 12 => Some(Update::Axes { first: 0, values: axes(6) }),
        1 if data.len() >= 6 => Some(Update::Axes { first: 0, values: axes(3) }),
        2 if data.len() >= 6 => Some(Update::Axes { first: 3, values: axes(3) }),
        3 => data.first().map(|b| Update::Buttons(*b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_of_both_firmwares() {
        assert_eq!(
            parse(&[1, 1, 0, 0xff, 0xff, 0x10, 0]),
            Some(Update::Axes { first: 0, values: vec![1, -1, 16] })
        );
        assert_eq!(parse(&[2, 0, 1, 0, 0, 0, 0]), Some(Update::Axes { first: 3, values: vec![256, 0, 0] }));
        let twelve = [1u8, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0];
        assert_eq!(parse(&twelve), Some(Update::Axes { first: 0, values: vec![1, 2, 3, 4, 5, 6] }));
        assert_eq!(parse(&[3, 2, 0, 0, 0, 0, 0]), Some(Update::Buttons(2)));
        assert_eq!(parse(&[3]), None);
        assert_eq!(parse(&[2, 0, 0, 0]), None);
        assert_eq!(parse(&[7, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(parse(&[]), None);
    }
}
