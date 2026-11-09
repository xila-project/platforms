//! Nanoglia model ownership and accelerator selection for platform assemblies.
use file_system::{Error, Result};
use nanoglia::{Encoding, Graph, GraphExecutionContext, GraphHandle, OwnedGraph};
use neural_network::{Backend, QuantizationEntry, TensorInfo};
use neural_network_driver::{MAX_TENSOR_RANK, NeuralNetworkDevice};
use synchronization::Arc;

pub use nanoglia::{Global, Target};
/// Aligned caller-owned storage; a VFS file can be read directly into its mutable slice.
pub type ModelBuffer = nanoglia::core::Block<Global>;
const _: () = assert!(nanoglia::core::MAX_RANK <= MAX_TENSOR_RANK);

/// Shared model owner, using Xila's fallback Arc on targets without pointer atomics.
#[doc(hidden)]
pub struct SharedGraph<G>(Arc<G>);
// SAFETY: the shared owner always lends the same immutable graph and model storage.
unsafe impl<G: GraphHandle> GraphHandle for SharedGraph<G> {
    type Allocator = G::Allocator;
    fn graph(&self) -> &Graph<'_, Self::Allocator> {
        self.0.graph()
    }
}

/// Prepared Nanoglia model; immutable weights and parameters are shared across opens.
pub struct NanogliaBackend<G = Graph<'static, Global>> {
    graph: Arc<G>,
}

impl<G: GraphHandle<Allocator = Global> + Send + Sync + 'static> Backend for NanogliaBackend<G> {
    type Execution = GraphExecutionContext<SharedGraph<G>, Global>;
    type Input<'a> = nanoglia::TensorMut<'a>;
    type Output<'a> = nanoglia::TensorRef<'a>;
    fn create_execution(&self) -> Result<Self::Execution> {
        GraphExecutionContext::new_in(SharedGraph(self.graph.clone()), Global).map_err(map_error)
    }
    fn owns_execution(&self, execution: &Self::Execution) -> bool {
        Arc::ptr_eq(&execution.graph_handle().0, &self.graph)
    }
    fn input_count(&self) -> usize {
        self.graph.graph().input_count()
    }
    fn output_count(&self) -> usize {
        self.graph.graph().output_count()
    }
    fn input_info(&self, index: usize) -> Result<TensorInfo> {
        Ok(tensor_info(
            self.graph.graph().input(index).map_err(map_error)?,
        ))
    }
    fn output_info(&self, index: usize) -> Result<TensorInfo> {
        Ok(tensor_info(
            self.graph.graph().output(index).map_err(map_error)?,
        ))
    }
    fn input_quantization(&self, tensor: usize, entry: usize) -> Result<QuantizationEntry> {
        quantization(
            self.graph.graph().input(tensor).map_err(map_error)?.quant,
            entry,
        )
    }
    fn output_quantization(&self, tensor: usize, entry: usize) -> Result<QuantizationEntry> {
        quantization(
            self.graph.graph().output(tensor).map_err(map_error)?.quant,
            entry,
        )
    }
    fn input<'a>(
        &self,
        execution: &'a mut Self::Execution,
        index: usize,
    ) -> Result<Self::Input<'a>> {
        execution.input_mut(index).map_err(map_error)
    }
    fn output<'a>(&self, execution: &'a Self::Execution, index: usize) -> Result<Self::Output<'a>> {
        execution.output(index).map_err(map_error)
    }
    fn compute(&self, execution: &mut Self::Execution) -> Result<()> {
        execution.compute().map_err(map_error)
    }
}

/// Construct a VFS device borrowing aligned ROM bytes without copying weights.
pub fn from_rom(
    model: &'static [u8],
    target: Target,
) -> Result<NeuralNetworkDevice<NanogliaBackend>> {
    if !(model.as_ptr() as usize).is_multiple_of(nanoglia::core::ALIGN) {
        return Err(Error::InvalidParameter);
    }
    let graph = Graph::load(&[model], Encoding::TensorFlowLite, target).map_err(map_error)?;
    Ok(NeuralNetworkDevice::new(NanogliaBackend {
        graph: Arc::new(graph),
    }))
}

/// Construct a VFS device taking an aligned file/model allocation without copying it.
pub fn from_owned_model(
    model: ModelBuffer,
    target: Target,
) -> Result<NeuralNetworkDevice<NanogliaBackend<OwnedGraph<Global>>>> {
    let graph =
        OwnedGraph::load_in(model, Encoding::TensorFlowLite, target, Global).map_err(map_error)?;
    Ok(NeuralNetworkDevice::new(NanogliaBackend {
        graph: Arc::new(graph),
    }))
}

/// Register a ROM-backed NN node from a platform's runtime assembly.
pub async fn mount_rom(
    vfs: &virtual_file_system::VirtualFileSystem,
    task: task::TaskIdentifier,
    path: impl AsRef<file_system::Path>,
    model: &'static [u8],
    target: Target,
) -> virtual_file_system::Result<()> {
    vfs.mount_character_device(task, path, from_rom(model, target)?)
        .await
}

/// Register an owned-model NN node from a platform's runtime assembly.
pub async fn mount_owned_model(
    vfs: &virtual_file_system::VirtualFileSystem,
    task: task::TaskIdentifier,
    path: impl AsRef<file_system::Path>,
    model: ModelBuffer,
    target: Target,
) -> virtual_file_system::Result<()> {
    vfs.mount_character_device(task, path, from_owned_model(model, target)?)
        .await
}

fn quantization(quant: nanoglia::Quantization<'_>, index: usize) -> Result<QuantizationEntry> {
    Ok(QuantizationEntry {
        scale: quant.scale.get(index).ok_or(Error::InvalidParameter)?,
        zero_point: quant.zero_point.get(index).ok_or(Error::InvalidParameter)?,
    })
}

fn tensor_info(desc: nanoglia::TensorDesc<'_>) -> TensorInfo {
    let mut dimensions = [0; MAX_TENSOR_RANK];
    let dims = desc.dims.as_slice();
    dimensions[..dims.len()].copy_from_slice(dims);
    TensorInfo {
        data_type: desc.dtype as u32,
        rank: dims.len() as u32,
        dimensions,
        byte_len: desc.byte_len().unwrap_or(0).min(u32::MAX as usize) as u32,
        quantization_axis: if desc.quant.is_quantized() {
            desc.quant.axis
        } else {
            -1
        },
        quantization_count: desc.quant.scale.len().min(u32::MAX as usize) as u32,
    }
}

fn map_error(error: nanoglia::Error) -> file_system::Error {
    match error.code {
        nanoglia::ErrorCode::InvalidArgument => file_system::Error::InvalidParameter,
        nanoglia::ErrorCode::InvalidEncoding => file_system::Error::InvalidFile,
        nanoglia::ErrorCode::TooLarge => file_system::Error::NoMemory,
        nanoglia::ErrorCode::NotFound => file_system::Error::NotFound,
        nanoglia::ErrorCode::UnsupportedOperation => file_system::Error::UnsupportedOperation,
        nanoglia::ErrorCode::Security => file_system::Error::PermissionDenied,
        nanoglia::ErrorCode::Timeout => file_system::Error::RessourceBusy,
        nanoglia::ErrorCode::RuntimeError | nanoglia::ErrorCode::Unknown => {
            file_system::Error::InputOutput
        }
    }
}
