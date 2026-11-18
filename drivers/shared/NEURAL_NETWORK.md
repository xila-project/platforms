# Nanoglia platform backend

Enable `xila-drivers-shared/neural-network` for the portable Rust kernels. Add
`neural-network-cmsis-nn` or `neural-network-esp-nn` for those accelerated backends.
The `neural-network` feature includes all operators by default (`neural-network-all-ops`);
firmware targeting microcontrollers can use `neural-network-core` and select specific
`nanoglia/op-*` operator features to minimize flash footprint.
Runtime assemblies select a `Target` and register a model at their chosen VFS path:

```rust,ignore
use drivers_shared::devices::neural_network::{self as nn, Global, NanogliaModelBuffer, Target};
use embedded_io_async::Read;

// Aligned &'static model bytes in ROM; weights are borrowed, never copied.
nn::mount_rom(vfs, task, "/devices/nn/model", MODEL, Target::Auto).await?;

// Or allocate an aligned model buffer and read a VFS file directly into it.
let mut model = NanogliaModelBuffer::new_zeroed(model_size, Global)?;
model_file.read_exact(model.as_mut_slice()).await?;
nn::mount_owned_model(vfs, task, "/devices/nn/model", model, Target::Cpu).await?;

// Or mount a dynamic runtime loader accepting models up to a maximum size.
nn::mount_loader(vfs, task, "/devices/nn/cpu", Target::Cpu, maximum_model_size).await?;
```

`from_rom`, `from_owned_model`, and `from_loader` construct unmounted devices when the assembly needs
separate registration. Caller-owned `model_size` must be checked against resource limits;
model contents are verified by Nanoglia. The shared immutable graph retains the heap model
until the last owner drops. Xila's Arc implementation also supports targets without pointer
atomics. The backend reports only graphs fitting Core's six-dimension VFS metadata layout.

## Runtime model loading protocol

When a loader device is mounted (for example at `/devices/nn/cpu`), applications dynamically load models:

1. **Open**: opening `/devices/nn/cpu` returns a file descriptor in the *Staging* state.
2. **Write model bytes**: write the raw TensorFlow Lite FlatBuffer sequentially from position 0. Writes must be contiguous; gaps return `InvalidParameter`. Writes exceeding the configured `maximum_model_size` fail with `FileTooLarge`.
3. **Load**: send the `LOAD` control command with a `LoadRequest` (`encoding = 0` for TensorFlow Lite). Nanoglia verifies and instantiates the graph; on success the descriptor transitions to the *Ready* state. Attempting inference commands before `LOAD` or sending a second `LOAD` on a ready handle returns `InvalidContext`.
4. **Inference**: the descriptor now behaves as a standard neural network device. Seek to 0, write inputs, issue `COMPUTE`, seek to 0, and read output tensors.
5. **Close**: closing the descriptor deallocates both the model storage and the execution arena.

### Transient memory peak during loading

Nanoglia's aligned [`Block<Global>`](nanoglia::core::Block) currently provides allocation of a new zeroed block (`new_zeroed`) and mutable slice access (`as_mut_slice`), without an in-place adopt mechanism from foreign allocations. When `NanogliaLoader::load` instantiates the backend from Core's `ModelBuffer`, it allocates a new `Block<Global>`, copies the model bytes into it, and immediately drops the Core buffer. Platforms should account for this transient ~2x peak model buffer memory usage during dynamic loading.

Core owns `neural_network::Backend`, `neural_network::Loader`, the VFS commands and per-open protocol state.
This module owns Nanoglia model loading, dynamic loaders, graph sharing, accelerator configuration, arena
creation, tensor views and error conversion. All backend and execution types are concrete:
there is no extra backend box, vtable, model copy or staging tensor allocation.

For both ROM and heap classifier fixtures, opening allocates exactly three blocks: the execution arena,
input-completeness counters and ordinary VFS context. Subsequent write/compute/read calls
allocate nothing. See `neural_network_allocations` for the counted regression test.
Hardware accelerator performance/correctness must still be checked on the selected target;
host tests execute CMSIS-NN's portable implementation.
