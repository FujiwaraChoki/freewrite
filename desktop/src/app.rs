//! The freewrite window: a page to write on, a quiet bottom bar, and a history sidebar.

use std::sync::Arc;
use std::time::Duration;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CursorIcon, Event, FontFamily, FontId, Frame, Id, Key, Layout, Margin,
    Modal, Panel, Popup, PopupCloseBehavior, Pos2, Rect, Response, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Ui, Vec2, ViewportCommand, Visuals, vec2,
};
use rand::seq::IndexedRandom;
use uuid::Uuid;

use crate::config::Settings;
use crate::fonts::{self, FontLibrary, LATO_NAME};
use crate::pdf;
use crate::storage::{self, Entry, Store, content_hash};
use crate::sync::{AccountStatus, SyncHandle};

const DEFAULT_TIME: i32 = 900;
const MAX_TIME: i32 = 2700;
const FONT_SIZES: [f32; 6] = [16.0, 18.0, 20.0, 22.0, 24.0, 26.0];
const NAV_HEIGHT: f32 = 68.0;
const SIDEBAR_WIDTH: f32 = 200.0;
const MAX_URL_LENGTH: usize = 6000;

const PLACEHOLDERS: [&str; 8] = [
    "Begin writing",
    "Pick a thought and go",
    "Start typing",
    "What's on your mind",
    "Just start",
    "Type your first thought",
    "Start with one sentence",
    "Just say it",
];

const CHATGPT_PROMPT: &str = "below is my journal entry. wyt? talk through it with me like a friend. don't therpaize me and give me a whole breakdown, don't repeat my thoughts with headings. really take all of this, and tell me back stuff truly as if you're an old homie.

Keep it casual, dont say yo, help me make new connections i don't see, comfort, validate, challenge, all of it. dont be afraid to say a lot. format with markdown headings if needed.

do not just go through every single thing i say, and say it back to me. you need to proccess everythikng is say, make connections i don't see it, and deliver it all back to me as a story that makes me feel what you think i wanna feel. thats what the best therapists do.

ideally, you're style/tone should sound like the user themselves. it's as if the user is hearing their own tone but it should still feel different, because you have different things to say and don't just repeat back they say.

else, start by saying, \"hey, thanks for showing me this. my thoughts:\"

my entry:";

const CLAUDE_PROMPT: &str = "Take a look at my journal entry below. I'd like you to analyze it and respond with deep insight that feels personal, not clinical.
Imagine you're not just a friend, but a mentor who truly gets both my tech background and my psychological patterns. I want you to uncover the deeper meaning and emotional undercurrents behind my scattered thoughts.
Keep it casual, dont say yo, help me make new connections i don't see, comfort, validate, challenge, all of it. dont be afraid to say a lot. format with markdown headings if needed.
Use vivid metaphors and powerful imagery to help me see what I'm really building. Organize your thoughts with meaningful headings that create a narrative journey through my ideas.
Don't just validate my thoughts - reframe them in a way that shows me what I'm really seeking beneath the surface. Go beyond the product concepts to the emotional core of what I'm trying to solve.
Be willing to be profound and philosophical without sounding like you're giving therapy. I want someone who can see the patterns I can't see myself and articulate them in a way that feels like an epiphany.
Start with 'hey, thanks for showing me this. my thoughts:' and then use markdown headings to structure your response.

Here's my journal entry:";

/// The guide every new install starts with, minus the macOS-only bits.
pub fn welcome_text() -> String {
    include_str!("../../freewrite/default.md")
        .replace("open it in Finder", "open it in your file manager")
        .replace("trackpad", "trackpad or mouse wheel")
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Moon,
    Sun,
    History,
    Export,
    Trash,
}

#[derive(Default)]
struct AccountForm {
    open: bool,
    email: String,
    password: String,
}

pub struct FreewriteApp {
    store: Arc<Store>,
    sync: SyncHandle,
    fonts: FontLibrary,
    welcome: String,
    settings: Settings,

    entries: Vec<Entry>,
    selected: Option<Uuid>,
    text: String,
    /// Hash of the selected entry as last read or written, so a save never
    /// clobbers a version that sync just wrote.
    disk_hash: Option<String>,

    font: String,
    random_font: Option<String>,
    /// Fonts registered this frame are usable from the next one.
    pending_font: Option<String>,
    font_size: f32,
    placeholder: &'static str,
    focus_editor: bool,

    time_remaining: i32,
    timer_running: bool,
    last_tick: f64,
    last_timer_click: Option<f64>,
    timer_scroll: f32,

