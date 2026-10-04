// openterm — native terminal + editor + ssh. Rust/egui UI, C vt parser,
// Zig grid ops, Go ssh sidecar.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod agent;
mod app;
mod editor;
mod fs_tree;
mod icons;
mod native;
mod pty;
mod settings;
mod ssh;
mod system_info;
mod term;
mod theme;
mod vault;
mod wake;

const APP_ID: &str = "dev.susmic.openterm";
const DEFAULT_WINDOW_SIZE: [f32; 2] = [1000.0, 640.0];
pub(crate) const MIN_WINDOW_SIZE: [f32; 2] = [760.0, 460.0];

fn persisted_inner_size(compact: &str) -> Option<(f32, f32)> {
    let rest = compact.split_once("inner_size_points:Some((x:")?.1;
    let (x, rest) = rest.split_once(",y:")?;
    let y = rest.split_once("))")?.0;
    Some((x.parse().ok()?, y.parse().ok()?))
}

fn strip_legacy_window_preset(data: &str) -> Option<String> {
    let mut removed = false;
    let mut migrated = String::with_capacity(data.len());
    for line in data.lines() {
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        let window_entry = compact.starts_with("\"window\":");
        let legacy = compact.contains("inner_size_points:Some((x:1280.0,y:800.0))")
            || compact.contains("inner_size_points:Some((x:1280,y:800))");
        let too_small = persisted_inner_size(&compact).is_some_and(|(width, height)| {
            !width.is_finite()
                || !height.is_finite()
                || width < MIN_WINDOW_SIZE[0]
                || height < MIN_WINDOW_SIZE[1]
        });
        if window_entry && (legacy || too_small) {
            removed = true;
        } else {
            migrated.push_str(line);
            migrated.push('\n');
        }
    }
    removed.then_some(migrated)
}

fn migrate_legacy_window_state() {
    let Some(dir) = eframe::storage_dir(APP_ID) else {
        return;
    };
    let marker = dir.join("window-geometry-v3");
    if marker.exists() || std::fs::create_dir_all(&dir).is_err() {
        return;
    }

    let state_path = dir.join("app.ron");
    if let Ok(data) = std::fs::read_to_string(&state_path) {
        if let Some(migrated) = strip_legacy_window_preset(&data) {
            if std::fs::write(&state_path, migrated).is_err() {
                return;
            }
        }
    }
    let _ = std::fs::write(marker, b"legacy and undersized window geometry migrated\n");
}

#[cfg(test)]
mod tests {
    use super::strip_legacy_window_preset;

    #[test]
    fn legacy_window_migration_removes_only_1280_by_800_geometry() {
        let old = "{\n  \"egui\": \"memory\",\n  \"window\": \"(fullscreen:false,inner_size_points:Some((x:1280.0,y:800.0)))\",\n}\n";
        let migrated = strip_legacy_window_preset(old).expect("legacy preset should be removed");
        assert!(migrated.contains("\"egui\""));
        assert!(!migrated.contains("\"window\""));

        let current = old.replace("1280.0", "1100.0");
        assert!(strip_legacy_window_preset(&current).is_none());
    }

    #[test]
    fn undersized_window_geometry_is_discarded() {
        let tiny = "{\n  \"window\": \"(fullscreen:false,inner_size_points:Some((x:150.0,y:65.0)))\",\n}\n";
        let migrated = strip_legacy_window_preset(tiny).expect("tiny geometry should be removed");
        assert!(!migrated.contains("\"window\""));

        let valid = tiny.replace("150.0", "900.0").replace("65.0", "600.0");
        assert!(strip_legacy_window_preset(&valid).is_none());
    }
}

fn main() -> eframe::Result<()> {
    env_logger::init();
    migrate_legacy_window_state();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("OpenTerm")
        .with_app_id(APP_ID)
        // These are logical points, not physical pixels. The old 1280x800
        // default became effectively full-screen on 125-150% Windows scaling.
        .with_inner_size(DEFAULT_WINDOW_SIZE)
        .with_min_inner_size(MIN_WINDOW_SIZE)
        .with_clamp_size_to_monitor_size(true)
        .with_maximized(false);

    if cfg!(target_os = "macos") {
        // keep the traffic lights, hide the native title bar
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    } else {
        // we draw our own title bar + window controls
        viewport = viewport.with_decorations(false).with_resizable(true);
    }

    let opts = eframe::NativeOptions {
        viewport,
        vsync: true,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        renderer: eframe::Renderer::Glow,
        persist_window: true,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "OpenTerm",
        opts,
        Box::new(|cc| {
            let settings = settings::Settings::load_or_default();
            let _ = settings.save();
            theme::install(&cc.egui_ctx, settings.theme);
            Ok(Box::new(app::OpenTerm::new(cc, settings)))
        }),
    )
}
