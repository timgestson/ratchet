use crate::gpu::{GPUBuffer, StaticResourcePoolAccessor, WgpuDevice};
use crate::CompiledOp;
use derive_new::new;
use wgpu::SubmissionIndex;

/// # Executable
///
/// A linear sequence of compiled operations, with a single uniform buffer
/// containing metadata for all operations.
#[derive(new)]
pub struct Executable {
    steps: Vec<CompiledOp>,
    _uniform_buffer: GPUBuffer, //Keep alive until drop
    uniform_group: wgpu::BindGroup,
}

impl Executable {
    #[cfg(not(feature = "gpu-profiling"))]
    pub fn dispatch_operations(&self, device: &WgpuDevice) -> SubmissionIndex {
        let pipeline_resources = device.pipeline_resources();
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            for step in self.steps.iter() {
                cpass.set_pipeline(pipeline_resources.get(step.pipeline_handle()).unwrap());

                for (index, bind_group) in step.storage_groups().iter().enumerate() {
                    cpass.set_bind_group(index as u32, bind_group, &[]);
                }

                let uniform_group_index = step.storage_groups().len() as u32;
                cpass.set_bind_group(uniform_group_index, &self.uniform_group, &[step.offset()]);

                let [x_count, y_count, z_count] = step.workgroup_count().as_slice();
                cpass.dispatch_workgroups(x_count, y_count, z_count);
            }
        }
        device.queue().submit(Some(encoder.finish()))
    }
}
