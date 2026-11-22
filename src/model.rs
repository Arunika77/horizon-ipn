//! Pure data types and persistence encoding for Inventory Profiles Next.

use std::collections::BTreeSet;

/// Number of persistent loadout profile presets.
pub const PROFILE_COUNT: usize = 5;

/// Order in which stacks are arranged when sorting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    CategoryName,
    NameAsc,
    IdAsc,
    CountDesc,
}

impl SortOrder {
    pub fn from_choice(choice: u32) -> Self {
        match choice {
            1 => Self::NameAsc,
            2 => Self::IdAsc,
            3 => Self::CountDesc,
            _ => Self::CategoryName,
        }
    }
}

/// Grid fill traversal order during sort.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillDirection {
    #[default]
    RowByRow,
    ColumnByColumn,
}

impl FillDirection {
    pub fn from_choice(choice: u32) -> Self {
        match choice {
            1 => Self::ColumnByColumn,
            _ => Self::RowByRow,
        }
    }
}

/// Dock side preference for the IPN side panel when not manually dragged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelPosition {
    #[default]
    AutoLeft,
    Left,
    Right,
}

impl PanelPosition {
    pub fn from_choice(choice: u32) -> Self {
        match choice {
            1 => Self::Left,
            2 => Self::Right,
            _ => Self::AutoLeft,
        }
    }
}

/// Behavior when clicking `Move Match` on a container with empty slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoveMatchMode {
    /// Move stacks whose item ID and NBT/components already exist in container (may fill empty slots).
    #[default]
    ExactItemAndNbt,
    /// Only top up existing partial (`< max_stack`) stacks in container; never fill empty slots.
    TopUpExistingOnly,
}

impl MoveMatchMode {
    pub fn from_choice(choice: u32) -> Self {
        match choice {
            1 => Self::TopUpExistingOnly,
            _ => Self::ExactItemAndNbt,
        }
    }
}

/// Container bulk-transfer operations between player inventory and container storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    /// Move unlocked player stacks whose item & NBT already exist in container storage.
    MoveMatching,
    /// Move all unlocked player main (and optionally hotbar) stacks into container storage.
    DumpAll,
    /// Pull container storage stacks whose item & NBT already exist in unlocked player inventory.
    PullMatching,
    /// Pull all container storage stacks into player inventory until full.
    PullAll,
}

/// Pure snapshot of an item stack used by offline planners and simulations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSnapshot {
    /// Namespaced item identifier (e.g. `"minecraft:diamond_pickaxe"`).
    pub item_id: String,
    /// Localized display / hover name for sorting and UX feedback.
    pub display_name: String,
    /// Effective max stack size (`1`, `16`, `64`, `99`).
    pub max_stack: u32,
    /// Effective max durability (`0` if not damageable).
    pub max_damage: u32,
    /// Damage taken (`0` = untouched).
    pub damage: u32,
    /// Stack count (`>= 1`).
    pub count: u32,
    /// Whether item displays an enchantment glint.
    pub enchanted: bool,
    /// Dense ID identifying stackable equivalence (`ItemStack::is_stackable_with`).
    pub stack_group: u32,
    /// Dense ID identifying matching item identity + components/NBT (ignoring wear `damage`).
    pub match_group: u32,
    /// Dense ID identifying full stack equality including components (`ItemStack == ItemStack`).
    pub exact_group: u32,
}

impl SlotSnapshot {
    pub fn remaining_durability(&self) -> u32 {
        self.max_damage.saturating_sub(self.damage)
    }

    pub fn is_damageable(&self) -> bool {
        self.max_damage > 0
    }

    pub fn can_stack_with(&self, other: &Self) -> bool {
        self.stack_group == other.stack_group && self.max_stack > 1 && other.max_stack > 1
    }

    pub fn matches_for_sort_swap(&self, other: &Self) -> bool {
        self.exact_group == other.exact_group && self.count == other.count
    }
}

/// Single slot expectation stored inside an inventory loadout profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSlotRule {
    /// Vanilla player slot index (`0..=8` hotbar, `36..=39` armor, `40` offhand).
    pub player_slot: u32,
    /// Item identifier expected in that slot (e.g. `"minecraft:diamond_sword"`).
    pub item_id: String,
    /// Display name captured when saved, used only as a tie-breaker when multiple variants exist.
    pub preferred_name: String,
}

/// Saved inventory loadout profile preset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryProfile {
    pub name: String,
    pub rules: Vec<ProfileSlotRule>,
}

impl InventoryProfile {
    pub fn default_numbered(index: usize) -> Self {
        Self {
            name: format!("P{}", index + 1),
            rules: Vec::new(),
        }
    }
}

/// Persistent client state saved across sessions in `horizon-mod-data/ipn/state.txt`.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistentState {
    pub locked_slots: BTreeSet<u32>,
    pub lock_mode: bool,
    pub active_profile: usize,
    pub custom_offset: Option<[f32; 2]>,
    pub search_offset: Option<[f32; 2]>,
    pub profiles: Vec<InventoryProfile>,
}

impl Default for PersistentState {
    fn default() -> Self {
        Self {
            locked_slots: BTreeSet::new(),
            lock_mode: false,
            active_profile: 0,
            custom_offset: None,
            search_offset: None,
            profiles: (0..PROFILE_COUNT)
                .map(InventoryProfile::default_numbered)
                .collect(),
        }
    }
}