    backspace_disabled: bool,
    show_sidebar: bool,
    copied_prompt: bool,
    account: AccountForm,
    was_signed_in: bool,
}

impl FreewriteApp {
    pub fn new(cc: &eframe::CreationContext<'_>, store: Arc<Store>) -> Self {
        let welcome = welcome_text();
        let fonts = FontLibrary::new();
        cc.egui_ctx.set_fonts(fonts.definitions());
        let sync = SyncHandle::spawn(store.clone(), format!("\n\n{welcome}"), cc.egui_ctx.clone());

        let mut app = FreewriteApp {
            store,
            sync,
            fonts,
            welcome,
            settings: Settings::load(),
            entries: Vec::new(),
            selected: None,
            text: "\n\n".into(),
            disk_hash: None,
            font: LATO_NAME.into(),
            random_font: None,
            pending_font: None,
            font_size: 18.0,
            placeholder: random_placeholder(),
            focus_editor: true,
            time_remaining: DEFAULT_TIME,
            timer_running: false,
            last_tick: 0.0,
            last_timer_click: None,
            timer_scroll: 0.0,
            backspace_disabled: false,
            show_sidebar: false,
            copied_prompt: false,
            account: AccountForm::default(),
            was_signed_in: false,
        };
        app.apply_theme(&cc.egui_ctx);
        app.load_existing_entries();
        app
    }

    // -- Colors ---------------------------------------------------------------

    fn dark(&self) -> bool {
        self.settings.dark_mode
    }

    fn background(&self) -> Color32 {
        if self.dark() {
            Color32::BLACK
        } else {
            Color32::WHITE
        }
    }

    fn text_color(&self) -> Color32 {
        if self.dark() {
            Color32::from_gray(128).gamma_multiply(0.8)
        } else {
            Color32::from_gray(128)
        }
    }

    fn hover_color(&self) -> Color32 {
        if self.dark() {
            Color32::WHITE
        } else {
            Color32::BLACK
        }
    }

