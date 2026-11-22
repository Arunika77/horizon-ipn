//! Inventory Profiles Next (IPN) port for Horizon.
//!
//! Provides the 5 core IPN features natively on client WASM:
//! 1. Persistent Locked Slots with slot overlays (`Alt+Click`, Lock Mode, or hotkey)
//! 2. Container transfers (`Move Matching`, `Dump All`, `Pull Matching`)
//! 3. Smart Lock-Aware Sort (Player inventory & Storage containers, Row/Column + Category rules)
//! 4. Auto-Refill (near-broken tool saver + depleted stack replacement from main inventory)
//! 5. Inventory Profiles / Loadout Presets (save & apply Hotbar, Armor, and Offhand layouts)

pub mod model;
pub mod planner;
pub mod search;

use crate::model::{
    FillDirection, MoveMatchMode, PROFILE_COUNT, PanelPosition, PersistentState, SlotSnapshot,
    SortOrder, TransferMode,
};
use crate::planner::{
    MenuSlotEntry, PlanClick, RefillHand, capture_profile_rules, collect_sortable_menu_indices,
    plan_apply_profile, plan_auto_refill, plan_sort_clicks_bounded, plan_throw_clicks_bounded,
    plan_transfer_clicks_bounded,
};
use crate::search::{
    SEARCH_BAR_H, build_search_bar_layout, build_searchable_haystack, clamp_bar_origin,
    matches_normalized_query, normalize_search_text, search_bar_width,
};
use mod_api::client::widgets::{TextField, WidgetId, Widgets};
use mod_api::{
    ClientHost, ClientModifiers, ClientTickPostEvent, Components, ConfigPath, Corner, Flow, Hooks,
    InputKeyEvent, ItemKey, ItemResource, ItemStack, KeyAction, KeyConflictContext,
    KeyRegistration, MenuClick, MenuRef, MenuSlotKind, MouseButton, Registrar,
    RegistrationContext, RenderGuiPostEvent, ScreenCharTypedPreEvent, ScreenExclusionAreasEvent,
    ScreenKeyPressedPreEvent, ScreenMouseButtonPressedPreEvent,
    ScreenMouseButtonReleasedPreEvent, ScreenMouseDraggedPreEvent, ScreenRenderForegroundEvent,
    ScreenRenderInventoryMobEffectsEvent, ScreenRenderPostEvent, SettingSpec, SwapWith, Text,
};
use std::collections::HashMap;
use std::sync::Mutex;

const MOD_ID: &str = "ipn";
const STATE_PATH: &str = "state.txt";

// Keybinding IDs (registered in Options > Controls).
const KEY_SORT: &str = "sort";
const KEY_LOCK_SLOT: &str = "lock_slot";
const KEY_MOVE_MATCHING: &str = "move_matching";
const KEY_DUMP_ALL: &str = "dump_all";
const KEY_PULL_MATCHING: &str = "pull_matching";
const KEY_PULL_ALL: &str = "pull_all";
const KEY_APPLY_PROFILE: &str = "apply_profile";
const KEY_SAVE_PROFILE: &str = "save_profile";
const KEY_NEXT_PROFILE: &str = "next_profile";
const KEY_TOGGLE_HIGHLIGHT: &str = "toggle_highlight";
const KEY_THROW_MATCHING: &str = "throw_matching";
const KEY_THROW_ALL: &str = "throw_all";
const KEY_FOCUS_SEARCH: &str = "focus_search";

// Settings keys (registered in Options > Mods > Inventory Profiles Next -> Config).
const SET_SORT_RULE: &str = "sort_rule";
const SET_FILL_DIR: &str = "fill_direction";
const SET_SORT_HOTBAR: &str = "sort_hotbar";
const SET_TRANSFER_HOTBAR: &str = "transfer_hotbar";
const SET_MOVE_MATCH_MODE: &str = "move_match_mode";
const SET_AUTO_REFILL: &str = "auto_refill";
const SET_REFILL_THRESHOLD: &str = "refill_threshold";
const SET_SHOW_BUTTONS: &str = "show_buttons";
const SET_HIGHLIGHT_MATCHING: &str = "highlight_matching";
const SET_PANEL_POSITION: &str = "panel_position";
const SET_PROTECT_LOCKED_CLICKS: &str = "prevent_locked_quickmove";
const SET_SHOW_SEARCH_BAR: &str = "show_search_bar";
const SET_KEEP_SEARCH_QUERY: &str = "keep_search_query";

// UI & click-batching limits.
const PANEL_W: f32 = 76.0;
const PANEL_PAD: f32 = 3.0;
const HEADER_H: f32 = 12.0;
const BTN_H: f32 = 13.0;
const BTN_GAP: f32 = 2.0;
/// Cap menu click batches per tick so double-chest sorts stay under 3 ms (and anticheat-safe).
const MAX_CLICKS_PER_TICK: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingAction {
    SortContainer,
    SortInventory,
    SortAuto {
        hovered_storage: bool,
        hovered_player: bool,
    },
    Transfer(TransferMode),
    ThrowMatching {
        target_storage: bool,
        include_hotbar: bool,
    },
    ThrowAll {
        target_storage: bool,
    },
    ApplyProfile(usize),
    SaveProfile(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonKind {
    SortChest,
    MoveMatching,
    DumpAll,
    PullMatching,
    PullAll,
    SortInv,
    ToggleLockMode,
    ToggleHighlight,
    PrevProfile,
    NextProfile,
    ApplyProfile,
    SaveProfile,
}

#[derive(Clone, Debug)]
struct PanelButton {
    kind: ButtonKind,
    rect: [f32; 4],
    label: String,
    active: bool,
    tooltip_title: String,
    tooltip_hint: String,
}

#[derive(Clone, Debug)]
struct PanelLayout {
    bounds: [f32; 4],
    header_rect: [f32; 4],
    header_pos: [f32; 2],
    header_text: String,
    profile_badge_rect: [f32; 4],
    profile_badge_text: String,
    buttons: Vec<PanelButton>,
}

#[derive(Default)]
struct RefillTracker {
    was_playing: bool,
    last_selected_slot: usize,
    last_main_hand: Option<SlotSnapshot>,
    last_off_hand: Option<SlotSnapshot>,
    drop_suppress_ticks: u32,
    cooldown_ticks: u32,
    hud_notice: Option<(String, u32)>,
}

#[derive(Clone, Debug)]
struct ItemStaticMeta {
    id: String,
    default_name: String,
    max_stack: u32,
    max_damage: u32,
}

#[derive(Clone, Debug)]
struct KeyLabels {
    sort: String,
    lock: String,
    highlight: String,
    move_matching: String,
    dump_all: String,
    pull_matching: String,
    pull_all: String,
    apply_profile: String,
    save_profile: String,
    focus_search: String,
}

impl Default for KeyLabels {
    fn default() -> Self {
        Self {
            sort: "R".to_owned(),
            lock: "L".to_owned(),
            highlight: "H".to_owned(),
            move_matching: "M".to_owned(),
            dump_all: "G".to_owned(),
            pull_matching: "U".to_owned(),
            pull_all: "Shift+U".to_owned(),
            apply_profile: "P".to_owned(),
            save_profile: "Shift+P".to_owned(),
            focus_search: "Ctrl+F".to_owned(),
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    loaded: bool,
    persistent: PersistentState,
    pending_action: Option<PendingAction>,
    pending_throw_target: Option<ItemStack>,
    toast: Option<(String, u32)>,
    last_screen_size: [f32; 2],
    last_panel_bounds: Option<[f32; 4]>,
    dragging_panel: Option<[f32; 2]>,
    search_widgets: Widgets,
    search_field_id: Option<WidgetId>,
    dragging_search: Option<[f32; 2]>,
    last_search_bounds: Option<[f32; 4]>,
    last_search_matches: usize,
    was_inventory_open: bool,
    search_text_cache: HashMap<ItemKey, Vec<(Option<Components>, String)>>,
    search_text_cache_entries: usize,
    item_meta_cache: HashMap<ItemKey, ItemStaticMeta>,
    cached_settings: ModSettings,
    settings_loaded: bool,
    cached_keys: KeyLabels,
    cache_refresh_ticks: u32,
    refill: RefillTracker,
}

impl RuntimeState {
    fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), 50));
    }

    fn settings(&mut self, host: &dyn ClientHost) -> ModSettings {
        if !self.settings_loaded {
            self.refresh_host_caches(host);
        }
        self.cached_settings
    }

    fn refresh_host_caches(&mut self, host: &dyn ClientHost) {
        self.cached_settings = IpnMod::read_settings(host);
        self.settings_loaded = true;
        self.cached_keys = KeyLabels {
            sort: key_display(host, KEY_SORT, "R"),
            lock: key_display(host, KEY_LOCK_SLOT, "L"),
            highlight: key_display(host, KEY_TOGGLE_HIGHLIGHT, "H"),
            move_matching: key_display(host, KEY_MOVE_MATCHING, "M"),
            dump_all: key_display(host, KEY_DUMP_ALL, "G"),
            pull_matching: key_display(host, KEY_PULL_MATCHING, "U"),
            pull_all: key_display(host, KEY_PULL_ALL, "Shift+U"),
            apply_profile: key_display(host, KEY_APPLY_PROFILE, "P"),
            save_profile: key_display(host, KEY_SAVE_PROFILE, "Shift+P"),
            focus_search: key_display(host, KEY_FOCUS_SEARCH, "Ctrl+F"),
        };
    }

    fn ensure_search_widget(&mut self) -> WidgetId {
        if let Some(id) = self.search_field_id {
            return id;
        }
        let mut field = TextField::new([0.0, 0.0, 140.0, 14.0], 64);
        field.bordered = true;
        field.message = "Search chest items".to_owned();
        field.suggestion = Some("Search...".to_owned());
        let id = self.search_widgets.add_text_field(field);
        self.search_field_id = Some(id);
        id
    }

    fn search_query(&self) -> &str {
        self.search_field_id
            .and_then(|id| self.search_widgets.text_field(id))
            .map_or("", TextField::value)
    }

    fn is_search_focused(&self) -> bool {
        self.search_field_id
            .is_some_and(|id| self.search_widgets.focused() == Some(id))
    }

    fn sync_search_suggestion(&mut self) {
        if let Some(id) = self.search_field_id
            && let Some(field) = self.search_widgets.text_field_mut(id)
        {
            field.suggestion = if field.value().is_empty() {
                Some("Search...".to_owned())
            } else {
                None
            };
        }
    }

    fn set_search_query(&mut self, value: &str) {
        let id = self.ensure_search_widget();
        if let Some(field) = self.search_widgets.text_field_mut(id) {
            field.set_value(value);
        }
        self.sync_search_suggestion();
    }

    fn focus_search_select_all(&mut self, host: &dyn ClientHost) {
        let id = self.ensure_search_widget();
        self.search_widgets.set_focus(host, Some(id));
        if let Some(field) = self.search_widgets.text_field_mut(id) {
            field.edit.select_all();
        }
    }
}

#[derive(Default)]
pub struct IpnMod {
    state: Mutex<RuntimeState>,
}

impl IpnMod {
    fn ensure_loaded(&self, host: &dyn ClientHost, st: &mut RuntimeState) {
        if !st.settings_loaded {
            st.refresh_host_caches(host);
        }
        if st.loaded {
            return;
        }
        st.loaded = true;
        if let Ok(path) = ConfigPath::new(STATE_PATH)
            && let Ok(bytes) = host.data_files().read(&path)
            && let Ok(text) = std::str::from_utf8(&bytes)
        {
            st.persistent = PersistentState::decode(text);
        }
        st.persistent.normalize();
    }

    fn save_persistent(&self, host: &dyn ClientHost, persistent: &PersistentState) {
        if let Ok(path) = ConfigPath::new(STATE_PATH) {
            let encoded = persistent.encode();
            let _ = host.data_files().write(&path, encoded.as_bytes());
        }
    }

