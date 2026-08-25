//! Capture one real ProcedureTab state into a deterministic offscreen texture.
//!
//! Usage from the repository root:
//! `cargo run -p deepseek-custom-tests --example procedure_visual_capture -- <state> <output.png>`

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use deepseek_custom::config::settings::{
    ApiProvider, BackendConfig, ProcedureSettings, RepositoryIndexLimits, Settings,
};
use deepseek_custom::gui::procedure_tab::ProcedureTab;
use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, OpenSpecValidation, ProcedureAttemptDisposition,
    ProcedureProgress, ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition,
};
use eframe::egui;
use egui_wgpu::wgpu;

const CAPTURE_SIZE: [u32; 2] = [1280, 900];
const PIXELS_PER_POINT: f32 = 1.0;

#[derive(Clone, Copy)]
enum CaptureState {
    Running,
    AwaitingReview,
    Approved,
    Rejected,
    Failed,
    Interrupted,
}

impl CaptureState {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "running" => Ok(Self::Running),
            "awaiting-review" => Ok(Self::AwaitingReview),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(format!(
                "unknown state {value:?}; expected running, awaiting-review, approved, rejected, failed, or interrupted"
            )),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::AwaitingReview => "awaiting-review",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    const fn run_id(self) -> &'static str {
        match self {
            Self::Running => "00000000-0000-4000-8000-000000000001",
            Self::AwaitingReview => "00000000-0000-4000-8000-000000000002",
            Self::Approved => "00000000-0000-4000-8000-000000000003",
            Self::Rejected => "00000000-0000-4000-8000-000000000004",
            Self::Failed => "00000000-0000-4000-8000-000000000005",
            Self::Interrupted => "00000000-0000-4000-8000-000000000006",
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let state = CaptureState::parse(
        &arguments
            .next()
            .ok_or("missing required Procedure state argument")?,
    )?;
    let output = PathBuf::from(
        arguments
            .next()
            .ok_or("missing required PNG output path argument")?,
    );
    if arguments.next().is_some() {
        return Err("expected exactly two arguments: <state> <output.png>".into());
    }

    let repository = std::env::current_dir()?;
    let project_root = repository
        .join("target")
        .join("procedure-visual-capture")
        .join(state.name());
    prepare_fixture(&project_root)?;
    let mut settings = fixture_settings();
    let mut tab = tab_for_state(state, &project_root, &settings)?;
    render_offscreen(&mut tab, &mut settings, &project_root, &output)?;
    std::fs::remove_dir_all(project_root).ok();
    Ok(())
}

fn render_offscreen(
    tab: &mut ProcedureTab,
    settings: &mut Settings,
    project_root: &Path,
    output_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let context = egui::Context::default();
    context.set_pixels_per_point(PIXELS_PER_POINT);
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(CAPTURE_SIZE[0] as f32, CAPTURE_SIZE[1] as f32),
        )),
        time: Some(1.0),
        ..Default::default()
    };
    let full_output = context.run(raw_input, |context| {
        egui::CentralPanel::default().show(context, |ui| {
            tab.render_for_test(ui, settings, project_root);
        });
    });
    let clear = context.style().visuals.panel_fill.to_normalized_gamma_f32();
    let paint_jobs = context.tessellate(full_output.shapes, full_output.pixels_per_point);

    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = runtime
        .block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| {
            std::io::Error::other(
                "no headless wgpu adapter is available for offscreen Procedure capture",
            )
        })?;
    eprintln!("offscreen adapter: {:?}", adapter.get_info());
    let (device, queue) = runtime.block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("procedure visual capture device"),
            ..Default::default()
        },
        None,
    ))?;

    let texture_format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("procedure visual capture texture"),
        size: wgpu::Extent3d {
            width: CAPTURE_SIZE[0],
            height: CAPTURE_SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: texture_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let unpadded_bytes_per_row = CAPTURE_SIZE[0] * 4;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("procedure visual capture readback"),
        size: (padded_bytes_per_row * CAPTURE_SIZE[1]) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: CAPTURE_SIZE,
        pixels_per_point: PIXELS_PER_POINT,
    };
    let mut renderer = egui_wgpu::Renderer::new(&device, texture_format, None, 1, false);
    for (id, delta) in &full_output.textures_delta.set {
        renderer.update_texture(&device, &queue, *id, delta);
    }

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("procedure visual capture encoder"),
    });
    let user_commands =
        renderer.update_buffers(&device, &queue, &mut encoder, &paint_jobs, &screen);
    {
        let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("procedure visual capture render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0] as f64,
                        g: clear[1] as f64,
                        b: clear[2] as f64,
                        a: clear[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.render(&mut render_pass.forget_lifetime(), &paint_jobs, &screen);
    }
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(CAPTURE_SIZE[1]),
            },
        },
        texture.size(),
    );
    queue.submit(user_commands.into_iter().chain([encoder.finish()]));
    for id in &full_output.textures_delta.free {
        renderer.free_texture(id);
    }

    let readback_slice = readback.slice(..);
    let (map_tx, map_rx) = std::sync::mpsc::channel();
    readback_slice.map_async(wgpu::MapMode::Read, move |result| {
        map_tx.send(result).ok();
    });
    device.poll(wgpu::Maintain::Wait);
    map_rx
        .recv()
        .map_err(|_| std::io::Error::other("wgpu readback callback was dropped"))??;
    let mapped = readback_slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((CAPTURE_SIZE[0] * CAPTURE_SIZE[1] * 4) as usize);
    for row in mapped.chunks(padded_bytes_per_row as usize) {
        rgba.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();

    let output_parent = output_path
        .parent()
        .ok_or("PNG output path must have a parent directory")?;
    std::fs::create_dir_all(output_parent)?;
    image::save_buffer(
        output_path,
        &rgba,
        CAPTURE_SIZE[0],
        CAPTURE_SIZE[1],
        image::ColorType::Rgba8,
    )?;
    Ok(())
}

