/* Keep at least the stack capacity of the known-good Wasm-only firmware. */
ASSERT(_stack_start - _stack_end >= 40 * 1024, "WT32-SC01 Plus requires a 40 KiB main stack");