    fn read_settings(host: &dyn ClientHost) -> ModSettings {
        ModSettings {
            sort_order: SortOrder::from_choice(host.setting_choice(SET_SORT_RULE).unwrap_or(0)),
            fill_direction: FillDirection::from_choice(
                host.setting_choice(SET_FILL_DIR).unwrap_or(0),
            ),
            sort_hotbar: host.setting_bool(SET_SORT_HOTBAR).unwrap_or(false),
            transfer_hotbar: host.setting_bool(SET_TRANSFER_HOTBAR).unwrap_or(false),
            move_match_mode: MoveMatchMode::from_choice(
                host.setting_choice(SET_MOVE_MATCH_MODE).unwrap_or(0),
            ),
            auto_refill: host.setting_bool(SET_AUTO_REFILL).unwrap_or(true),
            refill_threshold: host
                .setting_f64(SET_REFILL_THRESHOLD)
                .map(|v| v.round().clamp(0.0, 20.0) as u32)
                .unwrap_or(3),
            show_buttons: host.setting_bool(SET_SHOW_BUTTONS).unwrap_or(true),
            highlight_matching: host.setting_bool(SET_HIGHLIGHT_MATCHING).unwrap_or(true),
            panel_position: PanelPosition::from_choice(
                host.setting_choice(SET_PANEL_POSITION).unwrap_or(0),
            ),
            prevent_locked_quickmove: host
                .setting_bool(SET_PROTECT_LOCKED_CLICKS)
                .unwrap_or(false),
            show_search_bar: host.setting_bool(SET_SHOW_SEARCH_BAR).unwrap_or(true),
            keep_search_query: host.setting_bool(SET_KEEP_SEARCH_QUERY).unwrap_or(false),
        }
    }
}

#[derive(Clone, Copy)]
struct ModSettings {
    sort_order: SortOrder,
    fill_direction: FillDirection,
    sort_hotbar: bool,
    transfer_hotbar: bool,
    move_match_mode: MoveMatchMode,
    auto_refill: bool,
    refill_threshold: u32,
    show_buttons: bool,
    highlight_matching: bool,
    panel_position: PanelPosition,
    prevent_locked_quickmove: bool,
    show_search_bar: bool,
    keep_search_query: bool,
}

impl Default for ModSettings {
    fn default() -> Self {
        Self {
            sort_order: SortOrder::CategoryName,
            fill_direction: FillDirection::RowByRow,
            sort_hotbar: false,
            transfer_hotbar: false,
            move_match_mode: MoveMatchMode::ExactItemAndNbt,
            auto_refill: true,
            refill_threshold: 3,
            show_buttons: true,
            highlight_matching: true,
            panel_position: PanelPosition::AutoLeft,
            prevent_locked_quickmove: false,
            show_search_bar: true,
            keep_search_query: false,
        }
    }
}

fn menu_has_storage(menu_name: Option<&str>, kinds: &[MenuSlotKind]) -> bool {
    if menu_name.is_some_and(|m| m.contains("crafter")) {
        return false;
    }
    kinds.iter().any(|k| matches!(k, MenuSlotKind::Storage))
}

fn friendly_item_name(item_id: &str) -> String {
    let path = item_id.split(':').nth(1).unwrap_or(item_id);
    let mut out = String::with_capacity(path.len());
    for (idx, word) in path.split('_').enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            for upper in first.to_uppercase() {
                out.push(upper);
            }
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        item_id.to_owned()
    } else {
        out
    }
}

fn get_item_meta<'a>(
    host: &dyn ClientHost,
    cache: &'a mut HashMap<ItemKey, ItemStaticMeta>,
    item: ItemKey,
) -> &'a ItemStaticMeta {
    cache.entry(item).or_insert_with(|| {
        let id = item.id(host);
        let default_name = friendly_item_name(&id);
        let max_stack = host.max_stack_size(item).unwrap_or(64).max(1);
        let max_damage = host.max_damage(item).unwrap_or(0);
        ItemStaticMeta {
            id,
            default_name,
            max_stack,
            max_damage,
        }
    })
}

fn snapshot_from_stack_cached(
    host: &dyn ClientHost,
    cache: &mut HashMap<ItemKey, ItemStaticMeta>,
    stack: &ItemStack,
    stack_group: u32,
    match_group: u32,
    exact_group: u32,
    resolve_custom_name: bool,
) -> SlotSnapshot {
    let item = stack.item();
    let meta = get_item_meta(host, cache, item);
    let display_name = if resolve_custom_name && stack.components().is_some() {
        let custom = host.hover_name(stack);
        if custom.is_empty() {
            meta.default_name.clone()
        } else {
            custom
        }
    } else {
        meta.default_name.clone()
    };
    SlotSnapshot {
        item_id: meta.id.clone(),
        display_name,
        max_stack: meta.max_stack,
        max_damage: meta.max_damage,
        damage: stack.damage(),
        count: stack.count(),
        enchanted: stack.components().is_some(),
        stack_group,
        match_group,
        exact_group,
    }
}

/// Captures `MenuSlotEntry` snapshots using cached `ItemKey` metadata and bucketed comparisons.
fn snapshot_open_menu_fast(
    host: &dyn ClientHost,
    cache: &mut HashMap<ItemKey, ItemStaticMeta>,
    menu: MenuRef,
    resolve_custom_names: bool,
) -> Option<Vec<MenuSlotEntry>> {
    let kinds = menu.slot_kinds(host)?;
    let mut stackable_by_item: HashMap<ItemKey, Vec<(u32, ItemStack)>> = HashMap::new();
    let mut match_by_item: HashMap<ItemKey, Vec<(u32, ItemStack)>> = HashMap::new();
    let mut exact_by_item: HashMap<ItemKey, Vec<(u32, ItemStack)>> = HashMap::new();
    let mut next_sg = 0u32;
    let mut next_mg = 0u32;
    let mut next_eg = 0u32;
    let mut entries = Vec::with_capacity(kinds.len());

    for (idx, kind) in kinds.iter().enumerate() {
        let player_slot = match kind {
            MenuSlotKind::Player(ps) => Some(*ps),
            _ => None,
        };
        let is_storage = matches!(kind, MenuSlotKind::Storage);
        let raw_opt = menu.slot(host, idx)?;
        let stack = raw_opt.as_ref().map(|s| {
            let key = s.item();
            let sg_bucket = stackable_by_item.entry(key).or_default();
            let sg = sg_bucket
                .iter()
                .find_map(|(id, rep)| rep.is_stackable_with(s).then_some(*id))
                .unwrap_or_else(|| {
                    let id = next_sg;
                    next_sg += 1;
                    sg_bucket.push((id, s.clone()));
                    id
                });

            let mg_bucket = match_by_item.entry(key).or_default();
            let mg = mg_bucket
                .iter()
                .find_map(|(id, rep)| (rep.components() == s.components()).then_some(*id))
                .unwrap_or_else(|| {
                    let id = next_mg;
                    next_mg += 1;
                    mg_bucket.push((id, s.clone()));
                    id
                });

            let eg_bucket = exact_by_item.entry(key).or_default();
            let eg = eg_bucket
                .iter()
                .find_map(|(id, rep)| (rep == s).then_some(*id))
                .unwrap_or_else(|| {
                    let id = next_eg;
                    next_eg += 1;
                    eg_bucket.push((id, s.clone()));
                    id
                });

            snapshot_from_stack_cached(host, cache, s, sg, mg, eg, resolve_custom_names)
        });
        entries.push(MenuSlotEntry {
            menu_index: idx,
            player_slot,
            is_storage,
            stack,
        });
    }
    Some(entries)
}

fn to_menu_click(click: PlanClick) -> MenuClick {
    match click {
        PlanClick::LeftPickup(slot) => MenuClick::Pickup {
            slot: Some(slot),
            button: MouseButton::Left,
        },
        PlanClick::QuickMove(slot) => MenuClick::QuickMove { slot },
        PlanClick::SwapHotbar { slot, hotbar } => MenuClick::Swap {
            slot,
            with: SwapWith::Hotbar(hotbar),
        },
        PlanClick::SwapOffhand { slot } => MenuClick::Swap {
            slot,
            with: SwapWith::Offhand,
        },
        PlanClick::ThrowWholeStack(slot) => MenuClick::Throw {
            slot,
            whole_stack: true,
        },
    }
}

fn key_display(host: &dyn ClientHost, key_id: &str, fallback: &str) -> String {
    let Some((key, mods)) = host.mod_key(MOD_ID, key_id) else {
        return fallback.to_owned();
    };
    let short_key = key
        .strip_prefix("key.keyboard.")
        .unwrap_or(&key)
        .replace('_', " ")
        .to_ascii_uppercase();
    let mut parts = Vec::new();
    if mods.control() {
        parts.push("Ctrl".to_owned());
    }
    if mods.alt() {
        parts.push("Alt".to_owned());
    }
    if mods.shift() {
        parts.push("Shift".to_owned());
    }
    parts.push(short_key);
    parts.join("+")
}

fn matches_mod_key(host: &dyn ClientHost, key_id: &str, key: &str, modifiers: u32) -> bool {
    let Some((bound_key, bound_mods)) = host.mod_key(MOD_ID, key_id) else {
        return false;
    };
    // Compare shift/ctrl/alt (bits 0..3) while ignoring lock bits like caps lock if any.
    let masked = modifiers & 0b0111;
    let bound_masked = bound_mods.bits & 0b0111;
    key == bound_key && masked == bound_masked
}

/// Matches Minecraft's `AbstractContainerScreen.isHovering` hitbox: the 16x16 slot box expands
/// by 1 px on each side so the gaps between slots hover identically to item tooltips.
fn point_in_slot_rect(pt: [f32; 2], slot_rect: [f32; 4]) -> bool {
    let [x, y, w, h] = slot_rect;
    pt[0] >= x - 1.0 && pt[0] < x + w + 1.0 && pt[1] >= y - 1.0 && pt[1] < y + h + 1.0
}

fn hovered_slot_kind(
    host: &dyn ClientHost,
    menu: MenuRef,
    mouse: [f32; 2],
) -> Option<(usize, MenuSlotKind)> {
    let boxes = menu.slot_boxes(host)?;
    let kinds = menu.slot_kinds(host)?;
    for (idx, (box_opt, kind)) in boxes.into_iter().zip(kinds).enumerate() {
        if let Some(rect) = box_opt
            && point_in_slot_rect(mouse, rect)
        {
            return Some((idx, kind));
        }
    }
    None
}

fn point_in_rect(pt: [f32; 2], r: [f32; 4]) -> bool {
    pt[0] >= r[0] && pt[0] < r[0] + r[2] && pt[1] >= r[1] && pt[1] < r[1] + r[3]
}

fn rects_overlap(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[0] + b[2] && a[0] + a[2] > b[0] && a[1] < b[1] + b[3] && a[1] + a[3] > b[1]
}

fn panel_height(has_storage: bool) -> f32 {
    let mut h = PANEL_PAD + 11.0;
    if has_storage {
        h += 5.0 * (BTN_H + BTN_GAP) + 3.0;
    }
    h += 2.0 * (BTN_H + BTN_GAP) + 3.0;
    h += 2.0 * (BTN_H + BTN_GAP) + BTN_H + PANEL_PAD;
    h
}

fn resolve_panel_origin(
    screen_rect: [f32; 4],
    screen_size: [f32; 2],
    panel_h: f32,
    position: PanelPosition,
    custom_offset: Option<[f32; 2]>,
    has_storage: bool,
) -> [f32; 2] {
    let clamp_xy = |x: f32, y: f32| -> [f32; 2] {
        let cx = if screen_size[0] > PANEL_W + 4.0 {
            x.clamp(2.0, screen_size[0] - PANEL_W - 2.0)
        } else {
            x.max(2.0)
        };
        let cy = if screen_size[1] > panel_h + 4.0 {
            y.clamp(2.0, screen_size[1] - panel_h - 2.0)
        } else {
            y.max(2.0)
        };
        [cx, cy]
    };

    if let Some([dx, dy]) = custom_offset {
        return clamp_xy(screen_rect[0] + dx, screen_rect[1] + dy);
    }

    let y = screen_rect[1];
    let left_x = screen_rect[0] - PANEL_W - 4.0;
    let right_x = screen_rect[0] + screen_rect[2] + 4.0;

    match position {
        PanelPosition::Left => clamp_xy(left_x, y),
        PanelPosition::Right => clamp_xy(right_x, y),
        PanelPosition::AutoLeft => {
            // Heuristic: Recipe Book opens to the left when player inventory/crafting shifts right past center.
            let recipe_shifted = !has_storage
                && screen_size[0] > 0.0
                && screen_rect[2] < 200.0
                && screen_rect[0] > (screen_size[0] - screen_rect[2]) * 0.5 + 24.0;
            if recipe_shifted {
                let far_left = (screen_rect[0] - 148.0) - PANEL_W - 4.0;
                if far_left >= 2.0 {
                    return clamp_xy(far_left, y);
                }
                return clamp_xy(right_x, y);
            }
            if left_x >= 2.0 {
                clamp_xy(left_x, y)
            } else {
                clamp_xy(right_x, y)
            }
        }
    }
}

