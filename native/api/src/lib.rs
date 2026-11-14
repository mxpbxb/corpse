//! Corpse for Horizon: a port of Corpse by Max Henkel, for personal use.
//!
//! When a player dies, what they drop goes into their corpse instead of onto the ground: a body
//! lying where they died (a mannequin wearing their skin, asleep). Right-click it to open its
//! six-row menu; click a stack to take it, or the chest to take everything. The corpse goes away
//! once it is empty. Corpses keep their items across restarts (the world's `corpse/` folder), and
//! on respawn the player is told where their latest corpse lies.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use horizon_client::mod_api::{
    Compound, EntityJoinLevelEvent, Flow, Hooks, Host, InteractEntityEvent, ItemStack, MenuClickedEvent,
    MenuClosedEvent, PlayerDiedEvent, PlayerRespawnedEvent, ServerTickEvent, StackData, Tag, Text,
};

pub fn hooks() -> Box<dyn Hooks> {
    Box::new(Mod)
}

struct Mod;

const KIND: &str = "minecraft:mannequin";
const TAG: &str = "corpse";
const TAG_PREFIX: &str = "corpse_";
const SHOWN: usize = 45;
const INFO: usize = 45;
const TAKE_ALL: usize = 49;

#[derive(Clone, Debug, PartialEq)]
struct Corpse {
    id: u64,
    owner: u128,
    name: String,
    dimension: String,
    pos: [f64; 3],
    created: u64,
    items: Vec<ItemStack>,
}

/// A death whose drops are being gathered: who, and the item entities that fell.
struct Dying {
    owner: u128,
    dimension: String,
    first: Option<[f64; 3]>,
    items: Vec<(u32, [f64; 3])>,
}

#[derive(Default)]
struct State {
    /// The world folder the corpses were read from.
    world: Option<PathBuf>,
    corpses: HashMap<u64, Corpse>,
    dying: Vec<Dying>,
    /// Menus to open: (player, corpse).
    to_open: Vec<(u128, u64)>,
    /// Clicks to act on: (player, window, slot).
    clicks: Vec<(u128, i32, usize)>,
    /// Open corpse menus: player -> (window, corpse).
    open: HashMap<u128, (i32, u64)>,
    /// Corpse bodies to take away: (dimension, corpse).
    to_remove: Vec<(String, u64)>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn new_id(taken: &HashMap<u64, Corpse>) -> u64 {
    let mut id = (SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1) as u64) & 0xffff_ffff_ffff;
    while taken.contains_key(&id) || id == 0 {
        id = id.wrapping_add(1) & 0xffff_ffff_ffff;
    }
    id
}

// ---- Saving ----

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `ItemStack.CODEC` as SNBT: what `item_from_snbt` reads back.
fn stack_snbt(stack: &ItemStack) -> String {
    let mut c = Compound::new();
    c.insert("id".into(), Tag::String(stack.item.as_ref().into()));
    c.insert("count".into(), Tag::Int(stack.count as i32));
    if let Some(components) = horizon_client::item_data::encode_components(stack) {
        c.insert("components".into(), components);
    }
    horizon_client::snbt::to_snbt(&Tag::Compound(c))
}

fn save_text(corpses: &HashMap<u64, Corpse>) -> String {
    let mut list: Vec<&Corpse> = corpses.values().collect();
    list.sort_by_key(|c| (c.created, c.id));
    let mut out = String::from("# Corpse for Horizon: one corpse per `corpse` line, its stacks after it.\n");
    for c in list {
        out.push_str(&format!(
            "corpse\t{:x}\t{:032x}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.id,
            c.owner,
            escape(&c.name),
            escape(&c.dimension),
            c.pos[0],
            c.pos[1],
            c.pos[2],
            c.created
        ));
        for s in &c.items {
            out.push_str(&format!("item\t{}\n", escape(&stack_snbt(s))));
        }
    }
    out
}

