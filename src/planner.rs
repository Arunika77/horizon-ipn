//! Pure planning algorithms for all 5 IPN core features:
//! 1. Locked-slot tracking & validation
//! 2. Container transfers (Move Matching, Dump All, Pull Matching)
//! 3. Smart Lock-Aware Sort (merge partial stacks + swap permutation)
//! 4. Auto-Refill (tool saver threshold + depleted stack replacement)
//! 5. Inventory Profiles (capture + apply hotbar/armor/offhand loadouts)

use crate::model::{
    FillDirection, InventoryProfile, MoveMatchMode, ProfileSlotRule, SlotSnapshot, SortOrder,
    TransferMode,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Host-agnostic representation of a single visible slot in the open menu.
#[derive(Clone, Debug)]
pub struct MenuSlotEntry {
    /// Menu-order index used by `MenuRef::slot` and `MenuRef::click`.
    pub menu_index: usize,
    /// Player slot index (`0..=8` hotbar, `9..=35` main, `36..=39` armor, `40` offhand), if player slot.
    pub player_slot: Option<u32>,
    /// True if this is a container storage slot (`MenuSlotKind::Storage`).
    pub is_storage: bool,
    /// Current item stack snapshot, or `None` if empty.
    pub stack: Option<SlotSnapshot>,
}

/// Abstract menu action mapped 1-to-1 to `mod_api::MenuClick` on execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanClick {
    LeftPickup(usize),
    QuickMove(usize),
    SwapHotbar { slot: usize, hotbar: u8 },
    SwapOffhand { slot: usize },
    ThrowWholeStack(usize),
}

/// Category ranking used when `SortOrder::CategoryName` is active.
pub fn item_category_rank(item_id: &str) -> u8 {
    let path = item_id.split(':').nth(1).unwrap_or(item_id);
    if path.ends_with("_pickaxe")
        || path.ends_with("_axe") && !path.contains("waxed")
        || path.ends_with("_shovel")
        || path.ends_with("_hoe")
        || matches!(
            path,
            "shears" | "flint_and_steel" | "fishing_rod" | "brush" | "spyglass" | "compass" | "clock"
        )
    {
        return 0; // Tools
    }
    if path.ends_with("_sword")
        || path.ends_with("_spear")
        || matches!(path, "bow" | "crossbow" | "trident" | "mace" | "shield" | "totem_of_undying")
        || path.ends_with("_arrow")
        || path == "arrow"
    {
        return 1; // Combat
    }
    if path.ends_with("_helmet")
        || path.ends_with("_chestplate")
        || path.ends_with("_leggings")
        || path.ends_with("_boots")
        || path == "elytra"
        || path == "turtle_helmet"
        || path.ends_with("_horse_armor")
        || path.ends_with("_harness")
    {
        return 2; // Armor & Gear
    }
    if path.contains("potion")
        || matches!(
            path,
            "golden_apple"
                | "enchanted_golden_apple"
                | "golden_carrot"
                | "cooked_beef"
                | "cooked_porkchop"
                | "cooked_chicken"
                | "cooked_mutton"
                | "cooked_rabbit"
                | "cooked_cod"
                | "cooked_salmon"
                | "bread"
                | "apple"
                | "baked_potato"
                | "melon_slice"
                | "sweet_berries"
                | "glow_berries"
                | "carrot"
                | "pumpkin_pie"
                | "cake"
                | "cookie"
                | "honey_bottle"
                | "milk_bucket"
        )
        || path.ends_with("_stew")
        || path.ends_with("_soup")
    {
        return 3; // Food & Potions
    }
    if matches!(
        path,
        "redstone"
            | "redstone_torch"
            | "redstone_block"
            | "repeater"
            | "comparator"
            | "piston"
            | "sticky_piston"
            | "observer"
            | "hopper"
            | "dropper"
            | "dispenser"
            | "crafter"
            | "lever"
            | "target"
            | "daylight_detector"
            | "tripwire_hook"
            | "tnt"
    ) || path.ends_with("_rail")
        || path == "rail"
        || path.ends_with("_button")
        || path.ends_with("_pressure_plate")
    {
        return 4; // Redstone & Rails
    }
    if path.ends_with("_ore")
        || path.ends_with("_ingot")
        || path.ends_with("_nugget")
        || path.starts_with("raw_")
        || matches!(
            path,
            "diamond"
                | "emerald"
                | "lapis_lazuli"
                | "quartz"
                | "amethyst_shard"
                | "coal"
                | "charcoal"
                | "netherite_scrap"
                | "ancient_debris"
                | "blaze_rod"
                | "ender_pearl"
                | "ender_eye"
                | "ghast_tear"
                | "nether_star"
        )
    {
        return 5; // Ores, Ingots & Gems
    }
    if path.ends_with("_log")
        || path.ends_with("_wood")
        || path.ends_with("_stem")
        || path.ends_with("_hyphae")
        || path.ends_with("_planks")
        || path.ends_with("_stairs")
        || path.ends_with("_slab")
        || path.ends_with("_wall")
        || path.ends_with("_fence")
        || path.ends_with("_fence_gate")
        || path.ends_with("_door")
        || path.ends_with("_trapdoor")
        || path.ends_with("_bricks")
        || path.ends_with("_block")
        || path.ends_with("_terracotta")
        || path.ends_with("_concrete")
        || path.ends_with("_wool")
        || path.ends_with("_glass")
        || path.contains("stone")
        || matches!(
            path,
            "dirt" | "grass_block" | "sand" | "red_sand" | "gravel" | "deepslate" | "netherrack"
                | "end_stone" | "obsidian" | "crying_obsidian" | "torch" | "soul_torch" | "lantern"
        )
    {
        return 6; // Building & Natural Blocks
    }
    7 // Misc
}

fn compare_snapshots(a: &SlotSnapshot, b: &SlotSnapshot, order: SortOrder) -> std::cmp::Ordering {
    let base = match order {
        SortOrder::CategoryName => item_category_rank(&a.item_id)
            .cmp(&item_category_rank(&b.item_id))
            .then_with(|| {
                a.display_name
                    .to_ascii_lowercase()
                    .cmp(&b.display_name.to_ascii_lowercase())
            })
            .then_with(|| a.item_id.cmp(&b.item_id)),
        SortOrder::NameAsc => a
            .display_name
            .to_ascii_lowercase()
            .cmp(&b.display_name.to_ascii_lowercase())
            .then_with(|| a.item_id.cmp(&b.item_id)),
        SortOrder::IdAsc => a.item_id.cmp(&b.item_id),
        SortOrder::CountDesc => b
            .count
            .cmp(&a.count)
            .then_with(|| a.item_id.cmp(&b.item_id)),
    };
    base.then_with(|| a.exact_group.cmp(&b.exact_group))
        .then_with(|| b.enchanted.cmp(&a.enchanted))
        .then_with(|| a.damage.cmp(&b.damage))
        .then_with(|| b.count.cmp(&a.count))
}

/// Orders menu slot indices for grid sorting according to `FillDirection`.
/// Only selects unlocked slots inside the requested region (`storage` or `player_9..=35` or `hotbar`).
pub fn collect_sortable_menu_indices(
    entries: &[MenuSlotEntry],
    locked_slots: &BTreeSet<u32>,
    storage: bool,
    include_hotbar: bool,
    fill_dir: FillDirection,
) -> Vec<usize> {
    if storage {
        let storage_slots: Vec<usize> = entries
            .iter()
            .filter(|e| e.is_storage)
            .map(|e| e.menu_index)
            .collect();
        return reorder_grid_indices(&storage_slots, 9, fill_dir);
    }

    let player_map: HashMap<u32, usize> = entries
        .iter()
        .filter_map(|e| e.player_slot.map(|p| (p, e.menu_index)))
        .collect();

    let mut ordered_player_indices = Vec::with_capacity(36);
    match fill_dir {
        FillDirection::RowByRow => {
            ordered_player_indices.extend(9..=35u32);
        }
        FillDirection::ColumnByColumn => {
            for col in 0..9u32 {
                for row in 0..3u32 {
                    ordered_player_indices.push(9 + row * 9 + col);
                }
            }
        }
    }
    if include_hotbar {
        ordered_player_indices.extend(0..=8u32);
    }

    ordered_player_indices
        .into_iter()
        .filter(|ps| !locked_slots.contains(ps))
        .filter_map(|ps| player_map.get(&ps).copied())
        .collect()
}