    fn editor_color(&self) -> Color32 {
        if self.dark() {
            Color32::from_gray(230)
        } else {
            Color32::from_gray(51)
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark() {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.panel_fill = self.background();
        visuals.window_fill = if self.dark() {
            Color32::from_gray(28)
        } else {
            Color32::WHITE
        };
        visuals.extreme_bg_color = self.background();
        visuals.selection.bg_fill = if self.dark() {
            Color32::from_gray(70)
        } else {
            Color32::from_rgb(200, 220, 255)
        };
        visuals.text_cursor.stroke = Stroke::new(2.0, self.editor_color());
        ctx.set_visuals(visuals);
        // Labels here are buttons and captions; selectable ones would swallow clicks.
        ctx.all_styles_mut(|style| style.interaction.selectable_labels = false);
    }

    // -- Entries --------------------------------------------------------------

    /// Startup: reuse today's blank page (or the untouched guide), else start a new one.
    fn load_existing_entries(&mut self) {
        let listed = self.store.list();
        let guide = format!("\n\n{}", self.welcome);
        self.entries = listed.iter().map(|(entry, _)| entry.clone()).collect();

        let today = chrono::Local::now().date_naive();
        let reusable = listed.iter().find(|(entry, content)| {
            (entry.created.date() == today && content.trim().is_empty())
                || (listed.len() == 1 && *content == guide)
        });
        match reusable {
            Some((entry, _)) => {
                let id = entry.id;
                self.select(id);
            }
            None => self.create_new_entry(),
        }
    }

    /// After a sync: pick up new, changed and removed files.
    fn reload_entries(&mut self, changed: &std::collections::HashSet<Uuid>) {
        self.entries = self
            .store
            .list()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect();
        let Some(selected) = self.selected else {
            return;
        };
        match self.entries.iter().find(|e| e.id == selected).cloned() {
            None => match self.entries.first().map(|e| e.id) {
                Some(first) => self.load(first),
                None => self.create_new_entry(),
            },
            Some(entry) if changed.contains(&selected) => {
                if let Ok(content) = self.store.read(&entry.filename) {
                    self.disk_hash = Some(content_hash(&content));
                    self.text = content;
                }
            }
            Some(_) => {}
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let id = self.selected?;
        self.entries.iter().find(|e| e.id == id)
    }

    fn save_current(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if self.disk_hash.as_deref() == Some(content_hash(&self.text).as_str()) {
            return;
        }
        match self
            .store
            .write_if_unchanged(&entry.filename, self.disk_hash.as_deref(), &self.text)
        {
            Ok(true) => {
                self.disk_hash = Some(content_hash(&self.text));
                let preview = storage::preview_for(&self.text);
                if let Some(e) = self.entries.iter_mut().find(|e| e.id == entry.id) {
                    e.preview = preview;
                }
            }
            // Sync replaced the file a moment ago; its change event reloads it.
            Ok(false) => {}
            Err(err) => eprintln!("Error saving entry: {err}"),
        }
    }

    /// Show an entry without saving the current one first.
    fn load(&mut self, id: Uuid) {
        let Some(entry) = self.entries.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        self.selected = Some(id);
        let content = self
            .store
            .read(&entry.filename)
            .unwrap_or_else(|_| "\n\n".into());
        self.disk_hash = Some(content_hash(&content));
        self.text = content;
        self.focus_editor = true;
    }

    fn select(&mut self, id: Uuid) {
        if self.selected == Some(id) {
            return;
        }
        self.save_current();
        self.load(id);
        self.sync.sync_now();
    }

    fn create_new_entry(&mut self) {
        self.save_current();
        let entry = Entry::new();
        let first = self.entries.is_empty();
        self.text = if first {
            format!("\n\n{}", self.welcome)
        } else {
            self.placeholder = random_placeholder();
            "\n\n".into()
        };
        self.selected = Some(entry.id);
        self.entries.insert(0, entry);
        self.disk_hash = None;
        self.focus_editor = true;
        self.save_current();
    }

    fn delete_entry(&mut self, id: Uuid) {
        let Some(entry) = self.entries.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        if let Err(err) = self.store.delete(&entry.filename) {
            eprintln!("Error deleting entry: {err}");
            return;
        }
        self.entries.retain(|e| e.id != id);
        if self.selected == Some(id) {
            self.selected = None;
            match self.entries.first().map(|e| e.id) {
                Some(first) => self.load(first),
                None => self.create_new_entry(),
            }
        }
        self.sync.sync_now();
    }

    fn export_pdf(&mut self, id: Uuid) {
        if self.selected == Some(id) {
            self.save_current();
        }
        let Some(entry) = self.entries.iter().find(|e| e.id == id) else {
            return;
        };
        let Ok(content) = self.store.read(&entry.filename) else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.pdf", pdf::title_for(&content)))
            .add_filter("PDF", &["pdf"])
            .save_file()
        else {
            return;
        };
        let font = self
            .fonts
            .get(&self.font)
            .map(|f| f.data.as_ref().clone())
            .unwrap_or_else(|| fonts::LATO.to_vec());
        // Not every system font can be embedded; Lato always can.
        if let Err(err) = pdf::export(&content, font, self.font_size, &path) {
            eprintln!("Couldn't export with {}: {err}", self.font);
            if let Err(err) = pdf::export(&content, fonts::LATO.to_vec(), self.font_size, &path) {
                eprintln!("Error exporting PDF: {err}");
            }
        }
    }

    // -- Fonts ----------------------------------------------------------------

    fn choose_font(&mut self, ctx: &egui::Context, name: Option<String>, random: bool) {
        let Some(name) = name else {
            return;
        };
        self.random_font = random.then(|| name.clone());
        ctx.set_fonts(self.fonts.definitions());
        self.pending_font = Some(name);
    }

    fn editor_font(&self) -> FontId {
        FontId::new(self.font_size, FontFamily::Name(self.font.as_str().into()))
    }

    fn line_height(&self) -> f32 {
        self.font_size * 1.5
    }

    // -- Timer ----------------------------------------------------------------

    fn timer_title(&self) -> String {
        if !self.timer_running && self.time_remaining == DEFAULT_TIME {
            return "15:00".into();
        }
        format!(
            "{}:{:02}",
            self.time_remaining / 60,
            self.time_remaining % 60
        )
    }

    fn tick_timer(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.timer_running && self.time_remaining > 0 {
            if now - self.last_tick >= 1.0 {
                self.time_remaining -= 1;
                self.last_tick += 1.0;
            }
            ctx.request_repaint_after(Duration::from_secs_f64(
                (self.last_tick + 1.0 - now).max(0.01),
            ));
        } else if self.time_remaining == 0 {
            self.timer_running = false;
        }
    }

    fn click_timer(&mut self, now: f64) {
        if self.last_timer_click.is_some_and(|last| now - last < 0.3) {
            self.time_remaining = DEFAULT_TIME;
            self.timer_running = false;
            self.last_timer_click = None;
        } else {
            self.timer_running = !self.timer_running;
            self.last_tick = now;
            self.last_timer_click = Some(now);
        }
    }

    fn scroll_timer(&mut self, delta: f32) {
        self.timer_scroll += delta;
        if self.timer_scroll.abs() < 20.0 {
            return;
        }
        let step = if self.timer_scroll > 0.0 { 5 } else { -5 };
        self.timer_scroll = 0.0;
        let minutes = (self.time_remaining / 60 + step) / 5 * 5;
        self.time_remaining = (minutes * 60).clamp(0, MAX_TIME);
    }

    // -- Chat -----------------------------------------------------------------

    fn chat_url(&self, base: &str, prompt: &str) -> String {
        let full = format!("{prompt}\n\n{}", self.text.trim());
        format!("{base}{}", percent_encode(&full))
    }

    // -- UI -------------------------------------------------------------------

    fn nav_link(&self, ui: &mut Ui, label: &str) -> Response {
        let font = FontId::proportional(13.0);
        let galley = ui
            .painter()
            .layout_no_wrap(label.into(), font, Color32::WHITE);
        let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
        let color = if response.hovered() {
            self.hover_color()
        } else {
            self.text_color()
        };
        ui.painter()
            .galley_with_override_text_color(rect.min, galley, color);
        response.on_hover_cursor(CursorIcon::PointingHand)
    }

    fn icon_button(&self, ui: &mut Ui, icon: Icon, hover_color: Color32) -> Response {
        let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
        let color = if response.hovered() {
            hover_color
        } else {
            self.text_color()
        };
        paint_icon(ui, rect, icon, color, self.background());
        response.on_hover_cursor(CursorIcon::PointingHand)
    }

    fn bullet(&self, ui: &mut Ui) {
        ui.label(RichText::new("•").size(13.0).color(Color32::GRAY));
    }

    fn bottom_nav(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let nav_rect = ui.max_rect();
        let hovering = ui.rect_contains_pointer(nav_rect);
        let visible = hovering || !self.timer_running;
        let fade = if visible { 0.2 } else { 1.0 };
        let opacity = ctx.animate_bool_with_time(Id::unique("nav-opacity"), visible, fade);
        ui.multiply_opacity(opacity);
        ui.spacing_mut().item_spacing.x = 8.0;

        ui.horizontal_centered(|ui| {
            // Fonts, on the left.
            let size = self.nav_link(ui, &format!("{}px", self.font_size as i32));
            if size.clicked() {
                let next = FONT_SIZES
                    .iter()
                    .position(|s| *s == self.font_size)
                    .map_or(1, |i| (i + 1) % FONT_SIZES.len());
                self.font_size = FONT_SIZES[next];
            }
            for button in ["Lato", "Arial", "System", "Serif"] {
                self.bullet(ui);
                if self.nav_link(ui, button).clicked() {
                    let name = if button == "Lato" {
                        Some(LATO_NAME.to_string())
                    } else {
                        self.fonts.load_first(fonts::candidates(button))
                    };
                    self.choose_font(&ctx, name, false);
                }
            }
            self.bullet(ui);
            let random_title = match &self.random_font {
                Some(name) => format!("Random [{name}]"),
                None => "Random".into(),
            };
            if self.nav_link(ui, &random_title).clicked() {
                let name = self.fonts.load_random();
                self.choose_font(&ctx, name, true);
            }

            // Utilities, on the right (laid out right to left).
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self
                    .icon_button(ui, Icon::History, self.hover_color())
                    .clicked()
                {
                    self.show_sidebar = !self.show_sidebar;
                }
                self.bullet(ui);
                let theme_icon = if self.dark() { Icon::Sun } else { Icon::Moon };
                if self
                    .icon_button(ui, theme_icon, self.hover_color())
                    .clicked()
                {
                    self.settings.dark_mode = !self.settings.dark_mode;
                    self.settings.save();
                    self.apply_theme(&ctx);
                }
                self.bullet(ui);
                if self.nav_link(ui, "New Entry").clicked() {
                    self.create_new_entry();
                }
                self.bullet(ui);
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                let label = if fullscreen { "Minimize" } else { "Fullscreen" };
                if self.nav_link(ui, label).clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
                }
                self.bullet(ui);
                let label = if self.backspace_disabled {
                    "Backspace is Off"
                } else {
                    "Backspace is On"
                };
                if self.nav_link(ui, label).clicked() {
                    self.backspace_disabled = !self.backspace_disabled;
                }
                self.bullet(ui);
                let chat = self.nav_link(ui, "Chat");
                self.chat_popup(&chat);
                self.bullet(ui);
                let timer_title = self.timer_title();
                let timer = self.nav_link(ui, &timer_title);
                if timer.clicked() {
                    self.click_timer(ctx.input(|i| i.time));
                }
                if timer.hovered() {
                    let delta = ctx.input(|i| wheel_delta(&i.events));
                    if delta != 0.0 {
                        self.scroll_timer(delta);
                    }
                }
            });
        });
    }

