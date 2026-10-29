//! The editor's fonts: bundled Lato plus system fonts looked up by name.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};
use fontdb::{Database, Family, Query};
use rand::seq::IndexedRandom;

pub static LATO: &[u8] = include_bytes!("../../freewrite/fonts/Lato-Regular.ttf");

pub const LATO_NAME: &str = "Lato";

/// The fixed font buttons and what to look for on each platform.
pub fn candidates(button: &str) -> &'static [&'static str] {
    match button {
        "Arial" => &[
            "Arial",
            "Liberation Sans",
            "Arimo",
            "Helvetica",
            "Nimbus Sans",
        ],
        "Serif" => &[
            "Times New Roman",
            "Liberation Serif",
            "Tinos",
            "Nimbus Roman",
            "Noto Serif",
            "DejaVu Serif",
        ],
        "System" if cfg!(windows) => &["Segoe UI"],
        "System" => &["Cantarell", "Noto Sans", "Inter", "Ubuntu", "DejaVu Sans"],
        _ => &[],
    }
}

/// A font the editor can use.
#[derive(Clone)]
pub struct LoadedFont {
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

pub struct FontLibrary {
    db: Option<Database>,
    loaded: HashMap<String, LoadedFont>,
}

impl FontLibrary {
    pub fn new() -> Self {
        let mut loaded = HashMap::new();
        loaded.insert(
            LATO_NAME.to_string(),
            LoadedFont {
                data: Arc::new(LATO.to_vec()),
                index: 0,
            },
        );
        FontLibrary { db: None, loaded }
    }

    fn db(&mut self) -> &Database {
        self.db.get_or_insert_with(|| {
            let mut db = Database::new();
            db.load_system_fonts();
            db
        })
    }

    pub fn get(&self, name: &str) -> Option<&LoadedFont> {
        self.loaded.get(name)
    }

    /// Load the first installed family out of `names`, returning its name.
    pub fn load_first(&mut self, names: &[&str]) -> Option<String> {
        names.iter().find_map(|name| self.load(name))
    }

    /// Load a regular face of `family`, if it's installed and can show Latin text.
    pub fn load(&mut self, family: &str) -> Option<String> {
        if self.loaded.contains_key(family) {
            return Some(family.to_string());
        }
        let db = self.db();
        let id = db.query(&Query {
            families: &[Family::Name(family)],
            ..Query::default()
        })?;
        let font = db.with_face_data(id, |data, index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            let latin = "abcxyzABCXYZ"
                .chars()
                .all(|c| face.glyph_index(c).is_some());
            latin.then(|| LoadedFont {
                data: Arc::new(data.to_vec()),
                index,
            })
        })??;
        self.loaded.insert(family.to_string(), font);
        Some(family.to_string())
    }

    /// A random installed family that can show Latin text.
    pub fn load_random(&mut self) -> Option<String> {
        let families: Vec<String> = {
            let db = self.db();
            let mut names: Vec<String> = db
                .faces()
                .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
                .filter(|name| !name.starts_with('.'))
                .collect();
            names.sort();
            names.dedup();
            names
        };
        let mut rng = rand::rng();
        for _ in 0..10 {
            let name = families.choose(&mut rng)?;
            if let Some(name) = self.load(name) {
                return Some(name);
            }
        }
        None
    }

    /// Font definitions with every loaded family registered under its own name.
    pub fn definitions(&self) -> FontDefinitions {
        let mut defs = FontDefinitions::default();
        for (name, font) in &self.loaded {
            let data = FontData::from_blob(font.data.clone(), font.index);
            defs.font_data.insert(name.clone(), Arc::new(data));
        }
        let fallbacks = defs
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        for name in self.loaded.keys() {
            let mut chain = vec![name.clone()];
            chain.extend(fallbacks.iter().cloned());
            defs.families
                .insert(FontFamily::Name(name.as_str().into()), chain);
        }
        // The interface itself is set in Lato, like the macOS app's look.
        if let Some(proportional) = defs.families.get_mut(&FontFamily::Proportional) {
            proportional.insert(0, LATO_NAME.to_string());
        }
        defs
    }
}
