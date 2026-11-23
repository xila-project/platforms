use core::alloc::Layout;

pub(super) const ALIGNMENT: usize = 8;

pub(super) fn layout(size: usize) -> Option<Layout> {
    let padded = size.checked_add(ALIGNMENT)?.checked_add(ALIGNMENT - 1)?;
    Layout::from_size_align(padded & !(ALIGNMENT - 1), ALIGNMENT).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_payloads_include_an_aligned_header_and_tail() {
        for (size, expected) in [(0, 8), (1, 16), (7, 16), (8, 16), (9, 24), (101, 112)] {
            let layout = layout(size).unwrap();
            assert_eq!(layout.size(), expected);
            assert_eq!(layout.align(), ALIGNMENT);
        }
    }

    #[test]
    fn rejects_overflow_and_invalid_layout_sizes() {
        assert!(layout(usize::MAX).is_none());
        assert!(layout(usize::MAX - ALIGNMENT).is_none());
        assert!(layout(isize::MAX as usize).is_none());
    }
}