    fn chat_popup(&mut self, button: &Response) {
        let popup_id = Popup::default_response_id(button);
        if button.clicked() && !Popup::is_id_open(&button.ctx, popup_id) {
            self.copied_prompt = false;
        }
        let trimmed = self.text.trim().to_string();
        let gpt_url = self.chat_url("https://chat.openai.com/?prompt=", CHATGPT_PROMPT);
        let claude_url = self.chat_url("https://claude.ai/new?q=", CLAUDE_PROMPT);
        let too_long = gpt_url.len() > MAX_URL_LENGTH || claude_url.len() > MAX_URL_LENGTH;
        let is_guide = trimmed == self.welcome.trim();
        let text_color = if self.dark() {
            Color32::WHITE
        } else {
            Color32::from_gray(30)
        };
        let frame = Frame::popup(&button.ctx.global_style())
            .inner_margin(Margin::symmetric(12, 8))
            .corner_radius(8.0);

        Popup::from_toggle_button_response(button)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::TOP)
            .frame(frame)
            .show(|ui| {
                ui.set_max_width(250.0);
                ui.style_mut().override_text_style = Some(egui::TextStyle::Body);
                let message = |ui: &mut Ui, text: &str| {
                    ui.add(egui::Label::new(RichText::new(text).size(14.0).color(text_color)).wrap());
                };
                let item = |ui: &mut Ui, text: &str| {
                    let response = ui.add(
                        egui::Label::new(RichText::new(text).size(14.0).color(text_color))
                            .sense(Sense::click()),
                    );
                    response.on_hover_cursor(CursorIcon::PointingHand)
                };
                let copy_label = if self.copied_prompt { "Copied!" } else { "Copy Prompt" };

                if too_long {
                    ui.set_width(200.0);
                    message(ui, "Hey, your entry is quite long. You'll need to manually copy the prompt by clicking 'Copy Prompt' below and then paste it into AI of your choice (ex. ChatGPT). The prompt includes your entry as well. So just copy paste and go! See what the AI says.");
                    ui.separator();
                    if item(ui, copy_label).clicked() {
                        ui.ctx().copy_text(format!("{CHATGPT_PROMPT}\n\n{trimmed}"));
                        self.copied_prompt = true;
                    }
                } else if is_guide {
                    message(ui, "Yo. Sorry, you can't chat with the guide lol. Please write your own entry.");
                } else if self.text.chars().count() < 350 {
                    message(ui, "Please free write for at minimum 5 minutes first. Then click this. Trust.");
                } else {
                    ui.set_min_width(120.0);
                    if item(ui, "ChatGPT").clicked() {
                        let _ = open::that(&gpt_url);
                        ui.close();
                    }
                    ui.separator();
                    if item(ui, "Claude").clicked() {
                        let _ = open::that(&claude_url);
                        ui.close();
                    }
                    ui.separator();
                    if item(ui, copy_label).clicked() {
                        ui.ctx().copy_text(format!("{CHATGPT_PROMPT}\n\n{trimmed}"));
                        self.copied_prompt = true;
                    }
                }
            });
    }

    fn editor(&mut self, ui: &mut Ui) {
        if self.backspace_disabled && !self.account.open {
            ui.input_mut(|i| {
                i.events.retain(|event| {
                    !matches!(
                        event,
                        Event::Key {
                            key: Key::Backspace | Key::Delete,
                            ..
                        }
                    )
                })
            });
        }

        let font = self.editor_font();
        let color = self.editor_color();
        let line_height = self.line_height();
        let width = ui.available_width().min(650.0);
        let side = ((ui.available_width() - width) / 2.0).max(0.0);
        let min_height = ui.available_height();

        ScrollArea::vertical()
            .id_salt("editor")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(side);
                    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                        let mut format = TextFormat::simple(font.clone(), color);
                        format.line_height = Some(line_height);
                        let mut job = LayoutJob::single_section(text.as_str().to_owned(), format);
                        job.wrap.max_width = wrap_width;
                        ui.fonts_mut(|f| f.layout_job(job))
                    };
                    let output = TextEdit::multiline(&mut self.text)
                        .id(Id::unique("editor-text"))
                        .frame(Frame::NONE)
                        .desired_width(width)
                        .min_size(vec2(width, min_height))
                        .lock_focus(true)
                        .layouter(&mut layouter)
                        .show(ui);

                    if self.focus_editor {
                        output.response.request_focus();
                        self.focus_editor = false;
                    }

                    if self.text.trim().is_empty() {
                        let end = output
                            .galley
                            .pos_from_cursor(CCursor::new(self.text.chars().count()));
                        let pos = output.galley_pos + end.min.to_vec2();
                        let placeholder_color = if self.dark() {
                            Color32::GRAY.gamma_multiply(0.6)
                        } else {
                            Color32::GRAY.gamma_multiply(0.5)
                        };
                        ui.painter().text(
                            Pos2::new(pos.x, pos.y + (end.height() - self.font_size) / 2.0),
                            Align2::LEFT_TOP,
                            self.placeholder,
                            self.editor_font(),
                            placeholder_color,
                        );
                    }

                    if output.response.changed() {
                        // Entries always start with two blank lines.
                        if !self.text.starts_with("\n\n") {
                            self.text = format!("\n\n{}", self.text.trim_start_matches('\n'));
                        }
                        self.save_current();
                    }
                });
            });
    }

    fn sidebar(&mut self, ui: &mut Ui, status: &AccountStatus) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let secondary = Color32::GRAY;

        // Header: opens the entries folder.
        let header = ui
            .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                let hovered = ui.response().hovered();
                Frame::NONE
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let color = if hovered {
                            self.hover_color()
                        } else {
                            self.text_color()
                        };
                        ui.label(RichText::new("History ↗").size(13.0).color(color));
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(self.store.dir().display().to_string())
                                    .size(10.0)
                                    .color(secondary),
                            )
                            .truncate(),
                        );
                    });
            })
            .response
            .on_hover_cursor(CursorIcon::PointingHand);
        if header.clicked() {
            let _ = open::that(self.store.dir());
        }
        ui.separator();

        // Account.
        let account = ui
            .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                let hovered = ui.response().hovered();
                Frame::NONE
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let color = if hovered {
                            self.hover_color()
                        } else {
                            self.text_color()
                        };
                        let (title, detail) = account_summary(status);
                        ui.label(RichText::new(title).size(13.0).color(color));
                        if let Some(detail) = detail {
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(RichText::new(detail).size(10.0).color(secondary))
                                    .truncate(),
                            );
                        }
                    });
            })
            .response
            .on_hover_cursor(CursorIcon::PointingHand);
        if account.clicked() {
            self.account.open = true;
        }
        ui.separator();

        let mut select = None;
        let mut export = None;
        let mut delete = None;
        ScrollArea::vertical()
            .id_salt("history")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for entry in &self.entries {
                    let selected = self.selected == Some(entry.id);
                    let row =
                        ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                            let hovered = ui.response().hovered();
                            let fill = if selected {
                                Color32::GRAY.gamma_multiply(0.1)
                            } else if hovered {
                                Color32::GRAY.gamma_multiply(0.05)
                            } else {
                                Color32::TRANSPARENT
                            };
                            Frame::NONE
                                .fill(fill)
                                .corner_radius(4.0)
                                .inner_margin(Margin::symmetric(16, 8))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 8.0;
                                        if hovered {
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    let trash = self.icon_button(
                                                        ui,
                                                        Icon::Trash,
                                                        Color32::RED,
                                                    );
                                                    if trash.clicked() {
                                                        delete = Some(entry.id);
                                                    }
                                                    let pdf = self
                                                        .icon_button(
                                                            ui,
                                                            Icon::Export,
                                                            self.hover_color(),
                                                        )
                                                        .on_hover_text("Export entry as PDF");
                                                    if pdf.clicked() {
                                                        export = Some(entry.id);
                                                    }
                                                    ui.with_layout(
                                                        Layout::left_to_right(Align::Center),
                                                        |ui| {
                                                            ui.add(preview_label(
                                                                entry,
                                                                self.dark(),
                                                            ));
                                                        },
                                                    );
                                                },
                                            );
                                        } else {
                                            ui.add(preview_label(entry, self.dark()));
                                        }
                                    });
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(entry.display_date())
                                            .size(12.0)
                                            .color(secondary),
                                    );
                                });
                        });
                    if row.response.clicked() && delete.is_none() && export.is_none() {
                        select = Some(entry.id);
                    }
                    ui.separator();
                }
            });

        if let Some(id) = delete {
            self.delete_entry(id);
        } else if let Some(id) = export {
            self.export_pdf(id);
        } else if let Some(id) = select {
            self.select(id);
        }
    }

    fn account_modal(&mut self, ctx: &egui::Context, status: &AccountStatus) {
        if !self.account.open {
            return;
        }
        let text_color = if self.dark() {
            Color32::WHITE
        } else {
            Color32::from_gray(30)
        };
        let frame = Frame::popup(&ctx.global_style())
            .inner_margin(Margin::same(20))
            .corner_radius(8.0);
        let modal = Modal::new(Id::unique("account")).frame(frame).show(ctx, |ui| {
            ui.set_width(280.0);
            ui.spacing_mut().item_spacing.y = 10.0;
            let small = |text: String, color: Color32| RichText::new(text).size(12.0).color(color);

            if !status.configured {
                ui.label(RichText::new("Cloud sync isn't set up").size(15.0).color(text_color));
                ui.label(small(
                    format!(
                        "This build doesn't know which server to sync with. Set FREEWRITE_SUPABASE_URL and FREEWRITE_SUPABASE_ANON_KEY, or add a cloud.json to {}. Your writing is still saved on this computer.",
                        crate::config::dir().display()
                    ),
                    Color32::GRAY,
                ));
                if ui.button("Close").clicked() {
                    self.account.open = false;
                }
                return;
            }

            if let Some(email) = &status.email {
                ui.label(RichText::new("Account").size(15.0).color(text_color));
                ui.label(small(format!("Signed in as {email}"), text_color));
                let state = if status.syncing {
                    "Syncing…".to_string()
                } else if let Some(at) = status.last_synced {
                    format!("Last synced at {}", at.format("%-I:%M %p"))
                } else {
                    "Not synced yet".to_string()
                };
                ui.label(small(state, Color32::GRAY));
                if let Some(error) = &status.error {
                    ui.label(small(error.clone(), Color32::from_rgb(200, 60, 60)));
                }
                ui.horizontal(|ui| {
                    if ui.button("Sync now").clicked() {
                        self.save_current();
                        self.sync.sync_now();
                    }
                    if ui.button("Sign out").clicked() {
                        self.sync.sign_out();
                    }
                    if ui.button("Close").clicked() {
                        self.account.open = false;
                    }
                });
                return;
            }

            ui.label(RichText::new("Sign in to sync").size(15.0).color(text_color));
            ui.label(small(
                "Your entries stay on this computer and are also kept in your account, so they follow you to your other devices.".into(),
                Color32::GRAY,
            ));
            let email = ui.add(
                TextEdit::singleline(&mut self.account.email)
                    .hint_text("Email")
                    .desired_width(f32::INFINITY),
            );
            let password = ui.add(
                TextEdit::singleline(&mut self.account.password)
                    .hint_text("Password")
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            if self.account.email.is_empty() && !password.has_focus() && !email.has_focus() {
                email.request_focus();
            }
            if let Some(error) = &status.error {
                ui.label(small(error.clone(), Color32::from_rgb(200, 60, 60)));
            }
            if let Some(notice) = &status.notice {
                ui.label(small(notice.clone(), text_color));
            }
            let ready = !status.busy
                && self.account.email.contains('@')
                && !self.account.password.is_empty();
            let submitted = password.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                ui.add_enabled_ui(ready, |ui| {
                    if ui.button("Sign in").clicked() || (ready && submitted) {
                        self.save_current();
                        self.sync.sign_in(self.account.email.trim().into(), self.account.password.clone());
                    }
                    if ui.button("Create account").clicked() {
                        self.save_current();
                        self.sync.sign_up(self.account.email.trim().into(), self.account.password.clone());
                    }
                });
                if ui.button("Cancel").clicked() {
                    self.account.open = false;
                }
            });
            if status.busy {
                ui.label(small("Working…".into(), Color32::GRAY));
            }
        });
        if modal.should_close() {
            self.account.open = false;
        }
    }
}