fn prepare_fixture(project_root: &Path) -> Result<(), Box<dyn Error>> {
    let change = project_root.join("openspec/changes/harden-procedure-localization");
    std::fs::create_dir_all(&change)?;
    std::fs::write(
        change.join("tasks.md"),
        "## Tasks\n\n- [x] 4.0 Complete\n- [ ] 4.1 Review localization evidence\n",
    )?;
    Ok(())
}

fn fixture_settings() -> Settings {
    let mut backends = HashMap::new();
    backends.insert(
        "ollama-local".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::Ollama,
            model: "qwen2.5-coder:7b".to_string(),
            base_url: None,
            api_key: None,
            models: None,
        },
    );
    Settings {
        backends: Some(backends),
        procedure: Some(ProcedureSettings {
            localization_backend: Some("ollama-local".to_string()),
            local_patch_backend: Some("ollama-local".to_string()),
            frontier_patch_backend: None,
            repository_index: RepositoryIndexLimits::default(),
            verifier_commands: Vec::new(),
        }),
        ..Settings::default()
    }
}

fn tab_for_state(
    state: CaptureState,
    project_root: &Path,
    settings: &Settings,
) -> Result<ProcedureTab, Box<dyn Error>> {
    let mut tab = ProcedureTab::new(settings, project_root);
    let run_id: ProcedureRunId = serde_json::from_str(&format!("\"{}\"", state.run_id()))?;
    if matches!(state, CaptureState::Running) {
        tab.handle_progress(ProcedureProgress::RunStarted {
            run_id,
            change_id: "harden-procedure-localization".to_string(),
            task_id: "4.1".to_string(),
        });
        tab.handle_progress(ProcedureProgress::AttemptStarted {
            run_id,
            number: 1,
            backend: "ollama-local".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
        });
        return Ok(tab);
    }

    let report = report_for_state(state, run_id);
    let disposition = report
        .terminal_disposition
        .clone()
        .expect("visual fixture is terminal");
    ProcedureReportStore::for_project(project_root).save(&report)?;
    tab.handle_progress(ProcedureProgress::RunStarted {
        run_id,
        change_id: "harden-procedure-localization".to_string(),
        task_id: "4.1".to_string(),
    });
    tab.handle_progress(ProcedureProgress::AttemptStarted {
        run_id,
        number: 1,
        backend: "ollama-local".to_string(),
        model: "qwen2.5-coder:7b".to_string(),
    });
    tab.handle_progress(ProcedureProgress::RunFinished {
        run_id,
        disposition,
    });
    Ok(tab)
}

