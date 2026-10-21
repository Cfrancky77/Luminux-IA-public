//! Barre de menus (Fichier, Édition, Affichage, Bibliothèque, Accessibilité, Aide), fenêtres
//! « Raccourcis clavier » et « À propos », réglages d'accessibilité.

use super::*;

/// Tailles proposées pour l'interface.
const ZOOMS: [f32; 4] = [1.0, 1.25, 1.5, 2.0];

/// Raccourcis clavier : (touches, action).
const KEYS: [(&str, &str); 27] = [
    (k!("Ctrl+N"), k!("Nouveau document")),
    (k!("Ctrl+O"), k!("Ouvrir une photo")),
    (k!("Ctrl+E"), k!("Exporter")),
    (k!("Ctrl+Z"), k!("Annuler|défaire")),
    (k!("Ctrl+Maj+Z"), k!("Rétablir")),
    (k!("Ctrl+Maj+C"), k!("Copier les réglages")),
    (k!("Ctrl+Maj+V"), k!("Coller les réglages")),
    (k!("Ctrl+,"), k!("Préférences")),
    (k!("Ctrl+0"), k!("Ajuster à la fenêtre")),
    (k!("Ctrl+1"), k!("Zoom 100 %")),
    (k!("Ctrl+plus / Ctrl+moins"), k!("Taille de l'interface")),
    (k!("Ctrl+F"), k!("Rechercher dans la bibliothèque")),
    (k!("F1"), k!("Raccourcis clavier")),
    (k!("F11"), k!("Plein écran")),
    (k!("Ctrl+Q"), k!("Quitter")),
    (k!("G"), k!("Retour à la bibliothèque")),
    (k!("Échap"), k!("Fermer la fenêtre ouverte, sinon retour à la bibliothèque")),
    (k!("Flèches"), k!("Photo précédente ou suivante")),
    (k!("Entrée"), k!("Ouvrir la photo choisie (bibliothèque)")),
    (k!("0 à 5"), k!("Note de la photo")),
    (k!("I"), k!("Informations de la photo (bibliothèque)")),
    (k!("H"), k!("Historique")),
    (k!("P"), k!("Masquer ou afficher le panneau des réglages")),
    (k!("Suppr"), k!("Mettre à la corbeille (bibliothèque)")),
    (k!("B / Y"), k!("Avant / Avant-Après")),
    (k!("C / M"), k!("Recadrer / Afficher le masque")),
    (k!("Tab, Maj+Tab, Espace"), k!("Passer d'un bouton à l'autre, l'activer")),
];

/// Élément de menu avec son raccourci affiché ; ferme le menu au clic.
fn item(ui: &mut egui::Ui, enabled: bool, label: &str, keys: &str) -> bool {
    let r = ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys));
    r.clicked()
}