impl eframe::App for FreewriteApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if let Some(font) = self.pending_font.take() {
            self.font = font;
        }
        if let Some(changes) = self.sync.take_changes() {
            self.reload_entries(&changes.changed);
        }
        let status = self.sync.status();
        let signed_in = status.email.is_some();
        if signed_in && !self.was_signed_in {
            self.account.password.clear();
            self.account.open = false;
        }
        self.was_signed_in = signed_in;
        self.tick_timer(&ctx);

        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, self.background());
        let panel_frame = Frame::NONE.fill(self.background());

        if self.show_sidebar {
            Panel::right("sidebar")
                .exact_size(SIDEBAR_WIDTH)
                .resizable(false)
                .frame(panel_frame)
                .show(ui, |ui| self.sidebar(ui, &status));
        }
        Panel::bottom("nav")
            .exact_size(NAV_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(panel_frame.inner_margin(Margin::symmetric(24, 0)))
            .show(ui, |ui| self.bottom_nav(ui));
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| self.editor(ui));

        self.account_modal(&ctx, &status);
    }

    fn clear_color(&self, _visuals: &Visuals) -> [f32; 4] {
        self.background().to_normalized_gamma_f32()
    }

    fn on_exit(&mut self) {
        self.save_current();
        self.sync.shutdown(Duration::from_secs(5));
    }
}

