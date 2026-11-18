pub const PAGE_SIZE: usize = 4096;
pub const FLASH_SIZE: usize = 1024 * 1024; // 1 MB on nRF52840

/// Validate that the partition start address and length are valid.
/// Both must be multiples of 4096, length > 0, and fit within the internal flash.
pub fn validate_partition_range(
    start_address: usize,
    length: usize,
    flash_size: usize,
) -> Result<(), file_system::Error> {
    if length == 0 || !start_address.is_multiple_of(PAGE_SIZE) || !length.is_multiple_of(PAGE_SIZE)
    {
        return Err(file_system::Error::InvalidParameter);
    }
    let end = start_address
        .checked_add(length)
        .ok_or(file_system::Error::InvalidParameter)?;
    if end > flash_size {
        return Err(file_system::Error::InvalidParameter);
    }
    Ok(())
}

/// Validate that an I/O request falls entirely within the partition.
pub fn validate_access_range(
    position: u64,
    length: usize,
    partition_length: usize,
) -> Result<(), file_system::Error> {
    let end = position
        .checked_add(length as u64)
        .ok_or(file_system::Error::InvalidParameter)?;
    if end > partition_length as u64 {
        return Err(file_system::Error::InvalidParameter);
    }
    Ok(())
}

/// Check if `new_data` can be programmed over `existing` flash bytes without erasing.
/// In NOR flash, programming can only clear bits (change 1 to 0).
/// Every bit that is 1 in `new_data` must already be 1 in `existing`.
pub fn can_program_without_erase(existing: &[u8], new_data: &[u8]) -> bool {
    if existing.len() != new_data.len() {
        return false;
    }
    existing
        .iter()
        .zip(new_data.iter())
        .all(|(&old, &new)| (old & new) == new)
}

/// Merge new data into a 4096-byte RAM page buffer at the given offset within the page.
pub fn merge_page(page_buffer: &mut [u8; PAGE_SIZE], offset_in_page: usize, data: &[u8]) {
    let end = offset_in_page + data.len();
    assert!(end <= PAGE_SIZE);
    page_buffer[offset_in_page..end].copy_from_slice(data);
}

/// Fill `buffer` by repeating `pattern` starting from `pattern_offset`.
pub fn fill_with_pattern(buffer: &mut [u8], pattern: &[u8], mut pattern_offset: usize) {
    if pattern.is_empty() || buffer.is_empty() {
        return;
    }
    for byte in buffer.iter_mut() {
        *byte = pattern[pattern_offset];
        pattern_offset = (pattern_offset + 1) % pattern.len();
    }
}

/// Check if every byte in `data` equals `target`.
pub fn is_all_byte(data: &[u8], target: u8) -> bool {
    data.iter().all(|&b| b == target)
}

/// Breakdown of an unaligned write into 4-byte word-aligned write operations.
/// Unmodified byte lanes in word-boundary writes are padded with `0xFF` (no-op in NOR flash).
#[derive(Debug, PartialEq, Eq)]
pub enum WordAlignedWrite<'a> {
    SingleWord {
        address: usize,
        data: [u8; 4],
    },
    MultiWord {
        head: Option<(usize, [u8; 4])>,
        middle: Option<(usize, &'a [u8])>,
        tail: Option<(usize, [u8; 4])>,
    },
}