fn report_for_state(state: CaptureState, id: ProcedureRunId) -> ProcedureRun {
    let (review_disposition, terminal_disposition, attempt_disposition, validation_error) =
        match state {
            CaptureState::AwaitingReview => (
                ProcedureReviewDisposition::Pending,
                ProcedureTerminalDisposition::AwaitingReview,
                ProcedureAttemptDisposition::Accepted,
                None,
            ),
            CaptureState::Approved => (
                ProcedureReviewDisposition::Approved,
                ProcedureTerminalDisposition::AwaitingReview,
                ProcedureAttemptDisposition::Accepted,
                None,
            ),
            CaptureState::Rejected => (
                ProcedureReviewDisposition::Rejected,
                ProcedureTerminalDisposition::AwaitingReview,
                ProcedureAttemptDisposition::Accepted,
                None,
            ),
            CaptureState::Failed => (
                ProcedureReviewDisposition::Pending,
                ProcedureTerminalDisposition::Failed {
                    reason: "returned symbol missing_symbol is absent from the repository index"
                        .to_string(),
                },
                ProcedureAttemptDisposition::Rejected,
                Some("src/procedure/runner.rs::missing_symbol does not exist".to_string()),
            ),
            CaptureState::Interrupted => (
                ProcedureReviewDisposition::Pending,
                ProcedureTerminalDisposition::Interrupted,
                ProcedureAttemptDisposition::Interrupted,
                Some("localization stopped by the user".to_string()),
            ),
            CaptureState::Running => unreachable!("running has no terminal report"),
        };
    let accepted = attempt_disposition == ProcedureAttemptDisposition::Accepted;
    ProcedureRun {
        id,
        change_id: "harden-procedure-localization".to_string(),
        selected_task: ProcedureTask {
            id: "4.1".to_string(),
            text: "Render Procedure review states".to_string(),
            covers: Some("Localization is observable and non-mutating / visual QA".to_string()),
        },
        spec_fingerprint: Some("visual-fixture-spec".to_string()),
        repository_fingerprint: Some("visual-fixture-repository".to_string()),
        validation: Some(OpenSpecValidation {
            command: vec![
                "openspec".to_string(),
                "validate".to_string(),
                "harden-procedure-localization".to_string(),
                "--strict".to_string(),
            ],
            exit_code: Some(0),
            stdout: "Change 'harden-procedure-localization' is valid".to_string(),
            stderr: String::new(),
        }),
        scratchpad: ProcedureScratchpad::default(),
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "ollama-local".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            disposition: attempt_disposition,
            targets: if accepted {
                vec![
                        LocalizationTarget {
                            path: "crates/deepseek-custom/src/gui/procedure_tab.rs".to_string(),
                            symbol: Some("ProcedureTab::render_result".to_string()),
                            evidence: "This method renders proposed paths, optional symbols, evidence, and review controls.".to_string(),
                        },
                        LocalizationTarget {
                            path: "crates/deepseek-custom/src/procedure/report.rs".to_string(),
                            symbol: None,
                            evidence: "This module persists the run-scoped review disposition without modifying source files.".to_string(),
                        },
                    ]
            } else {
                Vec::new()
            },
            validation_error,
        }],
        review_disposition,
        terminal_disposition: Some(terminal_disposition),
    }
}
