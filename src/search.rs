//! Pure item-search matching, text normalization, NBT extraction, and search bar layout.

use mod_api::text::{TextContent, TextObject};
use mod_api::{Components, Tag, Text, TooltipElement};

/// Search bar outer dimensions & padding constants (in GUI pixels).
pub const SEARCH_BAR_H: f32 = 18.0;
pub const SEARCH_BAR_PAD: f32 = 2.0;
pub const SEARCH_GRIP_W: f32 = 10.0;
pub const SEARCH_CLEAR_W: f32 = 14.0;
pub const SEARCH_COUNT_W: f32 = 24.0;

const MAX_EXTRACT_DEPTH: usize = 64;

/// Computed GUI geometry for the draggable search bar widget group.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchBarLayout {
    pub bounds: [f32; 4],
    pub grip_rect: [f32; 4],
    pub field_rect: [f32; 4],
    pub count_rect: Option<[f32; 4]>,
    pub clear_rect: [f32; 4],
}

/// Width matched to the open container screen (standard 176 px chest/inventory width).
pub fn search_bar_width(screen_rect: [f32; 4]) -> f32 {
    screen_rect[2].clamp(140.0, 176.0)
}

/// Clamps a bar top-left coordinate so the full bar stays visible inside `screen_size`.
pub fn clamp_bar_origin(x: f32, y: f32, bar_w: f32, bar_h: f32, screen_size: [f32; 2]) -> [f32; 2] {
    let cx = if screen_size[0] > bar_w + 4.0 {
        x.clamp(2.0, screen_size[0] - bar_w - 2.0)
    } else {
        x.max(2.0)
    };
    let cy = if screen_size[1] > bar_h + 4.0 {
        y.clamp(2.0, screen_size[1] - bar_h - 2.0)
    } else {
        y.max(2.0)
    };
    [cx, cy]
}

/// Resolves the search bar origin: centered directly above `screen_rect` unless dragged.
pub fn resolve_search_bar_origin(
    screen_rect: [f32; 4],
    screen_size: [f32; 2],
    bar_w: f32,
    search_offset: Option<[f32; 2]>,
) -> [f32; 2] {
    if let Some([dx, dy]) = search_offset {
        return clamp_bar_origin(
            screen_rect[0] + dx,
            screen_rect[1] + dy,
            bar_w,
            SEARCH_BAR_H,
            screen_size,
        );
    }
    let default_x = screen_rect[0] + (screen_rect[2] - bar_w) * 0.5;
    let default_y = screen_rect[1] - SEARCH_BAR_H - 3.0;
    clamp_bar_origin(default_x, default_y, bar_w, SEARCH_BAR_H, screen_size)
}

/// Builds the full hit-test & render layout for the search bar.
pub fn build_search_bar_layout(
    screen_rect: [f32; 4],
    screen_size: [f32; 2],
    search_offset: Option<[f32; 2]>,
    has_query: bool,
) -> SearchBarLayout {
    let w = search_bar_width(screen_rect);
    let [x, y] = resolve_search_bar_origin(screen_rect, screen_size, w, search_offset);
    let inner_y = y + SEARCH_BAR_PAD;
    let inner_h = SEARCH_BAR_H - SEARCH_BAR_PAD * 2.0;

    let grip_rect = [x + SEARCH_BAR_PAD, inner_y, SEARCH_GRIP_W, inner_h];
    let clear_rect = [
        x + w - SEARCH_BAR_PAD - SEARCH_CLEAR_W,
        inner_y,
        SEARCH_CLEAR_W,
        inner_h,
    ];
    let field_x = grip_rect[0] + SEARCH_GRIP_W + SEARCH_BAR_PAD;

    let (count_rect, field_w) = if has_query {
        let count_x = clear_rect[0] - SEARCH_BAR_PAD - SEARCH_COUNT_W;
        (
            Some([count_x, inner_y, SEARCH_COUNT_W, inner_h]),
            (count_x - SEARCH_BAR_PAD - field_x).max(40.0),
        )
    } else {
        (None, (clear_rect[0] - SEARCH_BAR_PAD - field_x).max(40.0))
    };

    SearchBarLayout {
        bounds: [x, y, w, SEARCH_BAR_H],
        grip_rect,
        field_rect: [field_x, inner_y, field_w, inner_h],
        count_rect,
        clear_rect,
    }
}