fn reorder_grid_indices(indices: &[usize], cols: usize, fill_dir: FillDirection) -> Vec<usize> {
    if fill_dir == FillDirection::RowByRow || indices.len() <= cols || !indices.len().is_multiple_of(cols) {
        return indices.to_vec();
    }
    let rows = indices.len() / cols;
    let mut out = Vec::with_capacity(indices.len());
    for c in 0..cols {
        for r in 0..rows {
            out.push(indices[r * cols + c]);
        }
    }
    out
}

/// Simulates left-click inventory sorting across a sequence of unlocked menu indices.
struct SortSim {
    menu_indices: Vec<usize>,
    slots: Vec<Option<SlotSnapshot>>,
    hand: Option<SlotSnapshot>,
    clicks: Vec<PlanClick>,
}

impl SortSim {
    fn click(&mut self, i: usize) {
        self.clicks.push(PlanClick::LeftPickup(self.menu_indices[i]));
        let slot = self.slots[i].take();
        match (self.hand.take(), slot) {
            (None, s) => self.hand = s,
            (Some(h), None) => self.slots[i] = Some(h),
            (Some(mut h), Some(mut s)) if s.can_stack_with(&h) => {
                let space = s.max_stack.saturating_sub(s.count);
                let move_count = space.min(h.count);
                s.count += move_count;
                h.count -= move_count;
                self.slots[i] = Some(s);
                self.hand = (h.count > 0).then_some(h);
            }
            (Some(h), Some(s)) => {
                self.slots[i] = Some(h);
                self.hand = Some(s);
            }
        }
    }

    fn is_partial(&self, i: usize) -> Option<&SlotSnapshot> {
        self.slots[i].as_ref().filter(|s| s.count < s.max_stack)
    }
}

/// Plans smart, lock-aware Left-Pickup clicks to merge partial stacks and sort `ordered_indices`.
pub fn plan_sort_clicks(
    entries: &[MenuSlotEntry],
    ordered_indices: &[usize],
    order: SortOrder,
) -> Vec<PlanClick> {
    plan_sort_clicks_bounded(entries, ordered_indices, order, usize::MAX).0
}

/// Plans smart sort clicks and truncates at the first clean `hand == None` step boundary
/// once `max_clicks` is reached. Returns `(clicks, has_more_work)`.
pub fn plan_sort_clicks_bounded(
    entries: &[MenuSlotEntry],
    ordered_indices: &[usize],
    order: SortOrder,
    max_clicks: usize,
) -> (Vec<PlanClick>, bool) {
    if ordered_indices.len() < 2 {
        return (Vec::new(), false);
    }
    let by_menu: HashMap<usize, &Option<SlotSnapshot>> =
        entries.iter().map(|e| (e.menu_index, &e.stack)).collect();
    let initial: Vec<Option<SlotSnapshot>> = ordered_indices
        .iter()
        .map(|idx| by_menu.get(idx).and_then(|s| (*s).clone()))
        .collect();

    // Reject anomalous pre-overstacked inputs so we never risk leaving items on cursor.
    if initial
        .iter()
        .flatten()
        .any(|s| s.count == 0 || s.count > s.max_stack)
    {
        return (Vec::new(), false);
    }

    let n = initial.len();
    let mut sim = SortSim {
        menu_indices: ordered_indices.to_vec(),
        slots: initial,
        hand: None,
        clicks: Vec::new(),
    };
    let mut chunk_end: Option<usize> = None;

    // Phase A: Consolidate partial stackable stacks (later partial fills earlier partial).
    for i in 0..n {
        for j in (i + 1)..n {
            let Some(dst) = sim.is_partial(i).cloned() else {
                break;
            };
            let Some(src) = sim.is_partial(j) else {
                continue;
            };
            if !dst.can_stack_with(src) {
                continue;
            }
            sim.click(j);
            sim.click(i);
            if sim.hand.is_some() {
                sim.click(j);
            }
            if sim.hand.is_some() {
                return (Vec::new(), false);
            }
            if chunk_end.is_none() && sim.clicks.len() >= max_clicks {
                chunk_end = Some(sim.clicks.len());
            }
        }
    }

    // Build target sorted layout (full stacks appear before partial remainder due to count desc).
    let stacks: Vec<SlotSnapshot> = sim.slots.iter().flatten().cloned().collect();
    let mut rank_order: Vec<usize> = (0..stacks.len()).collect();
    rank_order.sort_by(|&a, &b| compare_snapshots(&stacks[a], &stacks[b], order));

    let mut target: Vec<Option<SlotSnapshot>> = rank_order
        .into_iter()
        .map(|i| Some(stacks[i].clone()))
        .collect();
    target.resize(n, None);

    // Phase B: Permute stacks into target order using Left-Pickup swaps.
    for i in 0..n {
        if slots_same(&sim.slots[i], &target[i]) {
            continue;
        }
        let Some(j) = ((i + 1)..n).find(|&j| slots_same(&sim.slots[j], &target[i])) else {
            return (Vec::new(), false);
        };
        sim.click(j);
        sim.click(i);
        if sim.hand.is_some() {
            sim.click(j);
        }
        if sim.hand.is_some() {
            return (Vec::new(), false);
        }
        if chunk_end.is_none() && sim.clicks.len() >= max_clicks {
            chunk_end = Some(sim.clicks.len());
        }
    }

    if sim.hand.is_some()
        || !sim
            .slots
            .iter()
            .zip(target.iter())
            .all(|(a, b)| slots_same(a, b))
    {
        return (Vec::new(), false);
    }

    if let Some(end) = chunk_end
        && end < sim.clicks.len()
    {
        sim.clicks.truncate(end);
        return (sim.clicks, true);
    }
    (sim.clicks, false)
}

fn slots_same(a: &Option<SlotSnapshot>, b: &Option<SlotSnapshot>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.matches_for_sort_swap(y),
        _ => false,
    }
}

fn push_quick_move_bounded(
    clicks: &mut Vec<PlanClick>,
    menu_index: usize,
    max_clicks: usize,
) -> bool {
    if clicks.len() >= max_clicks {
        return false;
    }
    clicks.push(PlanClick::QuickMove(menu_index));
    true
}

/// Plans atomic `LeftPickup` sequences that only top up existing `< max_stack` container slots.
fn plan_top_up_move_matching(
    storage_sim: &mut [Option<SlotSnapshot>],
    storage_sources: &[usize],
    player_sim: &mut [Option<SlotSnapshot>],
    player_sources: &[(usize, u32)],
    locked_slots: &BTreeSet<u32>,
    include_hotbar: bool,
    max_clicks: usize,
) -> (Vec<PlanClick>, bool) {
    let mut clicks = Vec::new();
    for (p_idx, &(p_menu_index, ps)) in player_sources.iter().enumerate() {
        if locked_slots.contains(&ps) || (ps < 9 && !include_hotbar) {
            continue;
        }
        let Some(src) = player_sim[p_idx].as_ref().cloned() else {
            continue;
        };
        if src.max_stack <= 1 {
            continue;
        }
        if !storage_sim
            .iter()
            .flatten()
            .any(|dst| dst.can_stack_with(&src) && dst.count < dst.max_stack)
        {
            continue;
        }
        let remaining_budget = max_clicks.saturating_sub(clicks.len());
        if remaining_budget < 2 {
            return (clicks, true);
        }

        let mut cursor = src.count;
        let mut group = vec![PlanClick::LeftPickup(p_menu_index)];
        let mut hit_limit = false;
        for (s_idx, &c_menu_index) in storage_sources.iter().enumerate() {
            if let Some(dst) = storage_sim[s_idx].as_mut()
                && dst.can_stack_with(&src)
                && dst.count < dst.max_stack
            {
                let space = dst.max_stack - dst.count;
                let will_empty = cursor <= space;
                let needed = group.len() + 1 + usize::from(!will_empty);
                if needed > remaining_budget {
                    hit_limit = true;
                    break;
                }
                let add = space.min(cursor);
                dst.count += add;
                cursor -= add;
                group.push(PlanClick::LeftPickup(c_menu_index));
                if cursor == 0 {
                    break;
                }
            }
        }
        if group.len() == 1 {
            // Budget cannot cover even `[pickup, dst, putback]`, defer cleanly with empty cursor.
            return (clicks, true);
        }
        if cursor > 0 {
            group.push(PlanClick::LeftPickup(p_menu_index));
        }
        if cursor == 0 {
            player_sim[p_idx] = None;
        } else if let Some(slot) = player_sim[p_idx].as_mut() {
            slot.count = cursor;
        }
        clicks.extend(group);
        if hit_limit {
            return (clicks, true);
        }
    }
    (clicks, false)
}