fn build_panel_layout(
    keys: &KeyLabels,
    screen_rect: [f32; 4],
    screen_size: [f32; 2],
    has_storage: bool,
    position: PanelPosition,
    move_match_mode: MoveMatchMode,
    highlight_matching: bool,
    persistent: &PersistentState,
) -> PanelLayout {
    let total_h = panel_height(has_storage);
    let [x, y] = resolve_panel_origin(
        screen_rect,
        screen_size,
        total_h,
        position,
        persistent.custom_offset,
        has_storage,
    );
    let inner_x = x + PANEL_PAD;
    let inner_w = PANEL_W - PANEL_PAD * 2.0;

    let mut cy = y + PANEL_PAD;
    let header_rect = [x, y, PANEL_W, HEADER_H];
    let header_pos = [inner_x + 1.0, cy + 1.0];
    let locked_count = persistent.locked_slots.len();
    let header_text = if locked_count > 0 {
        format!("IPN [{locked_count}L]")
    } else {
        "IPN".to_owned()
    };
    cy += 11.0;

    let mut buttons = Vec::new();
    let k_sort = keys.sort.as_str();
    let k_lock = keys.lock.as_str();
    let k_hl = keys.highlight.as_str();
    let k_move = keys.move_matching.as_str();
    let k_dump = keys.dump_all.as_str();
    let k_pull = keys.pull_matching.as_str();
    let k_pull_all = keys.pull_all.as_str();
    let k_apply = keys.apply_profile.as_str();
    let k_save = keys.save_profile.as_str();

    if has_storage {
        buttons.push(PanelButton {
            kind: ButtonKind::SortChest,
            rect: [inner_x, cy, inner_w, BTN_H],
            label: "Sort Chest".to_owned(),
            active: false,
            tooltip_title: format!("Sort Container ({k_sort})"),
            tooltip_hint: "Merge & organize container storage slots".to_owned(),
        });
        cy += BTN_H + BTN_GAP;

        buttons.push(PanelButton {
            kind: ButtonKind::MoveMatching,
            rect: [inner_x, cy, inner_w, BTN_H],
            label: "Move Match".to_owned(),
            active: false,
            tooltip_title: format!("Move Matching ({k_move})"),
            tooltip_hint: match move_match_mode {
                MoveMatchMode::ExactItemAndNbt => {
                    "Move matching unlocked items (same item & NBT) into container".to_owned()
                }
                MoveMatchMode::TopUpExistingOnly => {
                    "Top up existing container stacks only (never fills empty slots)".to_owned()
                }
            },
        });
        cy += BTN_H + BTN_GAP;

        buttons.push(PanelButton {
            kind: ButtonKind::DumpAll,
            rect: [inner_x, cy, inner_w, BTN_H],
            label: "Dump All".to_owned(),
            active: false,
            tooltip_title: format!("Dump Inventory ({k_dump})"),
            tooltip_hint: "Move all unlocked main slots into container".to_owned(),
        });
        cy += BTN_H + BTN_GAP;

        buttons.push(PanelButton {
            kind: ButtonKind::PullMatching,
            rect: [inner_x, cy, inner_w, BTN_H],
            label: "Pull Match".to_owned(),
            active: false,
            tooltip_title: format!("Pull Matching ({k_pull})"),
            tooltip_hint: "Pull container items matching your inventory".to_owned(),
        });
        cy += BTN_H + BTN_GAP;

        buttons.push(PanelButton {
            kind: ButtonKind::PullAll,
            rect: [inner_x, cy, inner_w, BTN_H],
            label: "Pull All".to_owned(),
            active: false,
            tooltip_title: format!("Pull All ({k_pull_all})"),
            tooltip_hint: "Take all container items until inventory is full".to_owned(),
        });
        cy += BTN_H + BTN_GAP + 3.0;
    }

    buttons.push(PanelButton {
        kind: ButtonKind::SortInv,
        rect: [inner_x, cy, inner_w, BTN_H],
        label: "Sort Inv".to_owned(),
        active: false,
        tooltip_title: format!("Sort Inventory ({k_sort})"),
        tooltip_hint: "Sort main inventory while preserving locked slots".to_owned(),
    });
    cy += BTN_H + BTN_GAP;

    let lock_w = 42.0;
    let hl_w = inner_w - lock_w - 2.0;
    buttons.push(PanelButton {
        kind: ButtonKind::ToggleLockMode,
        rect: [inner_x, cy, lock_w, BTN_H],
        label: if persistent.lock_mode {
            "Lock:ON"
        } else {
            "Lock"
        }
        .to_owned(),
        active: persistent.lock_mode,
        tooltip_title: format!("Slot Lock Mode ({k_lock} / Alt+Click)"),
        tooltip_hint: "Click slots to lock/unlock them against Sort, Dump & Throw".to_owned(),
    });
    buttons.push(PanelButton {
        kind: ButtonKind::ToggleHighlight,
        rect: [inner_x + lock_w + 2.0, cy, hl_w, BTN_H],
        label: "HL".to_owned(),
        active: highlight_matching,
        tooltip_title: format!("Highlight Matching ({k_hl})"),
        tooltip_hint: "Highlight slots matching the hovered item across chest and inventory"
            .to_owned(),
    });
    cy += BTN_H + BTN_GAP + 3.0;

    // Profile selector row: `<` [P1 (4)] `>`
    let nav_w = 13.0;
    buttons.push(PanelButton {
        kind: ButtonKind::PrevProfile,
        rect: [inner_x, cy, nav_w, BTN_H],
        label: "<".to_owned(),
        active: false,
        tooltip_title: "Previous Profile".to_owned(),
        tooltip_hint: "Switch to previous saved loadout preset".to_owned(),
    });
    buttons.push(PanelButton {
        kind: ButtonKind::NextProfile,
        rect: [inner_x + inner_w - nav_w, cy, nav_w, BTN_H],
        label: ">".to_owned(),
        active: false,
        tooltip_title: "Next Profile".to_owned(),
        tooltip_hint: "Switch to next saved loadout preset".to_owned(),
    });
    let active_idx = persistent.active_profile.min(PROFILE_COUNT.saturating_sub(1));
    let prof = &persistent.profiles[active_idx];
    let profile_badge_rect = [
        inner_x + nav_w + 1.0,
        cy,
        inner_w - nav_w * 2.0 - 2.0,
        BTN_H,
    ];
    let profile_badge_text = format!("{} ({})", prof.name, prof.rules.len());
    cy += BTN_H + BTN_GAP;

    buttons.push(PanelButton {
        kind: ButtonKind::ApplyProfile,
        rect: [inner_x, cy, inner_w, BTN_H],
        label: format!("Apply {}", prof.name),
        active: false,
        tooltip_title: format!("Apply {} ({k_apply})", prof.name),
        tooltip_hint: "Equip saved Hotbar/Armor/Offhand loadout".to_owned(),
    });
    cy += BTN_H + BTN_GAP;

    buttons.push(PanelButton {
        kind: ButtonKind::SaveProfile,
        rect: [inner_x, cy, inner_w, BTN_H],
        label: format!("Save {}", prof.name),
        active: false,
        tooltip_title: format!("Save {} ({k_save})", prof.name),
        tooltip_hint: "Capture current Hotbar, Armor & Offhand into preset".to_owned(),
    });
    cy += BTN_H + PANEL_PAD;

    let h = cy - y;
    PanelLayout {
        bounds: [x, y, PANEL_W, h],
        header_rect,
        header_pos,
        header_text,
        profile_badge_rect,
        profile_badge_text,
        buttons,
    }
}

fn draw_bordered_rect(draw: &mut dyn mod_api::DrawContext, r: [f32; 4], bg: u32, border: u32) {
    draw.fill(r, bg);
    draw.fill([r[0], r[1], r[2], 1.0], border);
    draw.fill([r[0], r[1] + r[3] - 1.0, r[2], 1.0], border);
    draw.fill([r[0], r[1], 1.0, r[3]], border);
    draw.fill([r[0] + r[2] - 1.0, r[1], 1.0, r[3]], border);
}

fn draw_grip_dots(draw: &mut dyn mod_api::DrawContext, gx: f32, gy: f32, color: u32) {
    for row in 0..3 {
        for col in 0..2 {
            draw.fill(
                [gx + (col as f32) * 2.0, gy + (row as f32) * 2.0, 1.0, 1.0],
                color,
            );
        }
    }
}

/// Draws a cyan match highlight around slots whose item ID and NBT match the hovered stack.
fn draw_slot_match_overlay(
    draw: &mut dyn mod_api::DrawContext,
    slot_rect: [f32; 4],
    is_hovered: bool,
) {
    let fill = if is_hovered { 0x1833EEFF } else { 0x3C33EEFF };
    draw_bordered_rect(draw, slot_rect, fill, 0xCC55EEFF);
}

/// Draws a crisp green search highlight around slots matching the active search query.
fn draw_slot_search_match_overlay(
    draw: &mut dyn mod_api::DrawContext,
    slot_rect: [f32; 4],
    is_hovered: bool,
) {
    let fill = if is_hovered { 0x2044FF66 } else { 0x3644FF66 };
    draw_bordered_rect(draw, slot_rect, fill, 0xEE55FF77);
}

/// Darkens non-matching slots (heavier on occupied slots, lighter on empty slots).
fn draw_slot_search_dim_overlay(
    draw: &mut dyn mod_api::DrawContext,
    slot_rect: [f32; 4],
    occupied: bool,
) {
    let fill = if occupied { 0xB8000000 } else { 0x60000000 };
    draw.fill(slot_rect, fill);
}

fn cached_stack_search_text<'a>(
    host: &dyn ClientHost,
    st: &'a mut RuntimeState,
    stack: &ItemStack,
) -> &'a str {
    let key = stack.item();
    let comps = stack.components();
    let pos = if let Some(pos) = st
        .search_text_cache
        .get(&key)
        .and_then(|list| list.iter().position(|(c, _)| c.as_ref() == comps))
    {
        pos
    } else {
        if st.search_text_cache_entries >= 512 {
            st.search_text_cache.clear();
            st.search_text_cache_entries = 0;
        }
        let (item_id, friendly_name) = {
            let meta = get_item_meta(host, &mut st.item_meta_cache, key);
            (meta.id.clone(), meta.default_name.clone())
        };
        let hover = host.hover_name(stack);
        let tooltip = ItemResource::of(stack).tooltip(host, false, false);
        let haystack = build_searchable_haystack(
            &item_id,
            &friendly_name,
            &hover,
            tooltip.as_deref(),
            comps,
        );
        let bucket = st.search_text_cache.entry(key).or_default();
        bucket.push((comps.cloned(), haystack));
        st.search_text_cache_entries += 1;
        bucket.len() - 1
    };
    &st.search_text_cache[&key][pos].1
}