/// Plan word-aligned NVMC writes for a potentially unaligned byte slice.
pub fn plan_word_aligned_write<'a>(address: usize, data: &'a [u8]) -> WordAlignedWrite<'a> {
    assert!(!data.is_empty());
    let len = data.len();
    let start_word = address & !3;
    let end_word = (address + len - 1) & !3;

    if start_word == end_word {
        let in_word_offset = address % 4;
        let mut word = [0xFFu8; 4];
        word[in_word_offset..in_word_offset + len].copy_from_slice(data);
        WordAlignedWrite::SingleWord {
            address: start_word,
            data: word,
        }
    } else {
        let lead = address % 4;
        let head = if lead != 0 {
            let head_len = 4 - lead;
            let mut word = [0xFFu8; 4];
            word[lead..4].copy_from_slice(&data[..head_len]);
            Some((start_word, word))
        } else {
            None
        };

        let head_len = if lead != 0 { 4 - lead } else { 0 };
        let remaining = len - head_len;
        let mid_len = (remaining / 4) * 4;
        let mid_addr = if lead != 0 {
            start_word + 4
        } else {
            start_word
        };
        let middle = if mid_len > 0 {
            Some((mid_addr, &data[head_len..head_len + mid_len]))
        } else {
            None
        };

        let tail_len = remaining % 4;
        let tail_addr = mid_addr + mid_len;
        let tail = if tail_len > 0 {
            let mut word = [0xFFu8; 4];
            word[..tail_len].copy_from_slice(&data[head_len + mid_len..]);
            Some((tail_addr, word))
        } else {
            None
        };

        WordAlignedWrite::MultiWord { head, middle, tail }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_partition_range() {
        // Valid partition within 1 MB flash
        assert!(validate_partition_range(0x10000, 0x20000, FLASH_SIZE).is_ok());
        assert!(validate_partition_range(0, FLASH_SIZE, FLASH_SIZE).is_ok());

        // Zero length
        assert_eq!(
            validate_partition_range(0x10000, 0, FLASH_SIZE),
            Err(file_system::Error::InvalidParameter)
        );

        // Unaligned start address
        assert_eq!(
            validate_partition_range(0x10001, 0x1000, FLASH_SIZE),
            Err(file_system::Error::InvalidParameter)
        );

        // Unaligned length
        assert_eq!(
            validate_partition_range(0x10000, 0x1001, FLASH_SIZE),
            Err(file_system::Error::InvalidParameter)
        );

        // Beyond flash size
        assert_eq!(
            validate_partition_range(FLASH_SIZE - 0x1000, 0x2000, FLASH_SIZE),
            Err(file_system::Error::InvalidParameter)
        );

        // Arithmetic overflow
        assert_eq!(
            validate_partition_range(usize::MAX - 0x1000 + 1, 0x1000, FLASH_SIZE),
            Err(file_system::Error::InvalidParameter)
        );
    }

    #[test]
    fn test_validate_access_range() {
        let partition_len = 0x20000; // 128 KB
        assert!(validate_access_range(0, 4096, partition_len).is_ok());
        assert!(validate_access_range(0, partition_len, partition_len).is_ok());
        assert!(validate_access_range(4096, partition_len - 4096, partition_len).is_ok());

        // Out of bounds
        assert_eq!(
            validate_access_range(0, partition_len + 1, partition_len),
            Err(file_system::Error::InvalidParameter)
        );
        assert_eq!(
            validate_access_range(partition_len as u64, 1, partition_len),
            Err(file_system::Error::InvalidParameter)
        );

        // Overflow
        assert_eq!(
            validate_access_range(u64::MAX, 1, partition_len),
            Err(file_system::Error::InvalidParameter)
        );
    }

    #[test]
    fn test_can_program_without_erase() {
        // Erased flash (0xFF) can be programmed with anything
        assert!(can_program_without_erase(
            &[0xFF; 4],
            &[0x00, 0xAA, 0x55, 0xFF]
        ));

        // Writing same byte is always allowed (no bit toggles)
        assert!(can_program_without_erase(&[0x12, 0x34], &[0x12, 0x34]));

        // Clearing bits (1 -> 0) is allowed
        // 0b1111 -> 0b1010
        assert!(can_program_without_erase(&[0x0F], &[0x0A]));

        // Setting bits (0 -> 1) is NOT allowed without erase
        // 0b0000 -> 0b0001
        assert!(!can_program_without_erase(&[0x00], &[0x01]));
        // 0b1010 -> 0b1110 (bit 2 was 0, wants to become 1)
        assert!(!can_program_without_erase(&[0x0A], &[0x0E]));

        // Length mismatch
        assert!(!can_program_without_erase(&[0xFF], &[0xFF, 0xFF]));
    }

    #[test]
    fn test_merge_page() {
        let mut buffer = [0xFFu8; PAGE_SIZE];
        let data = [1, 2, 3, 4];
        merge_page(&mut buffer, 10, &data);

        assert_eq!(&buffer[9..15], &[0xFF, 1, 2, 3, 4, 0xFF]);
        assert_eq!(buffer[0], 0xFF);
        assert_eq!(buffer[PAGE_SIZE - 1], 0xFF);
    }

    #[test]
    fn test_fill_with_pattern() {
        let mut buf = [0u8; 10];
        fill_with_pattern(&mut buf, &[1, 2, 3], 0);
        assert_eq!(&buf, &[1, 2, 3, 1, 2, 3, 1, 2, 3, 1]);

        let mut buf2 = [0u8; 5];
        fill_with_pattern(&mut buf2, &[1, 2, 3], 1);
        assert_eq!(&buf2, &[2, 3, 1, 2, 3]);
    }

    #[test]
    fn test_is_all_byte() {
        assert!(is_all_byte(&[0xFF, 0xFF, 0xFF], 0xFF));
        assert!(is_all_byte(&[0x00; 100], 0x00));
        assert!(!is_all_byte(&[0xFF, 0xFE, 0xFF], 0xFF));
        assert!(is_all_byte(&[], 0xFF));
    }

    #[test]
    fn test_plan_word_aligned_write_single_word() {
        // Aligned single word
        let data = [1, 2, 3, 4];
        assert_eq!(
            plan_word_aligned_write(0x1000, &data),
            WordAlignedWrite::SingleWord {
                address: 0x1000,
                data: [1, 2, 3, 4],
            }
        );

        // Unaligned within word: offset 1, len 2
        let data2 = [0xAA, 0xBB];
        assert_eq!(
            plan_word_aligned_write(0x1001, &data2),
            WordAlignedWrite::SingleWord {
                address: 0x1000,
                data: [0xFF, 0xAA, 0xBB, 0xFF],
            }
        );

        // Unaligned within word: offset 3, len 1
        let data3 = [0x42];
        assert_eq!(
            plan_word_aligned_write(0x1003, &data3),
            WordAlignedWrite::SingleWord {
                address: 0x1000,
                data: [0xFF, 0xFF, 0xFF, 0x42],
            }
        );
    }

    #[test]
    fn test_plan_word_aligned_write_multi_word() {
        // Aligned multi-word: 8 bytes
        let data = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            plan_word_aligned_write(0x1000, &data),
            WordAlignedWrite::MultiWord {
                head: None,
                middle: Some((0x1000, &data)),
                tail: None,
            }
        );

        // Unaligned head only: offset 1, len 7 (1..8)
        let data2 = [1, 2, 3, 4, 5, 6, 7];
        assert_eq!(
            plan_word_aligned_write(0x1001, &data2),
            WordAlignedWrite::MultiWord {
                head: Some((0x1000, [0xFF, 1, 2, 3])),
                middle: Some((0x1004, &[4, 5, 6, 7])),
                tail: None,
            }
        );

        // Unaligned head and tail: offset 1, len 9 (1..10)
        let data3 = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(
            plan_word_aligned_write(0x1001, &data3),
            WordAlignedWrite::MultiWord {
                head: Some((0x1000, [0xFF, 1, 2, 3])),
                middle: Some((0x1004, &[4, 5, 6, 7])),
                tail: Some((0x1008, [8, 9, 0xFF, 0xFF])),
            }
        );

        // Aligned head, unaligned tail: offset 0, len 6
        let data4 = [1, 2, 3, 4, 5, 6];
        assert_eq!(
            plan_word_aligned_write(0x1000, &data4),
            WordAlignedWrite::MultiWord {
                head: None,
                middle: Some((0x1000, &[1, 2, 3, 4])),
                tail: Some((0x1004, [5, 6, 0xFF, 0xFF])),
            }
        );
    }
}