/// Corpses from a saved file; each stack read by `read_stack`.
fn load_text(text: &str, mut read_stack: impl FnMut(&str) -> Option<ItemStack>) -> HashMap<u64, Corpse> {
    let mut out: HashMap<u64, Corpse> = HashMap::new();
    let mut cur: Option<u64> = None;
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.first() {
            Some(&"corpse") if f.len() >= 9 => {
                let parsed = (|| {
                    Some(Corpse {
                        id: u64::from_str_radix(f[1], 16).ok()?,
                        owner: u128::from_str_radix(f[2], 16).ok()?,
                        name: unescape(f[3]),
                        dimension: unescape(f[4]),
                        pos: [f[5].parse().ok()?, f[6].parse().ok()?, f[7].parse().ok()?],
                        created: f[8].parse().ok()?,
                        items: Vec::new(),
                    })
                })();
                cur = parsed.map(|c| {
                    let id = c.id;
                    out.insert(id, c);
                    id
                });
            }
            Some(&"item") if f.len() >= 2 => {
                if let Some(id) = cur
                    && let Some(stack) = read_stack(&unescape(f[1]))
                    && let Some(c) = out.get_mut(&id)
                {
                    c.items.push(stack);
                }
            }
            _ => {}
        }
    }
    out
}

fn file_of(world: &std::path::Path) -> PathBuf {
    world.join("corpse").join("corpses.txt")
}

fn save(state: &State) {
    let Some(world) = &state.world else { return };
    let path = file_of(world);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, save_text(&state.corpses)).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ---- The body ----

fn uuid_ints(uuid: u128) -> Vec<i32> {
    (0..4).map(|i| (uuid >> (96 - 32 * i)) as u32 as i32).collect()
}

fn body_nbt(c: &Corpse, yaw: f32) -> Compound {
    let mut profile = Compound::new();
    profile.insert("name".into(), Tag::String(c.name.as_str().into()));
    profile.insert("id".into(), Tag::IntArray(uuid_ints(c.owner)));
    let mut n = Compound::new();
    n.insert("profile".into(), Tag::Compound(profile));
    n.insert("pose".into(), Tag::String("sleeping".into()));
    n.insert("immovable".into(), Tag::Byte(1));
    n.insert("hide_description".into(), Tag::Byte(1));
    n.insert("Invulnerable".into(), Tag::Byte(1));
    n.insert("PersistenceRequired".into(), Tag::Byte(1));
    n.insert("Silent".into(), Tag::Byte(1));
    n.insert("CustomName".into(), Tag::String(format!("{}'s corpse", c.name).as_str().into()));
    n.insert("Rotation".into(), Tag::List(vec![Tag::Float(yaw.to_bits()), Tag::Float(0f32.to_bits())]));
    n.insert(
        "Tags".into(),
        Tag::List(vec![Tag::String(TAG.into()), Tag::String(format!("{TAG_PREFIX}{:x}", c.id).as_str().into())]),
    );
    n
}

fn corpse_of_tags(tags: &[String]) -> Option<u64> {
    tags.iter().find_map(|t| t.strip_prefix(TAG_PREFIX).and_then(|h| u64::from_str_radix(h, 16).ok()))
}

fn bottom_of(dimension: &str) -> f64 {
    if dimension == "minecraft:overworld" { -63.0 } else { 1.0 }
}

// ---- The menu ----

fn named(item: &str, name: String) -> ItemStack {
    let mut stack = ItemStack::new(item, 1);
    let data = StackData { custom_name: Some(Tag::String(name.as_str().into())), ..StackData::default() };
    stack.extra = Some(Box::new(data));
    stack
}

fn menu_items(c: &Corpse) -> Vec<Option<ItemStack>> {
    let mut items: Vec<Option<ItemStack>> = vec![None; 54];
    for (i, s) in c.items.iter().take(SHOWN).enumerate() {
        items[i] = Some(s.clone());
    }
    let more = c.items.len().saturating_sub(SHOWN);
    let info = if more > 0 {
        format!("{}'s corpse ({} more stacks after these)", c.name, more)
    } else {
        format!("{}'s corpse", c.name)
    };
    items[INFO] = Some(named("minecraft:skeleton_skull", info));
    items[TAKE_ALL] = Some(named("minecraft:chest", "Take everything".to_owned()));
    items
}