impl App {
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(t!("Fichier"), |ui| self.file_menu(ui, &ctx));
            ui.menu_button(t!("Édition|menu"), |ui| self.edit_menu(ui));
            ui.menu_button(t!("Affichage"), |ui| self.view_menu(ui, &ctx));
            ui.menu_button(t!("Bibliothèque"), |ui| self.library_menu(ui, &ctx));
            ui.menu_button(t!("Accessibilité"), |ui| self.access_menu(ui, &ctx));
            ui.menu_button(t!("Aide"), |ui| {
                if item(ui, true, t!("Raccourcis clavier…"), "F1") {
                    self.keys_open = true;
                }
                ui.hyperlink_to(t!("Mode d'emploi (en ligne)"), "https://github.com/Cfrancky77/Luminux-IA-public/blob/main/LISEZMOI.md");
                ui.hyperlink_to(t!("Journal des versions (en ligne)"), "https://github.com/Cfrancky77/Luminux-IA-public/blob/main/CHANGELOG.md");
                ui.separator();
                if item(ui, true, t!("À propos de Luminux IA…"), "") {
                    self.about_open = true;
                }
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if item(ui, true, t!("Nouveau…"), "Ctrl+N") {
            self.open_new_doc();
        }
        if item(ui, true, t!("Ouvrir…"), "Ctrl+O") {
            self.pick_open(ctx);
        }
        if item(ui, true, t!("Ajouter un dossier…"), "") {
            self.add_folder(ctx);
        }
        if item(ui, true, t!("Importer des looks et presets…"), "") {
            self.dialog(ctx, || rfd::FileDialog::new().add_filter(t!("Looks et presets"), LOOK_EXTS).pick_files().map(Picked::Import));
        }
        ui.separator();
        let can_export = if self.mode == Mode::Library { !self.targets().is_empty() } else { self.size.is_some() };
        let label = if self.mode == Mode::Library { tf!("Exporter ({n})…", self.targets().len()) } else { t!("Exporter…").to_string() };
        if item(ui, can_export && self.batch.is_none(), &label, "Ctrl+E") {
            self.export_open = true;
        }
        ui.separator();
        if item(ui, true, t!("Quitter"), "Ctrl+Q") {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        let editor = self.mode == Mode::Editor && self.size.is_some();
        if item(ui, editor && self.hist_pos > 0, t!("Annuler|défaire"), "Ctrl+Z") {
            self.undo();
        }
        if item(ui, editor && self.hist_pos + 1 < self.history.len(), t!("Rétablir"), "Ctrl+Maj+Z") {
            self.redo();
        }
        ui.separator();
        let has = if self.mode == Mode::Library { !self.targets().is_empty() } else { self.size.is_some() };
        if item(ui, has, t!("Copier les réglages"), "Ctrl+Maj+C") {
            self.copy_settings();
        }
        if item(ui, has && self.clipboard.is_some(), t!("Coller les réglages"), "Ctrl+Maj+V") {
            self.paste_settings();
        }
        if item(ui, editor, t!("Réglages automatiques"), "") {
            self.auto_enhance();
            self.edit_label = Some(t!("Auto").into());
        }
        if item(ui, editor, t!("Réinitialiser tout"), "") {
            self.adj = Adjustments::default();
            self.edit_label = Some(t!("Réinitialiser tout").into());
        }
        ui.separator();
        if item(ui, editor, t!("Historique"), "H") {
            self.toggle_history();
        }
        if item(ui, true, t!("Préférences…"), "Ctrl+,") {
            self.prefs_open = true;
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui.add(egui::Button::selectable(self.mode == Mode::Library, t!("Bibliothèque")).shortcut_text("G")).clicked() {
            self.leave_photo();
            self.mode = Mode::Library;
            self.scroll_to_sel = true;
        }
        if ui.add_enabled(self.size.is_some(), egui::Button::selectable(self.mode == Mode::Editor, t!("Édition"))).clicked() {
            self.mode = Mode::Editor;
        }
        ui.separator();
        let editor = self.mode == Mode::Editor && self.size.is_some();
        ui.add_enabled_ui(editor, |ui| {
            ui.add(egui::Button::selectable(self.before, t!("Avant")).shortcut_text("B")).clicked().then(|| self.before = !self.before);
            ui.add(egui::Button::selectable(self.split_view, t!("Avant/Après")).shortcut_text("Y")).clicked().then(|| self.split_view = !self.split_view);
            if item(ui, true, t!("Ajuster à la fenêtre"), "Ctrl+0") {
                self.fit = true;
            }
            if item(ui, true, t!("Zoom 100 %"), "Ctrl+1") {
                self.fit = false;
                self.zoom = 1.0;
            }
            if ui.add(egui::Button::selectable(self.panel_hidden, t!("Masquer le panneau Réglages")).shortcut_text("P")).clicked() {
                self.toggle_panel();
                ui.close();
            }
        });
        ui.separator();
        if ui.checkbox(&mut self.settings.split_layout, t!("Réglages à part")).changed() {
            self.settings.save();
        }
        if ui.checkbox(&mut self.settings.panel_left, t!("Barre d'outils à gauche")).changed() {
            self.settings.save();
        }
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if ui.add(egui::Button::selectable(full, t!("Plein écran")).shortcut_text("F11")).clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
    }

    fn library_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if item(ui, true, t!("Rechercher…"), "Ctrl+F") {
            self.open_search();
        }
        if item(ui, self.scanning.is_none(), t!("Actualiser"), "") {
            self.start_scan(ctx);
        }
        ui.separator();
        let (old_sort, old_filter) = (self.sort, self.filter);
        ui.label(egui::RichText::new(t!("Tri")).weak());
        for (v, label) in [(Sort::DateDesc, t!("Récentes")), (Sort::DateAsc, t!("Anciennes")), (Sort::Name, t!("Nom"))] {
            ui.radio_value(&mut self.sort, v, label);
        }
        ui.label(egui::RichText::new(t!("Filtre")).weak());
        for (v, label) in [(Filter::All, t!("Toutes")), (Filter::Edited, t!("Modifiées")), (Filter::Blurry, t!("Floues")), (Filter::Similar, t!("Semblables"))] {
            ui.radio_value(&mut self.filter, v, label);
        }
        if self.sort != old_sort || self.filter != old_filter {
            if matches!(self.filter, Filter::Blurry | Filter::Similar) {
                self.start_cull(ctx);
            }
            self.refresh_list();
        }
        ui.separator();
        let sel = self.selected.and_then(|i| self.photos.get(i)).map(|p| p.path.clone());
        let is_copy = sel.as_deref().is_some_and(|p| library::copy_number(p).is_some());
        if item(ui, sel.is_some(), t!("Créer une copie virtuelle"), "") {
            if let Some(p) = &sel {
                self.library_action(p, LibAction::Copy);
                self.refresh_dirs();
            }
        }
        if item(ui, is_copy, t!("Supprimer la copie virtuelle"), "") {
            if let Some(p) = &sel {
                self.library_action(p, LibAction::DeleteCopy);
                self.refresh_dirs();
            }
        }
        if item(ui, sel.is_some(), t!("Afficher dans le dossier"), "") {
            if let Some(p) = &sel {
                self.library_action(p, LibAction::ShowFile);
            }
        }
        ui.checkbox(&mut self.info_open, t!("Informations de la photo (I)"));
    }

    fn access_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(egui::RichText::new(t!("Taille de l'interface")).weak());
        let mut zoom = self.settings.ui_zoom;
        for z in ZOOMS {
            ui.radio_value(&mut zoom, z, format!("{:.0} %", z * 100.0));
        }
        ui.small(t!("Aussi Ctrl+plus et Ctrl+moins."));
        if zoom != self.settings.ui_zoom {
            self.settings.ui_zoom = zoom;
            ctx.set_zoom_factor(zoom);
            self.settings.save();
        }
        ui.separator();
        let mut changed = ui.checkbox(&mut self.settings.high_contrast, t!("Contraste élevé")).on_hover_text(t!("Textes blancs, contours marqués, fond noir")).changed();
        changed |= ui.checkbox(&mut self.settings.large_targets, t!("Grandes zones de clic")).on_hover_text(t!("Boutons et curseurs plus grands, plus espacés")).changed();
        changed |= ui.checkbox(&mut self.settings.reduce_motion, t!("Sans animations")).on_hover_text(t!("Sections, panneau et Avant/Après changent d'un coup")).changed();
        if changed {
            self.settings.save();
            apply_access(ctx, &self.settings);
        }
        ui.separator();
        if item(ui, true, t!("Navigation au clavier…"), "F1") {
            self.keys_open = true;
        }
        ui.label(egui::RichText::new(t!("Lecteur d'écran")).weak());
        ui.small(t!("Menus, boutons et curseurs sont lus par Orca (GNOME : Super+Alt+S pour l'activer)."));
    }

    /// Fenêtres « Raccourcis clavier » et « À propos ».
    pub(super) fn help_windows(&mut self, ctx: &egui::Context) {
        if self.keys_open {
            let mut open = true;
            egui::Window::new(t!("Raccourcis clavier")).open(&mut open).collapsible(false).resizable(false).vscroll(true).default_height(ctx.content_rect().height() - 120.0).show(ctx, |ui| {
                egui::Grid::new("raccourcis").num_columns(2).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                    for (keys, what) in KEYS {
                        ui.label(egui::RichText::new(crate::i18n::tr(keys)).monospace().strong());
                        ui.label(crate::i18n::tr(what));
                        ui.end_row();
                    }
                });
            });
            self.keys_open = open;
        }
        if self.about_open {
            let mut open = true;
            egui::Window::new(t!("À propos")).open(&mut open).collapsible(false).resizable(false).vscroll(true).default_height(ctx.content_rect().height() - 120.0).default_width(380.0).show(ctx, |ui| {
                ui.set_max_width(380.0);
                self.about_ui(ui);
            });
            if !open {
                self.about_copied = false;
            }
            self.about_open = open;
        }
    }

    /// Échap ferme d'abord la fenêtre ouverte (une à la fois). Renvoie vrai si une fenêtre a été fermée.
    pub(super) fn close_window_on_escape(&mut self, ctx: &egui::Context) -> bool {
        if !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return false;
        }
        let flags = [&mut self.keys_open, &mut self.about_open, &mut self.prefs_open, &mut self.export_open];
        for f in flags {
            if *f {
                *f = false;
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                return true;
            }
        }
        if self.import_report.take().is_some() {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            return true;
        }
        false
    }
}

