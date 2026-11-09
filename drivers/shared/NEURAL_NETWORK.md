# Nanoglia platform backend

Enable `xila-drivers-shared/neural-network` for the portable Rust kernels. Add
`neural-network-cmsis-nn` or `neural-network-esp-nn` for those accelerated backends.
Runtime assemblies select a `Target` and register a model at their chosen VFS path:

```rust,ignore
use drivers_shared::devices::neural_network::{self as nn, Global, ModelBuffer, Target};
use embedded_io_async::Read;

// Aligned &'static model bytes in ROM; weights are borrowed, never copied.
nn::mount_rom(vfs, task, "/devices/nn/model", MODEL, Target::Auto).await?;

// Or allocate an aligned model buffer and read a VFS file directly into it.
let mut model = ModelBuffer::new_zeroed(model_size, Global)?;
model_file.read_exact(model.as_mut_slice()).await?;
nn::mount_owned_model(vfs, task, "/devices/nn/model", model, Target::Cpu).await?;
```

`from_rom` and `from_owned_model` construct unmounted devices when the assembly needs
separate registration. Caller-owned `model_size` must be checked against resource limits;
model contents are verified by Nanoglia. The shared immutable graph retains the heap model
until the last owner drops. Xila's Arc implementation also supports targets without pointer
atomics. The backend reports only graphs fitting Core's six-dimension VFS metadata layout.

Core owns `neural_network::Backend`, the VFS commands and per-open protocol state.
This module owns Nanoglia model loading, graph sharing, accelerator configuration, arena
creation, tensor views and error conversion. All backend and execution types are concrete:
there is no extra backend box, vtable, model copy or staging tensor allocation.

For both ROM and heap classifier fixtures, opening allocates exactly three blocks: the execution arena,
input-completeness counters and ordinary VFS context. Subsequent write/compute/read calls
allocate nothing. See `neural_network_allocations` for the counted regression test.
Hardware accelerator performance/correctness must still be checked on the selected target;
host tests execute CMSIS-NN's portable implementation.