impl PersistentState {
    /// Ensures there are always `PROFILE_COUNT` profiles and valid active index.
    pub fn normalize(&mut self) {
        while self.profiles.len() < PROFILE_COUNT {
            self.profiles
                .push(InventoryProfile::default_numbered(self.profiles.len()));
        }
        self.profiles.truncate(PROFILE_COUNT);
        self.locked_slots.retain(|&s| s <= 40);
        if self.active_profile >= self.profiles.len() {
            self.active_profile = 0;
        }
        if let Some([dx, dy]) = self.custom_offset
            && (!dx.is_finite() || !dy.is_finite())
        {
            self.custom_offset = None;
        }
        if let Some([dx, dy]) = self.search_offset
            && (!dx.is_finite() || !dy.is_finite())
        {
            self.search_offset = None;
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        out.push_str("version=1\n");
        let locked_list: Vec<String> = self.locked_slots.iter().map(|s| s.to_string()).collect();
        out.push_str(&format!("locked={}\n", locked_list.join(",")));
        out.push_str(&format!("lock_mode={}\n", u8::from(self.lock_mode)));
        out.push_str(&format!("active_profile={}\n", self.active_profile));
        if let Some([dx, dy]) = self.custom_offset
            && dx.is_finite()
            && dy.is_finite()
        {
            out.push_str(&format!("custom_offset={dx},{dy}\n"));
        }
        if let Some([dx, dy]) = self.search_offset
            && dx.is_finite()
            && dy.is_finite()
        {
            out.push_str(&format!("search_offset={dx},{dy}\n"));
        }
        for (idx, prof) in self.profiles.iter().enumerate() {
            out.push_str(&format!("profile.{idx}.name={}\n", escape_field(&prof.name)));
            for rule in &prof.rules {
                out.push_str(&format!(
                    "profile.{idx}.slot.{}={}|{}\n",
                    rule.player_slot,
                    escape_field(&rule.item_id),
                    escape_field(&rule.preferred_name)
                ));
            }
        }
        out
    }

    pub fn decode(raw: &str) -> Self {
        let mut state = Self::default();
        for line in raw.lines() {
            let line = line.trim_end_matches('\r');
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            if key == "locked" {
                state.locked_slots.clear();
                for part in val.split(',').filter(|s| !s.is_empty()) {
                    if let Ok(slot) = part.parse::<u32>()
                        && slot <= 40
                    {
                        state.locked_slots.insert(slot);
                    }
                }
            } else if key == "lock_mode" {
                state.lock_mode = val == "1" || val.eq_ignore_ascii_case("true");
            } else if key == "active_profile" {
                if let Ok(idx) = val.parse::<usize>() {
                    state.active_profile = idx;
                }
            } else if key == "custom_offset" {
                if let Some(offset) = parse_offset(val) {
                    state.custom_offset = Some(offset);
                }
            } else if key == "search_offset" {
                if let Some(offset) = parse_offset(val) {
                    state.search_offset = Some(offset);
                }
            } else if let Some(rest) = key.strip_prefix("profile.") {
                let Some((idx_str, field)) = rest.split_once('.') else {
                    continue;
                };
                let Ok(idx) = idx_str.parse::<usize>() else {
                    continue;
                };
                if idx >= PROFILE_COUNT {
                    continue;
                }
                if field == "name" {
                    let unescaped = unescape_field(val);
                    if !unescaped.is_empty() {
                        state.profiles[idx].name = unescaped;
                    }
                } else if let Some(slot_str) = field.strip_prefix("slot.")
                    && let Ok(player_slot) = slot_str.parse::<u32>()
                    && player_slot <= 40
                {
                    let (item_raw, name_raw) = val.split_once('|').unwrap_or((val, ""));
                    let item_id = unescape_field(item_raw);
                    if !item_id.is_empty() {
                        state.profiles[idx].rules.retain(|r| r.player_slot != player_slot);
                        state.profiles[idx].rules.push(ProfileSlotRule {
                            player_slot,
                            item_id,
                            preferred_name: unescape_field(name_raw),
                        });
                    }
                }
            }
        }
        for prof in &mut state.profiles {
            prof.rules.sort_by_key(|r| r.player_slot);
        }
        state.normalize();
        state
    }
}

fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\p"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '=' => out.push_str("\\e"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('p') => out.push('|'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('e') => out.push('='),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_offset(val: &str) -> Option<[f32; 2]> {
    let (x_s, y_s) = val.split_once(',')?;
    let (Ok(dx), Ok(dy)) = (x_s.trim().parse::<f32>(), y_s.trim().parse::<f32>()) else {
        return None;
    };
    (dx.is_finite() && dy.is_finite()).then_some([dx, dy])
}

#[cfg(test)]
mod tests {
    use super::PersistentState;

    #[test]
    fn search_offset_round_trips_and_handles_legacy_or_invalid_values() {
        let mut state = PersistentState::default();
        state.custom_offset = Some([-82.0, 4.0]);
        state.search_offset = Some([12.5, -24.0]);
        let encoded = state.encode();
        assert!(encoded.contains("search_offset=12.5,-24\n"));

        let decoded = PersistentState::decode(&encoded);
        assert_eq!(decoded.custom_offset, Some([-82.0, 4.0]));
        assert_eq!(decoded.search_offset, Some([12.5, -24.0]));

        // Backward compatibility: state file written without search_offset decodes as None.
        let legacy = "version=1\nlocked=0,1\nlock_mode=0\nactive_profile=0\n";
        let decoded_legacy = PersistentState::decode(legacy);
        assert_eq!(decoded_legacy.search_offset, None);

        // Non-finite values are rejected on decode and normalize.
        let invalid = PersistentState::decode("version=1\nsearch_offset=NaN,inf\n");
        assert_eq!(invalid.search_offset, None);

        let mut non_finite = PersistentState::default();
        non_finite.search_offset = Some([f32::NAN, 10.0]);
        non_finite.normalize();
        assert_eq!(non_finite.search_offset, None);
    }
}