/// Plans unbounded `ExactItemAndNbt` transfer clicks for tests and full-pass callers.
pub fn plan_transfer_clicks(
    entries: &[MenuSlotEntry],
    locked_slots: &BTreeSet<u32>,
    mode: TransferMode,
    include_hotbar: bool,
) -> Vec<PlanClick> {
    plan_transfer_clicks_bounded(
        entries,
        locked_slots,
        mode,
        MoveMatchMode::ExactItemAndNbt,
        include_hotbar,
        usize::MAX,
    )
    .0
}

/// Plans bounded transfer clicks between player inventory and container storage.
/// Returns `(clicks, has_more)` where every batch leaves `cursor == None`.
pub fn plan_transfer_clicks_bounded(
    entries: &[MenuSlotEntry],
    locked_slots: &BTreeSet<u32>,
    mode: TransferMode,
    move_match_mode: MoveMatchMode,
    include_hotbar: bool,
    max_clicks: usize,
) -> (Vec<PlanClick>, bool) {
    let mut storage_sim: Vec<Option<SlotSnapshot>> = Vec::new();
    let mut player_sim: Vec<Option<SlotSnapshot>> = Vec::new();
    let mut player_sources: Vec<(usize, u32)> = Vec::new();
    let mut storage_sources: Vec<usize> = Vec::new();

    for entry in entries {
        if entry.is_storage {
            storage_sim.push(entry.stack.clone());
            storage_sources.push(entry.menu_index);
        } else if let Some(ps) = entry.player_slot
            && ps <= 35
        {
            player_sim.push(entry.stack.clone());
            player_sources.push((entry.menu_index, ps));
        }
    }

    if storage_sources.is_empty() || player_sources.is_empty() || max_clicks == 0 {
        return (Vec::new(), false);
    }

    match mode {
        TransferMode::MoveMatching if move_match_mode == MoveMatchMode::TopUpExistingOnly => {
            plan_top_up_move_matching(
                &mut storage_sim,
                &storage_sources,
                &mut player_sim,
                &player_sources,
                locked_slots,
                include_hotbar,
                max_clicks,
            )
        }
        TransferMode::MoveMatching | TransferMode::DumpAll => {
            let matching_groups: HashSet<u32> = storage_sim
                .iter()
                .flatten()
                .map(|s| s.match_group)
                .collect();
            if mode == TransferMode::MoveMatching && matching_groups.is_empty() {
                return (Vec::new(), false);
            }
            let mut clicks = Vec::new();
            for (idx, &(menu_index, ps)) in player_sources.iter().enumerate() {
                if locked_slots.contains(&ps) {
                    continue;
                }
                if ps < 9 && !include_hotbar {
                    continue;
                }
                let Some(stack) = player_sim[idx].as_ref() else {
                    continue;
                };
                if mode == TransferMode::MoveMatching
                    && !matching_groups.contains(&stack.match_group)
                {
                    continue;
                }
                if simulate_deposit(&mut storage_sim, stack) > 0
                    && !push_quick_move_bounded(&mut clicks, menu_index, max_clicks)
                {
                    return (clicks, true);
                }
            }
            (clicks, false)
        }
        TransferMode::PullMatching | TransferMode::PullAll => {
            let only_matching = mode == TransferMode::PullMatching;
            let matching_groups: HashSet<u32> = if only_matching {
                player_sources
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &(_, ps))| {
                        if ps < 9 && !include_hotbar {
                            return None;
                        }
                        player_sim[idx].as_ref().map(|s| s.match_group)
                    })
                    .collect()
            } else {
                HashSet::new()
            };
            if only_matching && matching_groups.is_empty() {
                return (Vec::new(), false);
            }
            let mut clicks = Vec::new();
            for (idx, &menu_index) in storage_sources.iter().enumerate() {
                let Some(stack) = storage_sim[idx].as_ref() else {
                    continue;
                };
                if only_matching && !matching_groups.contains(&stack.match_group) {
                    continue;
                }
                if simulate_deposit(&mut player_sim, stack) > 0
                    && !push_quick_move_bounded(&mut clicks, menu_index, max_clicks)
                {
                    return (clicks, true);
                }
            }
            (clicks, false)
        }
    }
}

/// Simulates adding as much of `stack` as possible into `dest_slots`; returns count moved (> 0 if any).
fn simulate_deposit(dest_slots: &mut [Option<SlotSnapshot>], stack: &SlotSnapshot) -> u32 {
    let mut remaining = stack.count;
    if stack.max_stack > 1 {
        for slot in dest_slots.iter_mut() {
            if let Some(existing) = slot.as_mut()
                && existing.can_stack_with(stack)
                && existing.count < existing.max_stack
            {
                let add = (existing.max_stack - existing.count).min(remaining);
                existing.count += add;
                remaining -= add;
                if remaining == 0 {
                    return stack.count;
                }
            }
        }
    }
    for slot in dest_slots.iter_mut() {
        if slot.is_none() {
            let mut placed = stack.clone();
            placed.count = remaining;
            *slot = Some(placed);
            return stack.count;
        }
    }
    stack.count - remaining
}

/// Plans bounded `ThrowWholeStack` clicks for `ThrowAll` (`target_match: None`) or
/// `ThrowMatching` (`target_match: Some(match_group)`) on container storage or unlocked player inventory.
pub fn plan_throw_clicks_bounded(
    entries: &[MenuSlotEntry],
    locked_slots: &BTreeSet<u32>,
    target_storage: bool,
    target_match: Option<u32>,
    include_hotbar: bool,
    max_clicks: usize,
) -> (Vec<PlanClick>, bool) {
    if max_clicks == 0 {
        return (Vec::new(), false);
    }
    let mut clicks = Vec::new();
    for entry in entries {
        if target_storage {
            if !entry.is_storage {
                continue;
            }
        } else if let Some(ps) = entry.player_slot {
            if ps > 35 || locked_slots.contains(&ps) || (ps < 9 && !include_hotbar) {
                continue;
            }
        } else {
            continue;
        }
        let Some(stack) = entry.stack.as_ref() else {
            continue;
        };
        if let Some(mg) = target_match
            && stack.match_group != mg
        {
            continue;
        }
        if clicks.len() >= max_clicks {
            return (clicks, true);
        }
        clicks.push(PlanClick::ThrowWholeStack(entry.menu_index));
    }
    (clicks, false)
}

/// Target hand for Auto-Refill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefillHand {
    Hotbar(u8),
    Offhand,
}

/// Plans a single atomic swap if held item in `hand` needs replacing (depleted or low-durability tool).
/// Searches unlocked main-inventory slots `Player(9..=35)` inside window 0.
pub fn plan_auto_refill(
    prev_item: Option<&SlotSnapshot>,
    curr_item: Option<&SlotSnapshot>,
    main_inventory: &[(usize, u32, Option<SlotSnapshot>)],
    locked_slots: &BTreeSet<u32>,
    hand: RefillHand,
    tool_threshold: u32,
) -> Option<(PlanClick, String)> {
    // Case 1: Held damageable tool is near breaking (`<= threshold`) and threshold > 0.
    if let Some(curr) = curr_item
        && curr.is_damageable()
        && tool_threshold > 0
        && curr.remaining_durability() <= tool_threshold
    {
        if let Some(src_menu) = find_refill_candidate(curr, true, tool_threshold, main_inventory, locked_slots)
        {
            let click = make_hand_swap(src_menu, hand);
            return Some((click, curr.display_name.clone()));
        }
        return None;
    }

    // Case 2: Item existed last tick and just disappeared (consumed last block/food or broke tool).
    let prev = prev_item?;
    if curr_item.is_some() {
        return None;
    }
    let src_menu = find_refill_candidate(prev, false, tool_threshold, main_inventory, locked_slots)?;
    Some((make_hand_swap(src_menu, hand), prev.display_name.clone()))
}