/// Folds fancy server typography (small-caps, fullwidth, math alphanumeric) to ASCII lowercase.
fn fold_unicode_char(ch: char) -> Option<char> {
    match ch {
        '\u{200B}'..='\u{200D}' | '\u{FEFF}' => None,
        '\u{3000}' => Some(' '),
        '\u{FF01}'..='\u{FF5E}' => char::from_u32((ch as u32) - 0xFEE0),
        '\u{210E}' => Some('h'),
        'ᴀ' => Some('a'),
        'ʙ' => Some('b'),
        'ᴄ' => Some('c'),
        'ᴅ' => Some('d'),
        'ᴇ' => Some('e'),
        'ꜰ' => Some('f'),
        'ɢ' => Some('g'),
        'ʜ' => Some('h'),
        'ɪ' => Some('i'),
        'ᴊ' => Some('j'),
        'ᴋ' => Some('k'),
        'ʟ' => Some('l'),
        'ᴍ' => Some('m'),
        'ɴ' => Some('n'),
        'ᴏ' => Some('o'),
        'ᴘ' => Some('p'),
        'ǫ' | 'ꞯ' => Some('q'),
        'ʀ' => Some('r'),
        'ꜱ' => Some('s'),
        'ᴛ' => Some('t'),
        'ᴜ' => Some('u'),
        'ᴠ' => Some('v'),
        'ᴡ' => Some('w'),
        'ʏ' => Some('y'),
        'ᴢ' => Some('z'),
        _ => {
            let cp = ch as u32;
            if (0x1D400..=0x1D6A3).contains(&cp) {
                let offset = ((cp - 0x1D400) % 26) as u8;
                Some((b'a' + offset) as char)
            } else if (0x1D7CE..=0x1D7FF).contains(&cp) {
                let offset = ((cp - 0x1D7CE) % 10) as u8;
                Some((b'0' + offset) as char)
            } else {
                Some(ch)
            }
        }
    }
}

/// Strips legacy `§` formatting codes, normalizes server glyphs, lowercases, and collapses spaces.
pub fn normalize_search_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    let mut prev_space = true;

    while let Some(raw) = chars.next() {
        if raw == '§' {
            let _ = chars.next();
            continue;
        }
        let Some(folded) = fold_unicode_char(raw) else {
            continue;
        };
        if folded.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        for lc in folded.to_lowercase() {
            if lc.is_whitespace() {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            } else {
                out.push(lc);
                prev_space = false;
            }
        }
    }

    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Checks whether every whitespace-separated token in `query_norm` appears in `haystack_norm`.
pub fn matches_normalized_query(haystack_norm: &str, query_norm: &str) -> bool {
    if query_norm.is_empty() {
        return false;
    }
    query_norm
        .split_ascii_whitespace()
        .all(|tok| haystack_norm.contains(tok))
}

