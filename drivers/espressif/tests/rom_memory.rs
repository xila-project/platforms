#[path = "../src/rom_memory/loops.rs"]
mod loops;

use loops::{copy_backward, copy_forward, fill, overlaps};

/// A buffer with a recognizable value in every byte.
fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index * 7 + 3) as u8).collect()
}

#[test]
fn a_range_overlaps_when_it_shares_a_byte() {
    let range = 0x3F80_0000..0x3FC0_0000;
    assert!(!overlaps(0x3F7F_FFF0, 16, &range));
    assert!(overlaps(0x3F7F_FFF0, 17, &range));
    assert!(overlaps(0x3F80_0000, 1, &range));
    assert!(overlaps(0x3FBF_FFFF, 1, &range));
    assert!(!overlaps(0x3FC0_0000, 64, &range));
    assert!(overlaps(0x3F00_0000, 0x0100_0000, &range));
    assert!(!overlaps(0x3F90_0000, 0, &range));
    assert!(!overlaps(usize::MAX - 3, 4, &range));
}

#[test]
fn copies_match_for_every_alignment_and_length() {
    let source = pattern(96);
    for source_offset in 0..5 {
        for destination_offset in 0..5 {
            for length in 0..40 {
                for backward in [false, true] {
                    let mut destination = vec![0xEE; 96];
                    // SAFETY: both ranges are inside their buffers.
                    unsafe {
                        let to = destination.as_mut_ptr().add(8 + destination_offset);
                        let from = source.as_ptr().add(source_offset);
                        if backward {
                            copy_backward(to, from, length);
                        } else {
                            copy_forward(to, from, length);
                        }
                    }
                    let start = 8 + destination_offset;
                    assert_eq!(
                        destination[start..start + length],
                        source[source_offset..source_offset + length]
                    );
                    // Nothing outside the range is written.
                    assert!(destination[..start].iter().all(|byte| *byte == 0xEE));
                    assert!(
                        destination[start + length..]
                            .iter()
                            .all(|byte| *byte == 0xEE)
                    );
                }
            }
        }
    }
}

#[test]
fn overlapping_copies_keep_the_source_bytes() {
    for distance in 1..9 {
        for length in 0..40 {
            // Towards lower addresses: lowest address first.
            let mut buffer = pattern(64);
            let mut expected = buffer.clone();
            expected.copy_within(8 + distance..8 + distance + length, 8);
            // SAFETY: both ranges are inside the buffer.
            unsafe {
                let base = buffer.as_mut_ptr().add(8);
                copy_forward(base, base.add(distance), length);
            }
            assert_eq!(buffer, expected);

            // Towards higher addresses: highest address first.
            let mut buffer = pattern(64);
            let mut expected = buffer.clone();
            expected.copy_within(8..8 + length, 8 + distance);
            // SAFETY: both ranges are inside the buffer.
            unsafe {
                let base = buffer.as_mut_ptr().add(8);
                copy_backward(base.add(distance), base, length);
            }
            assert_eq!(buffer, expected);
        }
    }
}

#[test]
fn fills_match_for_every_alignment_and_length() {
    for offset in 0..5 {
        for length in 0..40 {
            let mut buffer = vec![0xEE; 64];
            // SAFETY: the range is inside the buffer.
            unsafe { fill(buffer.as_mut_ptr().add(8 + offset), 0x5A, length) };
            let start = 8 + offset;
            assert!(buffer[..start].iter().all(|byte| *byte == 0xEE));
            assert!(
                buffer[start..start + length]
                    .iter()
                    .all(|byte| *byte == 0x5A)
            );
            assert!(buffer[start + length..].iter().all(|byte| *byte == 0xEE));
        }
    }
}
