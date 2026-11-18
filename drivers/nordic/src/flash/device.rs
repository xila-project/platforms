use core::cmp::min;
use core::task::Poll;
use embassy_nrf::Peri;
use embassy_nrf::nvmc::Nvmc;
use embassy_nrf::peripherals::NVMC;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectBlockDevice, Error,
    MountOperations, Result, Size,
    block_device::{ERASE, GET_BLOCK_COUNT, GET_BLOCK_SIZE},
    mount::MutexMountWrapper,
};
use shared::AnyByLayout;
use synchronization::blocking_mutex::raw::CriticalSectionRawMutex;

use super::logic::{
    FLASH_SIZE, PAGE_SIZE, WordAlignedWrite, can_program_without_erase, fill_with_pattern,
    is_all_byte, merge_page, plan_word_aligned_write, validate_access_range,
    validate_partition_range,
};

struct FlashInner {
    nvmc: Nvmc<'static>,
    page_buffer: [u8; PAGE_SIZE],
}

fn write_nvmc_word_aligned(nvmc: &mut Nvmc<'static>, address: usize, data: &[u8]) -> Result<()> {
    match plan_word_aligned_write(address, data) {
        WordAlignedWrite::SingleWord { address, data } => {
            nvmc.write(address as u32, &data)
                .map_err(|_| Error::InputOutput)?;
        }
        WordAlignedWrite::MultiWord { head, middle, tail } => {
            if let Some((addr, data)) = head {
                nvmc.write(addr as u32, &data)
                    .map_err(|_| Error::InputOutput)?;
            }
            if let Some((addr, data)) = middle {
                nvmc.write(addr as u32, data)
                    .map_err(|_| Error::InputOutput)?;
            }
            if let Some((addr, data)) = tail {
                nvmc.write(addr as u32, &data)
                    .map_err(|_| Error::InputOutput)?;
            }
        }
    }
    Ok(())
}

/// Internal flash block device for Nordic nRF52840 backed by the NVMC controller.
///
/// Provides truthful rewritable disk semantics over raw NOR flash.
pub struct InternalFlashDevice {
    start_address: usize,
    length: usize,
    inner: MutexMountWrapper<CriticalSectionRawMutex, FlashInner>,
}

impl InternalFlashDevice {
    /// Create a new `InternalFlashDevice` for the given partition range.
    ///
    /// Both `start_address` and `length` must be multiples of the 4096-byte page size,
    /// and the range must reside within the chip's internal flash.
    pub fn new(nvmc: Peri<'static, NVMC>, start_address: usize, length: usize) -> Result<Self> {
        validate_partition_range(start_address, length, FLASH_SIZE)?;
        Ok(Self {
            start_address,
            length,
            inner: MutexMountWrapper::new_mounted(FlashInner {
                nvmc: Nvmc::new(nvmc),
                page_buffer: [0xFFu8; PAGE_SIZE],
            }),
        })
    }
}