fn is_passthrough_nav_or_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "key.keyboard.left.shift"
            | "key.keyboard.right.shift"
            | "key.keyboard.left.control"
            | "key.keyboard.right.control"
            | "key.keyboard.left.alt"
            | "key.keyboard.right.alt"
            | "key.keyboard.left.super"
            | "key.keyboard.right.super"
            | "key.keyboard.caps.lock"
            | "key.keyboard.f1"
            | "key.keyboard.f2"
            | "key.keyboard.f3"
            | "key.keyboard.f4"
            | "key.keyboard.f5"
            | "key.keyboard.f6"
            | "key.keyboard.f7"
            | "key.keyboard.f8"
            | "key.keyboard.f9"
            | "key.keyboard.f10"
            | "key.keyboard.f11"
            | "key.keyboard.f12"
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_search_bar(
    draw: &mut dyn mod_api::DrawContext,
    host: &dyn ClientHost,
    st: &mut RuntimeState,
    s_layout: &crate::search::SearchBarLayout,
    mouse: [f32; 2],
    has_query: bool,
    match_count: usize,
    focused: bool,
    is_dragging: bool,
) {
    let border_color = if focused {
        0xFFFFCC55
    } else if has_query && match_count > 0 {
        0xFF55EE88
    } else if has_query {
        0xFFFF6666
    } else {
        0xFF4F5563
    };
    draw_bordered_rect(draw, s_layout.bounds, 0xC414161B, border_color);

    let grip_hovered = point_in_rect(mouse, s_layout.grip_rect);
    if is_dragging || grip_hovered {
        draw.fill(s_layout.grip_rect, 0x38FFCC55);
    }
    let grip_color = if is_dragging || grip_hovered {
        0xFFFFCC55
    } else {
        0xFF7B8499
    };
    draw_grip_dots(
        draw,
        s_layout.grip_rect[0] + 3.0,
        s_layout.grip_rect[1] + 4.0,
        grip_color,
    );

    let id = st.ensure_search_widget();
    if let Some(field) = st.search_widgets.text_field_mut(id) {
        field.rect = s_layout.field_rect;
    }
    st.sync_search_suggestion();
    let _ = st.search_widgets.draw(draw, host, mouse);

    if let Some(cr) = s_layout.count_rect {
        draw_bordered_rect(draw, cr, 0xFF1A1E26, 0xFF3E4450);
        let count_text = format!("{match_count}");
        let tw = draw.text_width(&count_text);
        let tx = cr[0] + ((cr[2] - tw) * 0.5).max(1.0);
        let ty = cr[1] + 3.0;
        let color = if match_count > 0 {
            0xFF77FF88
        } else {
            0xFFFF7777
        };
        draw.text(&count_text, [tx, ty], color, true);
    }

    let clear_hovered = point_in_rect(mouse, s_layout.clear_rect);
    let (clear_bg, clear_border, clear_fg) = if has_query {
        (
            if clear_hovered {
                0xFF9E3339
            } else {
                0xFF6E2529
            },
            0xFFFF7777,
            0xFFFFFFFF,
        )
    } else {
        (0xFF222630, 0xFF3E4450, 0xFF5C6370)
    };
    draw_bordered_rect(draw, s_layout.clear_rect, clear_bg, clear_border);
    let xw = draw.text_width("x");
    let xx = s_layout.clear_rect[0] + ((s_layout.clear_rect[2] - xw) * 0.5).max(1.0);
    let xy = s_layout.clear_rect[1] + 3.0;
    draw.text("x", [xx, xy], clear_fg, true);
}

/// Draws a crisp amber outline and tiny 4x5 corner padlock glyph without hiding stack counts.
fn draw_slot_lock_overlay(draw: &mut dyn mod_api::DrawContext, slot_rect: [f32; 4]) {
    let [x, y, w, h] = slot_rect;
    let border = 0xCCFFAA22;
    draw.fill([x, y, w, 1.0], border);
    draw.fill([x, y + h - 1.0, w, 1.0], border);
    draw.fill([x, y, 1.0, h], border);
    draw.fill([x + w - 1.0, y, 1.0, h], border);
    // Tiny corner padlock badge at top-left (4x5 px inside slot).
    draw.fill([x + 1.0, y + 1.0, 4.0, 5.0], 0xB0181208);
    draw.fill([x + 2.0, y + 1.0, 2.0, 1.0], 0xFFFFCC44); // shackle top
    draw.fill([x + 1.0, y + 2.0, 1.0, 1.0], 0xFFFFCC44); // shackle left
    draw.fill([x + 4.0, y + 2.0, 1.0, 1.0], 0xFFFFCC44); // shackle right
    draw.fill([x + 1.0, y + 3.0, 4.0, 3.0], 0xFFEEAA22); // lock body
}

fn toggle_highlight_setting(host: &dyn ClientHost, state: &Mutex<RuntimeState>) -> bool {
    let next = {
        let mut st = state.lock().unwrap();
        let val = !st.cached_settings.highlight_matching;
        st.cached_settings.highlight_matching = val;
        st.set_toast(if val {
            "Highlight Match: ON"
        } else {
            "Highlight Match: OFF"
        });
        val
    };
    host.set_setting(
        MOD_ID,
        SET_HIGHLIGHT_MATCHING,
        if next { "true" } else { "false" },
    );
    next
}

enum PreparedAction {
    Message(String),
    SaveProfile {
        idx: usize,
        rules: Vec<crate::model::ProfileSlotRule>,
    },
    ClickBatch {
        menu: MenuRef,
        clicks: Vec<MenuClick>,
        continue_action: Option<PendingAction>,
        toast_msg: Option<String>,
    },
    NoOp,
}

fn prepare_pending_action(
    host: &dyn ClientHost,
    st: &mut RuntimeState,
    action: PendingAction,
) -> PreparedAction {
    let Some(player) = host.local_player() else {
        return PreparedAction::NoOp;
    };
    if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
        return PreparedAction::Message("Skipped in Creative/Spectator".to_owned());
    }
    let Some(menu) = player.container_menu(host) else {
        return PreparedAction::NoOp;
    };
    if menu.carried(host).is_none_or(|c| c.is_some()) {
        return PreparedAction::Message("Drop held cursor item first".to_owned());
    }
    let settings = st.settings(host);
    let resolve_names = matches!(action, PendingAction::SaveProfile(_))
        || (settings.sort_order == SortOrder::NameAsc
            && matches!(
                action,
                PendingAction::SortContainer | PendingAction::SortInventory | PendingAction::SortAuto { .. }
            ));
    let Some(entries) = snapshot_open_menu_fast(host, &mut st.item_meta_cache, menu, resolve_names) else {
        return PreparedAction::NoOp;
    };
    let has_storage = entries.iter().any(|e| e.is_storage);

    match action {
        PendingAction::SortContainer => {
            if !has_storage {
                return PreparedAction::NoOp;
            }
            let ordered = collect_sortable_menu_indices(
                &entries,
                &st.persistent.locked_slots,
                true,
                settings.sort_hotbar,
                settings.fill_direction,
            );
            let (planned, has_more) = plan_sort_clicks_bounded(
                &entries,
                &ordered,
                settings.sort_order,
                MAX_CLICKS_PER_TICK,
            );
            let clicks: Vec<MenuClick> = planned.into_iter().map(to_menu_click).collect();
            if clicks.is_empty() {
                PreparedAction::Message("Chest sorted".to_owned())
            } else {
                PreparedAction::ClickBatch {
                    menu,
                    continue_action: has_more.then_some(PendingAction::SortContainer),
                    toast_msg: (!has_more).then(|| "Chest sorted".to_owned()),
                    clicks,
                }
            }
        }
        PendingAction::SortInventory => {
            let ordered = collect_sortable_menu_indices(
                &entries,
                &st.persistent.locked_slots,
                false,
                settings.sort_hotbar,
                settings.fill_direction,
            );
            let (planned, has_more) = plan_sort_clicks_bounded(
                &entries,
                &ordered,
                settings.sort_order,
                MAX_CLICKS_PER_TICK,
            );
            let clicks: Vec<MenuClick> = planned.into_iter().map(to_menu_click).collect();
            if clicks.is_empty() {
                PreparedAction::Message("Inventory sorted".to_owned())
            } else {
                PreparedAction::ClickBatch {
                    menu,
                    continue_action: has_more.then_some(PendingAction::SortInventory),
                    toast_msg: (!has_more).then(|| "Inventory sorted".to_owned()),
                    clicks,
                }
            }
        }
        PendingAction::SortAuto {
            hovered_storage,
            hovered_player,
        } => {
            let sort_storage = if hovered_storage {
                true
            } else if hovered_player {
                false
            } else {
                has_storage
            };
            let next = if sort_storage && has_storage {
                PendingAction::SortContainer
            } else {
                PendingAction::SortInventory
            };
            prepare_pending_action(host, st, next)
        }
        PendingAction::Transfer(mode) => {
            if !has_storage {
                return PreparedAction::Message("Open a container first".to_owned());
            }
            let (planned, has_more) = plan_transfer_clicks_bounded(
                &entries,
                &st.persistent.locked_slots,
                mode,
                settings.move_match_mode,
                settings.transfer_hotbar,
                MAX_CLICKS_PER_TICK,
            );
            let clicks: Vec<MenuClick> = planned.into_iter().map(to_menu_click).collect();
            let label = match mode {
                TransferMode::MoveMatching => "Moved match",
                TransferMode::DumpAll => "Dumped",
                TransferMode::PullMatching => "Pulled match",
                TransferMode::PullAll => "Pulled all",
            };
            if clicks.is_empty() {
                PreparedAction::Message(format!("{label}: done"))
            } else {
                let is_top_up = mode == TransferMode::MoveMatching
                    && settings.move_match_mode == MoveMatchMode::TopUpExistingOnly;
                PreparedAction::ClickBatch {
                    menu,
                    continue_action: has_more.then_some(PendingAction::Transfer(mode)),
                    toast_msg: (!has_more).then(|| {
                        if is_top_up {
                            "Moved match: topped up".to_owned()
                        } else {
                            format!("{label}: {} stack(s)", clicks.len())
                        }
                    }),
                    clicks,
                }
            }
        }
        PendingAction::ThrowMatching {
            target_storage,
            include_hotbar,
        } => {
            if target_storage && !has_storage {
                st.pending_throw_target = None;
                return PreparedAction::NoOp;
            }
            let Some(target) = st.pending_throw_target.clone() else {
                return PreparedAction::NoOp;
            };
            let target_mg = entries.iter().find_map(|e| {
                e.stack.as_ref().and_then(|snap| {
                    menu.slot(host, e.menu_index)
                        .flatten()
                        .is_some_and(|s| s.same_item(&target) && s.components() == target.components())
                        .then_some(snap.match_group)
                })
            });
            let Some(mg) = target_mg else {
                st.pending_throw_target = None;
                return PreparedAction::Message("Threw matching: done".to_owned());
            };
            let (planned, has_more) = plan_throw_clicks_bounded(
                &entries,
                &st.persistent.locked_slots,
                target_storage,
                Some(mg),
                include_hotbar,
                MAX_CLICKS_PER_TICK,
            );
            if !has_more || planned.is_empty() {
                st.pending_throw_target = None;
            }
            if planned.is_empty() {
                let any_locked = !target_storage
                    && entries.iter().any(|e| {
                        e.player_slot
                            .is_some_and(|ps| st.persistent.locked_slots.contains(&ps))
                            && e.stack.as_ref().is_some_and(|s| s.match_group == mg)
                    });
                return PreparedAction::Message(if any_locked {
                    "Slot locked!".to_owned()
                } else {
                    "Threw matching: done".to_owned()
                });
            }
            let clicks: Vec<MenuClick> = planned.into_iter().map(to_menu_click).collect();
            PreparedAction::ClickBatch {
                menu,
                continue_action: has_more.then_some(PendingAction::ThrowMatching {
                    target_storage,
                    include_hotbar,
                }),
                toast_msg: (!has_more).then(|| "Threw matching: done".to_owned()),
                clicks,
            }
        }
        PendingAction::ThrowAll { target_storage } => {
            if target_storage && !has_storage {
                return PreparedAction::Message("Open a container first".to_owned());
            }
            let (planned, has_more) = plan_throw_clicks_bounded(
                &entries,
                &st.persistent.locked_slots,
                target_storage,
                None,
                settings.transfer_hotbar,
                MAX_CLICKS_PER_TICK,
            );
            if planned.is_empty() {
                return PreparedAction::Message("Nothing to throw".to_owned());
            }
            let clicks: Vec<MenuClick> = planned.into_iter().map(to_menu_click).collect();
            PreparedAction::ClickBatch {
                menu,
                continue_action: has_more.then_some(PendingAction::ThrowAll { target_storage }),
                toast_msg: (!has_more).then(|| "Threw all: done".to_owned()),
                clicks,
            }
        }
        PendingAction::SaveProfile(idx) => {
            if idx >= st.persistent.profiles.len() {
                return PreparedAction::NoOp;
            }
            let rules = capture_profile_rules(&entries);
            PreparedAction::SaveProfile { idx, rules }
        }
        PendingAction::ApplyProfile(idx) => {
            let Some(prof) = st.persistent.profiles.get(idx) else {
                return PreparedAction::NoOp;
            };
            if prof.rules.is_empty() {
                return PreparedAction::Message(format!("{} is empty", prof.name));
            }
            let planned = plan_apply_profile(&entries, &st.persistent.locked_slots, prof);
            if planned.is_empty() {
                PreparedAction::Message(format!("{} equipped", prof.name))
            } else {
                let mut clicks: Vec<MenuClick> = planned.into_iter().map(to_menu_click).collect();
                // Keep batches small so profile swaps with armor stay well under budget.
                let has_more = clicks.len() > MAX_CLICKS_PER_TICK;
                if has_more {
                    let mut cut = MAX_CLICKS_PER_TICK;
                    while cut > 1 && matches!(clicks.get(cut - 1), Some(MenuClick::Pickup { .. })) {
                        // Avoid splitting a Pickup armor swap sequence mid-cursor.
                        cut -= 1;
                    }
                    clicks.truncate(cut.max(1));
                }
                PreparedAction::ClickBatch {
                    menu,
                    continue_action: has_more.then_some(PendingAction::ApplyProfile(idx)),
                    toast_msg: (!has_more).then(|| format!("Applied {}", prof.name)),
                    clicks,
                }
            }
        }
    }
}