fn title(c: &Corpse) -> Text {
    Text::literal(format!("{}'s corpse", c.name))
}

impl Mod {
    fn ensure_loaded(&self, host: &mut dyn Host, state: &mut State) {
        let world = host.world_dir();
        if world == state.world {
            return;
        }
        state.corpses = match &world {
            Some(w) => match std::fs::read_to_string(file_of(w)) {
                Ok(text) => load_text(&text, |s| host.item_from_snbt(s)),
                Err(_) => HashMap::new(),
            },
            None => HashMap::new(),
        };
        state.world = world;
        state.open.clear();
    }

    /// A death's gathered drops go into a new corpse where the player fell.
    fn bury(&self, host: &mut dyn Host, state: &mut State, d: Dying) {
        let Some(first) = d.first else { return };
        let player = host.server_players().into_iter().find(|p| p.uuid == d.owner);
        let (name, mut pos, yaw) = match &player {
            Some(p) if p.dimension == d.dimension => (p.name.clone(), p.position, p.yaw),
            Some(p) => (p.name.clone(), first, p.yaw),
            None => ("Someone".to_owned(), first, 0.0),
        };
        pos[1] = pos[1].max(bottom_of(&d.dimension));
        // The stacks of the drops, still lying there.
        let mut found: Vec<(u32, ItemStack)> = Vec::new();
        for (id, at) in &d.items {
            let r = 1.5;
            let near = host.item_entities(&d.dimension, [at[0] - r, at[1] - r, at[2] - r], [at[0] + r, at[1] + r, at[2] + r]);
            if let Some(e) = near.into_iter().find(|e| e.id == *id) {
                found.push((e.id, e.stack.clone()));
            }
        }
        if found.is_empty() {
            return;
        }
        let mut corpse = Corpse {
            id: new_id(&state.corpses),
            owner: d.owner,
            name,
            dimension: d.dimension.clone(),
            pos,
            created: now(),
            items: Vec::new(),
        };
        if host.summon(&d.dimension, KIND, pos, body_nbt(&corpse, yaw)).is_none() {
            // No body (an unloaded chunk, the void): the drops stay where they fell.
            return;
        }
        for (id, stack) in found {
            if host.set_item_entity(&d.dimension, id, ItemStack::new(&stack.item, 0)) {
                corpse.items.push(stack);
            }
        }
        if corpse.items.is_empty() {
            state.to_remove.push((corpse.dimension.clone(), corpse.id));
            return;
        }
        host.tell(
            d.owner,
            Text::literal(format!(
                "Your items are in your corpse at {}, {}, {} ({}).",
                pos[0].floor() as i64,
                pos[1].floor() as i64,
                pos[2].floor() as i64,
                pretty_dim(&corpse.dimension)
            )),
        );
        state.corpses.insert(corpse.id, corpse);
        save(state);
    }

    fn open(&self, host: &mut dyn Host, state: &mut State, player: u128, id: u64) {
        let Some(c) = state.corpses.get(&id) else {
            host.action_bar(player, Text::literal("This corpse is empty."));
            return;
        };
        if let Some(window) = host.open_menu(player, 6, title(c), menu_items(c)) {
            state.open.insert(player, (window, id));
        }
    }

    /// Everyone looking into corpse `id` sees it as it now is; an emptied corpse closes.
    fn refresh(&self, host: &mut dyn Host, state: &mut State, id: u64) {
        let viewers: Vec<u128> = state.open.iter().filter(|(_, (_, c))| *c == id).map(|(p, _)| *p).collect();
        match state.corpses.get(&id) {
            Some(c) => {
                let items = menu_items(c);
                for p in viewers {
                    host.set_menu_items(p, items.clone());
                }
            }
            None => {
                for p in viewers {
                    host.close_menu(p);
                    state.open.remove(&p);
                }
            }
        }
    }