fn make_hand_swap(slot: usize, hand: RefillHand) -> PlanClick {
    match hand {
        RefillHand::Hotbar(hb) => PlanClick::SwapHotbar { slot, hotbar: hb },
        RefillHand::Offhand => PlanClick::SwapOffhand { slot },
    }
}

fn find_refill_candidate(
    wanted: &SlotSnapshot,
    avoid_low_durability: bool,
    tool_threshold: u32,
    main_inventory: &[(usize, u32, Option<SlotSnapshot>)],
    locked_slots: &BTreeSet<u32>,
) -> Option<usize> {
    let mut candidates: Vec<(usize, &SlotSnapshot)> = main_inventory
        .iter()
        .filter(|(_, ps, _)| (9..=35).contains(ps) && !locked_slots.contains(ps))
        .filter_map(|(menu_idx, _, opt)| opt.as_ref().map(|s| (*menu_idx, s)))
        .filter(|(_, s)| {
            if s.item_id != wanted.item_id {
                return false;
            }
            if avoid_low_durability && s.is_damageable() && s.remaining_durability() <= tool_threshold {
                return false;
            }
            if wanted.max_stack > 1 && s.max_stack > 1 {
                // For stackable items (blocks, food, arrows), match exact item ID cleanly; prefer identical stack_group first.
                return true;
            }
            true
        })
        .collect();

    candidates.sort_by(|(_, a), (_, b)| {
        let same_group_a = a.stack_group == wanted.stack_group;
        let same_group_b = b.stack_group == wanted.stack_group;
        same_group_b
            .cmp(&same_group_a)
            .then_with(|| b.enchanted.cmp(&a.enchanted))
            .then_with(|| b.remaining_durability().cmp(&a.remaining_durability()))
            .then_with(|| b.count.cmp(&a.count))
    });

    candidates.first().map(|(menu_idx, _)| *menu_idx)
}

/// Captures a persistent loadout preset from `Hotbar (0..=8)`, `Armor (36..=39)`, and `Offhand (40)`.
pub fn capture_profile_rules(entries: &[MenuSlotEntry]) -> Vec<ProfileSlotRule> {
    let mut rules: Vec<ProfileSlotRule> = entries
        .iter()
        .filter_map(|e| {
            let ps = e.player_slot?;
            if !(ps <= 8 || (36..=40).contains(&ps)) {
                return None;
            }
            let st = e.stack.as_ref()?;
            Some(ProfileSlotRule {
                player_slot: ps,
                item_id: st.item_id.clone(),
                preferred_name: st.display_name.clone(),
            })
        })
        .collect();
    rules.sort_by_key(|r| r.player_slot);
    rules
}

/// Plans swap / pickup clicks to apply `profile` using unlocked slots in the current open menu.
/// Works in both Player Inventory (`0..=40`) and Containers (`0..=8`, plus `40` if exposed).
pub fn plan_apply_profile(
    entries: &[MenuSlotEntry],
    locked_slots: &BTreeSet<u32>,
    profile: &InventoryProfile,
) -> Vec<PlanClick> {
    if profile.rules.is_empty() {
        return Vec::new();
    }

    // Track mutable snapshot keyed by player_slot (`0..=40` -> `(menu_index, Option<SlotSnapshot>)`).
    let mut by_player: HashMap<u32, (usize, Option<SlotSnapshot>)> = entries
        .iter()
        .filter_map(|e| e.player_slot.map(|ps| (ps, (e.menu_index, e.stack.clone()))))
        .collect();

    let rule_map: HashMap<u32, &ProfileSlotRule> = profile
        .rules
        .iter()
        .map(|r| (r.player_slot, r))
        .collect();

    let mut clicks = Vec::new();

    for rule in &profile.rules {
        let target_ps = rule.player_slot;
        if locked_slots.contains(&target_ps) {
            continue;
        }
        let Some(&(target_menu, ref current_opt)) = by_player.get(&target_ps) else {
            continue;
        };
        if current_opt
            .as_ref()
            .is_some_and(|s| s.item_id == rule.item_id)
        {
            continue;
        }

        // Find best unlocked source that matches `rule.item_id` and is NOT already in its own satisfied rule slot.
        let mut sources: Vec<(u32, usize, SlotSnapshot)> = by_player
            .iter()
            .filter_map(|(&src_ps, (src_menu, opt))| {
                if src_ps == target_ps || locked_slots.contains(&src_ps) {
                    return None;
                }
                let stack = opt.as_ref()?;
                if stack.item_id != rule.item_id {
                    return None;
                }
                if let Some(other_rule) = rule_map.get(&src_ps)
                    && other_rule.item_id == stack.item_id
                {
                    return None;
                }
                // Armor target slots require single-item armor stacks (`max_stack == 1`).
                if (36..=39).contains(&target_ps) && stack.max_stack > 1 {
                    return None;
                }
                Some((src_ps, *src_menu, stack.clone()))
            })
            .collect();

        sources.sort_by(|(ps_a, _, a), (ps_b, _, b)| {
            // Prefer taking from main inventory `9..=35` before rearranging other hotbar slots.
            let in_main_a = (9..=35).contains(ps_a);
            let in_main_b = (9..=35).contains(ps_b);
            let name_a = a.display_name == rule.preferred_name;
            let name_b = b.display_name == rule.preferred_name;
            in_main_b
                .cmp(&in_main_a)
                .then_with(|| name_b.cmp(&name_a))
                .then_with(|| b.enchanted.cmp(&a.enchanted))
                .then_with(|| b.remaining_durability().cmp(&a.remaining_durability()))
                .then_with(|| b.count.cmp(&a.count))
        });

        let Some((src_ps, src_menu, _)) = sources.into_iter().next() else {
            continue;
        };

        if target_ps <= 8 {
            clicks.push(PlanClick::SwapHotbar {
                slot: src_menu,
                hotbar: target_ps as u8,
            });
            swap_sim_slots(&mut by_player, src_ps, target_ps);
        } else if target_ps == 40 {
            clicks.push(PlanClick::SwapOffhand { slot: src_menu });
            swap_sim_slots(&mut by_player, src_ps, target_ps);
        } else if (36..=39).contains(&target_ps) {
            // Armor slots cannot use SwapWith; use Left-Pickup swap cycle (item max_stack == 1).
            let had_target = by_player.get(&target_ps).and_then(|v| v.1.as_ref()).is_some();
            clicks.push(PlanClick::LeftPickup(src_menu));
            clicks.push(PlanClick::LeftPickup(target_menu));
            if had_target {
                clicks.push(PlanClick::LeftPickup(src_menu));
            }
            swap_sim_slots(&mut by_player, src_ps, target_ps);
        }
    }

    clicks
}