fn slot_needs_refill_fast(
    prev: Option<&SlotSnapshot>,
    curr: Option<&SlotSnapshot>,
    threshold: u32,
) -> bool {
    if let Some(c) = curr
        && c.is_damageable()
        && threshold > 0
        && c.remaining_durability() <= threshold
    {
        return true;
    }
    prev.is_some() && curr.is_none()
}

fn prepare_auto_refill_click(
    host: &dyn ClientHost,
    st: &mut RuntimeState,
    settings: ModSettings,
) -> Option<(MenuRef, usize, MenuClick, String)> {
    if !settings.auto_refill {
        st.refill.was_playing = false;
        return None;
    }
    if host.is_paused() || host.observed_screen_id().is_some() {
        st.refill.was_playing = false;
        st.refill.last_main_hand = None;
        st.refill.last_off_hand = None;
        return None;
    }

    let drop_down = host
        .mapping("key.drop")
        .ok()
        .flatten()
        .is_some_and(|m| m.down);
    if drop_down {
        st.refill.drop_suppress_ticks = 12;
    } else if st.refill.drop_suppress_ticks > 0 {
        st.refill.drop_suppress_ticks -= 1;
    }
    if st.refill.cooldown_ticks > 0 {
        st.refill.cooldown_ticks -= 1;
    }

    let Some(player) = host.local_player() else {
        st.refill.was_playing = false;
        return None;
    };
    if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
        st.refill.was_playing = false;
        return None;
    }
    let selected = player.selected_slot(host)?;
    if selected > 8 {
        return None;
    }
    let menu = player.container_menu(host)?;
    // During Screen::Playing, vanilla window 0 InventoryMenu always has 46 slots:
    // 9..=35 = Player(9..=35), 36..=44 = Hotbar(0..=8), 45 = Offhand(40).
    if menu.slot_count(host) != Some(46) || menu.carried(host) != Some(None) {
        st.refill.was_playing = false;
        return None;
    }

    let curr_main = menu.slot(host, 36 + selected).flatten().map(|s| {
        snapshot_from_stack_cached(host, &mut st.item_meta_cache, &s, 0, 0, 0, false)
    });
    let curr_off = menu.slot(host, 45).flatten().map(|s| {
        snapshot_from_stack_cached(host, &mut st.item_meta_cache, &s, 1, 1, 1, false)
    });

    let same_context = st.refill.was_playing
        && st.refill.last_selected_slot == selected
        && st.refill.drop_suppress_ticks == 0
        && st.refill.cooldown_ticks == 0;

    let need_main = same_context
        && slot_needs_refill_fast(
            st.refill.last_main_hand.as_ref(),
            curr_main.as_ref(),
            settings.refill_threshold,
        );
    let need_off = same_context
        && !need_main
        && slot_needs_refill_fast(
            st.refill.last_off_hand.as_ref(),
            curr_off.as_ref(),
            settings.refill_threshold,
        );

    let mut planned = None;
    // Fast path: only scan main inventory slots `9..=35` on ticks when a hand actually needs refill!
    if need_main || need_off {
        let mut main_inv = Vec::with_capacity(27);
        for ps in 9..=35usize {
            let opt = menu.slot(host, ps).flatten().map(|s| {
                snapshot_from_stack_cached(host, &mut st.item_meta_cache, &s, 0, 0, 0, false)
            });
            main_inv.push((ps, ps as u32, opt));
        }

        if need_main
            && let Some((click, name)) = plan_auto_refill(
                st.refill.last_main_hand.as_ref(),
                curr_main.as_ref(),
                &main_inv,
                &st.persistent.locked_slots,
                RefillHand::Hotbar(selected as u8),
                settings.refill_threshold,
            )
        {
            planned = Some((menu, selected, to_menu_click(click), format!("Refilled {name}")));
        } else if need_off
            && let Some((click, name)) = plan_auto_refill(
                st.refill.last_off_hand.as_ref(),
                curr_off.as_ref(),
                &main_inv,
                &st.persistent.locked_slots,
                RefillHand::Offhand,
                settings.refill_threshold,
            )
        {
            planned = Some((
                menu,
                selected,
                to_menu_click(click),
                format!("Refilled offhand {name}"),
            ));
        }
    }

    st.refill.last_main_hand = curr_main;
    st.refill.last_off_hand = curr_off;
    st.refill.last_selected_slot = selected;
    st.refill.was_playing = true;
    planned
}