    fn click(&self, host: &mut dyn Host, state: &mut State, player: u128, window: i32, slot: usize) {
        let Some(&(w, id)) = state.open.get(&player) else { return };
        if w != window {
            return;
        }
        let Some(c) = state.corpses.get_mut(&id) else { return };
        let mut full = false;
        if slot == TAKE_ALL {
            let mut kept = Vec::new();
            for s in std::mem::take(&mut c.items) {
                if full {
                    kept.push(s);
                    continue;
                }
                if let Some(rest) = host.give_stack(player, s) {
                    full = true;
                    kept.push(rest);
                }
            }
            c.items = kept;
        } else if slot < SHOWN && slot < c.items.len() {
            let s = c.items.remove(slot);
            if let Some(rest) = host.give_stack(player, s) {
                full = true;
                c.items.insert(slot, rest);
            }
        } else {
            return;
        }
        if full {
            host.action_bar(player, Text::literal("Your inventory is full."));
        }
        if c.items.is_empty() {
            let dim = c.dimension.clone();
            state.corpses.remove(&id);
            state.to_remove.push((dim, id));
        }
        save(state);
        self.refresh(host, state, id);
    }
}

fn pretty_dim(d: &str) -> String {
    match d {
        "minecraft:overworld" => "the Overworld".to_owned(),
        "minecraft:the_nether" => "the Nether".to_owned(),
        "minecraft:the_end" => "the End".to_owned(),
        o => o.to_owned(),
    }
}

impl Hooks for Mod {
    /// A player's death opens a gathering: the item entities their death drops are theirs.
    fn player_died(&self, _: &mut dyn Host, event: &mut PlayerDiedEvent<'_>) {
        if let Ok(mut s) = STATE.lock() {
            s.dying.push(Dying { owner: event.player, dimension: String::new(), first: None, items: Vec::new() });
        }
    }

    fn entity_join_level(&self, _: &mut dyn Host, event: &mut EntityJoinLevelEvent<'_>) -> Flow {
        if event.kind != "minecraft:item" || event.loaded_from_disk {
            return Flow::Continue;
        }
        if let Ok(mut s) = STATE.lock()
            && let Some(d) = s.dying.last_mut()
        {
            match d.first {
                None => {
                    d.first = Some(event.pos);
                    d.dimension = event.dimension.to_owned();
                    d.items.push((event.entity, event.pos));
                }
                Some(f) if d.dimension == event.dimension => {
                    let dist = ((f[0] - event.pos[0]).powi(2) + (f[1] - event.pos[1]).powi(2) + (f[2] - event.pos[2]).powi(2)).sqrt();
                    if dist <= 2.5 {
                        d.items.push((event.entity, event.pos));
                    }
                }
                _ => {}
            }
        }
        Flow::Continue
    }

    /// Right-click a corpse: its menu opens.
    fn interact_entity(&self, host: &mut dyn Host, event: &mut InteractEntityEvent<'_>) -> Flow {
        if event.kind != KIND {
            return Flow::Continue;
        }
        let Ok(id) = u32::try_from(event.entity) else { return Flow::Continue };
        let Some(entity) = host.entity(id) else { return Flow::Continue };
        let Some(corpse) = host.entity_tags(entity).and_then(|t| corpse_of_tags(&t)) else { return Flow::Continue };
        let Some(player) = host.acting_player() else { return Flow::Continue };
        if let Ok(mut s) = STATE.lock()
            && !s.to_open.iter().any(|(p, _)| *p == player.uuid)
        {
            s.to_open.push((player.uuid, corpse));
        }
        Flow::Cancel
    }

    fn menu_clicked(&self, _: &mut dyn Host, event: &mut MenuClickedEvent) {
        let Ok(slot) = usize::try_from(event.slot) else { return };
        if let Ok(mut s) = STATE.lock()
            && s.open.get(&event.uuid).is_some_and(|(w, _)| *w == event.window)
        {
            s.clicks.push((event.uuid, event.window, slot));
        }
    }

