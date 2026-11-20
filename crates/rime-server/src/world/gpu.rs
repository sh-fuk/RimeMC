//! GPU height-field generation (wgpu: Metal / Vulkan / DX12).
//!
//! Only used when a real hardware adapter exists; software rasterisers are
//! rejected because they are slower than the multi-core CPU path.

use std::sync::mpsc;
use std::time::Duration;

use parking_lot::Mutex;
use wgpu::util::DeviceExt;

/// Max chunks per dispatch (keeps buffers small and below dispatch limits).
const MAX_BATCH: usize = 4096;

pub struct GpuHeights {
    pub adapter_name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    // one batch at a time keeps GPU memory bounded
    lock: Mutex<()>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    seed: u32,
    count: u32,
    _pad: [u32; 2],
}

impl GpuHeights {
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .ok()?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rime heights"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_heights.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rime heights"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Some(Self { adapter_name: format!("{} ({:?})", info.name, info.backend), device, queue, pipeline, lock: Mutex::new(()) })
    }

    pub fn heights(&self, coords: &[(i32, i32)], seed: u32) -> Result<Vec<[i32; 256]>, String> {
        let mut out = Vec::with_capacity(coords.len());
        for batch in coords.chunks(MAX_BATCH) {
            out.extend(self.heights_batch(batch, seed)?);
        }
        Ok(out)
    }

    fn heights_batch(&self, coords: &[(i32, i32)], seed: u32) -> Result<Vec<[i32; 256]>, String> {
        let _guard = self.lock.lock();
        let n = coords.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let flat: Vec<i32> = coords.iter().flat_map(|&(x, z)| [x, z]).collect();
        let params = Params { seed, count: n as u32, _pad: [0; 2] };
        let d = &self.device;
        let params_buf = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let coord_buf = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&flat),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (n * 256 * 4) as u64;
        let out_buf = d.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read_buf = d.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = self.pipeline.get_bind_group_layout(0);
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: coord_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: out_buf.as_entire_binding() },
            ],
        });
        let mut enc = d.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            // one workgroup (256 threads) per chunk, folded into 2D to respect the 65535 limit
            let x = (n as u32).min(65535);
            let y = (n as u32).div_ceil(x);
            pass.dispatch_workgroups(x, y, 1);
        }
        enc.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, size);
        self.queue.submit([enc.finish()]);

        let slice = read_buf.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        d.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(10)) }).map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range().map_err(|e| format!("{e:?}"))?;
        let ints: &[i32] = bytemuck::cast_slice(&data[..]);
        let mut out = Vec::with_capacity(n);
        for c in ints.chunks_exact(256) {
            let mut a = [0i32; 256];
            a.copy_from_slice(c);
            out.push(a);
        }
        drop(data);
        read_buf.unmap();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::generator::chunk_heights_cpu;

    /// GPU and CPU must agree bit for bit (skipped when no GPU is present).
    #[test]
    fn gpu_matches_cpu() {
        let Some(g) = GpuHeights::new() else {
            eprintln!("no GPU available, skipping");
            return;
        };
        let coords: Vec<(i32, i32)> = (-20..20).flat_map(|x| (-3..3).map(move |z| (x * 37, z * -91))).collect();
        for seed in [0u32, 1, 0xDEADBEEF] {
            let gpu = g.heights(&coords, seed).unwrap();
            for (i, &(x, z)) in coords.iter().enumerate() {
                assert_eq!(gpu[i], chunk_heights_cpu(x, z, seed), "chunk {x},{z} seed {seed}");
            }
        }
    }
}