fn swap_sim_slots(
    by_player: &mut HashMap<u32, (usize, Option<SlotSnapshot>)>,
    a: u32,
    b: u32,
) {
    let stack_a = by_player.get_mut(&a).and_then(|entry| entry.1.take());
    let stack_b = by_player.get_mut(&b).and_then(|entry| entry.1.take());
    if let Some(entry_a) = by_player.get_mut(&a) {
        entry_a.1 = stack_b;
    }
    if let Some(entry_b) = by_player.get_mut(&b) {
        entry_b.1 = stack_a;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PersistentState;

    fn snap(item_id: &str, name: &str, count: u32, max_stack: u32) -> SlotSnapshot {
        let mut group = 1u32;
        for b in item_id.bytes() {
            group = group.wrapping_mul(31).wrapping_add(u32::from(b));
        }
        SlotSnapshot {
            item_id: item_id.to_owned(),
            display_name: name.to_owned(),
            max_stack,
            max_damage: 0,
            damage: 0,
            count,
            enchanted: false,
            stack_group: group,
            match_group: group,
            exact_group: group,
        }
    }

    fn snap_nbt(
        item_id: &str,
        name: &str,
        count: u32,
        max_stack: u32,
        nbt_variant: u32,
    ) -> SlotSnapshot {
        let mut s = snap(item_id, name, count, max_stack);
        s.enchanted = nbt_variant != 0;
        s.stack_group ^= nbt_variant.wrapping_mul(131);
        s.match_group ^= nbt_variant.wrapping_mul(131);
        s.exact_group ^= nbt_variant.wrapping_mul(131);
        s
    }

    fn tool(item_id: &str, name: &str, max_damage: u32, damage: u32, enchanted: bool) -> SlotSnapshot {
        let mut s = snap(item_id, name, 1, 1);
        s.max_damage = max_damage;
        s.damage = damage;
        s.enchanted = enchanted;
        s.match_group ^= u32::from(enchanted).wrapping_mul(97);
        s.exact_group ^= damage.wrapping_mul(17) ^ u32::from(enchanted);
        s
    }

    fn apply_plan_clicks(slots: &mut [Option<SlotSnapshot>], clicks: &[PlanClick]) {
        let mut hand: Option<SlotSnapshot> = None;
        for &click in clicks {
            match click {
                PlanClick::LeftPickup(i) => {
                    let cur = slots[i].take();
                    match (hand.take(), cur) {
                        (None, s) => hand = s,
                        (Some(h), None) => slots[i] = Some(h),
                        (Some(mut h), Some(mut s)) if s.can_stack_with(&h) => {
                            let move_n = s.max_stack.saturating_sub(s.count).min(h.count);
                            s.count += move_n;
                            h.count -= move_n;
                            slots[i] = Some(s);
                            hand = (h.count > 0).then_some(h);
                        }
                        (Some(h), Some(s)) => {
                            slots[i] = Some(h);
                            hand = Some(s);
                        }
                    }
                }
                _ => panic!("unexpected click kind in sort test: {click:?}"),
            }
        }
        assert_eq!(hand, None, "hand must always end empty");
    }

    #[test]
    fn sort_skips_locked_slots_even_when_inventory_is_full() {
        // Construct a 27-slot player main inventory (menu indices 9..=35 matching player slots 9..=35).
        let mut entries = Vec::new();
        for p in 9..=35u32 {
            let stack = match p {
                9 => Some(snap("minecraft:cobblestone", "Cobblestone", 20, 64)),
                10 => Some(snap("minecraft:diamond_pickaxe", "Pick", 1, 1)), // LOCKED!
                11 => Some(snap("minecraft:apple", "Apple", 5, 64)),
                12 => Some(snap("minecraft:cobblestone", "Cobblestone", 50, 64)),
                _ => Some(snap("minecraft:dirt", "Dirt", 64, 64)), // Completely full!
            };
            entries.push(MenuSlotEntry {
                menu_index: p as usize,
                player_slot: Some(p),
                is_storage: false,
                stack,
            });
        }
        let locked = BTreeSet::from([10u32]);
        let order_indices =
            collect_sortable_menu_indices(&entries, &locked, false, false, FillDirection::RowByRow);
        assert!(!order_indices.contains(&10), "locked slot 10 must be excluded");

        let clicks = plan_sort_clicks(&entries, &order_indices, SortOrder::CategoryName);
        assert!(!clicks.is_empty());
        assert!(
            !clicks.iter().any(|c| matches!(c, PlanClick::LeftPickup(10))),
            "sort must never click locked slot 10"
        );

        let mut sim_menu: Vec<Option<SlotSnapshot>> = vec![None; 36];
        for e in &entries {
            sim_menu[e.menu_index] = e.stack.clone();
        }
        apply_plan_clicks(&mut sim_menu, &clicks);

        // Locked slot 10 preserved identically; apples (food rank 3) precede cobblestone/dirt (block rank 6);
        // cobblestone stacks merged cleanly into 64 + 6!
        assert_eq!(sim_menu[10].as_ref().unwrap().item_id, "minecraft:diamond_pickaxe");
        assert_eq!(sim_menu[9].as_ref().unwrap().item_id, "minecraft:apple");
        assert_eq!(sim_menu[11].as_ref().unwrap().item_id, "minecraft:cobblestone");
        assert_eq!(sim_menu[11].as_ref().unwrap().count, 64);
        assert_eq!(sim_menu[12].as_ref().unwrap().item_id, "minecraft:cobblestone");
        assert_eq!(sim_menu[12].as_ref().unwrap().count, 6);
    }

    #[test]
    fn column_sort_fills_vertically_without_touching_locked_slot() {
        let mut entries = Vec::new();
        for p in 9..=35u32 {
            let stack = match p {
                12 => Some(snap("minecraft:iron_sword", "Iron Sword", 1, 1)),
                20 => Some(snap("minecraft:apple", "Apple", 4, 64)),
                28 => Some(snap("minecraft:bread", "Bread", 10, 64)),
                _ => None,
            };
            entries.push(MenuSlotEntry {
                menu_index: p as usize,
                player_slot: Some(p),
                is_storage: false,
                stack,
            });
        }
        let locked = BTreeSet::from([18u32]); // Second row of col 0 is locked!
        let order_indices = collect_sortable_menu_indices(
            &entries,
            &locked,
            false,
            false,
            FillDirection::ColumnByColumn,
        );
        // First 3 slots in column 0 are 9, 18(locked->skip), 27, then column 1 slot 10!
        assert_eq!(&order_indices[0..3], &[9, 27, 10]);

        let clicks = plan_sort_clicks(&entries, &order_indices, SortOrder::CategoryName);
        let mut sim_menu: Vec<Option<SlotSnapshot>> = vec![None; 36];
        for e in &entries {
            sim_menu[e.menu_index] = e.stack.clone();
        }
        apply_plan_clicks(&mut sim_menu, &clicks);
        assert_eq!(sim_menu[9].as_ref().unwrap().item_id, "minecraft:iron_sword");
        assert_eq!(sim_menu[18], None);
        assert_eq!(sim_menu[27].as_ref().unwrap().item_id, "minecraft:apple");
        assert_eq!(sim_menu[10].as_ref().unwrap().item_id, "minecraft:bread");
    }

    #[test]
    fn move_matching_and_dump_all_respect_locks_hotbar_and_capacity() {
        let mut entries = Vec::new();
        // 3 chest storage slots (only 1 free, 1 partial iron ingot, 1 full oak planks)
        entries.push(MenuSlotEntry {
            menu_index: 0,
            player_slot: None,
            is_storage: true,
            stack: Some(snap("minecraft:iron_ingot", "Iron Ingot", 50, 64)),
        });
        entries.push(MenuSlotEntry {
            menu_index: 1,
            player_slot: None,
            is_storage: true,
            stack: None,
        });
        entries.push(MenuSlotEntry {
            menu_index: 2,
            player_slot: None,
            is_storage: true,
            stack: Some(snap("minecraft:oak_planks", "Oak Planks", 64, 64)),
        });
        // Player main inv 9..=12 mapped to menu 3..=6 + hotbar 0 at menu 30
        entries.push(MenuSlotEntry {
            menu_index: 3,
            player_slot: Some(9),
            is_storage: false,
            stack: Some(snap("minecraft:iron_ingot", "Iron Ingot", 10, 64)), // Matches, fits into slot 0!
        });
        entries.push(MenuSlotEntry {
            menu_index: 4,
            player_slot: Some(10),
            is_storage: false,
            stack: Some(snap("minecraft:iron_ingot", "Iron Ingot", 10, 64)), // Matches, overflows into free slot 1!
        });
        entries.push(MenuSlotEntry {
            menu_index: 5,
            player_slot: Some(11),
            is_storage: false,
            stack: Some(snap("minecraft:oak_planks", "Oak Planks", 8, 64)), // Matches, LOCKED!
        });
        entries.push(MenuSlotEntry {
            menu_index: 6,
            player_slot: Some(12),
            is_storage: false,
            stack: Some(snap("minecraft:cobblestone", "Cobblestone", 32, 64)), // Doesn't match
        });
        entries.push(MenuSlotEntry {
            menu_index: 30,
            player_slot: Some(0),
            is_storage: false,
            stack: Some(snap("minecraft:iron_ingot", "Iron Ingot", 32, 64)), // Hotbar excluded by default
        });

        let locked = BTreeSet::from([11u32]);
        let matching = plan_transfer_clicks(&entries, &locked, TransferMode::MoveMatching, false);
        assert_eq!(
            matching,
            vec![PlanClick::QuickMove(3), PlanClick::QuickMove(4)]
        );

        let dump = plan_transfer_clicks(&entries, &locked, TransferMode::DumpAll, false);
        // Chest has space in slot 0 (14 iron) and slot 1 (empty), once slot 4 fills slot 1, cobblestone is ignored (chest full!)
        assert_eq!(dump, vec![PlanClick::QuickMove(3), PlanClick::QuickMove(4)]);

        let pull = plan_transfer_clicks(&entries, &locked, TransferMode::PullMatching, false);
        assert_eq!(pull, vec![PlanClick::QuickMove(0), PlanClick::QuickMove(2)]);

        let mut pull_all_entries = entries.clone();
        pull_all_entries.push(MenuSlotEntry {
            menu_index: 7,
            player_slot: Some(13),
            is_storage: false,
            stack: None,
        });
        pull_all_entries.push(MenuSlotEntry {
            menu_index: 8,
            player_slot: Some(14),
            is_storage: false,
            stack: None,
        });
        pull_all_entries.push(MenuSlotEntry {
            menu_index: 20,
            player_slot: None,
            is_storage: true,
            stack: Some(snap("minecraft:diamond", "Diamond", 3, 64)),
        });
        let pull_all =
            plan_transfer_clicks(&pull_all_entries, &locked, TransferMode::PullAll, false);
        assert_eq!(
            pull_all,
            vec![
                PlanClick::QuickMove(0),
                PlanClick::QuickMove(2),
                PlanClick::QuickMove(20)
            ]
        );
    }

    #[test]
    fn move_and_pull_matching_require_same_nbt_match_group() {
        let custom_salmon = snap_nbt("minecraft:salmon", "Golden Salmon", 12, 64, 1);
        let plain_salmon = snap("minecraft:salmon", "Raw Salmon", 64, 64);
        let ench_rod_fresh = tool("minecraft:fishing_rod", "Luck Rod", 64, 0, true);
        let ench_rod_worn = tool("minecraft:fishing_rod", "Luck Rod", 64, 25, true);
        let plain_rod = tool("minecraft:fishing_rod", "Fishing Rod", 64, 0, false);

        let entries = vec![
            // Storage: plain salmon, enchanted rod, 2 empty slots
            MenuSlotEntry {
                menu_index: 0,
                player_slot: None,
                is_storage: true,
                stack: Some(plain_salmon.clone()),
            },
            MenuSlotEntry {
                menu_index: 1,
                player_slot: None,
                is_storage: true,
                stack: Some(ench_rod_fresh),
            },
            MenuSlotEntry {
                menu_index: 2,
                player_slot: None,
                is_storage: true,
                stack: None,
            },
            MenuSlotEntry {
                menu_index: 3,
                player_slot: None,
                is_storage: true,
                stack: None,
            },
            // Player main: custom salmon (same item_id, different NBT -> must NOT move),
            // plain rod (different NBT -> must NOT move), matching plain salmon + matching worn ench rod
            MenuSlotEntry {
                menu_index: 4,
                player_slot: Some(9),
                is_storage: false,
                stack: Some(custom_salmon.clone()),
            },
            MenuSlotEntry {
                menu_index: 5,
                player_slot: Some(10),
                is_storage: false,
                stack: Some(plain_rod),
            },
            MenuSlotEntry {
                menu_index: 6,
                player_slot: Some(11),
                is_storage: false,
                stack: Some(plain_salmon),
            },
            MenuSlotEntry {
                menu_index: 7,
                player_slot: Some(12),
                is_storage: false,
                stack: Some(ench_rod_worn),
            },
        ];

        let locked = BTreeSet::new();
        let moved = plan_transfer_clicks(&entries, &locked, TransferMode::MoveMatching, false);
        assert_eq!(
            moved,
            vec![PlanClick::QuickMove(6), PlanClick::QuickMove(7)],
            "different-NBT items sharing base item_id must not fill empty chest slots"
        );

        // PullMatching also respects NBT match_group: custom salmon in chest must not match plain salmon in inv.
        let mut pull_entries = entries;
        pull_entries[0].stack = Some(custom_salmon);
        pull_entries[4].stack = None;
        let pulled = plan_transfer_clicks(&pull_entries, &locked, TransferMode::PullMatching, false);
        assert_eq!(
            pulled,
            vec![PlanClick::QuickMove(1)],
            "PullMatching should pull matching enchanted rod, not mismatched NBT salmon"
        );
    }

    #[test]
    fn move_match_top_up_only_never_fills_empty_chest_slots() {
        let mut entries = vec![
            // Storage 0..=4: partial salmon, full salmon, empty, empty, tool
            MenuSlotEntry {
                menu_index: 0,
                player_slot: None,
                is_storage: true,
                stack: Some(snap("minecraft:salmon", "Raw Salmon", 40, 64)),
            },
            MenuSlotEntry {
                menu_index: 1,
                player_slot: None,
                is_storage: true,
                stack: Some(snap("minecraft:salmon", "Raw Salmon", 64, 64)),
            },
            MenuSlotEntry {
                menu_index: 2,
                player_slot: None,
                is_storage: true,
                stack: None,
            },
            MenuSlotEntry {
                menu_index: 3,
                player_slot: None,
                is_storage: true,
                stack: None,
            },
            MenuSlotEntry {
                menu_index: 4,
                player_slot: None,
                is_storage: true,
                stack: Some(tool("minecraft:fishing_rod", "Rod", 64, 0, false)),
            },
            // Player 5..=8: salmon 64 (only 24 fits in slot 0!), extra salmon 20, rod, custom salmon
            MenuSlotEntry {
                menu_index: 5,
                player_slot: Some(9),
                is_storage: false,
                stack: Some(snap("minecraft:salmon", "Raw Salmon", 64, 64)),
            },
            MenuSlotEntry {
                menu_index: 6,
                player_slot: Some(10),
                is_storage: false,
                stack: Some(snap("minecraft:salmon", "Raw Salmon", 20, 64)),
            },
            MenuSlotEntry {
                menu_index: 7,
                player_slot: Some(11),
                is_storage: false,
                stack: Some(tool("minecraft:fishing_rod", "Rod", 64, 0, false)),
            },
            MenuSlotEntry {
                menu_index: 8,
                player_slot: Some(12),
                is_storage: false,
                stack: Some(snap_nbt("minecraft:salmon", "Special Salmon", 10, 64, 2)),
            },
        ];

        let locked = BTreeSet::new();
        let (clicks, has_more) = plan_transfer_clicks_bounded(
            &entries,
            &locked,
            TransferMode::MoveMatching,
            MoveMatchMode::TopUpExistingOnly,
            false,
            12,
        );
        assert!(!has_more);
        assert_eq!(
            clicks,
            vec![
                PlanClick::LeftPickup(5),
                PlanClick::LeftPickup(0),
                PlanClick::LeftPickup(5),
            ]
        );

        let mut slots: Vec<Option<SlotSnapshot>> = entries.iter().map(|e| e.stack.clone()).collect();
        apply_plan_clicks(&mut slots, &clicks);
        assert_eq!(slots[0].as_ref().unwrap().count, 64);
        assert_eq!(slots[2], None, "empty chest slot 2 must remain empty");
        assert_eq!(slots[3], None, "empty chest slot 3 must remain empty");
        assert_eq!(slots[5].as_ref().unwrap().count, 40);
        assert_eq!(slots[6].as_ref().unwrap().count, 20);
        assert_eq!(slots[7].as_ref().unwrap().count, 1);

        // Next call should report nothing left to top up and not touch empty slots.
        for (i, e) in entries.iter_mut().enumerate() {
            e.stack = slots[i].clone();
        }
        let (followup, _) = plan_transfer_clicks_bounded(
            &entries,
            &locked,
            TransferMode::MoveMatching,
            MoveMatchMode::TopUpExistingOnly,
            false,
            12,
        );
        assert!(followup.is_empty());
    }

    #[test]
    fn move_match_top_up_bounded_batch_keeps_cursor_empty() {
        let mut entries = Vec::new();
        // Chest 0..=4: 5 partial stacks of cobble x50, slot 5 empty
        for i in 0..5usize {
            entries.push(MenuSlotEntry {
                menu_index: i,
                player_slot: None,
                is_storage: true,
                stack: Some(snap("minecraft:cobblestone", "Cobble", 50, 64)),
            });
        }
        entries.push(MenuSlotEntry {
            menu_index: 5,
            player_slot: None,
            is_storage: true,
            stack: None,
        });
        // Player slots 6..=8: 3 stacks of cobble x64 (192 items total; only 5 * 14 = 70 can fit)
        for j in 0..3usize {
            entries.push(MenuSlotEntry {
                menu_index: 6 + j,
                player_slot: Some(9 + j as u32),
                is_storage: false,
                stack: Some(snap("minecraft:cobblestone", "Cobble", 64, 64)),
            });
        }

        let locked = BTreeSet::new();
        let mut slots: Vec<Option<SlotSnapshot>> = entries.iter().map(|e| e.stack.clone()).collect();
        let mut rounds = 0usize;
        loop {
            for (i, e) in entries.iter_mut().enumerate() {
                e.stack = slots[i].clone();
            }
            let (clicks, has_more) = plan_transfer_clicks_bounded(
                &entries,
                &locked,
                TransferMode::MoveMatching,
                MoveMatchMode::TopUpExistingOnly,
                false,
                4,
            );
            assert!(clicks.len() <= 4);
            apply_plan_clicks(&mut slots, &clicks);
            rounds += 1;
            assert!(rounds < 10, "bounded top-up loop did not terminate");
            if !has_more {
                break;
            }
        }
        for i in 0..5 {
            assert_eq!(slots[i].as_ref().unwrap().count, 64);
        }
        assert_eq!(slots[5], None, "empty chest slot must remain empty");
    }

    #[test]
    fn auto_refill_saves_near_broken_tool_and_refills_depleted_blocks() {
        let low_pick = tool("minecraft:diamond_pickaxe", "Miner", 1561, 1559, true); // 2 durability left
        let spare_bad = tool("minecraft:diamond_pickaxe", "Backup", 1561, 1560, false); // 1 left (too low!)
        let spare_good = tool("minecraft:diamond_pickaxe", "Fresh", 1561, 100, true);
        let cobble = snap("minecraft:cobblestone", "Cobble", 64, 64);
        let cobble_small = snap("minecraft:cobblestone", "Cobble", 12, 64);

        let main = vec![
            (9usize, 9u32, Some(spare_bad)),
            (10usize, 10u32, Some(spare_good.clone())), // Locked below to test lock skip!
            (11usize, 11u32, Some(spare_good)),
            (12usize, 12u32, Some(cobble_small)),
            (13usize, 13u32, Some(cobble.clone())),
        ];
        let locked = BTreeSet::from([10u32]);

        let tool_res = plan_auto_refill(
            Some(&low_pick),
            Some(&low_pick),
            &main,
            &locked,
            RefillHand::Hotbar(2),
            3,
        );
        assert_eq!(
            tool_res,
            Some((
                PlanClick::SwapHotbar {
                    slot: 11,
                    hotbar: 2
                },
                "Miner".to_owned()
            ))
        );

        let block_res = plan_auto_refill(
            Some(&cobble),
            None,
            &main,
            &locked,
            RefillHand::Offhand,
            3,
        );
        assert_eq!(
            block_res,
            Some((PlanClick::SwapOffhand { slot: 13 }, "Cobble".to_owned()))
        );
    }

    #[test]
    fn profile_capture_apply_and_state_roundtrip() {
        let entries = vec![
            MenuSlotEntry {
                menu_index: 5,
                player_slot: Some(39), // helmet
                is_storage: false,
                stack: None,
            },
            MenuSlotEntry {
                menu_index: 9,
                player_slot: Some(9),
                is_storage: false,
                stack: Some(tool("minecraft:diamond_helmet", "Helm", 363, 0, true)),
            },
            MenuSlotEntry {
                menu_index: 10,
                player_slot: Some(10),
                is_storage: false,
                stack: Some(snap("minecraft:diamond_sword", "Excalibur", 1, 1)),
            },
            MenuSlotEntry {
                menu_index: 11,
                player_slot: Some(11),
                is_storage: false,
                stack: Some(snap("minecraft:totem_of_undying", "Totem", 1, 1)),
            },
            MenuSlotEntry {
                menu_index: 36,
                player_slot: Some(0),
                is_storage: false,
                stack: Some(snap("minecraft:dirt", "Dirt", 3, 64)),
            },
            MenuSlotEntry {
                menu_index: 45,
                player_slot: Some(40),
                is_storage: false,
                stack: None,
            },
        ];

        let profile = InventoryProfile {
            name: "Combat=PvP|1".to_owned(),
            rules: vec![
                ProfileSlotRule {
                    player_slot: 0,
                    item_id: "minecraft:diamond_sword".to_owned(),
                    preferred_name: "Excalibur".to_owned(),
                },
                ProfileSlotRule {
                    player_slot: 39,
                    item_id: "minecraft:diamond_helmet".to_owned(),
                    preferred_name: "Helm".to_owned(),
                },
                ProfileSlotRule {
                    player_slot: 40,
                    item_id: "minecraft:totem_of_undying".to_owned(),
                    preferred_name: "Totem".to_owned(),
                },
            ],
        };

        let clicks = plan_apply_profile(&entries, &BTreeSet::new(), &profile);
        assert_eq!(
            clicks,
            vec![
                PlanClick::SwapHotbar {
                    slot: 10,
                    hotbar: 0
                },
                PlanClick::LeftPickup(9),
                PlanClick::LeftPickup(5),
                PlanClick::SwapOffhand { slot: 11 },
            ]
        );

        let mut st = PersistentState::default();
        st.locked_slots = BTreeSet::from([0, 39, 40]);
        st.lock_mode = true;
        st.active_profile = 2;
        st.custom_offset = Some([-82.0, 14.5]);
        st.profiles[2] = profile;
        let encoded = st.encode();
        let decoded = PersistentState::decode(&encoded);
        assert_eq!(st, decoded);
    }

    #[test]
    fn container_storage_sort_handles_column_fill_and_overstack_guard() {
        let mut entries = Vec::new();
        for s in 0..27usize {
            let stack = match s {
                5 => Some(snap("minecraft:torch", "Torch", 12, 64)),
                10 => Some(snap("minecraft:oak_log", "Oak Log", 48, 64)),
                20 => Some(snap("minecraft:torch", "Torch", 20, 64)),
                _ => None,
            };
            entries.push(MenuSlotEntry {
                menu_index: s,
                player_slot: None,
                is_storage: true,
                stack,
            });
        }
        let ordered = collect_sortable_menu_indices(
            &entries,
            &BTreeSet::new(),
            true,
            false,
            FillDirection::ColumnByColumn,
        );
        // Col 0 indices: 0, 9, 18; Col 1 start: 1
        assert_eq!(&ordered[0..4], &[0, 9, 18, 1]);

        let clicks = plan_sort_clicks(&entries, &ordered, SortOrder::CountDesc);
        let mut sim: Vec<Option<SlotSnapshot>> = entries.iter().map(|e| e.stack.clone()).collect();
        apply_plan_clicks(&mut sim, &clicks);
        // Sorted by count desc: Oak Log (48) at 0, Torch (32 merged) at 9!
        assert_eq!(sim[0].as_ref().unwrap().item_id, "minecraft:oak_log");
        assert_eq!(sim[0].as_ref().unwrap().count, 48);
        assert_eq!(sim[9].as_ref().unwrap().item_id, "minecraft:torch");
        assert_eq!(sim[9].as_ref().unwrap().count, 32);

        // Overstacked guard: refuse to click anything rather than strand cursor.
        entries[0].stack = Some(snap("minecraft:cobblestone", "Cobble", 99, 64));
        assert!(plan_sort_clicks(&entries, &ordered, SortOrder::CategoryName).is_empty());
    }

    #[test]
    fn capture_profile_and_locked_target_protection() {
        let entries = vec![
            MenuSlotEntry {
                menu_index: 9,
                player_slot: Some(9), // Main inventory must NOT be captured into loadout rules
                is_storage: false,
                stack: Some(snap("minecraft:cobblestone", "Cobble", 64, 64)),
            },
            MenuSlotEntry {
                menu_index: 36,
                player_slot: Some(0), // Hotbar 0
                is_storage: false,
                stack: Some(snap("minecraft:diamond_sword", "Sword", 1, 1)),
            },
            MenuSlotEntry {
                menu_index: 45,
                player_slot: Some(40), // Offhand
                is_storage: false,
                stack: Some(snap("minecraft:shield", "Shield", 1, 1)),
            },
        ];
        let rules = capture_profile_rules(&entries);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].player_slot, 0);
        assert_eq!(rules[1].player_slot, 40);

        // If slot 0 and source slot 9 are locked, plan_apply_profile skips touching both.
        let profile = InventoryProfile {
            name: "P1".to_owned(),
            rules: vec![ProfileSlotRule {
                player_slot: 0,
                item_id: "minecraft:cobblestone".to_owned(),
                preferred_name: "Cobble".to_owned(),
            }],
        };
        assert!(plan_apply_profile(&entries, &BTreeSet::from([0u32]), &profile).is_empty());
        assert!(plan_apply_profile(&entries, &BTreeSet::from([9u32]), &profile).is_empty());
    }

    #[test]
    fn bounded_multi_chunk_sort_converges_cleanly() {
        let items = [
            ("minecraft:dirt", "Dirt", 10, 64),
            ("minecraft:stone", "Stone", 64, 64),
            ("minecraft:apple", "Apple", 5, 64),
            ("minecraft:dirt", "Dirt", 60, 64),
            ("minecraft:diamond_pickaxe", "Diamond Pickaxe", 1, 1),
            ("minecraft:apple", "Apple", 12, 64),
            ("minecraft:iron_ingot", "Iron Ingot", 8, 64),
            ("minecraft:oak_planks", "Oak Planks", 16, 64),
            ("minecraft:torch", "Torch", 20, 64),
            ("minecraft:torch", "Torch", 50, 64),
        ];
        let mut sim_slots: Vec<Option<SlotSnapshot>> = items
            .iter()
            .map(|(id, name, c, m)| Some(snap(id, name, *c, *m)))
            .collect();
        let ordered: Vec<usize> = (0..sim_slots.len()).collect();

        let mut chunks = 0;
        loop {
            let entries: Vec<MenuSlotEntry> = sim_slots
                .iter()
                .enumerate()
                .map(|(idx, s)| MenuSlotEntry {
                    menu_index: idx,
                    player_slot: None,
                    is_storage: true,
                    stack: s.clone(),
                })
                .collect();
            let (batch, more) =
                plan_sort_clicks_bounded(&entries, &ordered, SortOrder::CategoryName, 6);
            if batch.is_empty() {
                assert!(!more);
                break;
            }
            assert!(batch.len() <= 9);
            apply_plan_clicks(&mut sim_slots, &batch);
            chunks += 1;
            assert!(chunks <= 20, "bounded sort should converge quickly");
            if !more {
                break;
            }
        }
        assert!(chunks > 1, "should have split into multiple safe ticks");
        // Verify sorted order: diamond pickaxe (tools 0) -> apples 17 (food 3) -> iron ingot (ores 5) -> blocks
        assert_eq!(sim_slots[0].as_ref().unwrap().item_id, "minecraft:diamond_pickaxe");
        assert_eq!(sim_slots[1].as_ref().unwrap().item_id, "minecraft:apple");
        assert_eq!(sim_slots[1].as_ref().unwrap().count, 17);
        assert_eq!(sim_slots[2].as_ref().unwrap().item_id, "minecraft:iron_ingot");
    }

    #[test]
    fn throw_matching_respects_side_nbt_and_locks() {
        let plain_salmon = snap("minecraft:salmon", "Raw Salmon", 32, 64);
        let custom_salmon = snap_nbt("minecraft:salmon", "Boss Salmon", 12, 64, 3);
        let cobble = snap("minecraft:cobblestone", "Cobble", 16, 64);

        let entries = vec![
            // Storage 0..=2
            MenuSlotEntry {
                menu_index: 0,
                player_slot: None,
                is_storage: true,
                stack: Some(plain_salmon.clone()),
            },
            MenuSlotEntry {
                menu_index: 1,
                player_slot: None,
                is_storage: true,
                stack: Some(custom_salmon),
            },
            MenuSlotEntry {
                menu_index: 2,
                player_slot: None,
                is_storage: true,
                stack: Some(plain_salmon.clone()),
            },
            // Player main 3..=5 + hotbar 6 (slot 0) + locked main 4 (slot 10)
            MenuSlotEntry {
                menu_index: 3,
                player_slot: Some(9),
                is_storage: false,
                stack: Some(plain_salmon.clone()),
            },
            MenuSlotEntry {
                menu_index: 4,
                player_slot: Some(10),
                is_storage: false,
                stack: Some(plain_salmon.clone()), // LOCKED
            },
            MenuSlotEntry {
                menu_index: 5,
                player_slot: Some(11),
                is_storage: false,
                stack: Some(cobble),
            },
            MenuSlotEntry {
                menu_index: 6,
                player_slot: Some(0),
                is_storage: false,
                stack: Some(plain_salmon.clone()),
            },
        ];

        let locked = BTreeSet::from([10u32]);
        let (chest_throws, more) = plan_throw_clicks_bounded(
            &entries,
            &locked,
            true,
            Some(plain_salmon.match_group),
            false,
            12,
        );
        assert!(!more);
        assert_eq!(
            chest_throws,
            vec![PlanClick::ThrowWholeStack(0), PlanClick::ThrowWholeStack(2)]
        );

        let (inv_throws_no_hotbar, _) = plan_throw_clicks_bounded(
            &entries,
            &locked,
            false,
            Some(plain_salmon.match_group),
            false,
            12,
        );
        assert_eq!(inv_throws_no_hotbar, vec![PlanClick::ThrowWholeStack(3)]);

        let (inv_throws_with_hotbar, _) = plan_throw_clicks_bounded(
            &entries,
            &locked,
            false,
            Some(plain_salmon.match_group),
            true,
            12,
        );
        assert_eq!(
            inv_throws_with_hotbar,
            vec![PlanClick::ThrowWholeStack(3), PlanClick::ThrowWholeStack(6)]
        );
    }

    #[test]
    fn throw_all_chunks_cleanly_and_protects_locked_player_slots() {
        let entries = vec![
            MenuSlotEntry {
                menu_index: 0,
                player_slot: Some(9),
                is_storage: false,
                stack: Some(snap("minecraft:dirt", "Dirt", 64, 64)),
            },
            MenuSlotEntry {
                menu_index: 1,
                player_slot: Some(10),
                is_storage: false,
                stack: Some(snap("minecraft:diamond", "Diamond", 5, 64)), // LOCKED
            },
            MenuSlotEntry {
                menu_index: 2,
                player_slot: Some(11),
                is_storage: false,
                stack: Some(snap("minecraft:gravel", "Gravel", 32, 64)),
            },
            MenuSlotEntry {
                menu_index: 3,
                player_slot: Some(12),
                is_storage: false,
                stack: Some(snap("minecraft:sand", "Sand", 16, 64)),
            },
            // Armor slot 39 & offhand 40 & hotbar 0 must not be thrown when include_hotbar = false
            MenuSlotEntry {
                menu_index: 4,
                player_slot: Some(39),
                is_storage: false,
                stack: Some(snap("minecraft:diamond_helmet", "Helmet", 1, 1)),
            },
            MenuSlotEntry {
                menu_index: 5,
                player_slot: Some(40),
                is_storage: false,
                stack: Some(snap("minecraft:shield", "Shield", 1, 1)),
            },
            MenuSlotEntry {
                menu_index: 6,
                player_slot: Some(0),
                is_storage: false,
                stack: Some(snap("minecraft:steak", "Steak", 32, 64)),
            },
        ];

        let locked = BTreeSet::from([10u32]);
        let (batch1, more1) = plan_throw_clicks_bounded(&entries, &locked, false, None, false, 2);
        assert!(more1);
        assert_eq!(
            batch1,
            vec![PlanClick::ThrowWholeStack(0), PlanClick::ThrowWholeStack(2)]
        );

        let (full, more_full) = plan_throw_clicks_bounded(&entries, &locked, false, None, false, 12);
        assert!(!more_full);
        assert_eq!(
            full,
            vec![
                PlanClick::ThrowWholeStack(0),
                PlanClick::ThrowWholeStack(2),
                PlanClick::ThrowWholeStack(3),
            ]
        );
    }
}