#[mod_api::horizon_mod]
impl Hooks for IpnMod {
    fn register(&self, registrar: &mut dyn Registrar, _cx: &RegistrationContext) {
        let _ = registrar.register_setting(
            SettingSpec::choice(
                SET_SORT_RULE,
                "ipn.setting.sort_rule",
                [
                    "ipn.setting.sort_rule.category",
                    "ipn.setting.sort_rule.name",
                    "ipn.setting.sort_rule.id",
                    "ipn.setting.sort_rule.count",
                ],
                0,
            )
            .tooltip("ipn.setting.sort_rule.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::choice(
                SET_FILL_DIR,
                "ipn.setting.fill_direction",
                ["ipn.setting.fill_direction.row", "ipn.setting.fill_direction.col"],
                0,
            )
            .tooltip("ipn.setting.fill_direction.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(SET_SORT_HOTBAR, "ipn.setting.sort_hotbar", false)
                .tooltip("ipn.setting.sort_hotbar.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(SET_TRANSFER_HOTBAR, "ipn.setting.transfer_hotbar", false)
                .tooltip("ipn.setting.transfer_hotbar.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::choice(
                SET_MOVE_MATCH_MODE,
                "ipn.setting.move_match_mode",
                [
                    "ipn.setting.move_match_mode.exact",
                    "ipn.setting.move_match_mode.top_up",
                ],
                0,
            )
            .tooltip("ipn.setting.move_match_mode.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(SET_AUTO_REFILL, "ipn.setting.auto_refill", true)
                .tooltip("ipn.setting.auto_refill.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::range(
                SET_REFILL_THRESHOLD,
                "ipn.setting.refill_threshold",
                0.0,
                20.0,
                1.0,
                3.0,
            )
            .tooltip("ipn.setting.refill_threshold.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(SET_SHOW_BUTTONS, "ipn.setting.show_buttons", true)
                .tooltip("ipn.setting.show_buttons.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(SET_HIGHLIGHT_MATCHING, "ipn.setting.highlight_matching", true)
                .tooltip("ipn.setting.highlight_matching.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::choice(
                SET_PANEL_POSITION,
                "ipn.setting.panel_position",
                [
                    "ipn.setting.panel_position.auto",
                    "ipn.setting.panel_position.left",
                    "ipn.setting.panel_position.right",
                ],
                0,
            )
            .tooltip("ipn.setting.panel_position.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(
                SET_PROTECT_LOCKED_CLICKS,
                "ipn.setting.prevent_locked_quickmove",
                false,
            )
            .tooltip("ipn.setting.prevent_locked_quickmove.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(SET_SHOW_SEARCH_BAR, "ipn.setting.show_search_bar", true)
                .tooltip("ipn.setting.show_search_bar.tooltip"),
        );
        let _ = registrar.register_setting(
            SettingSpec::toggle(
                SET_KEEP_SEARCH_QUERY,
                "ipn.setting.keep_search_query",
                false,
            )
            .tooltip("ipn.setting.keep_search_query.tooltip"),
        );

        registrar.register_key_mapping(
            KeyRegistration::new(KEY_SORT, "key.keyboard.r")
                .label("ipn.key.sort")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_LOCK_SLOT, "key.keyboard.l")
                .label("ipn.key.lock_slot")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_MOVE_MATCHING, "key.keyboard.m")
                .label("ipn.key.move_matching")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_DUMP_ALL, "key.keyboard.g")
                .label("ipn.key.dump_all")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_PULL_MATCHING, "key.keyboard.u")
                .label("ipn.key.pull_matching")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_PULL_ALL, "key.keyboard.u")
                .modifiers(ClientModifiers::new(1))
                .label("ipn.key.pull_all")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_APPLY_PROFILE, "key.keyboard.p")
                .label("ipn.key.apply_profile")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_SAVE_PROFILE, "key.keyboard.p")
                .modifiers(ClientModifiers::new(1))
                .label("ipn.key.save_profile")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_NEXT_PROFILE, "key.keyboard.right_bracket")
                .label("ipn.key.next_profile")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_TOGGLE_HIGHLIGHT, "key.keyboard.h")
                .label("ipn.key.toggle_highlight")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_THROW_MATCHING, "key.keyboard.q")
                .modifiers(ClientModifiers::new(1))
                .label("ipn.key.throw_matching")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_THROW_ALL, "key.keyboard.q")
                .modifiers(ClientModifiers::new(3))
                .label("ipn.key.throw_all")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
        registrar.register_key_mapping(
            KeyRegistration::new(KEY_FOCUS_SEARCH, "key.keyboard.f")
                .modifiers(ClientModifiers::new(2))
                .label("ipn.key.focus_search")
                .category("ipn.category")
                .context(KeyConflictContext::Gui),
        );
    }

    fn client_tick_post(&self, host: &mut dyn ClientHost, _event: &mut ClientTickPostEvent) {
        let screen_open = host
            .local_player()
            .and_then(|p| p.container_menu(host))
            .and_then(|m| m.slot_kinds(host))
            .is_some();
        let inventory_screen_open = host
            .observed_screen_id()
            .is_some_and(|s| s.id == "inventory");

        let (prepared_action, save_after, settings) = {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);

            st.cache_refresh_ticks = st.cache_refresh_ticks.saturating_add(1);
            if st.cache_refresh_ticks >= 20 {
                st.cache_refresh_ticks = 0;
                st.refresh_host_caches(host);
            }

            if inventory_screen_open != st.was_inventory_open {
                st.refresh_host_caches(host);
                if !inventory_screen_open {
                    st.dragging_panel = None;
                    st.dragging_search = None;
                    st.search_widgets.set_focus(host, None);
                    if !st.cached_settings.keep_search_query {
                        st.set_search_query("");
                    }
                }
                st.was_inventory_open = inventory_screen_open;
            } else if !inventory_screen_open
                && !st.cached_settings.keep_search_query
                && !st.search_query().is_empty()
            {
                st.set_search_query("");
            }

            if let Some((_, ttl)) = st.toast.as_mut() {
                if *ttl <= 1 {
                    st.toast = None;
                } else {
                    *ttl -= 1;
                }
            }
            if let Some((_, ttl)) = st.refill.hud_notice.as_mut() {
                if *ttl <= 1 {
                    st.refill.hud_notice = None;
                } else {
                    *ttl -= 1;
                }
            }

            if !screen_open {
                st.dragging_panel = None;
                st.dragging_search = None;
                st.pending_throw_target = None;
                if !matches!(st.pending_action, Some(PendingAction::SaveProfile(_))) {
                    st.pending_action = None;
                }
            }

            let prepared = st
                .pending_action
                .take()
                .map(|action| prepare_pending_action(host, &mut st, action));
            let mut persist_snap = None;
            if let Some(PreparedAction::SaveProfile { idx, ref rules }) = prepared
                && idx < st.persistent.profiles.len()
            {
                let name = st.persistent.profiles[idx].name.clone();
                let count = rules.len();
                st.persistent.profiles[idx].rules = rules.clone();
                persist_snap = Some(st.persistent.clone());
                st.set_toast(format!("Saved {name} ({count} slots)"));
            } else if let Some(PreparedAction::Message(ref msg)) = prepared {
                st.set_toast(msg.clone());
            }
            (prepared, persist_snap, st.cached_settings)
        };

        if let Some(snap) = save_after {
            self.save_persistent(host, &snap);
        }

        if let Some(PreparedAction::ClickBatch {
            menu,
            clicks,
            continue_action,
            toast_msg,
        }) = prepared_action
        {
            if menu.click(host, &clicks) {
                let mut st = self.state.lock().unwrap();
                if let Some(next) = continue_action {
                    st.pending_action = Some(next);
                }
                if let Some(msg) = toast_msg {
                    st.set_toast(msg);
                }
            }
        }

        let refill_plan = {
            let mut st = self.state.lock().unwrap();
            prepare_auto_refill_click(host, &mut st, settings)
        };
        if let Some((menu, selected, click, notice)) = refill_plan
            && menu.click(host, &[click])
        {
            let main_raw = menu.slot(host, 36 + selected).flatten();
            let off_raw = menu.slot(host, 45).flatten();
            let mut st = self.state.lock().unwrap();
            st.refill.cooldown_ticks = 8;
            st.refill.hud_notice = Some((notice, 50));
            st.refill.last_main_hand = main_raw.as_ref().map(|s| {
                snapshot_from_stack_cached(host, &mut st.item_meta_cache, s, 0, 0, 0, false)
            });
            st.refill.last_off_hand = off_raw.as_ref().map(|s| {
                snapshot_from_stack_cached(host, &mut st.item_meta_cache, s, 1, 1, 1, false)
            });
        }
    }

    fn screen_exclusion_areas(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenExclusionAreasEvent<'_>,
    ) {
        if event.screen.id != "inventory" {
            return;
        }
        let Some(player) = host.local_player() else {
            return;
        };
        if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
            return;
        }
        let has_storage = event.screen.menu.is_some();
        let mut st = self.state.lock().unwrap();
        self.ensure_loaded(host, &mut st);
        let settings = st.settings(host);

        if settings.show_search_bar {
            let has_query = !st.search_query().trim().is_empty();
            let s_layout = build_search_bar_layout(
                event.screen.rect,
                st.last_screen_size,
                st.persistent.search_offset,
                has_query,
            );
            st.last_search_bounds = Some(s_layout.bounds);
            event.areas.push(s_layout.bounds);
        } else {
            st.last_search_bounds = None;
        }

        if !settings.show_buttons {
            st.last_panel_bounds = None;
            return;
        }
        let panel_h = panel_height(has_storage);
        let [px, py] = resolve_panel_origin(
            event.screen.rect,
            st.last_screen_size,
            panel_h,
            settings.panel_position,
            st.persistent.custom_offset,
            has_storage,
        );
        let bounds = [px, py, PANEL_W, panel_h];
        st.last_panel_bounds = Some(bounds);
        event.areas.push(bounds);
    }

    fn screen_render_inventory_mob_effects(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenRenderInventoryMobEffectsEvent<'_>,
    ) -> Flow {
        if event.screen.id != "inventory" {
            return Flow::Continue;
        }
        let Some(player) = host.local_player() else {
            return Flow::Continue;
        };
        if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
            return Flow::Continue;
        }
        let mut st = self.state.lock().unwrap();
        self.ensure_loaded(host, &mut st);
        let settings = st.settings(host);
        if !settings.show_buttons {
            return Flow::Continue;
        }
        let has_storage = event.screen.menu.is_some();
        let panel_h = panel_height(has_storage);
        let [px, py] = resolve_panel_origin(
            event.screen.rect,
            st.last_screen_size,
            panel_h,
            settings.panel_position,
            st.persistent.custom_offset,
            has_storage,
        );
        let panel_bounds = [px, py, PANEL_W, panel_h];
        // Effect stack renders to the right of `screen.rect` starting at `rect[0] + rect[2] + 2`.
        let effects_x0 = event.screen.rect[0] + event.screen.rect[2] + 2.0 + (event.horizontal_offset as f32);
        let effects_w = if event.compact { 32.0 } else { 120.0 };
        let effects_rect = [effects_x0, event.screen.rect[1], effects_w, 166.0];
        if rects_overlap(panel_bounds, effects_rect) {
            let needed_x0 = (px + PANEL_W + 4.0).ceil() as i32;
            let current_x0 = effects_x0.floor() as i32;
            if needed_x0 > current_x0 {
                let shift = needed_x0 - current_x0;
                event.horizontal_offset += shift;
                if event.available_space - event.horizontal_offset < 120 {
                    event.compact = true;
                }
            }
        }
        Flow::Continue
    }

    fn screen_render_foreground(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenRenderForegroundEvent<'_>,
    ) {
        if event.screen.id != "inventory" {
            return;
        }
        let Some(player) = host.local_player() else {
            return;
        };
        if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
            return;
        }
        let Some(menu) = player.container_menu(host) else {
            return;
        };
        let Some(boxes) = menu.slot_boxes(host) else {
            return;
        };
        let Some(kinds) = menu.slot_kinds(host) else {
            return;
        };

        let screen_size = event.draw.size();
        let has_storage = menu_has_storage(event.screen.menu, &kinds);
        let mouse = event.screen.mouse;
        let (
            settings,
            locked_rects,
            layout_opt,
            is_dragging,
            toast_text,
            search_layout,
            search_has_query,
            search_matches,
            search_focused,
            is_dragging_search,
        ) = {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.last_screen_size = screen_size;
            let settings = st.settings(host);

            let locked_rects: Vec<[f32; 4]> = if st.persistent.locked_slots.is_empty() {
                Vec::new()
            } else {
                boxes
                    .iter()
                    .zip(&kinds)
                    .filter_map(|(slot_box, kind)| match (*slot_box, kind) {
                        (Some(rect), MenuSlotKind::Player(ps))
                            if st.persistent.locked_slots.contains(ps) =>
                        {
                            Some(rect)
                        }
                        _ => None,
                    })
                    .collect()
            };

            let layout_opt = if settings.show_buttons {
                let layout = build_panel_layout(
                    &st.cached_keys,
                    event.screen.rect,
                    screen_size,
                    has_storage,
                    settings.panel_position,
                    settings.move_match_mode,
                    settings.highlight_matching,
                    &st.persistent,
                );
                st.last_panel_bounds = Some(layout.bounds);
                Some(layout)
            } else {
                st.last_panel_bounds = None;
                None
            };

            let is_dragging = st.dragging_panel.is_some();
            let toast_text = st.toast.as_ref().map(|(s, _)| s.clone());

            let (search_layout, search_has_query, search_matches, search_focused, is_dragging_search) =
                if settings.show_search_bar {
                    st.ensure_search_widget();
                    let norm_query = normalize_search_text(st.search_query());
                    let has_query = !norm_query.is_empty();
                    let s_layout = build_search_bar_layout(
                        event.screen.rect,
                        screen_size,
                        st.persistent.search_offset,
                        has_query,
                    );
                    st.last_search_bounds = Some(s_layout.bounds);

                    let mut match_count = 0usize;
                    if has_query {
                        for (idx, rect_opt) in boxes.iter().enumerate() {
                            let Some(rect) = *rect_opt else {
                                continue;
                            };
                            if let Some(stack) = menu.slot(host, idx).flatten() {
                                let haystack = cached_stack_search_text(host, &mut st, &stack);
                                if matches_normalized_query(haystack, &norm_query) {
                                    match_count += 1;
                                    draw_slot_search_match_overlay(
                                        &mut *event.draw,
                                        rect,
                                        point_in_slot_rect(mouse, rect),
                                    );
                                } else {
                                    draw_slot_search_dim_overlay(&mut *event.draw, rect, true);
                                }
                            } else {
                                draw_slot_search_dim_overlay(&mut *event.draw, rect, false);
                            }
                        }
                    }
                    st.last_search_matches = match_count;
                    let focused = st.is_search_focused();
                    let dragging_s = st.dragging_search.is_some();
                    (Some(s_layout), has_query, match_count, focused, dragging_s)
                } else {
                    st.last_search_bounds = None;
                    st.last_search_matches = 0;
                    (None, false, 0, false, false)
                };

            (
                settings,
                locked_rects,
                layout_opt,
                is_dragging,
                toast_text,
                search_layout,
                search_has_query,
                search_matches,
                search_focused,
                is_dragging_search,
            )
        };

        // 1. Highlight slots whose Item + NBT matches the hovered item (below tooltips & carried stack).
        let on_panel = layout_opt
            .as_ref()
            .is_some_and(|l| point_in_rect(mouse, l.bounds));
        let on_search_bar = search_layout
            .as_ref()
            .is_some_and(|l| point_in_rect(mouse, l.bounds));
        if settings.highlight_matching
            && !on_panel
            && !on_search_bar
            && let Some((hovered_idx, target)) = boxes.iter().enumerate().find_map(|(idx, r)| {
                let rect = (*r)?;
                point_in_slot_rect(mouse, rect)
                    .then(|| menu.slot(host, idx).flatten())
                    .flatten()
                    .map(|stack| (idx, stack))
            })
        {
            for (idx, rect_opt) in boxes.iter().enumerate() {
                let Some(rect) = *rect_opt else {
                    continue;
                };
                let matches = idx == hovered_idx
                    || menu.slot(host, idx).flatten().is_some_and(|s| {
                        s.same_item(&target) && s.components() == target.components()
                    });
                if matches {
                    draw_slot_match_overlay(&mut *event.draw, rect, idx == hovered_idx);
                }
            }
        }

        // 2. Render Locked-Slot borders + corner padlock glyphs (below tooltips & carried stack).
        for rect in locked_rects {
            draw_slot_lock_overlay(&mut *event.draw, rect);
        }

        // 3. Render IPN Control Panel & Toast (before tooltips & carried stack so item lore is never occluded).
        if let Some(layout) = layout_opt {
            let header_hovered = point_in_rect(mouse, layout.header_rect);
            draw_bordered_rect(&mut *event.draw, layout.bounds, 0xC414161B, 0xFF4F5563);
            if is_dragging || header_hovered {
                event.draw.fill(
                    [
                        layout.header_rect[0] + 1.0,
                        layout.header_rect[1] + 1.0,
                        layout.header_rect[2] - 2.0,
                        layout.header_rect[3],
                    ],
                    0x38FFCC55,
                );
            }
            event
                .draw
                .text(&layout.header_text, layout.header_pos, 0xFFFFCC55, true);
            let gx = layout.bounds[0] + PANEL_W - 10.0;
            let gy = layout.bounds[1] + 4.0;
            let grip_color = if is_dragging || header_hovered {
                0xFFFFCC55
            } else {
                0xFF7B8499
            };
            draw_grip_dots(&mut *event.draw, gx, gy, grip_color);

            draw_bordered_rect(
                &mut *event.draw,
                layout.profile_badge_rect,
                0xFF1E222A,
                0xFF3E4450,
            );
            let badge_w = event.draw.text_width(&layout.profile_badge_text);
            let badge_tx = layout.profile_badge_rect[0]
                + ((layout.profile_badge_rect[2] - badge_w) * 0.5).max(1.0);
            event.draw.text(
                &layout.profile_badge_text,
                [badge_tx, layout.profile_badge_rect[1] + 2.5],
                0xFFE6EBF5,
                true,
            );

            for btn in &layout.buttons {
                let hovered = point_in_rect(mouse, btn.rect);
                let (bg, border, text_color) = if btn.active {
                    if btn.kind == ButtonKind::ToggleHighlight {
                        (
                            if hovered { 0xFF1B7D96 } else { 0xFF146478 },
                            0xFF55EEFF,
                            0xFFFFFFFF,
                        )
                    } else {
                        (
                            if hovered { 0xFFB87414 } else { 0xFF945A0D },
                            0xFFFFCC55,
                            0xFFFFFFFF,
                        )
                    }
                } else if hovered {
                    (0xFF3B4252, 0xFF88C0D0, 0xFFFFFFFF)
                } else {
                    (0xFF252A34, 0xFF4B5262, 0xFFDCE2EC)
                };
                draw_bordered_rect(&mut *event.draw, btn.rect, bg, border);
                let tw = event.draw.text_width(&btn.label);
                let tx = btn.rect[0] + ((btn.rect[2] - tw) * 0.5).max(2.0);
                let ty = btn.rect[1] + 2.5;
                event.draw.text(&btn.label, [tx, ty], text_color, true);
            }

            if let Some(msg) = toast_text {
                let tw = event.draw.text_width(&msg) + 6.0;
                let tr = [
                    layout.bounds[0],
                    (layout.bounds[1] - 13.0).max(2.0),
                    tw.max(PANEL_W),
                    11.0,
                ];
                draw_bordered_rect(&mut *event.draw, tr, 0xD8141A22, 0xFF66D9EF);
                event
                    .draw
                    .text(&msg, [tr[0] + 3.0, tr[1] + 1.5], 0xFF9EF0FF, true);
            }
        }

        // 4. Render Draggable Search Bar above container (beneath item tooltips & carried stack).
        if let Some(s_layout) = search_layout {
            let mut st = self.state.lock().unwrap();
            draw_search_bar(
                &mut *event.draw,
                host,
                &mut st,
                &s_layout,
                mouse,
                search_has_query,
                search_matches,
                search_focused,
                is_dragging_search,
            );
        }
    }

    fn screen_render_post(&self, host: &dyn ClientHost, event: &mut ScreenRenderPostEvent<'_>) {
        if event.screen.id != "inventory" {
            return;
        }
        let mouse = event.screen.mouse;
        let Some(player) = host.local_player() else {
            return;
        };
        if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
            return;
        }
        let Some(menu) = player.container_menu(host) else {
            return;
        };

        // Fast bail-out: only resolve hover tooltips when mouse is inside search bar or side panel.
        let tooltip_lines = {
            let st = self.state.lock().unwrap();
            if st.dragging_panel.is_some() || st.dragging_search.is_some() {
                return;
            }
            let on_search = st.cached_settings.show_search_bar
                && st
                    .last_search_bounds
                    .is_some_and(|bounds| point_in_rect(mouse, bounds));
            let on_panel = st.cached_settings.show_buttons
                && st
                    .last_panel_bounds
                    .is_some_and(|bounds| point_in_rect(mouse, bounds));
            if !on_search && !on_panel {
                return;
            }

            let mut search_tip = None;
            if on_search {
                let has_query = !st.search_query().trim().is_empty();
                let s_layout = build_search_bar_layout(
                    event.screen.rect,
                    st.last_screen_size,
                    st.persistent.search_offset,
                    has_query,
                );
                if point_in_rect(mouse, s_layout.grip_rect) {
                    search_tip = Some(vec![
                        Text::literal("IPN Item Search"),
                        Text::literal("Drag to move anywhere"),
                        Text::literal("Right-click to reset position"),
                    ]);
                } else if has_query && point_in_rect(mouse, s_layout.clear_rect) {
                    search_tip = Some(vec![
                        Text::literal("Clear Search"),
                        Text::literal("Tip: Right-click search box to clear"),
                    ]);
                } else if s_layout.count_rect.is_some_and(|cr| point_in_rect(mouse, cr)) {
                    search_tip = Some(vec![
                        Text::literal(format!("{} matching slot(s)", st.last_search_matches)),
                        Text::literal("Searches Name, Lore, and Item ID"),
                    ]);
                } else if point_in_rect(mouse, s_layout.field_rect)
                    && !has_query
                    && !st.is_search_focused()
                {
                    search_tip = Some(vec![
                        Text::literal(format!("Search Items ({})", st.cached_keys.focus_search)),
                        Text::literal("Matches Name, Lore, and Item ID"),
                    ]);
                }
            }

            if let Some(lines) = search_tip {
                lines
            } else if on_panel {
                let Some(kinds) = menu.slot_kinds(host) else {
                    return;
                };
                let has_storage = menu_has_storage(event.screen.menu, &kinds);
                let layout = build_panel_layout(
                    &st.cached_keys,
                    event.screen.rect,
                    st.last_screen_size,
                    has_storage,
                    st.cached_settings.panel_position,
                    st.cached_settings.move_match_mode,
                    st.cached_settings.highlight_matching,
                    &st.persistent,
                );
                if let Some(btn) = layout.buttons.iter().find(|b| point_in_rect(mouse, b.rect)) {
                    vec![
                        Text::literal(btn.tooltip_title.clone()),
                        Text::literal(btn.tooltip_hint.clone()),
                    ]
                } else if point_in_rect(mouse, layout.profile_badge_rect) {
                    let active_prof = &st.persistent.profiles[st
                        .persistent
                        .active_profile
                        .min(PROFILE_COUNT.saturating_sub(1))];
                    let mut lines = vec![Text::literal(format!(
                        "Profile {} ({} saved slots)",
                        active_prof.name,
                        active_prof.rules.len()
                    ))];
                    if active_prof.rules.is_empty() {
                        lines.push(Text::literal("Click Save to capture Hotbar/Armor/Offhand"));
                    } else {
                        for rule in active_prof.rules.iter().take(8) {
                            let slot_tag = match rule.player_slot {
                                0..=8 => format!("H{}", rule.player_slot + 1),
                                36 => "Boots".to_owned(),
                                37 => "Legs".to_owned(),
                                38 => "Chest".to_owned(),
                                39 => "Helm".to_owned(),
                                40 => "Offhand".to_owned(),
                                other => format!("S{other}"),
                            };
                            let item_label = if rule.preferred_name.is_empty() {
                                rule.item_id.as_str()
                            } else {
                                rule.preferred_name.as_str()
                            };
                            lines.push(Text::literal(format!("{slot_tag}: {item_label}")));
                        }
                        if active_prof.rules.len() > 8 {
                            lines.push(Text::literal(format!(
                                "... +{} more",
                                active_prof.rules.len() - 8
                            )));
                        }
                    }
                    lines
                } else if point_in_rect(mouse, layout.header_rect) {
                    vec![
                        Text::literal("IPN Control Panel"),
                        Text::literal("Drag header to move anywhere"),
                        Text::literal("Right-click header to reset position"),
                    ]
                } else {
                    return;
                }
            } else {
                return;
            }
        };

        let _ = event.draw.gui_tooltip(&tooltip_lines, mouse);
    }

    fn screen_mouse_button_pressed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseButtonPressedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "inventory" {
            return Flow::Continue;
        }
        let Some(player) = host.local_player() else {
            return Flow::Continue;
        };
        if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
            return Flow::Continue;
        }
        let Some(menu) = player.container_menu(host) else {
            return Flow::Continue;
        };
        let Some(kinds) = menu.slot_kinds(host) else {
            return Flow::Continue;
        };

        let settings = Self::read_settings(host);
        let mouse = event.screen.mouse;
        let has_storage = menu_has_storage(event.screen.menu, &kinds);
        let alt_held = (event.modifiers & 4) != 0;
        let shift_held = (event.modifiers & 1) != 0;

        // 0. Check clicks & drag handle on the Item Search Bar first.
        {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.cached_settings = settings;
            if settings.show_search_bar {
                let has_query = !st.search_query().trim().is_empty();
                let s_layout = build_search_bar_layout(
                    event.screen.rect,
                    st.last_screen_size,
                    st.persistent.search_offset,
                    has_query,
                );
                st.last_search_bounds = Some(s_layout.bounds);
                if point_in_rect(mouse, s_layout.grip_rect) {
                    if event.button == 0 {
                        st.dragging_search =
                            Some([mouse[0] - s_layout.bounds[0], mouse[1] - s_layout.bounds[1]]);
                        st.search_widgets.set_focus(host, None);
                    } else if event.button == 1 {
                        st.persistent.search_offset = None;
                        st.dragging_search = None;
                        self.save_persistent(host, &st.persistent);
                        st.set_toast("Search bar position reset");
                    }
                    return Flow::Cancel;
                }
                if has_query && event.button == 0 && point_in_rect(mouse, s_layout.clear_rect) {
                    st.set_search_query("");
                    return Flow::Cancel;
                }
                if point_in_rect(mouse, s_layout.field_rect) {
                    let id = st.ensure_search_widget();
                    if let Some(field) = st.search_widgets.text_field_mut(id) {
                        field.rect = s_layout.field_rect;
                    }
                    if event.button == 1 {
                        st.set_search_query("");
                        st.search_widgets.set_focus(host, Some(id));
                    } else if event.button == 0 {
                        let _ = st
                            .search_widgets
                            .mouse_pressed(host, mouse, event.button, event.modifiers);
                        st.sync_search_suggestion();
                    }
                    return Flow::Cancel;
                }
                if point_in_rect(mouse, s_layout.bounds) {
                    return Flow::Cancel;
                }
            }
            if st.is_search_focused() {
                st.search_widgets.set_focus(host, None);
            }
        }

        // 1. Check clicks & header drag on the IPN side panel.
        {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            if settings.show_buttons {
                let layout = build_panel_layout(
                    &st.cached_keys,
                    event.screen.rect,
                    st.last_screen_size,
                    has_storage,
                    st.cached_settings.panel_position,
                    st.cached_settings.move_match_mode,
                    st.cached_settings.highlight_matching,
                    &st.persistent,
                );
                if point_in_rect(mouse, layout.header_rect) {
                    if event.button == 0 {
                        st.dragging_panel =
                            Some([mouse[0] - layout.bounds[0], mouse[1] - layout.bounds[1]]);
                        return Flow::Cancel;
                    }
                    if event.button == 1 {
                        st.persistent.custom_offset = None;
                        st.dragging_panel = None;
                        self.save_persistent(host, &st.persistent);
                        st.set_toast("Panel position reset");
                        return Flow::Cancel;
                    }
                }
                if event.button == 0
                    && let Some(btn) = layout.buttons.iter().find(|b| point_in_rect(mouse, b.rect))
                {
                    match btn.kind {
                        ButtonKind::SortChest => {
                            st.pending_action = Some(PendingAction::SortContainer)
                        }
                        ButtonKind::MoveMatching => {
                            st.pending_action =
                                Some(PendingAction::Transfer(TransferMode::MoveMatching))
                        }
                        ButtonKind::DumpAll => {
                            st.pending_action =
                                Some(PendingAction::Transfer(TransferMode::DumpAll))
                        }
                        ButtonKind::PullMatching => {
                            st.pending_action =
                                Some(PendingAction::Transfer(TransferMode::PullMatching))
                        }
                        ButtonKind::PullAll => {
                            st.pending_action =
                                Some(PendingAction::Transfer(TransferMode::PullAll))
                        }
                        ButtonKind::SortInv => {
                            st.pending_action = Some(PendingAction::SortInventory)
                        }
                        ButtonKind::ToggleLockMode => {
                            st.persistent.lock_mode = !st.persistent.lock_mode;
                            self.save_persistent(host, &st.persistent);
                            let msg = if st.persistent.lock_mode {
                                "Lock Mode: ON (click slots)"
                            } else {
                                "Lock Mode: OFF"
                            };
                            st.set_toast(msg);
                        }
                        ButtonKind::ToggleHighlight => {
                            drop(st);
                            toggle_highlight_setting(host, &self.state);
                            return Flow::Cancel;
                        }
                        ButtonKind::PrevProfile => {
                            let n = st.persistent.profiles.len();
                            st.persistent.active_profile =
                                (st.persistent.active_profile + n - 1) % n;
                            let name = st.persistent.profiles[st.persistent.active_profile]
                                .name
                                .clone();
                            self.save_persistent(host, &st.persistent);
                            st.set_toast(format!("Selected {name}"));
                        }
                        ButtonKind::NextProfile => {
                            let n = st.persistent.profiles.len();
                            st.persistent.active_profile = (st.persistent.active_profile + 1) % n;
                            let name = st.persistent.profiles[st.persistent.active_profile]
                                .name
                                .clone();
                            self.save_persistent(host, &st.persistent);
                            st.set_toast(format!("Selected {name}"));
                        }
                        ButtonKind::ApplyProfile => {
                            st.pending_action =
                                Some(PendingAction::ApplyProfile(st.persistent.active_profile));
                        }
                        ButtonKind::SaveProfile => {
                            st.pending_action =
                                Some(PendingAction::SaveProfile(st.persistent.active_profile));
                        }
                    }
                    return Flow::Cancel;
                }
                if point_in_rect(mouse, layout.bounds) {
                    return Flow::Cancel;
                }
            }
        }

        // 2. Check Alt+Click or Lock-Mode click on any player slot.
        if event.button == 0
            && let Some((_, MenuSlotKind::Player(ps))) = hovered_slot_kind(host, menu, mouse)
        {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            if alt_held || st.persistent.lock_mode {
                let locked = if st.persistent.locked_slots.remove(&ps) {
                    false
                } else {
                    st.persistent.locked_slots.insert(ps);
                    true
                };
                self.save_persistent(host, &st.persistent);
                st.set_toast(if locked {
                    format!("Locked slot {ps}")
                } else {
                    format!("Unlocked slot {ps}")
                });
                return Flow::Cancel;
            }
            if settings.prevent_locked_quickmove
                && shift_held
                && st.persistent.locked_slots.contains(&ps)
            {
                st.set_toast("Slot locked!");
                return Flow::Cancel;
            }
        }

        Flow::Continue
    }

    fn screen_mouse_dragged_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseDraggedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "inventory" || event.button != 0 {
            return Flow::Continue;
        }
        let mut st = self.state.lock().unwrap();
        if let Some([grab_dx, grab_dy]) = st.dragging_search {
            let bar_w = search_bar_width(event.screen.rect);
            let [new_x, new_y] = clamp_bar_origin(
                event.screen.mouse[0] - grab_dx,
                event.screen.mouse[1] - grab_dy,
                bar_w,
                SEARCH_BAR_H,
                st.last_screen_size,
            );
            st.persistent.search_offset = Some([
                new_x - event.screen.rect[0],
                new_y - event.screen.rect[1],
            ]);
            return Flow::Cancel;
        }
        if st
            .search_widgets
            .mouse_dragged(host, event.screen.mouse, event.button)
            .handled()
        {
            return Flow::Cancel;
        }
        let Some([grab_dx, grab_dy]) = st.dragging_panel else {
            return Flow::Continue;
        };
        let has_storage = host
            .local_player()
            .and_then(|p| p.container_menu(host))
            .and_then(|m| m.slot_kinds(host))
            .is_some_and(|kinds| menu_has_storage(event.screen.menu, &kinds));
        let ph = panel_height(has_storage);
        let [new_x, new_y] = clamp_bar_origin(
            event.screen.mouse[0] - grab_dx,
            event.screen.mouse[1] - grab_dy,
            PANEL_W,
            ph,
            st.last_screen_size,
        );
        st.persistent.custom_offset = Some([
            new_x - event.screen.rect[0],
            new_y - event.screen.rect[1],
        ]);
        Flow::Cancel
    }

    fn screen_mouse_button_released_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseButtonReleasedPreEvent<'_>,
    ) -> Flow {
        if event.button != 0 {
            return Flow::Continue;
        }
        let mut st = self.state.lock().unwrap();
        if event.screen.id != "inventory" {
            st.dragging_panel = None;
            st.dragging_search = None;
            return Flow::Continue;
        }
        if st.dragging_search.take().is_some() {
            self.save_persistent(host, &st.persistent);
            st.set_toast("Search bar position saved");
            return Flow::Cancel;
        }
        if st.search_widgets.mouse_released(event.button).handled() {
            return Flow::Cancel;
        }
        if st.dragging_panel.take().is_some() {
            self.save_persistent(host, &st.persistent);
            st.set_toast("Panel position saved");
            return Flow::Cancel;
        }
        Flow::Continue
    }

    fn screen_key_pressed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenKeyPressedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "inventory" {
            return Flow::Continue;
        }
        let Some(player) = host.local_player() else {
            return Flow::Continue;
        };
        if player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true) {
            return Flow::Continue;
        }
        let Some(menu) = player.container_menu(host) else {
            return Flow::Continue;
        };
        let Some(kinds) = menu.slot_kinds(host) else {
            return Flow::Continue;
        };

        let settings = Self::read_settings(host);
        {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.cached_settings = settings;
            if settings.show_search_bar && st.is_search_focused() {
                if matches!(
                    event.key,
                    "key.keyboard.escape" | "key.keyboard.enter" | "key.keyboard.keypad.enter"
                ) {
                    st.search_widgets.set_focus(host, None);
                    return Flow::Cancel;
                }
                if matches_mod_key(host, KEY_FOCUS_SEARCH, event.key, event.modifiers) {
                    st.focus_search_select_all(host);
                    return Flow::Cancel;
                }
                let _ = st
                    .search_widgets
                    .key_pressed(host, event.key, event.modifiers);
                st.sync_search_suggestion();
                if is_passthrough_nav_or_modifier_key(event.key) {
                    return Flow::Continue;
                }
                return Flow::Cancel;
            }
            if !settings.show_search_bar && st.is_search_focused() {
                st.search_widgets.set_focus(host, None);
            }
        }

        if event.repeat {
            return Flow::Continue;
        }

        if settings.show_search_bar
            && matches_mod_key(host, KEY_FOCUS_SEARCH, event.key, event.modifiers)
        {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.focus_search_select_all(host);
            return Flow::Cancel;
        }

        let hovered = hovered_slot_kind(host, menu, event.screen.mouse);

        if settings.prevent_locked_quickmove
            && let Some((_, MenuSlotKind::Player(ps))) = hovered
            && event.key == "key.keyboard.q"
            && (event.modifiers & 0b0101) == 0
        {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            if st.persistent.locked_slots.contains(&ps) {
                st.set_toast("Slot locked!");
                return Flow::Cancel;
            }
        }

        let key = event.key;
        let mods = event.modifiers;

        if matches_mod_key(host, KEY_TOGGLE_HIGHLIGHT, key, mods) {
            toggle_highlight_setting(host, &self.state);
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_THROW_MATCHING, key, mods) {
            let Some((hovered_idx, kind)) = hovered else {
                let mut st = self.state.lock().unwrap();
                st.set_toast("Hover an item to throw matching");
                return Flow::Cancel;
            };
            let target_storage = match kind {
                MenuSlotKind::Storage => true,
                MenuSlotKind::Player(ps) if ps <= 35 => false,
                _ => return Flow::Continue,
            };
            let Some(stack) = menu.slot(host, hovered_idx).flatten() else {
                let mut st = self.state.lock().unwrap();
                st.set_toast("Hover an item to throw matching");
                return Flow::Cancel;
            };
            let include_hotbar = settings.transfer_hotbar
                || matches!(kind, MenuSlotKind::Player(ps) if ps < 9);
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.pending_throw_target = Some(stack);
            st.pending_action = Some(PendingAction::ThrowMatching {
                target_storage,
                include_hotbar,
            });
            st.refill.drop_suppress_ticks = 14;
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_THROW_ALL, key, mods) {
            let hovered_storage = matches!(hovered, Some((_, MenuSlotKind::Storage)));
            let hovered_player = matches!(hovered, Some((_, MenuSlotKind::Player(_))));
            let has_storage = menu_has_storage(event.screen.menu, &kinds);
            let target_storage = if hovered_storage {
                true
            } else if hovered_player {
                false
            } else {
                has_storage
            };
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.pending_throw_target = None;
            st.pending_action = Some(PendingAction::ThrowAll { target_storage });
            st.refill.drop_suppress_ticks = 14;
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_LOCK_SLOT, key, mods) {
            if let Some((_, MenuSlotKind::Player(ps))) = hovered {
                let mut st = self.state.lock().unwrap();
                self.ensure_loaded(host, &mut st);
                let locked = if st.persistent.locked_slots.remove(&ps) {
                    false
                } else {
                    st.persistent.locked_slots.insert(ps);
                    true
                };
                self.save_persistent(host, &st.persistent);
                st.set_toast(if locked {
                    format!("Locked slot {ps}")
                } else {
                    format!("Unlocked slot {ps}")
                });
                return Flow::Cancel;
            }
            return Flow::Continue;
        }

        if matches_mod_key(host, KEY_SORT, key, mods) {
            let hovered_storage = matches!(hovered, Some((_, MenuSlotKind::Storage)));
            let hovered_player = matches!(hovered, Some((_, MenuSlotKind::Player(_))));
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.pending_action = Some(PendingAction::SortAuto {
                hovered_storage,
                hovered_player,
            });
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_MOVE_MATCHING, key, mods)
            && menu_has_storage(event.screen.menu, &kinds)
        {
            let mut st = self.state.lock().unwrap();
            st.pending_action = Some(PendingAction::Transfer(TransferMode::MoveMatching));
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_DUMP_ALL, key, mods)
            && menu_has_storage(event.screen.menu, &kinds)
        {
            let mut st = self.state.lock().unwrap();
            st.pending_action = Some(PendingAction::Transfer(TransferMode::DumpAll));
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_PULL_ALL, key, mods)
            && menu_has_storage(event.screen.menu, &kinds)
        {
            let mut st = self.state.lock().unwrap();
            st.pending_action = Some(PendingAction::Transfer(TransferMode::PullAll));
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_PULL_MATCHING, key, mods)
            && menu_has_storage(event.screen.menu, &kinds)
        {
            let mut st = self.state.lock().unwrap();
            st.pending_action = Some(PendingAction::Transfer(TransferMode::PullMatching));
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_SAVE_PROFILE, key, mods) {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.pending_action = Some(PendingAction::SaveProfile(st.persistent.active_profile));
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_APPLY_PROFILE, key, mods) {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            st.pending_action = Some(PendingAction::ApplyProfile(st.persistent.active_profile));
            return Flow::Cancel;
        }

        if matches_mod_key(host, KEY_NEXT_PROFILE, key, mods) {
            let mut st = self.state.lock().unwrap();
            self.ensure_loaded(host, &mut st);
            let n = st.persistent.profiles.len();
            st.persistent.active_profile = (st.persistent.active_profile + 1) % n;
            let name = st.persistent.profiles[st.persistent.active_profile]
                .name
                .clone();
            self.save_persistent(host, &st.persistent);
            st.set_toast(format!("Selected {name}"));
            return Flow::Cancel;
        }

        Flow::Continue
    }

    fn screen_char_typed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenCharTypedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "inventory" {
            return Flow::Continue;
        }
        if let Some(player) = host.local_player()
            && (player.is_creative(host) == Some(true) || player.is_spectator(host) == Some(true))
        {
            return Flow::Continue;
        }
        let mut st = self.state.lock().unwrap();
        self.ensure_loaded(host, &mut st);
        if event.field.is_some() {
            if st.is_search_focused() {
                st.search_widgets.set_focus(host, None);
            }
            return Flow::Continue;
        }
        if !st.cached_settings.show_search_bar || !st.is_search_focused() {
            return Flow::Continue;
        }
        let _ = st.search_widgets.char_typed(host, event.c);
        st.sync_search_suggestion();
        Flow::Cancel
    }

    fn input_key(&self, host: &mut dyn ClientHost, event: &mut InputKeyEvent<'_>) {
        if !host.playing() || event.action != KeyAction::Press {
            return;
        }
        let is_drop_key = event.key == "key.keyboard.q"
            || host
                .mapping("key.drop")
                .ok()
                .flatten()
                .is_some_and(|m| m.down);
        if is_drop_key {
            let mut st = self.state.lock().unwrap();
            st.refill.drop_suppress_ticks = 14;
        }
    }

    fn render_gui_post(&self, host: &dyn ClientHost, event: &mut RenderGuiPostEvent<'_>) {
        if !host.native_corner_shown(Corner::TopLeft) {
            return;
        }
        let notice = {
            let st = self.state.lock().unwrap();
            st.refill.hud_notice.as_ref().map(|(s, _)| s.clone())
        };
        let Some(label) = notice else {
            return;
        };
        let w = event.draw.text_width(&label) + 6.0;
        let h = 12.0;
        let (x, y) = event.corners.place(Corner::TopLeft, w, h);
        event.draw.fill([x, y, w, h], 0x78000000);
        event.draw.text(&label, [x + 3.0, y + 2.0], 0xFFFFCC55, true);
    }
}

#[cfg(test)]
mod tests {
    use super::point_in_slot_rect;

    #[test]
    fn slot_hover_hitbox_matches_vanilla_18x18_margin() {
        // Container slot 0 is 16x16 at (8, 18); vanilla isHovering covers [x-1..x+17, y-1..y+17).
        let slot0 = [8.0, 18.0, 16.0, 16.0];
        let slot1 = [26.0, 18.0, 16.0, 16.0];
        // Outer 1 px margin on all four edges still hits slot 0 cleanly just like item tooltips.
        assert!(point_in_slot_rect([7.0, 17.0], slot0));
        assert!(point_in_slot_rect([24.5, 34.5], slot0));
        assert!(!point_in_slot_rect([6.9, 18.0], slot0));
        // Adjacent 18 px slot partitions cleanly at x = 25.0 without overlap.
        assert!(!point_in_slot_rect([25.0, 20.0], slot0));
        assert!(point_in_slot_rect([25.0, 20.0], slot1));
    }
}