fn random_placeholder() -> &'static str {
    PLACEHOLDERS
        .choose(&mut rand::rng())
        .copied()
        .unwrap_or("Begin writing")
}

fn preview_label(entry: &Entry, dark: bool) -> egui::Label {
    let color = if dark {
        Color32::from_gray(230)
    } else {
        Color32::from_gray(30)
    };
    egui::Label::new(RichText::new(&entry.preview).size(13.0).color(color))
        .truncate()
        .selectable(false)
}

fn account_summary(status: &AccountStatus) -> (String, Option<String>) {
    if !status.configured {
        return ("Cloud sync".into(), Some("Not set up".into()));
    }
    match &status.email {
        None if status.busy => ("Signing in…".into(), None),
        None => ("Sign in to sync".into(), status.error.clone()),
        Some(email) if status.syncing => ("Syncing…".into(), Some(email.clone())),
        Some(email) if status.error.is_some() => (
            "Sync paused".into(),
            status.error.clone().or(Some(email.clone())),
        ),
        Some(email) => ("Synced".into(), Some(email.clone())),
    }
}

/// Vertical wheel movement this frame, in points (positive = scrolling up).
fn wheel_delta(events: &[Event]) -> f32 {
    events
        .iter()
        .filter_map(|event| match event {
            Event::MouseWheel { unit, delta, .. } => Some(match unit {
                egui::MouseWheelUnit::Point => delta.y,
                egui::MouseWheelUnit::Line => delta.y * 20.0,
                egui::MouseWheelUnit::Page => delta.y * 200.0,
            }),
            _ => None,
        })
        .sum()
}