/// Thème sombre anthracite (gris légèrement bleuté, coins arrondis), à la manière de Luminar.
fn anthracite(style: &mut egui::Style) {
    use super::palette as c;
    let v = &mut style.visuals;
    v.panel_fill = c::PANEL;
    v.window_fill = c::WINDOW;
    v.extreme_bg_color = c::FIELD;
    v.faint_bg_color = c::ITEM;
    v.code_bg_color = c::FIELD;
    v.window_stroke = Stroke::new(1.0, c::LINE);
    v.window_corner_radius = egui::CornerRadius::same(10);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.selection.bg_fill = c::ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.slider_trailing_fill = false;
    v.widgets.noninteractive.bg_fill = c::PANEL;
    v.widgets.noninteractive.weak_bg_fill = c::PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, c::LINE);
    for (w, fill) in [(&mut v.widgets.inactive, c::ITEM), (&mut v.widgets.hovered, c::ITEM_HOVER), (&mut v.widgets.active, c::ITEM_ON), (&mut v.widgets.open, c::ITEM_ON)] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = egui::CornerRadius::same(6);
    }
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, c::LINE_STRONG);
    v.widgets.active.bg_stroke = Stroke::new(1.0, c::ACCENT);
    style.spacing.slider_rail_height = 4.0;
}

