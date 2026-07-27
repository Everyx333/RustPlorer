//! First-run prompt offering to install an external archive tool.
//!
//! # Why this only opens a download page
//!
//! The obvious implementation is to download an installer and run it. This
//! deliberately does not do that. Silently fetching an executable from the
//! internet and launching it with the user's privileges is exactly the
//! behaviour security tooling flags as malware, and it asks the user to trust
//! that RustPlorer verified the download. There is no signature check we could
//! do here that the vendor's own site does not already do better.
//!
//! So the prompt opens the official download page in the default browser. The
//! user sees the URL, the vendor's HTTPS certificate, and the real installer.
//! One extra click, no trust handed to us.
//!
//! RustPlorer works fully without any of this — every format is supported by
//! the built-in Rust implementations. An external tool is a speed and maturity
//! upgrade, not a requirement, and the dialog says so.

use crate::ui::app::RustPlorer;

/// Official download pages. HTTPS, vendor-controlled, no mirrors.
const SEVENZIP_URL: &str = "https://www.7-zip.org/download.html";
const WINRAR_URL: &str = "https://www.win-rar.com/download.html";

/// Which step of the prompt is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunStage {
    /// Not showing.
    Hidden,
    /// "Do you want to install an external archive tool?"
    Ask,
    /// "Which one?"
    Choose,
}

/// Draw the prompt. Returns true if config changed and should be saved.
pub fn draw(app: &mut RustPlorer, ctx: &egui::Context) -> bool {
    if app.first_run_stage == FirstRunStage::Hidden {
        return false;
    }

    let mut changed = false;

    egui::Window::new("External archive tool")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(440.0)
        .show(ctx, |ui| match app.first_run_stage {
            FirstRunStage::Ask => changed |= ask_stage(app, ui),
            FirstRunStage::Choose => changed |= choose_stage(app, ui),
            FirstRunStage::Hidden => {}
        });

    changed
}

fn ask_stage(app: &mut RustPlorer, ui: &mut egui::Ui) -> bool {
    let mut changed = false;

    ui.add_space(6.0);
    ui.label("Do you want to install an external archive tool?");
    ui.add_space(8.0);

    ui.label(
        egui::RichText::new(
            "RustPlorer already reads and writes ZIP, 7z, RAR, TAR, GZ and XZ on its own — \
             nothing extra is required.\n\n\
             Installing 7-Zip or WinRAR is optional. They are faster on very large archives \
             and handle unusual or damaged files more gracefully.",
        )
        .weak(),
    );

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if ui.button("Yes, show me the options").clicked() {
            app.first_run_stage = FirstRunStage::Choose;
        }

        if ui.button("No thanks").clicked() {
            app.first_run_stage = FirstRunStage::Hidden;
            // Record the answer so the prompt never reappears.
            app.config.archive.external_tool_prompted = true;
            changed = true;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Ask me later")
                .on_hover_text("Show this again next time RustPlorer starts")
                .clicked()
            {
                // Deliberately does NOT set `prompted`, so it asks again.
                app.first_run_stage = FirstRunStage::Hidden;
            }
        });
    });

    changed
}

fn choose_stage(app: &mut RustPlorer, ui: &mut egui::Ui) -> bool {
    let mut changed = false;

    ui.add_space(6.0);
    ui.label("Which would you like to install?");
    ui.add_space(4.0);

    ui.label(
        egui::RichText::new(
            "RustPlorer will open the official download page in your browser. \
             It does not download or run anything itself.",
        )
        .weak()
        .small(),
    );

    ui.add_space(12.0);

    let mut chosen: Option<&[&str]> = None;

    ui.vertical(|ui| {
        if ui
            .add_sized([ui.available_width(), 30.0], egui::Button::new("7-Zip"))
            .on_hover_text(
                "Free and open source. Handles the widest range of formats.\nRecommended.",
            )
            .clicked()
        {
            chosen = Some(&[SEVENZIP_URL]);
        }

        ui.add_space(4.0);

        if ui
            .add_sized([ui.available_width(), 30.0], egui::Button::new("WinRAR"))
            .on_hover_text("Commercial, with a trial period. Strongest RAR support.")
            .clicked()
        {
            chosen = Some(&[WINRAR_URL]);
        }

        ui.add_space(4.0);

        if ui
            .add_sized([ui.available_width(), 30.0], egui::Button::new("Both"))
            .on_hover_text("Opens both download pages.")
            .clicked()
        {
            chosen = Some(&[SEVENZIP_URL, WINRAR_URL]);
        }
    });

    if let Some(urls) = chosen {
        for url in urls {
            // Opening a browser can fail on a locked-down system; surface it
            // rather than leaving the user wondering why nothing happened.
            if let Err(e) = open::that_detached(url) {
                tracing::warn!(url, error = %e, "could not open download page");
                app.error_banner = Some(format!(
                    "Could not open your browser. Download it manually from {url}"
                ));
            } else {
                tracing::info!(url, "opened download page");
            }
        }

        app.first_run_stage = FirstRunStage::Hidden;
        app.config.archive.external_tool_prompted = true;
        changed = true;
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            app.first_run_stage = FirstRunStage::Ask;
        }
        if ui.button("Cancel").clicked() {
            app.first_run_stage = FirstRunStage::Hidden;
            app.config.archive.external_tool_prompted = true;
            changed = true;
        }
    });

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_urls_are_official_and_https() {
        for url in [SEVENZIP_URL, WINRAR_URL] {
            assert!(url.starts_with("https://"), "{url} must use HTTPS");
        }
        assert!(SEVENZIP_URL.contains("7-zip.org"));
        assert!(WINRAR_URL.contains("win-rar.com"));
    }

    #[test]
    fn stages_are_distinct() {
        assert_ne!(FirstRunStage::Hidden, FirstRunStage::Ask);
        assert_ne!(FirstRunStage::Ask, FirstRunStage::Choose);
    }
}