impl DirectBaseOperations for InternalFlashDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        absolute_position: Size,
    ) -> Poll<Result<usize>> {
        file_system::ready(|| {
            validate_access_range(absolute_position, buffer.len(), self.length)?;
            if buffer.is_empty() {
                return Ok(0);
            }
            let mut inner = self.inner.try_get()?;
            let flash_addr = (self.start_address as u64 + absolute_position) as u32;
            inner
                .nvmc
                .read(flash_addr, buffer)
                .map_err(|_| Error::InputOutput)?;
            Ok(buffer.len())
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        absolute_position: Size,
    ) -> Poll<Result<usize>> {
        file_system::ready(|| {
            validate_access_range(absolute_position, buffer.len(), self.length)?;
            if buffer.is_empty() {
                return Ok(0);
            }

            let mut inner_guard = self.inner.try_get()?;
            let FlashInner { nvmc, page_buffer } = &mut *inner_guard;
            let mut written = 0;
            let mut current_pos = absolute_position;
            let mut remaining = buffer.len();

            while remaining > 0 {
                let page_idx = (current_pos as usize) / PAGE_SIZE;
                let page_start_rel = page_idx * PAGE_SIZE;
                let offset_in_page = (current_pos as usize) - page_start_rel;
                let page_len = min(remaining, PAGE_SIZE - offset_in_page);
                let page_flash_addr = self.start_address + page_start_rel;
                let slice = &buffer[written..written + page_len];

                if offset_in_page == 0 && page_len == PAGE_SIZE {
                    nvmc.read(page_flash_addr as u32, page_buffer)
                        .map_err(|_| Error::InputOutput)?;

                    if page_buffer == slice {
                        // Already identical; no-op.
                    } else if can_program_without_erase(page_buffer, slice) {
                        nvmc.write(page_flash_addr as u32, slice)
                            .map_err(|_| Error::InputOutput)?;
                    } else {
                        nvmc.erase(page_flash_addr as u32, (page_flash_addr + PAGE_SIZE) as u32)
                            .map_err(|_| Error::InputOutput)?;
                        if !is_all_byte(slice, 0xFF) {
                            nvmc.write(page_flash_addr as u32, slice)
                                .map_err(|_| Error::InputOutput)?;
                        }
                    }
                } else {
                    nvmc.read(page_flash_addr as u32, page_buffer)
                        .map_err(|_| Error::InputOutput)?;

                    let existing_target = &page_buffer[offset_in_page..offset_in_page + page_len];
                    if existing_target == slice {
                        // Already identical; no-op.
                    } else if can_program_without_erase(existing_target, slice) {
                        write_nvmc_word_aligned(nvmc, page_flash_addr + offset_in_page, slice)?;
                    } else {
                        merge_page(page_buffer, offset_in_page, slice);
                        nvmc.erase(page_flash_addr as u32, (page_flash_addr + PAGE_SIZE) as u32)
                            .map_err(|_| Error::InputOutput)?;
                        nvmc.write(page_flash_addr as u32, page_buffer)
                            .map_err(|_| Error::InputOutput)?;
                    }
                }

                written += page_len;
                current_pos += page_len as u64;
                remaining -= page_len;
            }

            Ok(buffer.len())
        })
    }

    fn write_pattern(
        &self,
        _cx: &mut core::task::Context<'_>,
        pattern: &[u8],
        count: usize,
        absolute_position: Size,
    ) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let total_bytes = pattern
                .len()
                .checked_mul(count)
                .ok_or(Error::InvalidParameter)?;
            validate_access_range(absolute_position, total_bytes, self.length)?;
            if total_bytes == 0 {
                return Ok(0);
            }

            let mut inner_guard = self.inner.try_get()?;
            let FlashInner { nvmc, page_buffer } = &mut *inner_guard;
            let mut current_pos = absolute_position;
            let mut remaining = total_bytes;

            while remaining > 0 {
                let page_idx = (current_pos as usize) / PAGE_SIZE;
                let page_start_rel = page_idx * PAGE_SIZE;
                let offset_in_page = (current_pos as usize) - page_start_rel;
                let page_len = min(remaining, PAGE_SIZE - offset_in_page);
                let page_flash_addr = self.start_address + page_start_rel;
                let pattern_offset = ((current_pos - absolute_position) as usize) % pattern.len();

                // Whole-page fast path
                if offset_in_page == 0 && page_len == PAGE_SIZE {
                    if is_all_byte(pattern, 0xFF) {
                        nvmc.read(page_flash_addr as u32, page_buffer)
                            .map_err(|_| Error::InputOutput)?;
                        if !is_all_byte(page_buffer, 0xFF) {
                            nvmc.erase(
                                page_flash_addr as u32,
                                (page_flash_addr + PAGE_SIZE) as u32,
                            )
                            .map_err(|_| Error::InputOutput)?;
                        }
                    } else if is_all_byte(pattern, 0x00) {
                        page_buffer.fill(0x00);
                        nvmc.write(page_flash_addr as u32, page_buffer)
                            .map_err(|_| Error::InputOutput)?;
                    } else {
                        fill_with_pattern(page_buffer, pattern, pattern_offset);
                        let can_program = {
                            let flash_slice = unsafe {
                                core::slice::from_raw_parts(page_flash_addr as *const u8, PAGE_SIZE)
                            };
                            can_program_without_erase(flash_slice, page_buffer)
                        };

                        if can_program {
                            nvmc.write(page_flash_addr as u32, page_buffer)
                                .map_err(|_| Error::InputOutput)?;
                        } else {
                            nvmc.erase(
                                page_flash_addr as u32,
                                (page_flash_addr + PAGE_SIZE) as u32,
                            )
                            .map_err(|_| Error::InputOutput)?;
                            if !is_all_byte(page_buffer, 0xFF) {
                                nvmc.write(page_flash_addr as u32, page_buffer)
                                    .map_err(|_| Error::InputOutput)?;
                            }
                        }
                    }
                } else {
                    // Partial page write_pattern
                    nvmc.read(page_flash_addr as u32, page_buffer)
                        .map_err(|_| Error::InputOutput)?;

                    let can_program = (0..page_len).all(|i| {
                        let new_byte = pattern[(pattern_offset + i) % pattern.len()];
                        let old_byte = page_buffer[offset_in_page + i];
                        (old_byte & new_byte) == new_byte
                    });

                    if can_program {
                        let start_addr = page_flash_addr + offset_in_page;
                        let start_word = start_addr & !3;
                        let end_word = (start_addr + page_len + 3) & !3;
                        for w_addr in (start_word..end_word).step_by(4) {
                            let mut word = [0xFFu8; 4];
                            for (byte_idx, byte) in word.iter_mut().enumerate() {
                                let curr_addr = w_addr + byte_idx;
                                if curr_addr >= start_addr && curr_addr < start_addr + page_len {
                                    let offset_in_write = curr_addr - start_addr;
                                    *byte =
                                        pattern[(pattern_offset + offset_in_write) % pattern.len()];
                                }
                            }
                            nvmc.write(w_addr as u32, &word)
                                .map_err(|_| Error::InputOutput)?;
                        }
                    } else {
                        fill_with_pattern(
                            &mut page_buffer[offset_in_page..offset_in_page + page_len],
                            pattern,
                            pattern_offset,
                        );
                        nvmc.erase(page_flash_addr as u32, (page_flash_addr + PAGE_SIZE) as u32)
                            .map_err(|_| Error::InputOutput)?;
                        nvmc.write(page_flash_addr as u32, page_buffer)
                            .map_err(|_| Error::InputOutput)?;
                    }
                }

                current_pos += page_len as u64;
                remaining -= page_len;
            }

            Ok(total_bytes)
        })
    }

    fn flush(&self, _cx: &mut core::task::Context<'_>) -> Poll<Result<()>> {
        file_system::ready(|| {
            // Check that the resource is not busy.
            let _ = self.inner.try_get()?;
            Ok(())
        })
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        file_system::ready(|| match command {
            ERASE::IDENTIFIER => {
                let range = ERASE::cast_input(input)?;
                let (position, size) = (range.position as usize, range.size as usize);
                validate_access_range(range.position, size, self.length)?;
                // Erasing part of a page would destroy the rest of it.
                if !position.is_multiple_of(PAGE_SIZE) || !size.is_multiple_of(PAGE_SIZE) {
                    return Err(Error::InvalidParameter);
                }
                let start = (self.start_address + position) as u32;
                self.inner
                    .try_get()?
                    .nvmc
                    .erase(start, start + size as u32)
                    .map_err(|_| Error::InputOutput)
            }
            GET_BLOCK_SIZE::IDENTIFIER => {
                *GET_BLOCK_SIZE::cast_output(output)? = PAGE_SIZE as u32;
                Ok(())
            }
            GET_BLOCK_COUNT::IDENTIFIER => {
                *GET_BLOCK_COUNT::cast_output(output)? = (self.length / PAGE_SIZE) as u32;
                Ok(())
            }
            _ => Err(Error::UnsupportedOperation),
        })
    }
}

impl MountOperations for InternalFlashDevice {
    fn unmount(&self) -> Result<()> {
        self.inner.unmount()
    }
}

impl DirectBlockDevice for InternalFlashDevice {}