/// Style de l'interface selon les réglages d'accessibilité (contraste, taille des zones de clic).
pub(super) fn apply_access(ctx: &egui::Context, s: &Settings) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let mut style = egui::Style { visuals: if theme == egui::Theme::Dark { egui::Visuals::dark() } else { egui::Visuals::light() }, ..Default::default() };
        if theme == egui::Theme::Dark && !s.high_contrast {
            anthracite(&mut style);
        }
        if s.high_contrast {
            let v = &mut style.visuals;
            *v = egui::Visuals::dark();
            v.override_text_color = Some(Color32::WHITE);
            v.panel_fill = Color32::BLACK;
            v.window_fill = Color32::from_gray(8);
            v.extreme_bg_color = Color32::BLACK;
            v.faint_bg_color = Color32::from_gray(30);
            v.window_stroke = Stroke::new(2.0, Color32::WHITE);
            v.selection.bg_fill = Color32::from_rgb(0, 95, 210);
            v.selection.stroke = Stroke::new(2.0, Color32::WHITE);
            v.hyperlink_color = Color32::from_rgb(120, 200, 255);
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.5, Color32::from_gray(170));
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.5, Color32::WHITE);
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                w.bg_stroke = Stroke::new(1.5, Color32::from_gray(220));
                w.fg_stroke = Stroke::new(1.5, Color32::WHITE);
            }
            v.widgets.inactive.bg_fill = Color32::from_gray(35);
            v.widgets.inactive.weak_bg_fill = Color32::from_gray(35);
            v.widgets.hovered.bg_fill = Color32::from_gray(70);
            v.widgets.hovered.weak_bg_fill = Color32::from_gray(70);
            v.widgets.hovered.bg_stroke = Stroke::new(2.5, Color32::from_rgb(255, 210, 0));
        }
        if s.large_targets {
            let sp = &mut style.spacing;
            sp.interact_size = vec2(48.0, 32.0);
            sp.button_padding = vec2(10.0, 6.0);
            sp.item_spacing = vec2(10.0, 8.0);
            sp.icon_width = 20.0;
            sp.icon_width_inner = 12.0;
            sp.slider_width *= 1.3;
        }
        // Consignes (ui.small) lisibles : 9 px par défaut dans egui, trop petit.
        style.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        // Durée des glissements et fondus (sections, panneau, Avant/Après).
        style.animation_time = if s.reduce_motion { 0.0 } else { 0.18 };
        ctx.set_style_of(theme, style);
    }
}