fn push_segment_sep(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

/// Appends plain text from a `mod_api::Text` component tree into `out`.
pub fn append_text_plain(text: &Text, out: &mut String) {
    append_text_plain_depth(text, out, 0);
}

fn append_text_plain_depth(text: &Text, out: &mut String, depth: usize) {
    if depth >= MAX_EXTRACT_DEPTH {
        return;
    }
    match &text.content {
        TextContent::Literal(s) => out.push_str(s),
        TextContent::Translatable {
            key,
            fallback,
            arguments,
        } => {
            if let Some(fb) = fallback {
                out.push_str(fb);
            } else {
                out.push_str(key);
            }
            for arg in arguments {
                push_segment_sep(out);
                append_text_plain_depth(arg, out, depth + 1);
            }
        }
        TextContent::Keybind(k) => out.push_str(k),
        TextContent::Selector { pattern, separator } => {
            out.push_str(pattern);
            if let Some(sep) = separator {
                push_segment_sep(out);
                append_text_plain_depth(sep, out, depth + 1);
            }
        }
        TextContent::Score { name, objective } => {
            out.push_str(name);
            push_segment_sep(out);
            out.push_str(objective);
        }
        TextContent::Nbt {
            path, separator, ..
        } => {
            out.push_str(path);
            if let Some(sep) = separator {
                push_segment_sep(out);
                append_text_plain_depth(sep, out, depth + 1);
            }
        }
        TextContent::Object(obj) => {
            let fallback = match obj {
                TextObject::Player { fallback, .. } | TextObject::Atlas { fallback, .. } => {
                    fallback.as_deref()
                }
            };
            if let Some(fb) = fallback {
                append_text_plain_depth(fb, out, depth + 1);
            }
        }
    }
    for child in &text.children {
        append_text_plain_depth(child, out, depth + 1);
    }
}

/// Appends only visible text keys from an NBT text component (`text`, `extra`, `fallback`, `with`),
/// skipping styling metadata (`color`, `bold`, `italic`, `font`, etc.) to prevent false matches.
pub fn append_nbt_text_component(tag: &Tag, out: &mut String, depth: usize) {
    if depth >= MAX_EXTRACT_DEPTH {
        return;
    }
    match tag {
        Tag::String(s) => out.push_str(&s.to_string_lossy()),
        Tag::List(items) => {
            for item in items {
                append_nbt_text_component(item, out, depth + 1);
            }
        }
        Tag::Compound(map) => {
            if let Some(text) = map
                .iter()
                .find_map(|(k, v)| (k.to_string_lossy() == "text").then_some(v))
            {
                append_nbt_text_component(text, out, depth + 1);
            }
            if let Some(fallback) = map
                .iter()
                .find_map(|(k, v)| (k.to_string_lossy() == "fallback").then_some(v))
            {
                append_nbt_text_component(fallback, out, depth + 1);
            }
            if let Some(with) = map
                .iter()
                .find_map(|(k, v)| (k.to_string_lossy() == "with").then_some(v))
            {
                if let Tag::List(args) = with {
                    for arg in args {
                        push_segment_sep(out);
                        append_nbt_text_component(arg, out, depth + 1);
                    }
                } else {
                    push_segment_sep(out);
                    append_nbt_text_component(with, out, depth + 1);
                }
            }
            if let Some(extra) = map
                .iter()
                .find_map(|(k, v)| (k.to_string_lossy() == "extra").then_some(v))
            {
                append_nbt_text_component(extra, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn append_nbt_lore_lines(tag: &Tag, out: &mut String) {
    match tag {
        Tag::List(lines) => {
            for line in lines {
                push_segment_sep(out);
                append_nbt_text_component(line, out, 0);
            }
        }
        other => {
            push_segment_sep(out);
            append_nbt_text_component(other, out, 0);
        }
    }
}

/// Builds a single normalized search haystack combining item ID, default/hover names,
/// tooltip text (including lore & enchants), and raw custom_name/item_name/lore NBT fallback.
pub fn build_searchable_haystack(
    item_id: &str,
    friendly_name: &str,
    hover_name: &str,
    tooltip_lines: Option<&[TooltipElement]>,
    components: Option<&Components>,
) -> String {
    let mut combined = String::with_capacity(128);
    if !item_id.is_empty() {
        combined.push_str(item_id);
        if let Some((_, path)) = item_id.split_once(':') {
            push_segment_sep(&mut combined);
            combined.push_str(path);
        }
    }
    if !friendly_name.is_empty() {
        push_segment_sep(&mut combined);
        combined.push_str(friendly_name);
    }
    if !hover_name.is_empty() {
        push_segment_sep(&mut combined);
        combined.push_str(hover_name);
    }
    if let Some(elements) = tooltip_lines {
        for elem in elements {
            if let TooltipElement::Text(line, _) = elem {
                push_segment_sep(&mut combined);
                append_text_plain(line, &mut combined);
            }
        }
    }
    if let Some(comp) = components {
        for (k, v) in comp.as_nbt() {
            match k.to_string_lossy().as_ref() {
                "minecraft:custom_name" | "minecraft:item_name" => {
                    push_segment_sep(&mut combined);
                    append_nbt_text_component(v, &mut combined, 0);
                }
                "minecraft:lore" => {
                    append_nbt_lore_lines(v, &mut combined);
                }
                _ => {}
            }
        }
    }
    normalize_search_text(&combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mod_api::Compound;

    #[test]
    fn normalizes_legacy_codes_small_caps_and_whitespace() {
        assert_eq!(
            normalize_search_text("§a§lCOMMON   §r§fKEY"),
            "common key"
        );
        assert_eq!(normalize_search_text("ᴄᴏᴍᴍᴏɴ ᴋᴇʏ"), "common key");
        assert_eq!(normalize_search_text("𝐂𝐨𝐦𝐦𝐨𝐧 Ｋｅｙ"), "common key");
        assert_eq!(
            normalize_search_text("  §x§f§f§0§0§0§0HAMA§7COMMON§fKEY  "),
            "hamacommonkey"
        );
    }

    #[test]
    fn haystack_matches_server_custom_item_name_lore_and_id() {
        let tooltip = vec![
            TooltipElement::Text(Text::literal("§f§lCOMMON KEY"), u32::MAX),
            TooltipElement::Text(
                Text::literal("")
                    .append(Text::literal("HAMA"))
                    .append(Text::literal("COMMON"))
                    .append(Text::literal("KEY")),
                u32::MAX,
            ),
            TooltipElement::Text(
                Text::literal("Key khusus yang digunakan membuka Gacah Crate"),
                u32::MAX,
            ),
            TooltipElement::Text(Text::literal("Pergi ke [/warp crate]"), u32::MAX),
        ];

        let haystack = build_searchable_haystack(
            "minecraft:trial_key",
            "Trial Key",
            "COMMON KEY",
            Some(&tooltip),
            None,
        );

        for query in [
            "common key",
            "hamacommonkey",
            "crate",
            "trial_key",
            "trial key",
            "key crate",
        ] {
            let qn = normalize_search_text(query);
            assert!(
                matches_normalized_query(&haystack, &qn),
                "expected query '{query}' to match haystack '{haystack}'"
            );
        }

        for non_match in ["rare key", "diamond", ""] {
            let qn = normalize_search_text(non_match);
            assert!(
                !matches_normalized_query(&haystack, &qn),
                "expected query '{non_match}' NOT to match"
            );
        }
    }

    #[test]
    fn nbt_extraction_ignores_style_keys_like_color_and_italic() {
        // Guard: searching "red" or "italic" must NOT match NBT styling metadata.
        let mut custom_name_obj = Compound::new();
        custom_name_obj.insert("text".into(), Tag::String("".into()));
        custom_name_obj.insert("color".into(), Tag::String("red".into()));
        custom_name_obj.insert("italic".into(), Tag::Byte(0));

        let mut child1 = Compound::new();
        child1.insert("text".into(), Tag::String("COMMON ".into()));
        child1.insert("color".into(), Tag::String("gold".into()));
        let mut child2 = Compound::new();
        child2.insert("text".into(), Tag::String("KEY".into()));
        child2.insert("bold".into(), Tag::Byte(1));

        custom_name_obj.insert(
            "extra".into(),
            Tag::List(vec![Tag::Compound(child1), Tag::Compound(child2)]),
        );

        let mut lore_line = Compound::new();
        lore_line.insert("text".into(), Tag::String("Membuka Crate".into()));
        lore_line.insert("color".into(), Tag::String("aqua".into()));

        let mut patch = Compound::new();
        patch.insert("minecraft:custom_name".into(), Tag::Compound(custom_name_obj));
        patch.insert("minecraft:lore".into(), Tag::List(vec![Tag::Compound(lore_line)]));
        let components = Components::from(patch);

        let haystack = build_searchable_haystack(
            "minecraft:trial_key",
            "Trial Key",
            "COMMON KEY",
            None,
            Some(&components),
        );

        assert!(matches_normalized_query(&haystack, "common key"));
        assert!(matches_normalized_query(&haystack, "membuka crate"));
        assert!(!matches_normalized_query(&haystack, "red"));
        assert!(!matches_normalized_query(&haystack, "gold"));
        assert!(!matches_normalized_query(&haystack, "aqua"));
        assert!(!matches_normalized_query(&haystack, "italic"));
        assert!(!matches_normalized_query(&haystack, "bold"));
    }

    #[test]
    fn search_bar_layout_centers_above_container_and_clamps_on_small_screens() {
        // Vanilla chest screen_rect: 176x166 centered on 480x270 GUI.
        let screen_rect = [152.0, 52.0, 176.0, 166.0];
        let screen_size = [480.0, 270.0];

        let idle = build_search_bar_layout(screen_rect, screen_size, None, false);
        assert_eq!(idle.bounds, [152.0, 31.0, 176.0, SEARCH_BAR_H]);
        assert!(idle.count_rect.is_none());
        assert!(idle.field_rect[2] > 100.0);

        let active = build_search_bar_layout(screen_rect, screen_size, None, true);
        assert_eq!(active.bounds, idle.bounds);
        let count_rect = active.count_rect.expect("count badge shown when query active");
        assert!(active.field_rect[0] + active.field_rect[2] < count_rect[0]);
        assert!(count_rect[0] + count_rect[2] < active.clear_rect[0]);

        // 720p double chest near top edge clamps y >= 2.0 cleanly.
        let tall_double_chest = [132.0, 12.0, 176.0, 222.0];
        let clamped = build_search_bar_layout(tall_double_chest, [440.0, 240.0], None, false);
        assert_eq!(clamped.bounds[1], 2.0);

        // Custom drag offset persists relative to screen_rect and still clamps inside viewport.
        let dragged = build_search_bar_layout(screen_rect, screen_size, Some([500.0, 500.0]), false);
        assert_eq!(dragged.bounds[0], screen_size[0] - 176.0 - 2.0);
        assert_eq!(dragged.bounds[1], screen_size[1] - SEARCH_BAR_H - 2.0);
    }
}