/// Encode everything except RFC 3986 unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Small line icons, drawn rather than taken from a font so they look the same everywhere.
fn paint_icon(ui: &Ui, rect: Rect, icon: Icon, color: Color32, background: Color32) {
    let painter = ui.painter();
    let c = rect.center();
    let stroke = Stroke::new(1.4, color);
    match icon {
        Icon::Moon => {
            painter.circle_filled(c, 6.0, color);
            painter.circle_filled(c + vec2(3.5, -3.0), 5.0, background);
        }
        Icon::Sun => {
            painter.circle_filled(c, 3.5, color);
            for i in 0..8 {
                let angle = i as f32 * std::f32::consts::FRAC_PI_4;
                let dir = Vec2::angled(angle);
                painter.line_segment([c + dir * 5.5, c + dir * 7.5], stroke);
            }
        }
        Icon::History => {
            painter.circle_stroke(c, 6.5, stroke);
            painter.line_segment([c, c + vec2(0.0, -4.0)], stroke);
            painter.line_segment([c, c + vec2(3.0, 1.5)], stroke);
        }
        Icon::Export => {
            painter.circle_stroke(c, 6.0, stroke);
            painter.line_segment([c + vec2(0.0, -3.5), c + vec2(0.0, 3.0)], stroke);
            painter.line_segment([c + vec2(-2.5, 0.5), c + vec2(0.0, 3.0)], stroke);
            painter.line_segment([c + vec2(2.5, 0.5), c + vec2(0.0, 3.0)], stroke);
        }
        Icon::Trash => {
            let body = Rect::from_min_max(c + vec2(-4.0, -3.0), c + vec2(4.0, 6.5));
            painter.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Middle);
            painter.line_segment([c + vec2(-6.0, -3.5), c + vec2(6.0, -3.5)], stroke);
            painter.line_segment([c + vec2(-1.5, -5.5), c + vec2(1.5, -5.5)], stroke);
            painter.line_segment(
                [c + vec2(0.0, -1.0), c + vec2(0.0, 4.5)],
                Stroke::new(1.0, color),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent_encode("a b&c=d+e\n"), "a%20b%26c%3Dd%2Be%0A");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn welcome_text_mentions_no_mac_only_features() {
        assert!(!welcome_text().contains("Finder"));
    }
}