    fn menu_closed(&self, _: &mut dyn Host, event: &mut MenuClosedEvent) {
        if let Ok(mut s) = STATE.lock()
            && s.open.get(&event.uuid).is_some_and(|(w, _)| *w == event.window)
        {
            s.open.remove(&event.uuid);
        }
    }

    fn player_respawned(&self, host: &mut dyn Host, event: &mut PlayerRespawnedEvent<'_>) {
        let Ok(s) = STATE.lock() else { return };
        let latest = s.corpses.values().filter(|c| c.owner == event.player).max_by_key(|c| (c.created, c.id));
        if let Some(c) = latest {
            host.tell(
                event.player,
                Text::literal(format!(
                    "Your corpse lies at {}, {}, {} ({}).",
                    c.pos[0].floor() as i64,
                    c.pos[1].floor() as i64,
                    c.pos[2].floor() as i64,
                    pretty_dim(&c.dimension)
                )),
            );
        }
    }

    fn server_tick(&self, host: &mut dyn Host, _: &mut ServerTickEvent<'_>) {
        let Ok(mut guard) = STATE.lock() else { return };
        let state = &mut *guard;
        self.ensure_loaded(host, state);
        for d in std::mem::take(&mut state.dying) {
            self.bury(host, state, d);
        }
        for (player, id) in std::mem::take(&mut state.to_open) {
            self.open(host, state, player, id);
        }
        for (player, window, slot) in std::mem::take(&mut state.clicks) {
            self.click(host, state, player, window, slot);
        }
        for (dim, id) in std::mem::take(&mut state.to_remove) {
            host.run_command(&format!("execute in {dim} run kill @e[type={KIND},tag={TAG_PREFIX}{id:x}]"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escapes_round_trip() {
        let s = "a\tb\\c\nd";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn corpses_survive_the_file() {
        let mut map = HashMap::new();
        map.insert(
            0xabc,
            Corpse {
                id: 0xabc,
                owner: 0x0123_4567_89ab_cdef_0011_2233_4455_6677,
                name: "Steve".into(),
                dimension: "minecraft:overworld".into(),
                pos: [1.5, 64.0, -3.25],
                created: 99,
                items: vec![ItemStack::new("minecraft:stone", 12), ItemStack::new("minecraft:diamond", 3)],
            },
        );
        let text = save_text(&map);
        // Without the game's reader, read each stack's id and count back by hand.
        let back = load_text(&text, |snbt| {
            let id = snbt.split("id:\"").nth(1)?.split('"').next()?.to_owned();
            let count: u32 = snbt.split("count:").nth(1)?.trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
            Some(ItemStack::new(&id, count))
        });
        let c = &back[&0xabc];
        assert_eq!((c.owner, c.name.as_str(), c.pos, c.created), (map[&0xabc].owner, "Steve", [1.5, 64.0, -3.25], 99));
        assert_eq!(c.items.iter().map(|s| (s.item.to_string(), s.count)).collect::<Vec<_>>(),
                   vec![("minecraft:stone".to_owned(), 12), ("minecraft:diamond".to_owned(), 3)]);
    }

    #[test]
    fn the_body_carries_its_corpse_tag() {
        assert_eq!(corpse_of_tags(&["corpse".into(), "corpse_1f".into()]), Some(0x1f));
        assert_eq!(corpse_of_tags(&["other".into()]), None);
        assert_eq!(uuid_ints(0x0000_0001_0000_0002_0000_0003_0000_0004), vec![1, 2, 3, 4]);
    }

    #[test]
    fn the_menu_shows_45_stacks_and_its_buttons() {
        let c = Corpse { id: 1, owner: 1, name: "A".into(), dimension: "d".into(), pos: [0.0; 3], created: 0,
                         items: (0..50).map(|_| ItemStack::new("minecraft:dirt", 1)).collect() };
        let m = menu_items(&c);
        assert!(m[44].is_some() && m[INFO].is_some() && m[TAKE_ALL].is_some());
        assert!(m[46].is_none() && m[53].is_none());
    }
}
