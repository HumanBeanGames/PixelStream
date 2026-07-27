//! Generic runtime setting descriptors for PixelStream host applications.
//!
//! This crate intentionally knows nothing about any particular game. A downstream
//! app describes the settings it wants exposed, and PixelStream can render a
//! generic editor from those descriptors.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

/// A stable identifier for a setting category.
pub type PixelSettingCategoryId = String;

/// A stable identifier for a setting field.
pub type PixelSettingFieldId = String;

/// Describes one group of settings in a setup or preview UI.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PixelSettingCategory {
    pub id: PixelSettingCategoryId,
    pub label: String,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub fields: Vec<PixelSettingField>,
}

impl PixelSettingCategory {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            order: 0,
            fields: Vec::new(),
        }
    }

    pub fn with_order(mut self, order: i32) -> Self {
        self.order = order;
        self
    }

    pub fn with_field(mut self, field: PixelSettingField) -> Self {
        self.fields.push(field);
        self
    }
}

/// A single value that can be exposed through the generic settings UI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PixelSettingField {
    pub id: PixelSettingFieldId,
    pub label: String,
    pub value: PixelSettingValue,
    pub kind: PixelSettingKind,
    #[serde(default)]
    pub help: Option<String>,
}

impl PixelSettingField {
    pub fn number(
        id: impl Into<String>,
        label: impl Into<String>,
        value: f64,
        range: PixelSettingNumberRange,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: PixelSettingValue::Number(value),
            kind: PixelSettingKind::Number(range),
            help: None,
        }
    }

    pub fn boolean(id: impl Into<String>, label: impl Into<String>, value: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: PixelSettingValue::Bool(value),
            kind: PixelSettingKind::Bool,
            help: None,
        }
    }

    pub fn text(id: impl Into<String>, label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: PixelSettingValue::Text(value.into()),
            kind: PixelSettingKind::Text,
            help: None,
        }
    }

    pub fn choice(
        id: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
        options: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: PixelSettingValue::Text(value.into()),
            kind: PixelSettingKind::Choice(options.into_iter().map(Into::into).collect()),
            help: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// UI behavior for a setting value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PixelSettingKind {
    Number(PixelSettingNumberRange),
    Bool,
    Text,
    Choice(Vec<String>),
}

/// Numeric editor limits and ergonomics.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct PixelSettingNumberRange {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub drag_step: f64,
}

impl PixelSettingNumberRange {
    pub fn new(min: f64, max: f64, step: f64) -> Self {
        Self {
            min,
            max,
            step,
            drag_step: step,
        }
    }

    pub fn with_drag_step(mut self, drag_step: f64) -> Self {
        self.drag_step = drag_step;
        self
    }
}

/// Runtime value for a setting field.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PixelSettingValue {
    Number(f64),
    Bool(bool),
    Text(String),
}

impl PixelSettingValue {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

/// Stores all settings currently exposed by the host app.
#[derive(Clone, Debug, Default, Resource, Serialize, Deserialize, PartialEq)]
pub struct PixelSettingsRegistry {
    categories: BTreeMap<PixelSettingCategoryId, PixelSettingCategory>,
    revision: u64,
}

impl PixelSettingsRegistry {
    pub fn publish_category(&mut self, category: PixelSettingCategory) {
        if category.id.trim().is_empty() {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        self.categories.insert(category.id.clone(), category);
    }

    pub fn remove_category(&mut self, id: &str) {
        if self.categories.remove(id).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn categories(&self) -> impl Iterator<Item = &PixelSettingCategory> {
        self.categories.values()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn set_value(
        &mut self,
        category_id: &str,
        field_id: &str,
        value: PixelSettingValue,
    ) -> Option<PixelSettingChanged> {
        let category = self.categories.get_mut(category_id)?;
        let field = category
            .fields
            .iter_mut()
            .find(|field| field.id == field_id)?;
        if field.value == value {
            return None;
        }
        field.value = value.clone();
        self.revision = self.revision.wrapping_add(1);
        Some(PixelSettingChanged {
            category_id: category_id.to_owned(),
            field_id: field_id.to_owned(),
            value,
        })
    }

    pub fn save_json(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        fs::write(path, json).map_err(|error| error.to_string())
    }

    pub fn load_json(path: impl AsRef<Path>) -> Result<Self, String> {
        let json = fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde_json::from_str(&json).map_err(|error| error.to_string())
    }
}

/// Message emitted when a generic settings editor changes a value.
#[derive(Clone, Debug, Message, PartialEq)]
pub struct PixelSettingChanged {
    pub category_id: String,
    pub field_id: String,
    pub value: PixelSettingValue,
}

/// Registers the generic settings resources and messages.
#[derive(Default)]
pub struct PixelSettingsPlugin;

impl Plugin for PixelSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PixelSettingsRegistry>()
            .add_message::<PixelSettingChanged>();
    }
}

pub trait PixelSettingsAppExt {
    fn add_pixel_settings_category(&mut self, category: PixelSettingCategory) -> &mut Self;
}

impl PixelSettingsAppExt for App {
    fn add_pixel_settings_category(&mut self, category: PixelSettingCategory) -> &mut Self {
        self.init_resource::<PixelSettingsRegistry>();
        self.world_mut()
            .resource_mut::<PixelSettingsRegistry>()
            .publish_category(category);
        self
    }
}
